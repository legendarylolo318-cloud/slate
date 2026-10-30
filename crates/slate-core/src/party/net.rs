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

use super::proto::{
    self, GuestMsg, HostMsg, PingSample, PlayState, RateLimit, Roster, MAX_MESSAGE_BYTES,
    PROTOCOL_VERSION,
};

const ALPN: &[u8] = b"slate/party/1";
const HANDSHAKE: Duration = Duration::from_secs(10);
const ONLINE_WAIT: Duration = Duration::from_secs(12);

/// What the app is told.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Guest side: a message from the host.
    Host(HostMsg),
    /// Host side.
    GuestJoined(String),
    GuestLeft(String),
    Suggestion {
        guest: String,
        track_id: u64,
    },
    /// The connection ended (guest side) or the party closed (host side).
    Closed(String),
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
    let mut b = Endpoint::builder(presets::N0);
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

// ---------------------------------------------------------------- host

struct HostState {
    play: PlayState,
    queue: Vec<u64>,
    roster: Roster,
    limit: RateLimit,
    kicks: HashMap<u64, CancellationToken>,
}

struct HostInner {
    secret: String,
    started: Instant,
    host_name: String,
    allow_suggest: bool,
    tx: broadcast::Sender<HostMsg>,
    state: Mutex<HostState>,
    events: mpsc::UnboundedSender<Event>,
}

impl HostInner {
    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
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
    ticket: String,
}

impl Host {
    /// Starts a party. `relay`: a self-hosted `iroh-relay` URL, or None for the default relays.
    pub async fn start(
        relay: Option<&str>,
        host_name: &str,
        allow_suggest: bool,
        events: mpsc::UnboundedSender<Event>,
    ) -> Result<Self, String> {
        let endpoint = bind(relay, true).await?;
        if relay.map(str::trim) != Some("none") {
            let _ = tokio::time::timeout(ONLINE_WAIT, endpoint.online()).await;
        }
        let secret = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let ticket = encode_ticket(&Ticket {
            a: endpoint.addr(),
            s: secret.clone(),
        })?;
        let (tx, _) = broadcast::channel(64);
        let inner = Arc::new(HostInner {
            secret,
            started: Instant::now(),
            host_name: proto::clean_name(host_name),
            allow_suggest,
            tx,
            state: Mutex::new(HostState {
                play: PlayState::default(),
                queue: Vec::new(),
                roster: Roster::default(),
                limit: RateLimit::default(),
                kicks: HashMap::new(),
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
            ticket,
        })
    }

    /// The invite: `slate://party/<ticket>`.
    pub fn link(&self) -> String {
        proto::ticket_link(&self.ticket)
    }

    pub fn guest_names(&self) -> Vec<String> {
        self.inner.lock().roster.names()
    }

    /// A track started (or restarted).
    pub fn track_changed(&self, id: u64, position_ms: u64, playing: bool) {
        let at_ms = self.inner.now_ms();
        self.inner.lock().play = PlayState {
            track: Some(id),
            position_ms,
            playing,
            at_ms,
        };
        let _ = self.inner.tx.send(HostMsg::Track {
            id,
            position_ms,
            playing,
            at_ms,
        });
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
        let _ = self.inner.tx.send(HostMsg::State {
            position_ms,
            playing,
            at_ms,
        });
    }

    /// The upcoming queue changed; the next few ids go to the guests.
    pub fn queue_changed(&self, upcoming: &[u64]) {
        let next = proto::prefetch_list(upcoming);
        self.inner.lock().queue = next.clone();
        let _ = self.inner.tx.send(HostMsg::Queue { next });
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
                t.cancel();
                true
            }
            None => false,
        }
    }

    /// Ends the party: guests are told, the ticket dies with the endpoint.
    pub async fn end(self) {
        let _ = self.inner.tx.send(HostMsg::Ended);
        tokio::time::sleep(Duration::from_millis(250)).await;
        self.cancel.cancel();
        self.endpoint.close().await;
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
                    let _ = inner.tx.send(HostMsg::Heartbeat { position_ms: p.position_ms, playing: p.playing, at_ms: p.at_ms });
                }
            }
        }
    }
}

async fn accept_loop(endpoint: Endpoint, inner: Arc<HostInner>, cancel: CancellationToken) {
    loop {
        let incoming = tokio::select! {
            () = cancel.cancelled() => return,
            i = endpoint.accept() => i,
        };
        let Some(incoming) = incoming else { return };
        let (inner, cancel) = (Arc::clone(&inner), cancel.clone());
        tokio::spawn(async move {
            let Ok(conn) = incoming.await else { return };
            if let Err(e) = serve_guest(conn, inner, cancel).await {
                log::debug!("party: guest connection ended: {e}");
            }
        });
    }
}

async fn serve_guest(
    conn: iroh::endpoint::Connection,
    inner: Arc<HostInner>,
    cancel: CancellationToken,
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
        let _ = write_frame(
            &mut send,
            &proto::encode(&reject("Different Slate versions: update both."))?,
        )
        .await;
        linger(&mut send, &conn).await;
        return Err("version mismatch".into());
    }
    if !secret_matches(&secret, &inner.secret) {
        let _ = write_frame(
            &mut send,
            &proto::encode(&reject("This invite is not valid."))?,
        )
        .await;
        linger(&mut send, &conn).await;
        return Err("bad ticket".into());
    }
    let joined: Result<(u64, String, HostMsg, CancellationToken), String> = {
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
                    allow_suggest: inner.allow_suggest,
                };
                Ok((id, name, welcome, kick))
            }
        }
    };
    let (id, name, welcome, kick) = match joined {
        Ok(j) => j,
        Err(reason) => {
            let _ = write_frame(&mut send, &proto::encode(&reject(&reason))?).await;
            linger(&mut send, &conn).await;
            return Err("party full".into());
        }
    };
    write_frame(&mut send, &proto::encode(&welcome)?).await?;
    let queued = inner.lock().queue.clone();
    if !queued.is_empty() {
        write_frame(&mut send, &proto::encode(&HostMsg::Queue { next: queued })?).await?;
    }
    let _ = inner.events.send(Event::GuestJoined(name.clone()));
    let _ = inner.tx.send(HostMsg::Guests {
        guests: inner.lock().roster.names(),
    });

    let mut rx = inner.tx.subscribe();
    let result: Result<(), String> = async {
        loop {
            tokio::select! {
                () = cancel.cancelled() => return Ok(()),
                () = kick.cancelled() => {
                    let _ = write_frame(&mut send, &proto::encode(&HostMsg::Kicked)?).await;
                    linger(&mut send, &conn).await;
                    return Ok(());
                }
                msg = rx.recv() => match msg {
                    Ok(m) => write_frame(&mut send, &proto::encode(&m)?).await?,
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                },
                frame = read_frame(&mut recv) => {
                    let m: GuestMsg = proto::decode(&frame?)?;
                    proto::validate_guest(&m)?;
                    match m {
                        GuestMsg::Ping { token } => {
                            let pong = HostMsg::Pong { token, host_ms: inner.now_ms() };
                            write_frame(&mut send, &proto::encode(&pong)?).await?;
                        }
                        GuestMsg::Suggest { track_id } => {
                            let ok = inner.allow_suggest && inner.lock().limit.allow(id, inner.now_ms());
                            if ok {
                                let _ = inner.events.send(Event::Suggestion { guest: name.clone(), track_id });
                            }
                        }
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
    }
    let _ = inner.events.send(Event::GuestLeft(name));
    let _ = inner.tx.send(HostMsg::Guests {
        guests: inner.lock().roster.names(),
    });
    conn.close(0u32.into(), b"bye");
    result
}

// --------------------------------------------------------------- guest

pub struct Guest {
    endpoint: Endpoint,
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
        let t = decode_ticket(ticket)?;
        let endpoint = bind(relay, false).await?;
        let result = Self::handshake(&endpoint, t, name, events).await;
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
    ) -> Result<(Self, HostMsg), String> {
        let conn = tokio::time::timeout(Duration::from_secs(20), endpoint.connect(t.a, ALPN))
            .await
            .map_err(|_| "Could not reach the host (timed out).".to_string())?
            .map_err(|_| "Could not reach the host. Is the party still running?".to_string())?;
        let (mut send, mut recv) = conn.open_bi().await.map_err(|e| e.to_string())?;
        let hello = GuestMsg::Hello {
            v: PROTOCOL_VERSION,
            name: proto::clean_name(name),
            secret: t.s,
        };
        write_frame(&mut send, &proto::encode(&hello)?).await?;
        let first = tokio::time::timeout(HANDSHAKE, read_frame(&mut recv))
            .await
            .map_err(|_| "The host did not answer.".to_string())??;
        let welcome: HostMsg = proto::decode(&first)?;
        if let HostMsg::Rejected { reason } = &welcome {
            return Err(reason.clone());
        }
        proto::validate_host(&welcome)?;
        let HostMsg::Welcome { host, .. } = &welcome else {
            return Err("unexpected reply from the host".into());
        };
        let host_name = host.clone();

        // Clock offset: three pings now, then none until the party ends.
        let started = Instant::now();
        let mut samples = Vec::new();
        for token in 0..3u64 {
            let sent = started.elapsed().as_millis() as u64;
            write_frame(&mut send, &proto::encode(&GuestMsg::Ping { token })?).await?;
            loop {
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
                } else {
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
                    Ok(HostMsg::Ended) => break "The host ended the party.".to_string(),
                    Ok(HostMsg::Kicked) => break "You were removed from the party.".to_string(),
                    Ok(m) => {
                        let _ = events.send(Event::Host(m));
                    }
                    Err(_) => break "Lost the connection to the host.".to_string(),
                }
            };
            let _ = events.send(Event::Closed(reason));
        });
        let guest = Self {
            endpoint: endpoint.clone(),
            send: tokio::sync::Mutex::new(send),
            started,
            offset_ms,
            cancel,
            host_name,
        };
        Ok((guest, welcome))
    }

    /// The host's clock now, in ms.
    pub fn host_now_ms(&self) -> u64 {
        let local = self.started.elapsed().as_millis() as i64;
        (local + self.offset_ms).max(0) as u64
    }

    pub async fn suggest(&self, track_id: u64) -> Result<(), String> {
        let bytes = proto::encode(&GuestMsg::Suggest { track_id })?;
        write_frame(&mut *self.send.lock().await, &bytes).await
    }

    pub async fn leave(self) {
        if let Ok(b) = proto::encode(&GuestMsg::Leave) {
            let _ = write_frame(&mut *self.send.lock().await, &b).await;
        }
        self.cancel.cancel();
        self.endpoint.close().await;
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

    #[test]
    fn secrets_compare_exactly() {
        assert!(secret_matches("abc", "abc"));
        assert!(!secret_matches("abd", "abc"));
        assert!(!secret_matches("abcd", "abc"));
        assert!(!secret_matches("", "abc"));
    }
}
