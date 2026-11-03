//! The MPD listener, per-client loop, limits and command dispatch.
//!
//! Limits: 8 clients, 60 commands per 10 s per client, 64 KiB per line, an idle connection
//! (one not waiting in `idle`) is closed after 10 minutes. With the server stopped there is no
//! socket and no task.

use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{tcp::OwnedReadHalf, TcpListener, TcpStream};
use tokio::sync::{broadcast, watch};

use super::proto::{self, ack, kv, song_block};
use super::{Action, Config, Host, HostError, PlayState, Song, Status};

pub const MAX_CLIENTS: usize = 8;
pub const RATE_COMMANDS: usize = 60;
pub const RATE_WINDOW: Duration = Duration::from_secs(10);
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(600);
/// A connection that has not given the password yet is dropped after this long, so silent
/// sockets cannot hold the 8 client slots.
pub const PREAUTH_TIMEOUT: Duration = Duration::from_secs(30);

/// How long to wait for the next line from a peer.
pub fn read_timeout(authed: bool) -> Duration {
    if authed {
        IDLE_TIMEOUT
    } else {
        PREAUTH_TIMEOUT
    }
}
/// Searches go to TIDAL: at most one at a time, this far apart (all clients together).
const SEARCH_SPACING: Duration = Duration::from_millis(500);
const VERSION: &str = "0.23.5";

// `idle` subsystems.
const PLAYLIST: u8 = 1;
const PLAYER: u8 = 2;
const MIXER: u8 = 4;
const OPTIONS: u8 = 8;
const ALL_SUBSYSTEMS: u8 = PLAYLIST | PLAYER | MIXER | OPTIONS | 16 | 32;
const SUBSYSTEMS: [(&str, u8); 6] = [
    ("playlist", PLAYLIST),
    ("player", PLAYER),
    ("mixer", MIXER),
    ("options", OPTIONS),
    ("stored_playlist", 16),
    ("output", 32),
];

/// Every command the server answers, for `commands`.
const COMMANDS: &[&str] = &[
    "add",
    "addid",
    "albumart",
    "binarylimit",
    "clear",
    "clearerror",
    "close",
    "commands",
    "consume",
    "count",
    "currentsong",
    "decoders",
    "delete",
    "deleteid",
    "find",
    "getvol",
    "idle",
    "list",
    "listplaylistinfo",
    "listplaylists",
    "load",
    "lsinfo",
    "move",
    "moveid",
    "next",
    "noidle",
    "notcommands",
    "outputs",
    "password",
    "pause",
    "ping",
    "play",
    "playid",
    "playlist",
    "playlistid",
    "playlistinfo",
    "plchanges",
    "plchangesposid",
    "previous",
    "random",
    "readpicture",
    "repeat",
    "replay_gain_status",
    "search",
    "seek",
    "seekcur",
    "seekid",
    "setvol",
    "shuffle",
    "single",
    "stats",
    "status",
    "stop",
    "tagtypes",
    "urlhandlers",
    "volume",
];

/// Commands a client may run before it has given the password.
const OPEN_COMMANDS: &[&str] = &["close", "commands", "notcommands", "password", "ping"];

/// What the last status looked like, to tell which subsystems changed.
#[derive(Clone, PartialEq)]
struct Seen {
    state: PlayState,
    song: Option<(usize, u64)>,
    volume: u8,
    repeat: bool,
    random: bool,
    hash: u64,
}

struct Track {
    seen: Option<Seen>,
    /// MPD's playlist version: counts up whenever the queue changes.
    version: u32,
}

struct Shared {
    host: Arc<dyn Host>,
    password: String,
    events: broadcast::Sender<u8>,
    track: Mutex<Track>,
    search_gate: tokio::sync::Mutex<Instant>,
    clients: AtomicUsize,
    started: Instant,
    stop: watch::Receiver<bool>,
}

impl Shared {
    /// Notes `status`; returns the playlist version and emits `idle` events for what changed.
    fn observe(&self, st: &Status) -> u32 {
        let now = Seen {
            state: st.state,
            song: st.pos.zip(st.current.as_ref().map(|s| s.id)),
            volume: st.volume,
            repeat: st.repeat,
            random: st.random,
            hash: st.queue_hash,
        };
        let (version, changed) = {
            let mut t = self.track.lock().unwrap_or_else(|p| p.into_inner());
            let mut changed = 0u8;
            if let Some(old) = t.seen.clone() {
                if old.hash != now.hash {
                    t.version = t.version.wrapping_add(1);
                    changed |= PLAYLIST;
                }
                if old.state != now.state || old.song != now.song {
                    changed |= PLAYER;
                }
                if old.volume != now.volume {
                    changed |= MIXER;
                }
                if old.repeat != now.repeat || old.random != now.random {
                    changed |= OPTIONS;
                }
            }
            t.seen = Some(now);
            (t.version, changed)
        };
        if changed != 0 {
            // No receiver (nobody is in `idle`) is fine.
            let _ = self.events.send(changed);
        }
        version
    }
}

/// A running server.
pub struct Handle {
    addr: SocketAddr,
    shared: Arc<Shared>,
    stop: watch::Sender<bool>,
    /// The accept loop; it owns the listening socket.
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Handle {
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Tells idle clients what changed since the last report. Cheap; called by the app when
    /// playback, the queue, the volume or the modes change.
    pub fn feed(&self, st: &Status) {
        self.shared.observe(st);
    }

    pub fn client_count(&self) -> usize {
        self.shared.clients.load(Ordering::Relaxed)
    }

    /// Closes the socket and every connection.
    pub fn stop(&self) {
        let _ = self.stop.send(true);
    }

    /// Stops and waits until the listening socket is closed, so the port can be bound again
    /// at once (a restart for a new password or LAN setting).
    pub async fn shutdown(mut self) {
        self.stop();
        if let Some(task) = self.task.take() {
            let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Binds and serves. `port` 0 picks a free one (tests).
pub async fn start(host: Arc<dyn Host>, cfg: Config) -> std::io::Result<Handle> {
    cfg.check()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let ip: IpAddr = if cfg.lan {
        Ipv4Addr::UNSPECIFIED.into()
    } else {
        Ipv4Addr::LOCALHOST.into()
    };
    let listener = TcpListener::bind(SocketAddr::new(ip, cfg.port)).await?;
    let addr = listener.local_addr()?;
    let (stop_tx, stop_rx) = watch::channel(false);
    let (events, _) = broadcast::channel(16);
    let shared = Arc::new(Shared {
        host,
        password: cfg.password,
        events,
        track: Mutex::new(Track {
            seen: None,
            version: 1,
        }),
        search_gate: tokio::sync::Mutex::new(Instant::now() - SEARCH_SPACING),
        clients: AtomicUsize::new(0),
        started: Instant::now(),
        stop: stop_rx.clone(),
    });
    let sh = Arc::clone(&shared);
    let mut stop = stop_rx;
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = stop.changed() => break,
                accepted = listener.accept() => {
                    let Ok((sock, peer)) = accepted else {
                        // A failed accept (out of file descriptors…): do not spin.
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        continue;
                    };
                    let sh = Arc::clone(&sh);
                    tokio::spawn(async move { serve(sh, sock, peer).await });
                }
            }
        }
        // The listener drops here: the socket is gone.
    });
    Ok(Handle {
        addr,
        shared,
        stop: stop_tx,
        task: Some(task),
    })
}

/// Keeps `clients` right however a connection ends.
struct Slot(Arc<Shared>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.clients.fetch_sub(1, Ordering::Relaxed);
    }
}

async fn serve(sh: Arc<Shared>, mut sock: TcpStream, _peer: SocketAddr) {
    if sh.clients.fetch_add(1, Ordering::Relaxed) >= MAX_CLIENTS {
        sh.clients.fetch_sub(1, Ordering::Relaxed);
        let _ = sock
            .write_all(ack(proto::ACK_SYSTEM, 0, "", "too many connections").as_bytes())
            .await;
        return;
    }
    let _slot = Slot(Arc::clone(&sh));
    let _ = sock.set_nodelay(true);
    let mut stop = sh.stop.clone();
    let (rd, mut wr) = sock.into_split();
    let mut client = Peer::new(rd, sh.events.subscribe(), sh.password.is_empty());
    let result = tokio::select! {
        _ = stop.changed() => Ok(()),
        r = client.run(&sh, &mut wr) => r,
    };
    if let Err(e) = result {
        log::debug!("[mpd] connection ended: {e}");
    }
    let _ = wr.shutdown().await;
}

/// Reads lines of at most [`proto::MAX_LINE`] bytes. Cancel safe.
struct Lines {
    rd: OwnedReadHalf,
    buf: Vec<u8>,
}

enum LineError {
    TooLong,
    Closed,
}

impl Lines {
    async fn next(&mut self) -> Result<String, LineError> {
        loop {
            if let Some(i) = self.buf.iter().position(|b| *b == b'\n') {
                let mut line: Vec<u8> = self.buf.drain(..=i).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if line.len() > proto::MAX_LINE {
                    return Err(LineError::TooLong);
                }
                return Ok(String::from_utf8_lossy(&line).into_owned());
            }
            if self.buf.len() > proto::MAX_LINE {
                return Err(LineError::TooLong);
            }
            let mut chunk = [0u8; 4096];
            match self.rd.read(&mut chunk).await {
                Ok(0) | Err(_) => return Err(LineError::Closed),
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
            }
        }
    }
}

struct Peer {
    lines: Lines,
    events: broadcast::Receiver<u8>,
    /// Changes that happened since the client last asked.
    pending: u8,
    authed: bool,
    password_failures: u8,
    stamps: VecDeque<Instant>,
    binary_limit: usize,
}

type Out = Vec<u8>;

enum Flow {
    Continue,
    Close,
}

impl Peer {
    fn new(rd: OwnedReadHalf, events: broadcast::Receiver<u8>, authed: bool) -> Self {
        Self {
            lines: Lines {
                rd,
                buf: Vec::new(),
            },
            events,
            pending: 0,
            authed,
            password_failures: 0,
            stamps: VecDeque::new(),
            binary_limit: 8192,
        }
    }

    fn drain_events(&mut self) {
        loop {
            match self.events.try_recv() {
                Ok(bits) => self.pending |= bits,
                Err(broadcast::error::TryRecvError::Lagged(_)) => self.pending = ALL_SUBSYSTEMS,
                Err(_) => break,
            }
        }
    }

    /// Counts a command against the rate limit; false when over it.
    fn allow(&mut self) -> bool {
        let now = Instant::now();
        while self
            .stamps
            .front()
            .is_some_and(|t| now.duration_since(*t) > RATE_WINDOW)
        {
            self.stamps.pop_front();
        }
        if self.stamps.len() >= RATE_COMMANDS {
            return false;
        }
        self.stamps.push_back(now);
        true
    }

    async fn run(
        &mut self,
        sh: &Arc<Shared>,
        wr: &mut tokio::net::tcp::OwnedWriteHalf,
    ) -> Result<(), String> {
        wr.write_all(format!("OK MPD {VERSION}\n").as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        loop {
            let line =
                match tokio::time::timeout(read_timeout(self.authed), self.lines.next()).await {
                    Err(_) => return Ok(()),
                    Ok(Err(LineError::Closed)) => return Ok(()),
                    Ok(Err(LineError::TooLong)) => {
                        let _ = wr
                            .write_all(ack(proto::ACK_ARG, 0, "", "line too long").as_bytes())
                            .await;
                        return Ok(());
                    }
                    Ok(Ok(l)) => l,
                };
            let mut out: Out = Vec::new();
            let flow = self.handle_line(sh, line, &mut out, wr).await?;
            if !out.is_empty() {
                wr.write_all(&out).await.map_err(|e| e.to_string())?;
            }
            if matches!(flow, Flow::Close) || self.password_failures >= 3 {
                return Ok(());
            }
        }
    }

    /// One line from the client: a command, `idle`, or the start of a command list.
    async fn handle_line(
        &mut self,
        sh: &Arc<Shared>,
        line: String,
        out: &mut Out,
        wr: &mut tokio::net::tcp::OwnedWriteHalf,
    ) -> Result<Flow, String> {
        let words = match proto::tokenize(&line) {
            Some(w) => w,
            None => {
                out.extend(ack(proto::ACK_ARG, 0, "", "Missing closing '\"'").as_bytes());
                return Ok(Flow::Continue);
            }
        };
        let Some(cmd) = words.first().map(String::as_str) else {
            out.extend(ack(proto::ACK_UNKNOWN, 0, "", "No command given").as_bytes());
            return Ok(Flow::Continue);
        };
        match cmd {
            "command_list_begin" | "command_list_ok_begin" => {
                let ok_mode = cmd == "command_list_ok_begin";
                let mut list: Vec<String> = Vec::new();
                let mut bytes = 0usize;
                loop {
                    let l = tokio::time::timeout(read_timeout(self.authed), self.lines.next())
                        .await
                        .map_err(|_| "timed out in a command list".to_string())?
                        .map_err(|_| "closed in a command list".to_string())?;
                    if l.trim() == "command_list_end" {
                        break;
                    }
                    bytes += l.len();
                    if list.len() >= proto::MAX_LIST_COMMANDS || bytes > proto::MAX_LIST_BYTES {
                        out.extend(ack(proto::ACK_ARG, 0, "", "command list too long").as_bytes());
                        return Ok(Flow::Close);
                    }
                    list.push(l);
                }
                for (i, l) in list.iter().enumerate() {
                    let words = match proto::tokenize(l) {
                        Some(w) if !w.is_empty() => w,
                        _ => {
                            out.extend(ack(proto::ACK_ARG, i, "", "Malformed command").as_bytes());
                            return Ok(Flow::Continue);
                        }
                    };
                    let name = words[0].clone();
                    if matches!(name.as_str(), "idle" | "noidle" | "close") {
                        out.extend(
                            ack(proto::ACK_ARG, i, &name, "not allowed in a command list")
                                .as_bytes(),
                        );
                        return Ok(Flow::Continue);
                    }
                    if let Err(e) = self.command(sh, &words, out).await {
                        out.extend(ack(e.code, i, &name, &e.msg).as_bytes());
                        return Ok(Flow::Continue);
                    }
                    if ok_mode {
                        out.extend(b"list_OK\n");
                    }
                }
                out.extend(b"OK\n");
                Ok(Flow::Continue)
            }
            // `idle` waits without a timeout, so it must not let a peer without the password
            // hold a connection open.
            "idle" if !self.authed => {
                out.extend(
                    ack(
                        proto::ACK_PERMISSION,
                        0,
                        "idle",
                        "you don't have permission for this command",
                    )
                    .as_bytes(),
                );
                Ok(Flow::Continue)
            }
            "idle" => {
                let mask = if words.len() > 1 {
                    let mut m = 0;
                    for w in &words[1..] {
                        match SUBSYSTEMS.iter().find(|(n, _)| n == w) {
                            Some((_, b)) => m |= b,
                            None => {
                                out.extend(
                                    ack(proto::ACK_ARG, 0, "idle", "Unrecognized idle event")
                                        .as_bytes(),
                                );
                                return Ok(Flow::Continue);
                            }
                        }
                    }
                    m
                } else {
                    ALL_SUBSYSTEMS
                };
                self.idle(mask, out, wr).await
            }
            "noidle" => Ok(Flow::Continue),
            "close" => Ok(Flow::Close),
            _ => {
                if let Err(e) = self.command(sh, &words, out).await {
                    out.extend(ack(e.code, 0, cmd, &e.msg).as_bytes());
                } else {
                    out.extend(b"OK\n");
                }
                Ok(Flow::Continue)
            }
        }
    }

    /// Waits until a subsystem in `mask` changes or the client sends `noidle`.
    async fn idle(
        &mut self,
        mask: u8,
        out: &mut Out,
        wr: &mut tokio::net::tcp::OwnedWriteHalf,
    ) -> Result<Flow, String> {
        self.drain_events();
        loop {
            let hit = self.pending & mask;
            if hit != 0 {
                self.pending &= !hit;
                for (name, bit) in SUBSYSTEMS {
                    if hit & bit != 0 {
                        out.extend(format!("changed: {name}\n").as_bytes());
                    }
                }
                out.extend(b"OK\n");
                return Ok(Flow::Continue);
            }
            tokio::select! {
                ev = self.events.recv() => match ev {
                    Ok(bits) => self.pending |= bits,
                    Err(broadcast::error::RecvError::Lagged(_)) => self.pending = ALL_SUBSYSTEMS,
                    Err(broadcast::error::RecvError::Closed) => return Ok(Flow::Close),
                },
                l = self.lines.next() => match l {
                    Ok(l) if l.trim() == "noidle" => {
                        out.extend(b"OK\n");
                        return Ok(Flow::Continue);
                    }
                    // Only `noidle` is allowed while idle.
                    Ok(_) | Err(_) => {
                        let _ = wr.flush().await;
                        return Ok(Flow::Close);
                    }
                },
            }
        }
    }

    /// Runs one ordinary command, appending its lines (without the final `OK`) to `out`.
    async fn command(
        &mut self,
        sh: &Arc<Shared>,
        words: &[String],
        out: &mut Out,
    ) -> Result<(), HostError> {
        let cmd = words[0].as_str();
        let args = &words[1..];
        if !self.allow() {
            return Err(HostError::system("rate limit: too many commands"));
        }
        if !self.authed && !OPEN_COMMANDS.contains(&cmd) {
            return Err(HostError {
                code: proto::ACK_PERMISSION,
                msg: "you don't have permission for this command".into(),
            });
        }
        let mut text = String::new();
        let host = &sh.host;
        match cmd {
            "ping" | "clearerror" => {}
            "password" => {
                let given = args
                    .first()
                    .ok_or_else(|| HostError::arg("wrong number of arguments"))?;
                if proto::secret_eq(given, &sh.password) {
                    self.authed = true;
                    self.password_failures = 0;
                } else {
                    self.password_failures += 1;
                    // Slows guessing down and ends the connection after a few misses.
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    return Err(HostError {
                        code: proto::ACK_PASSWORD,
                        msg: "incorrect password".into(),
                    });
                }
            }
            "commands" => {
                for c in COMMANDS {
                    kv(&mut text, "command", c);
                }
            }
            "notcommands" => {}
            "tagtypes" => {
                if args.is_empty() {
                    for t in ["Artist", "AlbumArtist", "Album", "Title", "Track", "Date"] {
                        kv(&mut text, "tagtype", t);
                    }
                }
            }
            "urlhandlers" => kv(&mut text, "handler", "tidal://"),
            "decoders" | "lsinfo" | "list" | "listall" | "listallinfo" => {}
            "count" => {
                kv(&mut text, "songs", 0);
                kv(&mut text, "playtime", 0);
            }
            "outputs" => {
                kv(&mut text, "outputid", 0);
                kv(&mut text, "outputname", "Slate");
                kv(&mut text, "outputenabled", 1);
            }
            "stats" => {
                kv(&mut text, "artists", 0);
                kv(&mut text, "albums", 0);
                kv(&mut text, "songs", 0);
                kv(&mut text, "uptime", sh.started.elapsed().as_secs());
                kv(&mut text, "playtime", 0);
                kv(&mut text, "db_playtime", 0);
                kv(&mut text, "db_update", 0);
            }
            "replay_gain_status" => kv(&mut text, "replay_gain_mode", "off"),
            "binarylimit" => {
                let n: usize = num(args, 0)?;
                if n < 64 {
                    return Err(HostError::arg("Value too small"));
                }
                self.binary_limit = n.min(1 << 20);
            }
            "status" => {
                let st = host.status().await;
                let version = sh.observe(&st);
                status_lines(&mut text, &st, version);
            }
            "currentsong" => {
                let st = host.status().await;
                sh.observe(&st);
                if let (Some(song), Some(pos)) = (&st.current, st.pos) {
                    song_block(&mut text, song, Some(pos));
                }
            }
            "getvol" => {
                let st = host.status().await;
                kv(&mut text, "volume", st.volume);
            }
            "play" => match args.first() {
                None => host.act(Action::Resume).await?,
                Some(_) => host.act(Action::PlayPos(num(args, 0)?)).await?,
            },
            "playid" => match args.first() {
                None => host.act(Action::Resume).await?,
                Some(_) => {
                    let pos = position_of_id(host.as_ref(), num(args, 0)?).await?;
                    host.act(Action::PlayPos(pos)).await?;
                }
            },
            "pause" => {
                let st = host.status().await;
                let pause = match args.first().map(String::as_str) {
                    Some("1") => true,
                    Some("0") => false,
                    None => st.state == PlayState::Play,
                    Some(_) => return Err(HostError::arg("Boolean (0/1) expected")),
                };
                host.act(Action::Pause(pause)).await?;
            }
            "stop" => host.act(Action::Stop).await?,
            "next" => host.act(Action::Next).await?,
            "previous" => host.act(Action::Previous).await?,
            "seek" => {
                let (pos, secs): (usize, f64) = (num(args, 0)?, num(args, 1)?);
                let st = host.status().await;
                if st.pos != Some(pos) {
                    host.act(Action::PlayPos(pos)).await?;
                }
                host.act(Action::Seek(secs.max(0.0))).await?;
            }
            "seekid" => {
                let (id, secs): (u64, f64) = (num(args, 0)?, num(args, 1)?);
                let pos = position_of_id(host.as_ref(), id).await?;
                let st = host.status().await;
                if st.pos != Some(pos) {
                    host.act(Action::PlayPos(pos)).await?;
                }
                host.act(Action::Seek(secs.max(0.0))).await?;
            }
            "seekcur" => {
                let raw = args
                    .first()
                    .ok_or_else(|| HostError::arg("wrong number of arguments"))?;
                let secs: f64 = raw.parse().map_err(|_| HostError::arg("Float expected"))?;
                let target = if raw.starts_with('+') || raw.starts_with('-') {
                    host.status().await.elapsed + secs
                } else {
                    secs
                };
                host.act(Action::Seek(target.max(0.0))).await?;
            }
            "setvol" => {
                let v: i64 = num(args, 0)?;
                if !(0..=100).contains(&v) {
                    return Err(HostError::arg("Invalid volume value"));
                }
                host.act(Action::SetVolume(v as u8)).await?;
            }
            "volume" => {
                let d: i64 = num(args, 0)?;
                let now = i64::from(host.status().await.volume);
                host.act(Action::SetVolume((now + d).clamp(0, 100) as u8))
                    .await?;
            }
            "random" => host.act(Action::SetRandom(flag(args)?)).await?,
            "repeat" => host.act(Action::SetRepeat(flag(args)?)).await?,
            // Accepted so clients' toggles do not fail; Slate has no such modes.
            "single" | "consume" => {
                flag_or_oneshot(args)?;
            }
            "shuffle" => host.act(Action::Shuffle).await?,
            "clear" => host.act(Action::Clear).await?,
            "playlistinfo" | "playlistid" | "playlist" => {
                let queue = host.queue().await;
                let (range, by_id) = match (cmd, args.first()) {
                    (_, None) => (0..queue.len(), false),
                    ("playlistid", Some(_)) => (0..0, true),
                    (_, Some(a)) => (
                        proto::parse_range(a, queue.len())
                            .ok_or_else(|| HostError::no_exist("Bad song index"))?,
                        false,
                    ),
                };
                if by_id {
                    let id: u64 = num(args, 0)?;
                    let pos = queue
                        .iter()
                        .position(|s| s.id == id)
                        .ok_or_else(|| HostError::no_exist("No such song"))?;
                    song_block(&mut text, &queue[pos], Some(pos));
                } else if cmd == "playlist" {
                    for p in range {
                        kv(&mut text, &p.to_string(), queue[p].uri());
                    }
                } else {
                    for p in range {
                        song_block(&mut text, &queue[p], Some(p));
                    }
                }
            }
            "plchanges" | "plchangesposid" => {
                let theirs: u32 = num(args, 0)?;
                let st = host.status().await;
                let version = sh.observe(&st);
                // A client that is behind gets the whole queue; the queue is rebuilt as a
                // whole whenever it changes, so there is no finer delta to report.
                if theirs != version {
                    let queue = host.queue().await;
                    for (p, s) in queue.iter().enumerate() {
                        if cmd == "plchanges" {
                            song_block(&mut text, s, Some(p));
                        } else {
                            kv(&mut text, "cpos", p);
                            kv(&mut text, "Id", s.id);
                        }
                    }
                }
            }
            "delete" => {
                let queue_len = host.status().await.queue_len;
                let range = proto::parse_range(
                    args.first()
                        .ok_or_else(|| HostError::arg("wrong number of arguments"))?,
                    queue_len,
                )
                .ok_or_else(|| HostError::no_exist("Bad song index"))?;
                // Highest first, so the positions below it hold still.
                for p in range.rev() {
                    host.act(Action::Delete(p)).await?;
                }
            }
            "deleteid" => {
                let pos = position_of_id(host.as_ref(), num(args, 0)?).await?;
                host.act(Action::Delete(pos)).await?;
            }
            "move" => {
                let from = args
                    .first()
                    .and_then(|a| a.split(':').next())
                    .and_then(|a| a.parse().ok())
                    .ok_or_else(|| HostError::arg("Integer expected"))?;
                host.act(Action::Move {
                    from,
                    to: num(args, 1)?,
                })
                .await?;
            }
            "moveid" => {
                let from = position_of_id(host.as_ref(), num(args, 0)?).await?;
                host.act(Action::Move {
                    from,
                    to: num(args, 1)?,
                })
                .await?;
            }
            "add" | "addid" => {
                let uri = args
                    .first()
                    .ok_or_else(|| HostError::arg("wrong number of arguments"))?
                    .clone();
                let pos = match args.get(1) {
                    Some(p) => Some(p.parse().map_err(|_| HostError::arg("Integer expected"))?),
                    None => None,
                };
                if proto::track_id_of(&uri).is_none() {
                    return Err(HostError::no_exist("No such song"));
                }
                let id = proto::track_id_of(&uri).unwrap_or(0);
                host.act(Action::Add { uri, pos }).await?;
                if cmd == "addid" {
                    kv(&mut text, "Id", id);
                }
            }
            "listplaylists" => {
                for name in host.playlists().await? {
                    kv(&mut text, "playlist", name);
                }
            }
            "listplaylistinfo" => {
                let name = args
                    .first()
                    .ok_or_else(|| HostError::arg("wrong number of arguments"))?;
                for s in host.playlist_songs(name.clone()).await? {
                    song_block(&mut text, &s, None);
                }
            }
            "load" => {
                let name = args
                    .first()
                    .ok_or_else(|| HostError::arg("wrong number of arguments"))?;
                host.act(Action::Load(name.clone())).await?;
            }
            "search" | "find" => {
                let terms = proto::parse_terms(args);
                let words = proto::query_text(&terms);
                if words.is_empty() {
                    return Err(HostError::arg("wrong number of arguments"));
                }
                let songs = paced_search(sh, words).await?;
                for s in songs
                    .iter()
                    .filter(|s| proto::matches(s, &terms, cmd == "find"))
                {
                    song_block(&mut text, s, None);
                }
            }
            "albumart" | "readpicture" => {
                let uri = args
                    .first()
                    .ok_or_else(|| HostError::arg("wrong number of arguments"))?;
                let offset: usize = if args.len() > 1 { num(args, 1)? } else { 0 };
                let id = if uri.is_empty() {
                    None
                } else {
                    proto::track_id_of(uri)
                };
                let bytes = host.cover(id).await;
                match bytes {
                    None if cmd == "albumart" => {
                        return Err(HostError::no_exist("No file exists"));
                    }
                    None => {}
                    Some(bytes) => {
                        if offset > bytes.len() {
                            return Err(HostError::arg("Offset too large"));
                        }
                        let end = (offset + self.binary_limit).min(bytes.len());
                        kv(&mut text, "size", bytes.len());
                        if cmd == "readpicture" {
                            kv(&mut text, "type", proto::image_mime(&bytes));
                        }
                        kv(&mut text, "binary", end - offset);
                        out.extend(text.as_bytes());
                        out.extend(&bytes[offset..end]);
                        out.push(b'\n');
                        return Ok(());
                    }
                }
            }
            other => {
                return Err(HostError {
                    code: proto::ACK_UNKNOWN,
                    msg: format!("unknown command \"{other}\""),
                });
            }
        }
        out.extend(text.as_bytes());
        Ok(())
    }
}

/// One search at a time, all clients together, at least [`SEARCH_SPACING`] apart. TIDAL
/// requests then also pass the client's own rate gate.
async fn paced_search(sh: &Arc<Shared>, words: String) -> Result<Vec<Song>, HostError> {
    let mut last = sh.search_gate.lock().await;
    let wait = SEARCH_SPACING.saturating_sub(last.elapsed());
    if !wait.is_zero() {
        tokio::time::sleep(wait).await;
    }
    let result = sh.host.search(words).await;
    *last = Instant::now();
    result
}

async fn position_of_id(host: &dyn Host, id: u64) -> Result<usize, HostError> {
    host.queue()
        .await
        .iter()
        .position(|s| s.id == id)
        .ok_or_else(|| HostError::no_exist("No such song"))
}

fn num<T: std::str::FromStr>(args: &[String], i: usize) -> Result<T, HostError> {
    args.get(i)
        .ok_or_else(|| HostError::arg("wrong number of arguments"))?
        .parse()
        .map_err(|_| HostError::arg("Integer expected"))
}

fn flag(args: &[String]) -> Result<bool, HostError> {
    match args.first().map(String::as_str) {
        Some("1") => Ok(true),
        Some("0") => Ok(false),
        _ => Err(HostError::arg("Boolean (0/1) expected")),
    }
}

fn flag_or_oneshot(args: &[String]) -> Result<(), HostError> {
    match args.first().map(String::as_str) {
        Some("0" | "1" | "oneshot") => Ok(()),
        _ => Err(HostError::arg("Boolean (0/1) expected")),
    }
}

fn status_lines(out: &mut String, st: &Status, version: u32) {
    kv(out, "volume", st.volume);
    kv(out, "repeat", u8::from(st.repeat));
    kv(out, "random", u8::from(st.random));
    kv(out, "single", 0);
    kv(out, "consume", 0);
    kv(out, "partition", "default");
    kv(out, "playlist", version);
    kv(out, "playlistlength", st.queue_len);
    kv(out, "mixrampdb", 0);
    kv(out, "state", st.state.word());
    if let (Some(pos), Some(song)) = (st.pos, &st.current) {
        kv(out, "song", pos);
        kv(out, "songid", song.id);
        if st.state != PlayState::Stop {
            kv(
                out,
                "time",
                format!(
                    "{}:{}",
                    st.elapsed.round() as u64,
                    st.duration.round() as u64
                ),
            );
            kv(out, "elapsed", format!("{:.3}", st.elapsed));
            kv(out, "duration", format!("{:.3}", st.duration));
        }
    }
    if let Some((pos, id)) = st.next {
        kv(out, "nextsong", pos);
        kv(out, "nextsongid", id);
    }
}

/// Used by tests and the app: an IPv6-free description of where the server listens.
pub fn describe(addr: SocketAddr) -> String {
    match addr.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => format!("0.0.0.0:{}", addr.port()),
        IpAddr::V6(ip) if ip == Ipv6Addr::UNSPECIFIED => format!("[::]:{}", addr.port()),
        _ => addr.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peers_without_the_password_get_a_short_leash() {
        assert_eq!(read_timeout(true), Duration::from_secs(600));
        assert_eq!(read_timeout(false), Duration::from_secs(30));
        assert!(PREAUTH_TIMEOUT < IDLE_TIMEOUT);
    }
}
