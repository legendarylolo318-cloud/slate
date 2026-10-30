//! Everything the MCP server keeps while it runs: policy, bearer token, the activity log, the
//! approval queue, subscribers, the event watcher and the play log. Created by `start_server`
//! and dropped when it stops, so a stopped server owns nothing.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rmcp::model::ResourceUpdatedNotificationParam;
use rmcp::service::{Peer, RoleServer};
use tokio::sync::oneshot;

use super::activity::Log;
use super::policy::Policy;
use super::stats::{self, PlayRec};
use super::watch::{Event, Watch};

pub const URI_NOW_PLAYING: &str = "slate://now-playing";
pub const URI_QUEUE: &str = "slate://queue";
pub const URI_EVENTS: &str = "slate://events";

/// How long an approval waits for the user before it counts as "Deny".
pub const APPROVAL_TIMEOUT: Duration = Duration::from_secs(60);

pub const EV_APPROVAL_REQUEST: &str = "mcp:approval-request";
pub const EV_APPROVAL_CLOSED: &str = "mcp:approval-closed";

struct Subscriber {
    key: usize,
    peer: Peer<RoleServer>,
    uris: HashSet<String>,
}

pub struct Runtime {
    policy: Mutex<Policy>,
    bearer: Mutex<String>,
    pub log: Mutex<Log>,
    log_path: Option<PathBuf>,
    plays: Mutex<Vec<PlayRec>>,
    plays_path: Option<PathBuf>,
    approvals: Mutex<HashMap<u64, oneshot::Sender<bool>>>,
    next_approval: AtomicU64,
    subs: Mutex<Vec<Subscriber>>,
    watch: Mutex<Option<Watch>>,
    recent_events: Mutex<VecDeque<String>>,
    pub dj: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Runtime {
    pub fn new(policy: Policy, bearer: String, dj: bool, config_dir: Option<PathBuf>) -> Self {
        let log_path = config_dir.as_ref().map(|d| d.join("agent_activity.json"));
        let plays_path = config_dir.as_ref().map(|d| d.join("agent_plays.json"));
        let log = log_path.as_deref().map(Log::load).unwrap_or_default();
        let plays = plays_path
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            policy: Mutex::new(policy),
            bearer: Mutex::new(bearer),
            log: Mutex::new(log),
            log_path,
            plays: Mutex::new(plays),
            plays_path,
            approvals: Mutex::new(HashMap::new()),
            next_approval: AtomicU64::new(1),
            subs: Mutex::new(Vec::new()),
            watch: Mutex::new(None),
            recent_events: Mutex::new(VecDeque::new()),
            dj: AtomicBool::new(dj),
        }
    }

    pub fn policy(&self) -> Policy {
        *lock(&self.policy)
    }

    pub fn set_policy(&self, p: Policy) {
        *lock(&self.policy) = p;
    }

    pub fn bearer(&self) -> String {
        lock(&self.bearer).clone()
    }

    pub fn set_bearer(&self, token: String) {
        *lock(&self.bearer) = token;
    }

    /// Writes the log and the play log. Called when the server stops and every few entries.
    pub fn persist(&self) {
        if let Some(p) = &self.log_path {
            if let Err(e) = lock(&self.log).save(p) {
                log::warn!("could not save the agent activity log: {e}");
            }
        }
        if let Some(p) = &self.plays_path {
            let json = serde_json::to_string(&*lock(&self.plays)).unwrap_or_default();
            if let Err(e) = std::fs::write(p, json) {
                log::warn!("could not save the play log: {e}");
            }
        }
    }

    // ---- approvals ----

    /// Asks the user (through the app) and waits up to 60 s. No answer is a "no".
    pub async fn ask_user(&self, app: &crate::AppHandle, client: &str, summary: &str) -> bool {
        self.ask_user_within(app, client, summary, APPROVAL_TIMEOUT)
            .await
    }

    pub(crate) async fn ask_user_within(
        &self,
        app: &crate::AppHandle,
        client: &str,
        summary: &str,
        wait: Duration,
    ) -> bool {
        let id = self.next_approval.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        lock(&self.approvals).insert(id, tx);
        let _ = app.emit(
            EV_APPROVAL_REQUEST,
            serde_json::json!({ "id": id, "client": client, "summary": summary }),
        );
        match tokio::time::timeout(wait, rx).await {
            Ok(Ok(allow)) => allow,
            _ => {
                lock(&self.approvals).remove(&id);
                let _ = app.emit(EV_APPROVAL_CLOSED, serde_json::json!({ "id": id }));
                false
            }
        }
    }

    /// The user's answer from the app's dialog. False when the request had already ended.
    pub fn answer(&self, id: u64, allow: bool) -> bool {
        match lock(&self.approvals).remove(&id) {
            Some(tx) => tx.send(allow).is_ok(),
            None => false,
        }
    }

    // ---- subscribers ----

    pub fn subscribe(&self, key: usize, peer: Peer<RoleServer>, uri: &str) {
        {
            let mut subs = lock(&self.subs);
            match subs.iter_mut().find(|s| s.key == key) {
                Some(s) => {
                    s.uris.insert(uri.to_string());
                    s.peer = peer;
                }
                None => subs.push(Subscriber {
                    key,
                    peer,
                    uris: HashSet::from([uri.to_string()]),
                }),
            }
        }
        // Events are computed only from the first subscription on.
        lock(&self.watch).get_or_insert_with(Watch::new);
    }

    pub fn unsubscribe(&self, key: usize, uri: &str) {
        let mut subs = lock(&self.subs);
        if let Some(s) = subs.iter_mut().find(|s| s.key == key) {
            s.uris.remove(uri);
        }
        subs.retain(|s| !s.uris.is_empty() && !s.peer.is_transport_closed());
        if subs.is_empty() {
            *lock(&self.watch) = None;
        }
    }

    /// Whether anyone subscribed to `uri` (closed connections are dropped first).
    pub fn has_subscribers(&self, uri: &str) -> bool {
        let mut subs = lock(&self.subs);
        subs.retain(|s| !s.peer.is_transport_closed());
        if subs.is_empty() {
            *lock(&self.watch) = None;
        }
        subs.iter().any(|s| s.uris.contains(uri))
    }

    pub fn any_subscribers(&self) -> bool {
        !lock(&self.subs).is_empty()
    }

    /// Tells subscribers of `uri` that it changed.
    pub fn notify_changed(&self, uri: &str) {
        let peers: Vec<Peer<RoleServer>> = lock(&self.subs)
            .iter()
            .filter(|s| s.uris.contains(uri))
            .map(|s| s.peer.clone())
            .collect();
        for peer in peers {
            let uri = uri.to_string();
            tokio::spawn(async move {
                let _ = peer
                    .notify_resource_updated(ResourceUpdatedNotificationParam::new(uri))
                    .await;
            });
        }
    }

    /// Runs `f` on the watcher, if a client is subscribed.
    pub fn with_watch<T>(&self, f: impl FnOnce(&mut Watch) -> T) -> Option<T> {
        lock(&self.watch).as_mut().map(f)
    }

    /// Records an event and tells the clients subscribed to `slate://events` (they read the resource
    /// for the new event).
    pub fn publish_event(&self, ev: &Event) {
        let data = match ev {
            Event::TrackSkipped {
                track_id,
                position_secs,
            } => {
                serde_json::json!({ "event": ev.name(), "track_id": track_id, "position_seconds": position_secs })
            }
            Event::TrackReplayed { track_id } => {
                serde_json::json!({ "event": ev.name(), "track_id": track_id })
            }
            Event::QueueLow { remaining } => {
                serde_json::json!({ "event": ev.name(), "remaining": remaining })
            }
        };
        {
            let mut r = lock(&self.recent_events);
            r.push_back(data.to_string());
            while r.len() > 20 {
                r.pop_front();
            }
        }
        self.notify_changed(URI_EVENTS);
    }

    pub fn recent_events(&self) -> Vec<String> {
        lock(&self.recent_events).iter().cloned().collect()
    }

    // ---- play log ----

    pub fn record_play(&self, rec: PlayRec) {
        stats::push(&mut lock(&self.plays), rec);
    }

    pub fn stats(&self, period: u64, limit: usize) -> stats::Stats {
        stats::stats(&lock(&self.plays), crate::now_secs(), period, limit)
    }
}

/// Convenience for the app: the runtime, if the server is running.
pub fn current(state: &crate::AppState) -> Option<Arc<Runtime>> {
    if !state.mcp_active.load(Ordering::Relaxed) {
        return None;
    }
    state
        .mcp_rt
        .read()
        .ok()
        .and_then(|g| g.as_ref().map(Arc::clone))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt() -> Runtime {
        Runtime::new(Policy::default(), String::new(), false, None)
    }

    #[test]
    fn a_fresh_runtime_owns_no_subscribers_or_watcher() {
        let r = rt();
        assert!(!r.any_subscribers());
        assert!(r.with_watch(|_| ()).is_none());
        assert!(r.recent_events().is_empty());
        assert!(!r.dj.load(Ordering::Relaxed));
    }

    #[test]
    fn policy_and_bearer_can_change_while_running() {
        let r = rt();
        assert!(!r.policy().read_only);
        r.set_policy(Policy {
            read_only: true,
            ask_destructive: false,
        });
        assert!(r.policy().read_only && !r.policy().ask_destructive);
        r.set_bearer("abc".into());
        assert_eq!(r.bearer(), "abc");
    }

    #[test]
    fn answering_an_unknown_approval_is_harmless() {
        assert!(!rt().answer(99, true));
    }

    #[test]
    fn the_play_log_feeds_stats() {
        let r = rt();
        let now = crate::now_secs();
        r.record_play(PlayRec {
            t: now,
            id: 1,
            title: "T".into(),
            artist: "A".into(),
        });
        let s = r.stats(86_400, 5);
        assert_eq!(s.plays, 1);
        assert_eq!(s.top_artists[0].name, "A");
    }

    #[tokio::test]
    async fn an_unanswered_approval_is_denied_after_the_timeout() {
        let r = rt();
        let app = crate::AppHandle::new(crate::events::Events::null());
        let allowed = r
            .ask_user_within(
                &app,
                "Hermes",
                "delete playlist “X”",
                Duration::from_millis(30),
            )
            .await;
        assert!(!allowed);
        // The request is gone: a late answer finds nothing.
        assert!(!r.answer(1, true));
    }

    #[tokio::test]
    async fn an_answer_reaches_the_waiting_call() {
        let r = Arc::new(rt());
        let app = crate::AppHandle::new(crate::events::Events::null());
        let r2 = Arc::clone(&r);
        let waiting = tokio::spawn(async move {
            r2.ask_user_within(&app, "Hermes", "clear the queue", Duration::from_secs(5))
                .await
        });
        // The request registers itself as id 1.
        for _ in 0..100 {
            if r.answer(1, true) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(waiting.await.expect("task"));
    }
}
