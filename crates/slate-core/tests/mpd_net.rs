//! The MPD server over real loopback sockets, with a fake player behind it.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use slate_core::mpd::server::{self, Handle};
use slate_core::mpd::{Action, BoxFut, Config, Host, HostError, PlayState, Song, Status};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[derive(Default)]
struct Inner {
    queue: Vec<Song>,
    pos: Option<usize>,
    state: PlayState,
    volume: u8,
    acts: Vec<Action>,
    searches: Vec<Instant>,
}

#[derive(Default)]
struct Fake(Mutex<Inner>);

fn song(id: u64, title: &str, artist: &str) -> Song {
    Song {
        id,
        title: title.into(),
        artist: artist.into(),
        album: "Album".into(),
        duration: 200.0,
        track: Some(id as u32),
        ..Song::default()
    }
}

impl Fake {
    fn with_queue() -> Arc<Self> {
        let f = Fake::default();
        {
            let mut i = f.0.lock().unwrap();
            i.queue = vec![
                song(1, "One", "A"),
                song(2, "Two", "B"),
                song(3, "Three", "A"),
            ];
            i.pos = Some(0);
            i.state = PlayState::Play;
            i.volume = 70;
        }
        Arc::new(f)
    }

    fn status_now(&self) -> Status {
        let i = self.0.lock().unwrap();
        Status {
            state: i.state,
            volume: i.volume,
            elapsed: 12.0,
            duration: 200.0,
            queue_len: i.queue.len(),
            queue_hash: i.queue.iter().fold(0u64, |h, s| h * 31 + s.id),
            pos: i.pos,
            current: i.pos.and_then(|p| i.queue.get(p).cloned()),
            next: i
                .pos
                .and_then(|p| i.queue.get(p + 1).map(|s| (p + 1, s.id))),
            ..Status::default()
        }
    }
}

impl Host for Fake {
    fn status(&self) -> BoxFut<'_, Status> {
        Box::pin(async move { self.status_now() })
    }
    fn queue(&self) -> BoxFut<'_, Vec<Song>> {
        Box::pin(async move { self.0.lock().unwrap().queue.clone() })
    }
    fn act(&self, action: Action) -> BoxFut<'_, Result<(), HostError>> {
        Box::pin(async move {
            let mut i = self.0.lock().unwrap();
            i.acts.push(action.clone());
            match action {
                Action::Delete(p) if p >= i.queue.len() => {
                    Err(HostError::no_exist("Bad song index"))
                }
                Action::Delete(p) => {
                    i.queue.remove(p);
                    Ok(())
                }
                Action::SetVolume(v) => {
                    i.volume = v;
                    Ok(())
                }
                _ => Ok(()),
            }
        })
    }
    fn search(&self, text: String) -> BoxFut<'_, Result<Vec<Song>, HostError>> {
        Box::pin(async move {
            self.0.lock().unwrap().searches.push(Instant::now());
            Ok(vec![
                song(10, &format!("{text} song"), "Found"),
                song(11, "Other", "Else"),
            ])
        })
    }
    fn playlists(&self) -> BoxFut<'_, Result<Vec<String>, HostError>> {
        Box::pin(async move { Ok(vec!["Mix one".to_string(), "Two".to_string()]) })
    }
    fn playlist_songs(&self, name: String) -> BoxFut<'_, Result<Vec<Song>, HostError>> {
        Box::pin(async move {
            if name == "Mix one" {
                Ok(vec![song(20, "In playlist", "P")])
            } else {
                Err(HostError::no_exist("No such playlist"))
            }
        })
    }
    fn cover(&self, _id: Option<u64>) -> BoxFut<'_, Option<Vec<u8>>> {
        Box::pin(async move { Some((0..20_000u32).map(|n| (n % 251) as u8).collect()) })
    }
}

async fn start(host: Arc<Fake>, password: &str) -> Handle {
    server::start(
        host,
        Config {
            port: 0,
            lan: false,
            password: password.into(),
        },
    )
    .await
    .expect("server")
}

struct Conn(TcpStream);

impl Conn {
    async fn open(h: &Handle) -> Conn {
        let mut c = Conn(TcpStream::connect(h.local_addr()).await.expect("connect"));
        let greeting = c.line().await;
        assert!(greeting.starts_with("OK MPD "), "{greeting}");
        c
    }

    async fn line(&mut self) -> String {
        let mut out = Vec::new();
        loop {
            let mut b = [0u8; 1];
            let n = tokio::time::timeout(Duration::from_secs(10), self.0.read(&mut b))
                .await
                .expect("answer in time")
                .expect("read");
            if n == 0 || b[0] == b'\n' {
                return String::from_utf8_lossy(&out).into_owned();
            }
            out.push(b[0]);
        }
    }

    /// Sends a command and returns every line up to and including `OK` / `ACK …`.
    async fn cmd(&mut self, c: &str) -> Vec<String> {
        self.0.write_all(format!("{c}\n").as_bytes()).await.unwrap();
        self.reply().await
    }

    async fn reply(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        loop {
            let l = self.line().await;
            let done = l == "OK" || l.starts_with("ACK ");
            lines.push(l);
            if done {
                return lines;
            }
        }
    }
}

fn has(lines: &[String], l: &str) -> bool {
    lines.iter().any(|x| x == l)
}

#[tokio::test]
async fn status_current_song_and_the_queue() {
    let h = start(Fake::with_queue(), "").await;
    let mut c = Conn::open(&h).await;
    assert_eq!(c.cmd("ping").await, vec!["OK"]);
    let st = c.cmd("status").await;
    for want in [
        "volume: 70",
        "state: play",
        "song: 0",
        "songid: 1",
        "playlistlength: 3",
        "nextsong: 1",
        "nextsongid: 2",
        "time: 12:200",
        "random: 0",
    ] {
        assert!(has(&st, want), "{want} in {st:?}");
    }
    let cur = c.cmd("currentsong").await;
    assert!(
        has(&cur, "Title: One") && has(&cur, "Pos: 0") && has(&cur, "Id: 1"),
        "{cur:?}"
    );
    let all = c.cmd("playlistinfo").await;
    assert_eq!(all.iter().filter(|l| l.starts_with("file: ")).count(), 3);
    let one = c.cmd("playlistinfo 1").await;
    assert!(has(&one, "Title: Two"), "{one:?}");
    assert!(c.cmd("playlistinfo 9").await[0].starts_with("ACK [50@0] {playlistinfo}"));
    let by_id = c.cmd("playlistid 3").await;
    assert!(has(&by_id, "Pos: 2"), "{by_id:?}");
    h.stop();
}

#[tokio::test]
async fn commands_reach_the_player() {
    let host = Fake::with_queue();
    let h = start(host.clone(), "").await;
    let mut c = Conn::open(&h).await;
    for line in [
        "pause 1",
        "play 2",
        "playid 3",
        "seekcur 30",
        "setvol 40",
        "random 1",
        "repeat 0",
        "next",
        "previous",
        "stop",
        "deleteid 2",
        "move 0 1",
        "add tidal://track/55",
        "load \"Mix one\"",
        "clear",
        "volume +5",
    ] {
        let r = c.cmd(line).await;
        assert_eq!(r, vec!["OK"], "{line}: {r:?}");
    }
    let acts = host.0.lock().unwrap().acts.clone();
    assert!(acts.contains(&Action::Pause(true)));
    assert!(acts.contains(&Action::PlayPos(2)));
    assert!(acts.contains(&Action::Seek(30.0)));
    assert!(acts.contains(&Action::SetVolume(40)));
    assert!(acts.contains(&Action::SetRandom(true)));
    assert!(acts.contains(&Action::Move { from: 0, to: 1 }));
    assert!(acts.contains(&Action::Add {
        uri: "tidal://track/55".into(),
        pos: None
    }));
    assert!(acts.contains(&Action::Load("Mix one".into())));
    assert!(acts.contains(&Action::SetVolume(45)));
    h.stop();
}

#[tokio::test]
async fn bad_input_is_an_ack_never_a_crash() {
    let h = start(Fake::with_queue(), "").await;
    let mut c = Conn::open(&h).await;
    assert!(c.cmd("frobnicate").await[0].starts_with("ACK [5@0] {frobnicate} unknown command"));
    assert!(c.cmd("play x").await[0].starts_with("ACK [2@0] {play}"));
    assert!(c.cmd("setvol 101").await[0].starts_with("ACK [2@0] {setvol}"));
    assert!(c.cmd("setvol").await[0].starts_with("ACK [2@0]"));
    assert!(c.cmd("add \"unterminated").await[0].starts_with("ACK [2@0]"));
    assert!(c.cmd("add file:///music/x.flac").await[0].starts_with("ACK [50@0]"));
    assert!(c.cmd("deleteid 99").await[0].starts_with("ACK [50@0]"));
    assert!(c.cmd("delete 7").await[0].starts_with("ACK [50@0]"));
    assert!(c.cmd("random 2").await[0].starts_with("ACK [2@0]"));
    assert!(c.cmd("idle bogus").await[0].starts_with("ACK [2@0]"));
    assert!(c.cmd("").await[0].starts_with("ACK [5@0]"));
    // Raw bytes that are not UTF-8, then a normal command on the same connection.
    c.0.write_all(&[0xFF, 0xFE, b'\n']).await.unwrap();
    assert!(c.reply().await[0].starts_with("ACK"));
    assert_eq!(c.cmd("ping").await, vec!["OK"]);
    // Every command `commands` lists is really implemented.
    let listed = c.cmd("commands").await;
    for l in listed.iter().filter_map(|l| l.strip_prefix("command: ")) {
        if matches!(l, "idle" | "noidle" | "close") {
            continue;
        }
        let r = c.cmd(l).await;
        assert!(!r[0].contains("unknown command"), "{l}: {r:?}");
    }
    h.stop();
}

#[tokio::test]
async fn command_lists() {
    let h = start(Fake::with_queue(), "").await;
    let mut c = Conn::open(&h).await;
    c.0.write_all(b"command_list_ok_begin\nping\nsetvol 10\nstatus\ncommand_list_end\n")
        .await
        .unwrap();
    let r = c.reply().await;
    assert_eq!(r.iter().filter(|l| *l == "list_OK").count(), 3, "{r:?}");
    assert!(has(&r, "volume: 10") && r.last().unwrap() == "OK");
    // An error stops the list and names the failing command's index.
    c.0.write_all(b"command_list_begin\nping\nfrobnicate\nping\ncommand_list_end\n")
        .await
        .unwrap();
    let r = c.reply().await;
    assert_eq!(r.len(), 1);
    assert!(r[0].starts_with("ACK [5@1] {frobnicate}"), "{r:?}");
    // `idle` is not allowed inside one.
    c.0.write_all(b"command_list_begin\nidle\ncommand_list_end\n")
        .await
        .unwrap();
    assert!(c.reply().await[0].starts_with("ACK [2@0] {idle}"));
    h.stop();
}

#[tokio::test]
async fn idle_wakes_on_a_change_and_noidle_ends_it() {
    let host = Fake::with_queue();
    let h = start(host.clone(), "").await;
    let mut c = Conn::open(&h).await;
    // First status: the server learns the baseline.
    c.cmd("status").await;
    c.0.write_all(b"idle player mixer\n").await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    host.0.lock().unwrap().volume = 20;
    h.feed(&host.status_now());
    let r = c.reply().await;
    assert_eq!(r, vec!["changed: mixer", "OK"]);
    // A change before `idle` is still reported when `idle` comes.
    host.0.lock().unwrap().state = PlayState::Pause;
    h.feed(&host.status_now());
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(c.cmd("idle player").await, vec!["changed: player", "OK"]);
    // Nothing relevant: waits, and noidle ends it with OK.
    c.0.write_all(b"idle options\n").await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    c.0.write_all(b"noidle\n").await.unwrap();
    assert_eq!(c.reply().await, vec!["OK"]);
    // Queue changes show up as playlist, and move the version `plchanges` compares.
    let v = |lines: &[String]| -> String {
        lines
            .iter()
            .find_map(|l| l.strip_prefix("playlist: "))
            .unwrap()
            .to_string()
    };
    let before = v(&c.cmd("status").await);
    host.0.lock().unwrap().queue.push(song(4, "Four", "C"));
    c.0.write_all(b"idle playlist\n").await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    h.feed(&host.status_now());
    assert_eq!(c.reply().await, vec!["changed: playlist", "OK"]);
    let after = v(&c.cmd("status").await);
    assert_ne!(before, after);
    assert_eq!(c.cmd(&format!("plchanges {after}")).await, vec!["OK"]);
    let all = c.cmd(&format!("plchanges {before}")).await;
    assert_eq!(all.iter().filter(|l| l.starts_with("file: ")).count(), 4);
    h.stop();
}

#[tokio::test]
async fn a_password_gates_everything_else() {
    let h = start(Fake::with_queue(), "s3cret").await;
    let mut c = Conn::open(&h).await;
    assert_eq!(c.cmd("ping").await, vec!["OK"]);
    assert!(c.cmd("status").await[0].starts_with("ACK [4@0] {status}"));
    assert!(c.cmd("pause").await[0].starts_with("ACK [4@0]"));
    assert!(c.cmd("password nope").await[0].starts_with("ACK [3@0] {password}"));
    assert!(c.cmd("status").await[0].starts_with("ACK [4@0]"));
    assert_eq!(c.cmd("password s3cret").await, vec!["OK"]);
    assert!(has(&c.cmd("status").await, "state: play"));
    // Three wrong guesses end the connection.
    let mut d = Conn::open(&h).await;
    for _ in 0..3 {
        assert!(d.cmd("password x").await[0].starts_with("ACK [3@0]"));
    }
    let mut buf = [0u8; 1];
    assert_eq!(
        d.0.read(&mut buf).await.unwrap_or(0),
        0,
        "connection closed"
    );
    h.stop();
}

#[tokio::test]
async fn lan_mode_needs_a_password() {
    let refused = server::start(
        Fake::with_queue(),
        Config {
            port: 0,
            lan: true,
            password: String::new(),
        },
    )
    .await;
    assert!(refused.is_err());
    let ok = server::start(
        Fake::with_queue(),
        Config {
            port: 0,
            lan: true,
            password: "x".into(),
        },
    )
    .await
    .expect("lan with password");
    assert!(ok.local_addr().ip().is_unspecified());
    ok.stop();
    // Local mode listens on loopback only.
    let local = start(Fake::with_queue(), "").await;
    assert!(local.local_addr().ip().is_loopback());
    local.stop();
}

#[tokio::test]
async fn limits_clients_line_size_and_rate() {
    let h = start(Fake::with_queue(), "").await;
    let mut conns = Vec::new();
    for _ in 0..server::MAX_CLIENTS {
        conns.push(Conn::open(&h).await);
    }
    let mut extra = TcpStream::connect(h.local_addr()).await.unwrap();
    let mut s = String::new();
    extra.read_to_string(&mut s).await.unwrap();
    assert!(s.starts_with("ACK [52@0]") && s.contains("too many"), "{s}");
    // A slot frees up when a client leaves.
    let mut gone = conns.pop().unwrap();
    gone.0.write_all(b"close\n").await.unwrap();
    drop(gone);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut again = Conn::open(&h).await;
    assert_eq!(again.cmd("ping").await, vec!["OK"]);

    // A line over 64 KiB is refused and the connection closed.
    let mut c = conns.pop().unwrap();
    c.0.write_all(&vec![b'a'; 70_000]).await.unwrap();
    let r = c.line().await;
    assert!(r.starts_with("ACK [2@0]"), "{r}");
    let mut buf = [0u8; 1];
    assert_eq!(c.0.read(&mut buf).await.unwrap_or(0), 0);

    // 60 commands in 10 s, then ACK; other clients are not affected.
    let mut busy = conns.pop().unwrap();
    for n in 0..server::RATE_COMMANDS {
        assert_eq!(busy.cmd("ping").await, vec!["OK"], "command {n}");
    }
    assert!(busy.cmd("ping").await[0].starts_with("ACK [52@0]"));
    assert_eq!(conns[0].cmd("ping").await, vec!["OK"]);
    h.stop();
}

#[tokio::test]
async fn search_find_and_stored_playlists() {
    let host = Fake::with_queue();
    let h = start(host.clone(), "").await;
    let mut c = Conn::open(&h).await;
    let r = c.cmd("search any \"daft\"").await;
    assert!(has(&r, "Title: daft song"), "{r:?}");
    assert!(!has(&r, "Title: Other"), "filtered by the term");
    let r = c.cmd("find artist \"Found\"").await;
    assert_eq!(
        r.iter().filter(|l| l.starts_with("file: ")).count(),
        1,
        "{r:?}"
    );
    let r = c.cmd("search \"(Artist == 'Found')\"").await;
    assert!(has(&r, "Artist: Found"), "{r:?}");
    assert!(c.cmd("search").await[0].starts_with("ACK [2@0]"));
    assert_eq!(
        c.cmd("listplaylists").await,
        vec!["playlist: Mix one", "playlist: Two", "OK"]
    );
    let r = c.cmd("listplaylistinfo \"Mix one\"").await;
    assert!(has(&r, "Title: In playlist"));
    assert!(c.cmd("listplaylistinfo Nope").await[0].starts_with("ACK [50@0]"));

    // Two clients searching at once reach TIDAL at least 500 ms apart.
    let mut d = Conn::open(&h).await;
    host.0.lock().unwrap().searches.clear();
    let (a, b) = tokio::join!(c.cmd("search any x"), d.cmd("search any y"));
    assert_eq!(a.last().unwrap(), "OK");
    assert_eq!(b.last().unwrap(), "OK");
    let s = host.0.lock().unwrap().searches.clone();
    assert_eq!(s.len(), 2);
    let gap = s[1].duration_since(s[0]);
    assert!(gap >= Duration::from_millis(450), "{gap:?}");
    h.stop();
}

#[tokio::test]
async fn cover_art_comes_in_chunks() {
    let h = start(Fake::with_queue(), "").await;
    let mut c = Conn::open(&h).await;
    assert_eq!(c.cmd("binarylimit 10000").await, vec!["OK"]);
    c.0.write_all(b"albumart tidal://track/1 0\n")
        .await
        .unwrap();
    assert_eq!(c.line().await, "size: 20000");
    assert_eq!(c.line().await, "binary: 10000");
    let mut data = vec![0u8; 10_000];
    c.0.read_exact(&mut data).await.unwrap();
    assert_eq!(data[5], 5);
    assert_eq!(c.line().await, "");
    assert_eq!(c.line().await, "OK");
    c.0.write_all(b"albumart tidal://track/1 10000\n")
        .await
        .unwrap();
    assert_eq!(c.line().await, "size: 20000");
    assert_eq!(c.line().await, "binary: 10000");
    let mut data = vec![0u8; 10_000];
    c.0.read_exact(&mut data).await.unwrap();
    assert_eq!(c.line().await, "");
    assert_eq!(c.line().await, "OK");
    assert!(c.cmd("albumart tidal://track/1 99999").await[0].starts_with("ACK [2@0]"));
    let r = {
        c.0.write_all(b"readpicture tidal://track/1 0\n")
            .await
            .unwrap();
        c.line().await
    };
    assert_eq!(r, "size: 20000");
    assert_eq!(c.line().await, "type: image/jpeg");
    h.stop();
}

#[tokio::test]
async fn stopping_closes_the_socket_and_every_connection() {
    let h = start(Fake::with_queue(), "").await;
    let addr = h.local_addr();
    let mut c = Conn::open(&h).await;
    assert_eq!(h.client_count(), 1);
    h.stop();
    let mut buf = [0u8; 1];
    let n = tokio::time::timeout(Duration::from_secs(5), c.0.read(&mut buf))
        .await
        .expect("closed in time")
        .unwrap_or(0);
    assert_eq!(n, 0);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        TcpStream::connect(addr).await.is_err(),
        "no socket after stop"
    );
    drop(h);
}

#[tokio::test]
async fn a_client_that_hangs_up_mid_command_changes_nothing() {
    let host = Fake::with_queue();
    let h = start(host, "").await;
    {
        let mut c = TcpStream::connect(h.local_addr()).await.unwrap();
        c.write_all(b"command_list_begin\nping\n").await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(h.client_count(), 0);
    let mut c = Conn::open(&h).await;
    assert_eq!(c.cmd("ping").await, vec!["OK"]);
    h.stop();
}

#[tokio::test]
async fn idle_and_command_lists_need_the_password_too() {
    let h = start(Fake::with_queue(), "secret").await;
    let mut c = Conn::open(&h).await;
    // `idle` waits without a timeout; before the password it must not wait at all.
    let r = c.cmd("idle").await;
    assert!(r[0].starts_with("ACK [4@0] {idle}"), "{r:?}");
    c.0.write_all(b"command_list_begin\nstatus\ncommand_list_end\n")
        .await
        .unwrap();
    let r = c.reply().await;
    assert!(r[0].starts_with("ACK [4@0] {status}"), "{r:?}");
    assert_eq!(c.cmd("password secret").await, vec!["OK"]);
    c.0.write_all(b"idle\nnoidle\n").await.unwrap();
    assert_eq!(c.reply().await, vec!["OK"]);
    h.stop();
}
