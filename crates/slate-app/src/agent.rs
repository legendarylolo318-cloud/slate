//! The app's side of the MCP upgrade: the "allow or deny" dialog for agent actions, the agent
//! options and activity list in Settings → Integrations, and the agent-DJ wait before the
//! autoplay radio. Nothing here runs unless the MCP server is on.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde_json::Value;
use slate_core::commands::mcp as mcp_cmds;
use slate_core::mcp::activity::Entry;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::backend::Backend;
use crate::bootstrap::{AppWindow, SettingsState};

/// How long the DJ waits for the agent before the autoplay radio takes over.
pub const DJ_WAIT: Duration = Duration::from_secs(10);

/// Approvals waiting for the user, one dialog at a time: (id, client, summary).
static PENDING: Mutex<VecDeque<(u64, String, String)>> = Mutex::new(VecDeque::new());
/// Bumped for every DJ wait, so a stale timer does nothing.
static DJ_EPOCH: AtomicU64 = AtomicU64::new(0);
static DJ_WAITING: AtomicBool = AtomicBool::new(false);

/// Adds a request; returns true when it is the one to show now.
fn enqueue(id: u64, client: String, summary: String) -> bool {
    let Ok(mut q) = PENDING.lock() else {
        return false;
    };
    q.push_back((id, client, summary));
    q.len() == 1
}

/// Drops request `id`; returns the next one to show if the front changed.
fn remove(id: u64) -> (bool, Option<(u64, String, String)>) {
    let Ok(mut q) = PENDING.lock() else {
        return (false, None);
    };
    let was_front = q.front().is_some_and(|f| f.0 == id);
    q.retain(|f| f.0 != id);
    (was_front, q.front().cloned())
}

fn front() -> Option<(u64, String, String)> {
    PENDING.lock().ok().and_then(|q| q.front().cloned())
}

/// "9:41" style time for the activity list.
fn clock(unix: u64) -> String {
    let day = unix % 86_400;
    format!("{:02}:{:02}", day / 3600, (day % 3600) / 60)
}

pub fn activity_line(e: &Entry) -> String {
    let undone = if e.undone { " (undone)" } else { "" };
    format!(
        "{} · {} · {} — {}{}",
        clock(e.time),
        e.client,
        e.tool,
        e.result,
        undone
    )
}

/// Options and activity list for Settings → Integrations.
pub(crate) fn fill_agent_options(w: &AppWindow, state: &slate_core::AppState) {
    let o = mcp_cmds::mcp_get_options(state);
    let s = w.global::<SettingsState>();
    s.set_agent_read_only(o.read_only);
    s.set_agent_ask(o.ask_destructive);
    s.set_agent_dj(o.agent_dj);
    s.set_agent_has_bearer(!o.bearer.is_empty());
    let entries = mcp_cmds::mcp_activity(state, 40);
    let lines: Vec<SharedString> = entries
        .iter()
        .map(|e| SharedString::from(activity_line(e)))
        .collect();
    let ids: Vec<i32> = entries.iter().map(|e| e.id as i32).collect();
    s.set_agent_activity(ModelRc::new(VecModel::from(lines)));
    s.set_agent_activity_ids(ModelRc::new(VecModel::from(ids)));
}

impl Backend {
    /// `mcp:approval-request` from the server.
    pub(crate) fn approval_requested(&self, payload: &Value) {
        let (Some(id), client, summary) = (
            payload.get("id").and_then(Value::as_u64),
            payload
                .get("client")
                .and_then(Value::as_str)
                .unwrap_or("An agent"),
            payload.get("summary").and_then(Value::as_str).unwrap_or(""),
        ) else {
            return;
        };
        if enqueue(id, client.to_string(), summary.to_string()) {
            self.show_approval();
        }
    }

    /// `mcp:approval-closed` (timed out).
    pub(crate) fn approval_closed(&self, payload: &Value) {
        if let Some(id) = payload.get("id").and_then(Value::as_u64) {
            let (was_front, _) = remove(id);
            if was_front {
                self.show_approval();
            }
        }
    }

    fn show_approval(&self) {
        let next = front();
        self.ui(move |w| match next {
            Some((id, client, summary)) => {
                w.set_approval_id(id as i32);
                w.set_approval_text(format!("{client} wants to {summary}").into());
                w.set_approval_open(true);
            }
            None => w.set_approval_open(false),
        });
    }

    /// Allow / Deny in the dialog.
    pub fn approval_answer(&self, id: i32, allow: bool) {
        let id = u64::try_from(id).unwrap_or(0);
        remove(id);
        if let Some(state) = self.state() {
            mcp_cmds::mcp_answer_approval(&state, id, allow);
        }
        self.show_approval();
    }

    // ---- settings ----

    pub(crate) fn agent_option(&self, which: &str, on: bool) -> Result<(), String> {
        let state = self.state().ok_or("Not ready")?;
        mcp_cmds::mcp_set_option(&state, which, on).map_err(|e| e.log_safe())
    }

    pub(crate) fn agent_bearer_action(&self, action: &str) {
        let Some(state) = self.state() else { return };
        match action {
            "agent-bearer-generate" => match mcp_cmds::mcp_generate_bearer(&state) {
                Ok(token) => {
                    self.copy_text(
                        token,
                        "Token copied. Clients must send it as “Authorization: Bearer …”.",
                    );
                }
                Err(e) => self.settings_status(format!("Could not make a token: {}", e.log_safe())),
            },
            "agent-bearer-copy" => {
                let token = mcp_cmds::mcp_get_options(&state).bearer;
                if !token.is_empty() {
                    self.copy_text(token, "Token copied");
                }
            }
            "agent-bearer-clear" => {
                if let Err(e) = mcp_cmds::mcp_clear_bearer(&state) {
                    self.settings_status(format!("Could not remove the token: {}", e.log_safe()));
                } else {
                    self.settings_status("Token removed: the server accepts requests without one.");
                }
            }
            _ => {}
        }
        self.load_settings_ui();
    }

    fn copy_text(&self, text: String, done: &'static str) {
        let this = self.clone();
        self.rt.spawn(async move {
            let r = tokio::task::spawn_blocking(move || {
                arboard::Clipboard::new().and_then(|mut c| c.set_text(text))
            })
            .await;
            match r {
                Ok(Ok(())) => this.settings_status(done),
                _ => this.settings_status("Could not access the clipboard."),
            }
        });
    }

    pub fn agent_undo(&self, id: i32) {
        let id = u64::try_from(id).unwrap_or(0);
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            let msg = match mcp_cmds::mcp_undo(&state, this.app.clone(), id).await {
                Ok(m) => m,
                Err(e) => e,
            };
            this.settings_status(msg);
            this.load_settings_ui();
        });
    }

    // ---- agent DJ ----

    /// The queue ran dry while an agent could still refill it: wait once, then let the normal
    /// autoplay take over. Returns true when a wait started.
    pub(crate) fn dj_wait(&self) -> bool {
        let Some(state) = self.state() else {
            return false;
        };
        if !slate_core::mcp::hooks::dj_waiting(&state) || DJ_WAITING.swap(true, Ordering::SeqCst) {
            return false;
        }
        let epoch = DJ_EPOCH.fetch_add(1, Ordering::SeqCst) + 1;
        let this = self.clone();
        // The only timer: it exists for these ten seconds and ends with them.
        self.rt.spawn(async move {
            tokio::time::sleep(DJ_WAIT).await;
            if DJ_EPOCH.load(Ordering::SeqCst) != epoch || !DJ_WAITING.swap(false, Ordering::SeqCst)
            {
                return;
            }
            // Nobody refilled the queue: the existing autoplay radio.
            if let Some(ctl) = this.controller() {
                ctl.allow_autoplay_once();
                this.next();
            }
        });
        true
    }

    /// Tracks arrived while waiting: cancel the timer and continue playback from them.
    pub(crate) fn dj_refilled(&self) {
        if DJ_WAITING.swap(false, Ordering::SeqCst) {
            DJ_EPOCH.fetch_add(1, Ordering::SeqCst);
            self.next();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The approval queue is global, so these tests take turns.
    static TURN: Mutex<()> = Mutex::new(());

    fn reset() {
        if let Ok(mut q) = PENDING.lock() {
            q.clear();
        }
    }

    #[test]
    fn approvals_show_one_at_a_time_in_order() {
        let _turn = TURN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset();
        assert!(enqueue(1, "Hermes".into(), "delete playlist “A”".into()));
        assert!(!enqueue(2, "Hermes".into(), "clear the queue".into()));
        assert_eq!(front().map(|f| f.0), Some(1));
        let (was_front, next) = remove(1);
        assert!(was_front);
        assert_eq!(next.map(|n| n.0), Some(2));
        let (was_front, next) = remove(2);
        assert!(was_front && next.is_none());
        reset();
    }

    #[test]
    fn closing_a_waiting_request_leaves_the_shown_one() {
        let _turn = TURN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset();
        enqueue(10, "a".into(), "x".into());
        enqueue(11, "a".into(), "y".into());
        let (was_front, _) = remove(11);
        assert!(!was_front);
        assert_eq!(front().map(|f| f.0), Some(10));
        reset();
    }

    #[test]
    fn activity_lines_read_well() {
        let e = Entry {
            id: 1,
            time: 3600 * 9 + 60 * 41,
            client: "Hermes".into(),
            tool: "add_to_queue".into(),
            summary: String::new(),
            result: "ok".into(),
            undo: None,
            undone: true,
            not_undoable: false,
        };
        assert_eq!(
            activity_line(&e),
            "09:41 · Hermes · add_to_queue — ok (undone)"
        );
    }
}
