//! The iroh transport for listening parties (cargo feature `party`).
//!
//! An iroh endpoint exists only while a party is running: `Host::start` / `Guest::join` create
//! it, `end` / `leave` close it. Peers talk over one bidirectional QUIC stream per guest with
//! length-prefixed JSON messages (see `proto`). Tickets and peer addresses are never logged.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use iroh::endpoint::{presets, RecvStream, SendStream};
use iroh::{Endpoint, EndpointAddr, RelayMode};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use super::chat::{Chat, Refused};
use super::proto::{
    self, Flood, GuestMsg, HostMsg, PingSample, PlayState, RateLimit, ReqView, Roster,
    MAX_MESSAGE_BYTES, MAX_REQUEST_VIEWS, PROTOCOL_VERSION,
};
use super::reactions::Reactions;
use super::requests::{
    Policy, Reject, Request, RequestBook, RequestMode, RequestTrack, Submitted, Vote, VoteSkip,
    RECENT_HISTORY,
};

const ALPN: &[u8] = b"slate/party/1";
const HANDSHAKE: Duration = Duration::from_secs(10);
const ONLINE_WAIT: Duration = Duration::from_secs(12);
/// Connections that may be in the handshake at once (before the invite secret is checked).
const MAX_HANDSHAKES: usize = 8;
/// Messages a guest reads while waiting for its pongs at join; a host sending more is broken.
const MAX_HANDSHAKE_MESSAGES: usize = 64;

/// What the app is told.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Guest side: a message from the host.
    Host(HostMsg),
    /// Host side.
    GuestJoined(String),
    GuestLeft(String),
    /// A guest asked for a track; the app looks it up and calls [`Host::submit`].
    Suggestion {
        guest_id: u64,
        guest: String,
        track_id: u64,
    },
    /// The waiting requests changed (host side).
    RequestsChanged,
    /// A co-host decided a request (host side); the app calls [`Host::decide`].
    CohostDecide {
        request_id: Option<u64>,
        approve: bool,
        by: String,
    },
    CohostSkip(String),
    CohostMove {
        from: u32,
        to: u32,
        by: String,
    },
    /// Enough guests voted to skip (host side).
    VoteSkipPassed,
    /// Someone reacted to the playing track (host side); `tally` is the track's counts.
    Reaction {
        from: String,
        emoji: u8,
        tally: Vec<(u8, u32)>,
    },
    /// A chat message (host side: from a guest or the host itself).
    Chat {
        from: String,
        text: String,
    },
    /// The connection ended on purpose (guest side: ended, kicked, left) or the party closed.
    Closed(String),
    /// Guest side: the connection dropped without a goodbye; the host may come back.
    Lost(String),
}

#[derive(Serialize, Deserialize)]
struct Ticket {
    a: EndpointAddr,
    s: String,
}

fn encode_ticket(t: &Ticket) -> Result<String, String> {
    let json = serde_json::to_vec(t).map_err(|e| e.to_string())?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json))
}

fn decode_ticket(s: &str) -> Result<Ticket, String> {
    let raw = proto::parse_ticket(s).ok_or("That is not a Slate party link.")?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| "That is not a Slate party link.".to_string())?;
    serde_json::from_slice(&bytes).map_err(|_| "That party link is damaged.".to_string())
}

async fn bind(relay: Option<&str>, accept: bool) -> Result<Endpoint, String> {
    bind_with(relay, accept, None).await
}

async fn bind_with(
    relay: Option<&str>,
    accept: bool,
    key: Option<iroh::SecretKey>,
) -> Result<Endpoint, String> {
    let mut b = Endpoint::builder(presets::N0);
    if let Some(key) = key {
        b = b.secret_key(key);
    }
    if accept {
        b = b.alpns(vec![ALPN.to_vec()]);
    }
    if relay.map(str::trim) == Some("none") {
        // Direct connections only (same network, and the tests).
        b = b.relay_mode(RelayMode::Disabled);
    } else if let Some(url) = relay.map(str::trim).filter(|u| !u.is_empty()) {
        let url = url
            .parse()
            .map_err(|_| "The custom relay URL is not valid.".to_string())?;
        b = b.relay_mode(RelayMode::custom([url]));
    }
    b.bind()
        .await
        .map_err(|e| format!("Could not start networking: {e}"))
}

/// What `slate --self-test --party-relay` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayReport {
    /// The relay this Slate would use ("default relays" or the configured URL).
    pub which: String,
    /// The relay our endpoint connected to.
    pub home: String,
    pub millis: u128,
}

/// Starts an endpoint (nothing else) and waits up to `wait` for it to reach its relay. Used by
/// the self-test only; the endpoint is closed again before returning.
pub async fn check_relay(relay: Option<&str>, wait: Duration) -> Result<RelayReport, String> {
    let which = match relay.map(str::trim) {
        Some("none") => return Err("Relays are disabled (\"none\"): nothing to check.".into()),
        Some(u) if !u.is_empty() => u.to_string(),
        _ => "default relays".to_string(),
    };
    let endpoint = bind(relay, false).await?;
    let t0 = Instant::now();
    let online = tokio::time::timeout(wait, endpoint.online()).await.is_ok();
    let millis = t0.elapsed().as_millis();
    let home = endpoint.addr().relay_urls().next().map(ToString::to_string);
    endpoint.close().await;
    match (online, home) {
        (true, Some(home)) => Ok(RelayReport {
            which,
            home,
            millis,
        }),
        _ => Err(format!(
            "Could not reach the {which} within {} seconds. Check your network, firewall or proxy.",
            wait.as_secs()
        )),
    }
}

async fn write_frame(w: &mut SendStream, bytes: &[u8]) -> Result<(), String> {
    let len = u16::try_from(bytes.len()).map_err(|_| "message too large".to_string())?;
    w.write_all(&len.to_be_bytes())
        .await
        .map_err(|e| e.to_string())?;
    w.write_all(bytes).await.map_err(|e| e.to_string())
}

async fn read_frame(r: &mut RecvStream) -> Result<Vec<u8>, String> {
    let mut len = [0u8; 2];
    r.read_exact(&mut len).await.map_err(|e| e.to_string())?;
    let len = usize::from(u16::from_be_bytes(len));
    if len > MAX_MESSAGE_BYTES {
        return Err("message too large".into());
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await.map_err(|e| e.to_string())?;
    Ok(buf)
}

/// Lets a final message reach the peer before the connection is closed on top of it.
async fn linger(send: &mut SendStream, conn: &iroh::endpoint::Connection) {
    let _ = send.finish();
    let _ = tokio::time::timeout(Duration::from_millis(600), conn.closed()).await;
}

fn secret_matches(given: &str, expected: &str) -> bool {
    let (a, b) = (given.as_bytes(), expected.as_bytes());
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        diff |= usize::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(1));
    }
    diff == 0
}

/// What the successor was given: the key to host under and the invite secret.
#[derive(Clone, PartialEq, Eq)]
pub struct Heir {
    /// URL-safe base64 of the 32-byte key.
    pub key: String,
    pub secret: String,
}

impl std::fmt::Debug for Heir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Heir(..)")
    }
}

impl Heir {
    fn encode_key(k: &iroh::SecretKey) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(k.to_bytes())
    }

    fn secret_key(&self) -> Result<iroh::SecretKey, String> {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&self.key)
            .map_err(|_| "The handoff key is damaged.".to_string())?;
        let arr: [u8; 32] = bytes
            .try_into()
            .map_err(|_| "The handoff key is damaged.".to_string())?;
        Ok(iroh::SecretKey::from_bytes(&arr))
    }
}

/// The invite link of a party that continues under `id` (the public id from
/// [`HostMsg::Successor`]) with `secret`. No addresses: the guest finds the new host
/// through iroh's address lookup, like any endpoint known only by its id.
pub fn heir_link(id: &str, secret: &str) -> Result<String, String> {
    let id: iroh::PublicKey = id.parse().map_err(|_| "The new host's id is not valid.")?;
    let ticket = encode_ticket(&Ticket {
        a: EndpointAddr::new(id),
        s: secret.to_string(),
    })?;
    Ok(proto::ticket_link(&ticket))
}

// ---------------------------------------------------------------- host

struct HostState {
    play: PlayState,
    queue: Vec<u64>,
    roster: Roster,
    limit: RateLimit,
    kicks: HashMap<u64, CancellationToken>,
    book: RequestBook,
    vote: VoteSkip,
    reactions: Reactions,
    chat: Chat,
    /// Every upcoming track id (capped) and who asked for the first few.
    upcoming: Vec<u64>,
    by: Vec<String>,
    /// Recently played ids, oldest first.
    recent: Vec<u64>,
    /// Handed to the successor, who hosts under it after this host is gone, and the
    /// guest it was handed to. A new successor gets a new key, so a guest who stopped
    /// being the successor (kicked, left, a co-host was named) cannot host the party.
    /// Kept beside the roster: the successor and its key are decided under one lock,
    /// or a join racing a leave could hand the key to a guest who is no longer next.
    heir: (iroh::SecretKey, Option<u64>),
}

impl HostState {
    /// The successor, the key it hosts under and whether that key is new (the successor
    /// changed). None while there are no guests.
    fn assign_heir(&mut self) -> Option<(iroh::SecretKey, u64, bool)> {
        let successor = super::requests::successor(&self.roster.successor_input())?;
        let rotated = self.heir.1.is_some_and(|g| g != successor);
        if rotated {
            self.heir.0 = iroh::SecretKey::generate();
        }
        self.heir.1 = Some(successor);
        Some((self.heir.0.clone(), successor, rotated))
    }
}

/// Who a broadcast is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Audience {
    All,
    Guest(u64),
    Cohosts,
}

struct HostInner {
    /// The invite's secret. A kick replaces it, so the kicked guest's link stops working.
    secret: Mutex<String>,
    started: Instant,
    host_name: String,
    tx: broadcast::Sender<(Audience, HostMsg)>,
    state: Mutex<HostState>,
    events: mpsc::UnboundedSender<Event>,
}

impl HostInner {
    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    fn send(&self, to: Audience, m: HostMsg) {
        let _ = self.tx.send((to, m));
    }

    /// The waiting requests, for co-hosts and the host's own tray.
    fn views(&self, s: &HostState) -> (Vec<ReqView>, u32) {
        let all = s.book.pending();
        let v = all
            .iter()
            .take(MAX_REQUEST_VIEWS)
            .map(|r| ReqView {
                id: r.id,
                guest: r.guest.clone(),
                title: proto::clip(&r.track.title, 48),
                artist: proto::clip(&r.track.artist, 32),
                quality: proto::clip(&r.track.quality, 16),
                unavailable: s.book.unavailable_for(r.track.id) as u32,
            })
            .collect();
        (v, all.len() as u32)
    }

    fn push_requests(&self) {
        let (pending, total) = self.views(&self.lock());
        self.send(Audience::Cohosts, HostMsg::Requests { pending, total });
        let _ = self.events.send(Event::RequestsChanged);
    }

    fn push_guests(&self) {
        let (guests, cohosts) = {
            let s = self.lock();
            (s.roster.names(), s.roster.cohosts())
        };
        self.send(Audience::All, HostMsg::Guests { guests, cohosts });
        self.push_heir(None);
    }

    /// Tells the guests where the party continues if this host goes away, and gives the
    /// successor (a co-host, else the earliest guest) the key to host it.
    ///
    /// `only`: just that guest (a joiner, whose connection subscribes after the roster
    /// broadcast); `None`: everyone.
    fn push_heir(&self, only: Option<u64>) {
        // The broadcasts go out under the state lock too, so two pushes racing each
        // other reach the guests in the order their keys were decided.
        let mut s = self.lock();
        let Some((key, successor, rotated)) = s.assign_heir() else {
            return;
        };
        // A new key moves the party's next home: everyone hears it, not only `only`.
        let only = if rotated { None } else { only };
        let secret = self.secret().clone();
        self.send(
            only.map_or(Audience::All, Audience::Guest),
            HostMsg::Successor {
                id: key.public().to_string(),
                secret,
            },
        );
        if only.is_none_or(|g| g == successor) {
            self.send(
                Audience::Guest(successor),
                HostMsg::Heir {
                    key: proto::Redacted(Heir::encode_key(&key)),
                },
            );
        }
        drop(s);
    }

    fn secret(&self) -> std::sync::MutexGuard<'_, String> {
        self.secret
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HostState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

pub struct Host {
    endpoint: Endpoint,
    inner: Arc<HostInner>,
    cancel: CancellationToken,
}

fn new_secret() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

impl Host {
    /// Starts a party. `relay`: a self-hosted `iroh-relay` URL, or None for the default relays.
    pub async fn start(
        relay: Option<&str>,
        host_name: &str,
        policy: Policy,
        vote_percent: Option<u8>,
        events: mpsc::UnboundedSender<Event>,
    ) -> Result<Self, String> {
        Self::start_as(relay, host_name, policy, vote_percent, events, None).await
    }

    /// [`Host::start`] continuing a party whose host went away: `heir` is what the old
    /// host gave this guest (see [`Heir`]), so the party keeps its public id and invite
    /// secret and the other guests can find it without a new link.
    pub async fn start_as(
        relay: Option<&str>,
        host_name: &str,
        policy: Policy,
        vote_percent: Option<u8>,
        events: mpsc::UnboundedSender<Event>,
        heir: Option<Heir>,
    ) -> Result<Self, String> {
        let (key, secret) = match heir {
            Some(h) => (h.secret_key()?, h.secret),
            None => (iroh::SecretKey::generate(), new_secret()),
        };
        // The key the party continues under if this host goes away in turn.
        let heir_key = iroh::SecretKey::generate();
        let endpoint = bind_with(relay, true, Some(key)).await?;
        if relay.map(str::trim) != Some("none") {
            let _ = tokio::time::timeout(ONLINE_WAIT, endpoint.online()).await;
        }
        // Checked once here, so `link` cannot fail later.
        encode_ticket(&Ticket {
            a: endpoint.addr(),
            s: secret.clone(),
        })?;
        let (tx, _) = broadcast::channel(64);
        let inner = Arc::new(HostInner {
            secret: Mutex::new(secret),
            started: Instant::now(),
            host_name: proto::clean_name(host_name),
            tx,
            state: Mutex::new(HostState {
                play: PlayState::default(),
                queue: Vec::new(),
                roster: Roster::default(),
                limit: RateLimit::default(),
                kicks: HashMap::new(),
                book: RequestBook::new(policy),
                vote: VoteSkip::new(vote_percent),
                reactions: Reactions::default(),
                chat: Chat::default(),
                upcoming: Vec::new(),
                by: Vec::new(),
                recent: Vec::new(),
                heir: (heir_key, None),
            }),
            events,
        });
        let cancel = CancellationToken::new();
        tokio::spawn(accept_loop(
            endpoint.clone(),
            Arc::clone(&inner),
            cancel.clone(),
        ));
        tokio::spawn(heartbeat_loop(Arc::clone(&inner), cancel.clone()));
        Ok(Self {
            endpoint,
            inner,
            cancel,
        })
    }

    /// This host's public id, as announced to guests in [`HostMsg::Successor`] when this
    /// party was the heir of an earlier one.
    pub fn id(&self) -> String {
        self.endpoint.id().to_string()
    }

    /// The invite: `slate://party/<ticket>`. It changes after a kick.
    pub fn link(&self) -> String {
        let ticket = encode_ticket(&Ticket {
            a: self.endpoint.addr(),
            s: self.inner.secret().clone(),
        })
        .unwrap_or_default();
        proto::ticket_link(&ticket)
    }

    /// The host's own reaction to the playing track (guest id 0 is never a guest).
    pub fn react(&self, e: u8) -> bool {
        let counted = {
            let mut s = self.inner.lock();
            let now = self.inner.now_ms();
            s.reactions.add(0, e, now).then(|| s.reactions.tally())
        };
        let Some(tally) = counted else { return false };
        let from = self.inner.host_name.clone();
        self.inner.send(
            Audience::All,
            HostMsg::Reaction {
                from: from.clone(),
                e,
            },
        );
        let _ = self.inner.events.send(Event::Reaction {
            from,
            emoji: e,
            tally,
        });
        true
    }

    /// The host's own chat message.
    pub fn chat(&self, text: &str) -> Result<(), Refused> {
        let line = {
            let mut s = self.inner.lock();
            let now = self.inner.now_ms();
            s.chat.post(0, &self.inner.host_name, text, now)?
        };
        self.inner.send(
            Audience::All,
            HostMsg::Chat {
                from: line.from.clone(),
                text: line.text.clone(),
            },
        );
        let _ = self.inner.events.send(Event::Chat {
            from: line.from,
            text: line.text,
        });
        Ok(())
    }

    /// Mutes or unmutes a guest's chat. False when no guest has that name.
    pub fn set_chat_muted(&self, name: &str, muted: bool) -> bool {
        let mut s = self.inner.lock();
        let id = s
            .roster
            .guests()
            .iter()
            .find(|g| g.name == name)
            .map(|g| g.id);
        match id {
            Some(id) => {
                s.chat.set_muted(id, muted);
                true
            }
            None => false,
        }
    }

    pub fn guest_names(&self) -> Vec<String> {
        self.inner.lock().roster.names()
    }

    /// A track started (or restarted).
    pub fn track_changed(&self, id: u64, position_ms: u64, playing: bool) {
        let at_ms = self.inner.now_ms();
        {
            let mut s = self.inner.lock();
            if let Some(prev) = s.play.track.filter(|p| *p != id) {
                s.recent.push(prev);
                let extra = s.recent.len().saturating_sub(RECENT_HISTORY);
                s.recent.drain(..extra);
            }
            s.vote.reset();
            s.reactions.new_track();
            let HostState {
                book,
                upcoming,
                recent,
                ..
            } = &mut *s;
            book.retain_unavailable(|t| t == id || upcoming.contains(&t) || recent.contains(&t));
            s.play = PlayState {
                track: Some(id),
                position_ms,
                playing,
                at_ms,
            };
        }
        self.inner.send(
            Audience::All,
            HostMsg::Track {
                id,
                position_ms,
                playing,
                at_ms,
            },
        );
    }

    /// Pause, resume or seek.
    pub fn state_changed(&self, position_ms: u64, playing: bool) {
        let at_ms = self.inner.now_ms();
        {
            let mut s = self.inner.lock();
            s.play.position_ms = position_ms;
            s.play.playing = playing;
            s.play.at_ms = at_ms;
        }
        self.inner.send(
            Audience::All,
            HostMsg::State {
                position_ms,
                playing,
                at_ms,
            },
        );
    }

    /// The upcoming queue changed; the next few ids go to the guests. `by[i]` names who asked
    /// for `upcoming[i]` (empty for the host's own tracks).
    pub fn queue_changed(&self, upcoming: &[u64], by: &[String]) {
        let next = proto::prefetch_list(upcoming);
        let by: Vec<String> = by
            .iter()
            .take(next.len())
            .map(|n| proto::clip(n, proto::MAX_NAME_CHARS))
            .collect();
        {
            let mut s = self.inner.lock();
            s.queue = next.clone();
            s.by = by.clone();
            s.upcoming = upcoming.iter().copied().take(1000).collect();
        }
        self.inner.send(Audience::All, HostMsg::Queue { next, by });
    }

    pub fn set_policy(&self, policy: Policy) {
        self.inner.lock().book.policy = policy;
    }

    pub fn set_vote_percent(&self, percent: Option<u8>) {
        self.inner.lock().vote.percent = percent;
    }

    /// A guest's request, looked up by the app. Tells the guest what happened. `Approved`
    /// (auto mode) means the app should place it now.
    pub fn submit(&self, guest_id: u64, track: RequestTrack) -> Result<Submitted, Reject> {
        let track_id = track.id;
        let (result, guest_known) = {
            let mut s = self.inner.lock();
            let known = s
                .roster
                .guests()
                .iter()
                .find(|g| g.id == guest_id)
                .map(|g| g.name.clone());
            let Some(name) = known else {
                return Err(Reject::Off);
            };
            let HostState {
                book,
                upcoming,
                recent,
                ..
            } = &mut *s;
            let view = super::requests::QueueView { upcoming, recent };
            (book.submit(guest_id, &name, track, view), true)
        };
        let _ = guest_known;
        let (ok, reason) = match &result {
            Ok(Submitted::Pending(_)) => (true, "Sent to the host.".to_string()),
            Ok(Submitted::Approved(_)) => (true, "Added to the queue.".to_string()),
            Err(r) => (false, r.message()),
        };
        self.inner.send(
            Audience::Guest(guest_id),
            HostMsg::RequestResult {
                track_id,
                ok,
                reason,
            },
        );
        if matches!(result, Ok(Submitted::Pending(_))) {
            self.inner.push_requests();
        }
        result
    }

    /// Approve or deny one request. On approve the caller places the returned request.
    pub fn decide(&self, request_id: u64, approve: bool) -> Option<Request> {
        let r = self.inner.lock().book.take(request_id)?;
        self.tell_decision(&r, approve);
        self.inner.push_requests();
        Some(r)
    }

    pub fn decide_all(&self, approve: bool) -> Vec<Request> {
        let all = self.inner.lock().book.take_all();
        for r in &all {
            self.tell_decision(r, approve);
        }
        self.inner.push_requests();
        all
    }

    fn tell_decision(&self, r: &Request, approve: bool) {
        let reason = if approve {
            "The host approved your request."
        } else {
            "The host passed on your request."
        };
        self.inner.send(
            Audience::Guest(r.guest_id),
            HostMsg::RequestResult {
                track_id: r.track.id,
                ok: approve,
                reason: reason.into(),
            },
        );
    }

    /// Tells a guest their request could not be looked up or placed.
    pub fn reject(&self, guest_id: u64, track_id: u64, reason: &str) {
        self.inner.send(
            Audience::Guest(guest_id),
            HostMsg::RequestResult {
                track_id,
                ok: false,
                reason: proto::clip(reason, 120),
            },
        );
    }

    /// Waiting requests with how many guests cannot play each.
    pub fn pending(&self) -> Vec<(Request, usize)> {
        let s = self.inner.lock();
        s.book
            .pending()
            .iter()
            .map(|r| (r.clone(), s.book.unavailable_for(r.track.id)))
            .collect()
    }

    /// How many guests said they cannot play `track_id`.
    pub fn unavailable_for(&self, track_id: u64) -> usize {
        self.inner.lock().book.unavailable_for(track_id)
    }

    /// Guests and whether each is a co-host, in join order.
    pub fn guests_detail(&self) -> Vec<(String, bool)> {
        self.inner
            .lock()
            .roster
            .guests()
            .iter()
            .map(|g| (g.name.clone(), g.cohost))
            .collect()
    }

    /// Names of the guests whose chat is muted.
    pub fn chat_muted_names(&self) -> Vec<String> {
        let s = self.inner.lock();
        s.roster
            .guests()
            .iter()
            .filter(|g| s.chat.is_muted(g.id))
            .map(|g| g.name.clone())
            .collect()
    }

    /// Promotes or demotes a guest by name.
    pub fn set_cohost(&self, name: &str, on: bool) -> bool {
        let id = {
            let mut s = self.inner.lock();
            let id = s
                .roster
                .guests()
                .iter()
                .find(|g| g.name == name)
                .map(|g| g.id);
            match id {
                Some(id) if s.roster.set_cohost(id, on) => id,
                _ => return false,
            }
        };
        self.inner
            .send(Audience::Guest(id), HostMsg::Role { cohost: on });
        self.inner.push_guests();
        if on {
            self.inner.push_requests();
        }
        true
    }

    pub fn notice(&self, text: &str) {
        self.inner.send(
            Audience::All,
            HostMsg::Notice {
                text: proto::clip(text, 120),
            },
        );
    }

    /// The guest who should be offered the party if this host vanishes (for tests and the UI).
    pub fn successor(&self) -> Option<String> {
        let s = self.inner.lock();
        let input = s.roster.successor_input();
        super::requests::successor(&input)
            .and_then(|id| input.into_iter().find(|g| g.0 == id).map(|g| g.1))
    }

    pub fn kick(&self, name: &str) -> bool {
        let id = self
            .inner
            .lock()
            .roster
            .guests()
            .iter()
            .find(|g| g.name == name)
            .map(|g| g.id);
        let token = id.and_then(|id| self.inner.lock().kicks.get(&id).cloned());
        match token {
            Some(t) => {
                // A new invite: the kicked guest's link (and anyone they passed it to) is dead.
                // Guests still in the party stay connected. The new secret reaches them
                // with the roster change once the kicked guest is gone; sending it here
                // would race the kick and could hand it to the kicked guest.
                *self.inner.secret() = new_secret();
                t.cancel();
                true
            }
            None => false,
        }
    }

    /// Ends the party: guests are told, the ticket dies with the endpoint.
    pub async fn end(self) {
        self.inner.send(Audience::All, HostMsg::Ended);
        tokio::time::sleep(Duration::from_millis(250)).await;
        self.cancel.cancel();
        self.endpoint.close().await;
    }
}

impl Drop for Host {
    /// A host dropped without [`Host::end`] still stops its accept and heartbeat tasks, so the
    /// endpoint they hold is released.
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

async fn heartbeat_loop(inner: Arc<HostInner>, cancel: CancellationToken) {
    let mut tick = tokio::time::interval(Duration::from_secs(proto::HEARTBEAT_SECS));
    tick.tick().await;
    loop {
        tokio::select! {
            () = cancel.cancelled() => return,
            _ = tick.tick() => {
                let (p, has_guests) = {
                    let s = inner.lock();
                    (s.play.clone(), !s.roster.is_empty())
                };
                if has_guests && p.track.is_some() {
                    inner.send(Audience::All, HostMsg::Heartbeat { position_ms: p.position_ms, playing: p.playing, at_ms: p.at_ms });
                }
            }
        }
    }
}

async fn accept_loop(endpoint: Endpoint, inner: Arc<HostInner>, cancel: CancellationToken) {
    let handshakes = Arc::new(tokio::sync::Semaphore::new(MAX_HANDSHAKES));
    loop {
        let incoming = tokio::select! {
            () = cancel.cancelled() => return,
            i = endpoint.accept() => i,
        };
        let Some(incoming) = incoming else { return };
        // Someone holding an old link could open connections without end; only a few may be
        // in the handshake at a time, the rest are dropped.
        let Ok(permit) = Arc::clone(&handshakes).try_acquire_owned() else {
            continue;
        };
        let (inner, cancel) = (Arc::clone(&inner), cancel.clone());
        tokio::spawn(async move {
            let Ok(conn) = incoming.await else { return };
            if let Err(e) = serve_guest(conn, inner, cancel, permit).await {
                log::debug!("party: guest connection ended: {e}");
            }
        });
    }
}

async fn serve_guest(
    conn: iroh::endpoint::Connection,
    inner: Arc<HostInner>,
    cancel: CancellationToken,
    handshake: tokio::sync::OwnedSemaphorePermit,
) -> Result<(), String> {
    let (mut send, mut recv) = tokio::time::timeout(HANDSHAKE, conn.accept_bi())
        .await
        .map_err(|_| "handshake timed out".to_string())?
        .map_err(|e| e.to_string())?;
    let hello = tokio::time::timeout(HANDSHAKE, read_frame(&mut recv))
        .await
        .map_err(|_| "handshake timed out".to_string())??;
    let GuestMsg::Hello { v, name, secret } = proto::decode(&hello)? else {
        return Err("expected hello".into());
    };
    let reject = |reason: &str| HostMsg::Rejected {
        reason: reason.to_string(),
    };
    if v != PROTOCOL_VERSION {
        let _ = write_frame(&mut send, &proto::encode(&reject(&proto::version_gap(v)))?).await;
        linger(&mut send, &conn).await;
        return Err("version mismatch".into());
    }
    let current = inner.secret().clone();
    if !secret_matches(&secret, &current) {
        let _ = write_frame(
            &mut send,
            &proto::encode(&reject("This invite is not valid."))?,
        )
        .await;
        linger(&mut send, &conn).await;
        return Err("bad ticket".into());
    }
    let joined: Result<
        (
            u64,
            String,
            HostMsg,
            CancellationToken,
            Vec<u64>,
            Vec<String>,
        ),
        String,
    > = {
        let mut s = inner.lock();
        match s.roster.join(&name) {
            Err(reason) => Err(reason),
            Ok(id) => {
                let name = s
                    .roster
                    .guests()
                    .iter()
                    .find(|g| g.id == id)
                    .map(|g| g.name.clone())
                    .unwrap_or_default();
                let kick = CancellationToken::new();
                s.kicks.insert(id, kick.clone());
                let welcome = HostMsg::Welcome {
                    v: PROTOCOL_VERSION,
                    host: inner.host_name.clone(),
                    guests: s.roster.names(),
                    state: s.play.clone(),
                    allow_suggest: s.book.policy.mode != RequestMode::Off,
                    you: name.clone(),
                    vote_skip: s.vote.percent.is_some(),
                };
                Ok((id, name, welcome, kick, s.queue.clone(), s.by.clone()))
            }
        }
    };
    let (id, name, welcome, kick, queued, queued_by) = match joined {
        Ok(j) => j,
        Err(reason) => {
            let _ = write_frame(&mut send, &proto::encode(&reject(&reason))?).await;
            linger(&mut send, &conn).await;
            return Err("party full".into());
        }
    };
    write_frame(&mut send, &proto::encode(&welcome)?).await?;
    // A late joiner gets the current track and position (in the welcome) and the prefetch
    // list straight away.
    if !queued.is_empty() {
        let q = HostMsg::Queue {
            next: queued,
            by: queued_by,
        };
        write_frame(&mut send, &proto::encode(&q)?).await?;
    }
    drop(handshake);
    let _ = inner.events.send(Event::GuestJoined(name.clone()));
    inner.push_guests();

    let mut rx = inner.tx.subscribe();
    inner.push_heir(Some(id));
    let mut flood = Flood::default();
    let result: Result<(), String> = async {
        loop {
            tokio::select! {
                // A kick wins over anything queued for this guest (a new secret).
                biased;
                () = cancel.cancelled() => return Ok(()),
                () = kick.cancelled() => {
                    let _ = write_frame(&mut send, &proto::encode(&HostMsg::Kicked)?).await;
                    linger(&mut send, &conn).await;
                    return Ok(());
                }
                msg = rx.recv() => match msg {
                    Ok((to, m)) => {
                        let mine = match to {
                            Audience::All => true,
                            Audience::Guest(g) => g == id,
                            Audience::Cohosts => inner.lock().roster.is_cohost(id),
                        };
                        if mine {
                            write_frame(&mut send, &proto::encode(&m)?).await?;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                },
                frame = read_frame(&mut recv) => {
                    let m: GuestMsg = proto::decode(&frame?)?;
                    if !flood.allow(inner.now_ms()) {
                        return Err("too many messages".into());
                    }
                    proto::validate_guest(&m)?;
                    match m {
                        GuestMsg::Ping { token } => {
                            let pong = HostMsg::Pong { token, host_ms: inner.now_ms() };
                            write_frame(&mut send, &proto::encode(&pong)?).await?;
                        }
                        GuestMsg::Suggest { track_id } => {
                            let (on, ok) = {
                                let mut s = inner.lock();
                                let on = s.book.policy.mode != RequestMode::Off;
                                let now = inner.now_ms();
                                (on, on && s.limit.allow(id, now))
                            };
                            if ok {
                                let _ = inner.events.send(Event::Suggestion { guest_id: id, guest: name.clone(), track_id });
                            } else {
                                let reason = if on { "Slow down: too many requests." } else { &Reject::Off.message() };
                                inner.send(Audience::Guest(id), HostMsg::RequestResult { track_id, ok: false, reason: reason.to_string() });
                            }
                        }
                        GuestMsg::Unavailable { track_id } => {
                            // Only the tracks the party is dealing with count; anything else
                            // (or a repeat) changes nothing and is not passed on.
                            let changed = {
                                let mut s = inner.lock();
                                let relevant = s.play.track == Some(track_id)
                                    || s.upcoming.contains(&track_id)
                                    || s.book.is_pending(track_id);
                                relevant && s.book.mark_unavailable(track_id, id)
                            };
                            if changed {
                                inner.push_requests();
                            }
                        }
                        GuestMsg::React { e } => {
                            // An index this version does not know, or a guest over the
                            // limit, is dropped quietly.
                            let counted = {
                                let mut s = inner.lock();
                                let now = inner.now_ms();
                                s.reactions.add(id, e, now).then(|| s.reactions.tally())
                            };
                            if let Some(tally) = counted {
                                inner.send(Audience::All, HostMsg::Reaction { from: name.clone(), e });
                                let _ = inner.events.send(Event::Reaction { from: name.clone(), emoji: e, tally });
                            }
                        }
                        GuestMsg::Chat { text } => {
                            let posted = {
                                let mut s = inner.lock();
                                let now = inner.now_ms();
                                s.chat.post(id, &name, &text, now)
                            };
                            match posted {
                                Ok(line) => {
                                    inner.send(Audience::All, HostMsg::Chat { from: line.from.clone(), text: line.text.clone() });
                                    let _ = inner.events.send(Event::Chat { from: line.from, text: line.text });
                                }
                                Err(Refused::TooFast) => inner.send(Audience::Guest(id), HostMsg::Notice { text: "Slow down: too many messages.".into() }),
                                Err(Refused::Muted) => inner.send(Audience::Guest(id), HostMsg::Notice { text: "The host muted your chat.".into() }),
                                Err(Refused::Empty) => {}
                            }
                        }
                        GuestMsg::VoteSkip => {
                            let outcome = {
                                let mut s = inner.lock();
                                let n = s.roster.len();
                                s.vote.vote(id, n)
                            };
                            match outcome {
                                Some(Vote::Passed) => {
                                    inner.send(Audience::All, HostMsg::Notice { text: "Vote passed: skipping.".into() });
                                    let _ = inner.events.send(Event::VoteSkipPassed);
                                }
                                Some(Vote::Waiting { have, need }) => inner.send(
                                    Audience::All,
                                    HostMsg::Notice { text: format!("{name} voted to skip ({have} of {need})") },
                                ),
                                Some(Vote::Again) => {}
                                None => inner.send(Audience::Guest(id), HostMsg::Notice { text: "Vote skip is off.".into() }),
                            }
                        }
                        GuestMsg::Decide { request_id, approve } if inner.lock().roster.is_cohost(id) => {
                            let _ = inner.events.send(Event::CohostDecide { request_id: Some(request_id), approve, by: name.clone() });
                        }
                        GuestMsg::DecideAll { approve } if inner.lock().roster.is_cohost(id) => {
                            let _ = inner.events.send(Event::CohostDecide { request_id: None, approve, by: name.clone() });
                        }
                        GuestMsg::Skip if inner.lock().roster.is_cohost(id) => {
                            let _ = inner.events.send(Event::CohostSkip(name.clone()));
                        }
                        GuestMsg::Move { from, to } if inner.lock().roster.is_cohost(id) => {
                            let _ = inner.events.send(Event::CohostMove { from, to, by: name.clone() });
                        }
                        // Co-host actions from a guest who is not one are ignored.
                        GuestMsg::Decide { .. } | GuestMsg::DecideAll { .. } | GuestMsg::Skip | GuestMsg::Move { .. } | GuestMsg::Unknown => {}
                        GuestMsg::Leave => return Ok(()),
                        GuestMsg::Hello { .. } => return Err("unexpected hello".into()),
                    }
                }
            }
        }
    }
    .await;
    {
        let mut s = inner.lock();
        s.roster.leave(id);
        s.limit.forget(id);
        s.kicks.remove(&id);
        s.book.forget_guest(id);
        s.vote.forget(id);
        s.reactions.forget(id);
        s.chat.forget(id);
    }
    let _ = inner.events.send(Event::GuestLeft(name));
    inner.push_guests();
    inner.push_requests();
    conn.close(0u32.into(), b"bye");
    result
}

// --------------------------------------------------------------- guest

pub struct Guest {
    endpoint: Endpoint,
    /// The name the host gave us.
    pub you: String,
    send: tokio::sync::Mutex<SendStream>,
    started: Instant,
    offset_ms: i64,
    cancel: CancellationToken,
    pub host_name: String,
}

impl Guest {
    /// Joins a party from its link or a pasted ticket. The `Welcome` is returned with the
    /// guest; later host messages arrive as `Event::Host`.
    pub async fn join(
        ticket: &str,
        name: &str,
        relay: Option<&str>,
        events: mpsc::UnboundedSender<Event>,
    ) -> Result<(Self, HostMsg), String> {
        Self::join_with_protocol(ticket, name, relay, events, PROTOCOL_VERSION).await
    }

    /// `join` speaking another protocol version (tests: what a Slate 1.2 guest sees).
    #[doc(hidden)]
    pub async fn join_with_protocol(
        ticket: &str,
        name: &str,
        relay: Option<&str>,
        events: mpsc::UnboundedSender<Event>,
        protocol: u32,
    ) -> Result<(Self, HostMsg), String> {
        let t = decode_ticket(ticket)?;
        let endpoint = bind(relay, false).await?;
        let result = Self::handshake(&endpoint, t, name, events, protocol).await;
        if result.is_err() {
            endpoint.close().await;
        }
        result
    }

    async fn handshake(
        endpoint: &Endpoint,
        t: Ticket,
        name: &str,
        events: mpsc::UnboundedSender<Event>,
        protocol: u32,
    ) -> Result<(Self, HostMsg), String> {
        let conn = tokio::time::timeout(Duration::from_secs(20), endpoint.connect(t.a, ALPN))
            .await
            .map_err(|_| "Could not reach the host (timed out).".to_string())?
            .map_err(|_| "Could not reach the host. Is the party still running?".to_string())?;
        let (mut send, mut recv) = conn.open_bi().await.map_err(|e| e.to_string())?;
        let hello = GuestMsg::Hello {
            v: protocol,
            name: proto::clean_name(name),
            secret: t.s,
        };
        write_frame(&mut send, &proto::encode(&hello)?).await?;
        let first = tokio::time::timeout(HANDSHAKE, read_frame(&mut recv))
            .await
            .map_err(|_| "The host did not answer.".to_string())??;
        let welcome: HostMsg = proto::decode(&first)?;
        if let HostMsg::Rejected { reason } = &welcome {
            return Err(proto::rejection_text(reason));
        }
        proto::validate_host(&welcome)?;
        let HostMsg::Welcome { host, you, .. } = &welcome else {
            return Err("unexpected reply from the host".into());
        };
        let (host_name, you) = (host.clone(), you.clone());

        // Clock offset: three pings now, then none until the party ends.
        let started = Instant::now();
        let mut samples = Vec::new();
        let mut budget = MAX_HANDSHAKE_MESSAGES;
        for token in 0..3u64 {
            let sent = started.elapsed().as_millis() as u64;
            write_frame(&mut send, &proto::encode(&GuestMsg::Ping { token })?).await?;
            loop {
                budget = budget
                    .checked_sub(1)
                    .ok_or("The host sent too many messages.")?;
                let m: HostMsg = proto::decode(
                    &tokio::time::timeout(HANDSHAKE, read_frame(&mut recv))
                        .await
                        .map_err(|_| "The host stopped answering.".to_string())??,
                )?;
                if let HostMsg::Pong { token: t, host_ms } = m {
                    if t == token {
                        samples.push(PingSample {
                            sent_ms: sent,
                            received_ms: started.elapsed().as_millis() as u64,
                            host_ms,
                        });
                        break;
                    }
                } else if proto::validate_host(&m).is_ok() && m != HostMsg::Unknown {
                    // Updates that arrive during the handshake are not lost.
                    let _ = events.send(Event::Host(m));
                }
            }
        }
        let offset_ms = proto::best_offset(&samples).unwrap_or(0);
        let cancel = CancellationToken::new();
        let reader_cancel = cancel.clone();
        tokio::spawn(async move {
            let reason = loop {
                let frame = tokio::select! {
                    () = reader_cancel.cancelled() => break "left".to_string(),
                    f = read_frame(&mut recv) => f,
                };
                match frame
                    .and_then(|b| proto::decode::<HostMsg>(&b))
                    .and_then(|m| proto::validate_host(&m).map(|()| m))
                {
                    Ok(HostMsg::Ended) => {
                        let _ = events.send(Event::Closed("The host ended the party.".into()));
                        return;
                    }
                    Ok(HostMsg::Kicked) => {
                        let _ =
                            events.send(Event::Closed("You were removed from the party.".into()));
                        return;
                    }
                    Ok(HostMsg::Unknown) => {}
                    Ok(m) => {
                        let _ = events.send(Event::Host(m));
                    }
                    Err(_) => break "Lost the connection to the host.".to_string(),
                }
            };
            // A deliberate leave is not a loss.
            let ev = if reader_cancel.is_cancelled() {
                Event::Closed(reason)
            } else {
                Event::Lost(reason)
            };
            let _ = events.send(ev);
        });
        let guest = Self {
            endpoint: endpoint.clone(),
            send: tokio::sync::Mutex::new(send),
            started,
            offset_ms,
            cancel,
            host_name,
            you,
        };
        Ok((guest, welcome))
    }

    /// The host's clock now, in ms.
    pub fn host_now_ms(&self) -> u64 {
        let local = self.started.elapsed().as_millis() as i64;
        (local + self.offset_ms).max(0) as u64
    }

    async fn send_msg(&self, m: &GuestMsg) -> Result<(), String> {
        let bytes = proto::encode(m)?;
        write_frame(&mut *self.send.lock().await, &bytes).await
    }

    pub async fn suggest(&self, track_id: u64) -> Result<(), String> {
        self.send_msg(&GuestMsg::Suggest { track_id }).await
    }

    /// Tells the host this guest cannot play `track_id` (region or plan).
    pub async fn unavailable(&self, track_id: u64) -> Result<(), String> {
        self.send_msg(&GuestMsg::Unavailable { track_id }).await
    }

    /// Reacts to the playing track with emoji number `e`.
    pub async fn react(&self, e: u8) -> Result<(), String> {
        self.send_msg(&GuestMsg::React { e }).await
    }

    /// Sends a chat message; the host cleans and limits it.
    pub async fn chat(&self, text: &str) -> Result<(), String> {
        self.send_msg(&GuestMsg::Chat {
            text: text.to_string(),
        })
        .await
    }

    pub async fn vote_skip(&self) -> Result<(), String> {
        self.send_msg(&GuestMsg::VoteSkip).await
    }

    /// Co-host actions; the host ignores them from anyone else.
    pub async fn decide(&self, request_id: u64, approve: bool) -> Result<(), String> {
        self.send_msg(&GuestMsg::Decide {
            request_id,
            approve,
        })
        .await
    }

    pub async fn decide_all(&self, approve: bool) -> Result<(), String> {
        self.send_msg(&GuestMsg::DecideAll { approve }).await
    }

    pub async fn skip(&self) -> Result<(), String> {
        self.send_msg(&GuestMsg::Skip).await
    }

    pub async fn move_upcoming(&self, from: u32, to: u32) -> Result<(), String> {
        self.send_msg(&GuestMsg::Move { from, to }).await
    }

    /// Tells the host and closes the connection. Takes `&self` so it works while the event
    /// reader still holds the guest.
    pub async fn leave(&self) {
        if let Ok(b) = proto::encode(&GuestMsg::Leave) {
            let _ = write_frame(&mut *self.send.lock().await, &b).await;
        }
        self.cancel.cancel();
        self.endpoint.close().await;
    }
}

impl Drop for Guest {
    /// The reader task stops with the guest, even when `leave` was never called.
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tickets_round_trip_and_reject_garbage() {
        assert!(decode_ticket("nope!").is_err());
        assert!(decode_ticket("AAAA").is_err());
        assert!(decode_ticket("").is_err());
    }

    #[tokio::test]
    async fn the_relay_check_refuses_bad_urls_and_reports_an_unreachable_relay() {
        let e = check_relay(Some("not a url"), Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(e.contains("not valid"), "{e}");
        let e = check_relay(Some("none"), Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(e.contains("disabled"), "{e}");
        let e = check_relay(Some("https://127.0.0.1:9"), Duration::from_secs(2))
            .await
            .unwrap_err();
        assert!(e.contains("Could not reach"), "{e}");
    }

    fn host_state() -> HostState {
        HostState {
            play: PlayState::default(),
            queue: Vec::new(),
            roster: Roster::default(),
            limit: RateLimit::default(),
            kicks: HashMap::new(),
            book: RequestBook::new(Policy::default()),
            vote: VoteSkip::new(None),
            reactions: Reactions::default(),
            chat: Chat::default(),
            upcoming: Vec::new(),
            by: Vec::new(),
            recent: Vec::new(),
            heir: (iroh::SecretKey::generate(), None),
        }
    }

    #[test]
    fn the_heir_key_follows_the_successor_and_changes_with_it() {
        let mut s = host_state();
        assert!(s.assign_heir().is_none(), "no guests, no successor");
        let a = s.roster.join("a").unwrap();
        let b = s.roster.join("b").unwrap();
        let (k1, who, rotated) = s.assign_heir().unwrap();
        assert_eq!((who, rotated), (a, false));
        let (k1b, _, rotated) = s.assign_heir().unwrap();
        assert!(
            !rotated && k1b.public() == k1.public(),
            "same successor keeps its key"
        );
        s.roster.leave(a);
        let (k2, who, rotated) = s.assign_heir().unwrap();
        assert_eq!((who, rotated), (b, true));
        assert!(
            k2.public() != k1.public(),
            "the guest who left cannot host under the new key"
        );
        assert_eq!(s.heir.1, Some(b));
    }

    #[test]
    fn secrets_compare_exactly() {
        assert!(secret_matches("abc", "abc"));
        assert!(!secret_matches("abd", "abc"));
        assert!(!secret_matches("abcd", "abc"));
        assert!(!secret_matches("", "abc"));
    }
}
