//! Listening-party wire protocol and the rules around it. Pure: no sockets here.
//!
//! Everyone plays from their own TIDAL account; only playback state travels. The host is
//! authoritative and sends a message only when something changes. Every message from a peer is
//! untrusted input: it is size-limited, versioned and validated before use.

use serde::{Deserialize, Serialize};

/// 1 = Slate 1.2, 2 = Slate 1.3 (requests, co-host, vote skip). A peer on another version is
/// refused with a message saying which side should update; it never crashes either side.
pub const PROTOCOL_VERSION: u32 = 2;
/// Requests shown to co-hosts in one message (keeps it under [`MAX_MESSAGE_BYTES`]).
pub const MAX_REQUEST_VIEWS: usize = 12;
/// Largest message accepted from a peer, in bytes.
pub const MAX_MESSAGE_BYTES: usize = 4096;
pub const MAX_GUESTS: usize = 10;
/// Tracks the host announces ahead, so guests can prefetch.
pub const PREFETCH_TRACKS: usize = 5;
/// Seconds between heartbeats while a party is active.
pub const HEARTBEAT_SECS: u64 = 30;
/// Drift (ms) above which a guest seeks to catch up.
pub const DRIFT_LIMIT_MS: i64 = 750;
pub const MAX_NAME_CHARS: usize = 32;
pub const TICKET_PREFIX: &str = "slate://party/";
/// Suggestions per guest per minute.
pub const SUGGEST_PER_MINUTE: usize = 3;
pub const MAX_PENDING_SUGGESTIONS: usize = 20;

/// What the host tells guests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum HostMsg {
    Welcome {
        v: u32,
        host: String,
        guests: Vec<String>,
        state: PlayState,
        allow_suggest: bool,
        /// The name the host gave this guest (names are made unique).
        #[serde(default)]
        you: String,
        #[serde(default)]
        vote_skip: bool,
    },
    /// A track started (or the first state for a late joiner).
    Track {
        id: u64,
        position_ms: u64,
        playing: bool,
        at_ms: u64,
    },
    /// Pause / resume / seek.
    State {
        position_ms: u64,
        playing: bool,
        at_ms: u64,
    },
    /// The next few track ids, for prefetch.
    Queue {
        next: Vec<u64>,
        /// Who asked for each of `next` ("" for the host's own tracks).
        #[serde(default)]
        by: Vec<String>,
    },
    Guests {
        guests: Vec<String>,
        #[serde(default)]
        cohosts: Vec<String>,
    },
    /// The answer to a request.
    RequestResult {
        track_id: u64,
        ok: bool,
        reason: String,
    },
    /// Co-hosts only: the requests waiting for a decision.
    Requests {
        pending: Vec<ReqView>,
        total: u32,
    },
    /// Sent to a guest when they are made (or stop being) co-host.
    Role {
        cohost: bool,
    },
    Notice {
        text: String,
    },
    Pong {
        token: u64,
        host_ms: u64,
    },
    Heartbeat {
        position_ms: u64,
        playing: bool,
        at_ms: u64,
    },
    Kicked,
    Ended,
    Rejected {
        reason: String,
    },
    /// A message from a newer Slate this one does not know; ignored.
    #[serde(other)]
    Unknown,
}

/// A waiting request as shown to the host's helpers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReqView {
    pub id: u64,
    pub guest: String,
    pub title: String,
    pub artist: String,
    pub quality: String,
    pub unavailable: u32,
}

/// What a guest tells the host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum GuestMsg {
    Hello {
        v: u32,
        name: String,
        secret: String,
    },
    Ping {
        token: u64,
    },
    Suggest {
        track_id: u64,
    },
    /// This guest cannot play the track (region or plan).
    Unavailable {
        track_id: u64,
    },
    VoteSkip,
    /// Co-host only.
    Decide {
        request_id: u64,
        approve: bool,
    },
    /// Co-host only.
    DecideAll {
        approve: bool,
    },
    /// Co-host only.
    Skip,
    /// Co-host only: move upcoming entry `from` to `to`.
    Move {
        from: u32,
        to: u32,
    },
    Leave,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlayState {
    pub track: Option<u64>,
    pub position_ms: u64,
    pub playing: bool,
    /// Host clock (ms) when `position_ms` was true.
    pub at_ms: u64,
}

pub fn encode<T: Serialize>(m: &T) -> Result<Vec<u8>, String> {
    let b = serde_json::to_vec(m).map_err(|e| e.to_string())?;
    if b.len() > MAX_MESSAGE_BYTES {
        return Err("message too large".into());
    }
    Ok(b)
}

/// Parses a message from a peer; anything oversized or malformed is an error.
pub fn decode<T: for<'a> Deserialize<'a>>(bytes: &[u8]) -> Result<T, String> {
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err("message too large".into());
    }
    serde_json::from_slice(bytes).map_err(|_| "malformed message".to_string())
}

/// A guest's display name: no control characters, at most 32 characters, never empty.
pub fn clean_name(raw: &str) -> String {
    let n: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect();
    let n = n.trim();
    if n.is_empty() {
        "Guest".into()
    } else {
        n.into()
    }
}

/// What a host tells a guest whose Slate speaks protocol version `theirs`.
pub fn version_gap(theirs: u32) -> String {
    if theirs < PROTOCOL_VERSION {
        "This party's host is on a newer version of Slate. Update Slate to join.".into()
    } else {
        "This party's host is on an older version of Slate. Ask them to update.".into()
    }
}

/// What a guest says about a host that speaks protocol version `theirs`.
pub fn host_version_gap(theirs: u32) -> String {
    if theirs > PROTOCOL_VERSION {
        "The host is on a newer version of Slate. Update Slate to join.".into()
    } else {
        "The host is on an older version of Slate. Ask them to update.".into()
    }
}

/// Clips text for the wire, on a character boundary.
pub fn clip(s: &str, max_chars: usize) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(max_chars)
        .collect()
}

/// Validates a guest message beyond its shape.
pub fn validate_guest(m: &GuestMsg) -> Result<(), String> {
    match m {
        GuestMsg::Hello { v, .. } if *v != PROTOCOL_VERSION => Err(version_gap(*v)),
        GuestMsg::Suggest { track_id } | GuestMsg::Unavailable { track_id } if *track_id == 0 => {
            Err("bad track id".into())
        }
        GuestMsg::Move { from, to } if *from > 64 || *to > 64 => Err("bad position".into()),
        _ => Ok(()),
    }
}

/// Validates a host message beyond its shape.
pub fn validate_host(m: &HostMsg) -> Result<(), String> {
    match m {
        HostMsg::Welcome { v, .. } if *v != PROTOCOL_VERSION => Err(host_version_gap(*v)),
        HostMsg::Queue { next, by }
            if next.len() > PREFETCH_TRACKS * 2 || by.len() > next.len() =>
        {
            Err("queue too long".into())
        }
        HostMsg::Requests { pending, .. } if pending.len() > MAX_REQUEST_VIEWS => {
            Err("too many requests".into())
        }
        HostMsg::Welcome { guests, .. } | HostMsg::Guests { guests, .. }
            if guests.len() > MAX_GUESTS + 1 =>
        {
            Err("too many guests".into())
        }
        _ => Ok(()),
    }
}

// ---- clock ----

/// One ping/pong exchange: our send and receive times and the host's clock in the reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PingSample {
    pub sent_ms: u64,
    pub received_ms: u64,
    pub host_ms: u64,
}

impl PingSample {
    pub fn rtt(&self) -> u64 {
        self.received_ms.saturating_sub(self.sent_ms)
    }

    /// host clock − local clock, assuming the reply was stamped halfway through the round trip.
    pub fn offset(&self) -> i64 {
        let mid = (self.sent_ms + self.received_ms) / 2;
        self.host_ms as i64 - mid as i64
    }
}

/// Offset from the sample with the shortest round trip (the most trustworthy).
pub fn best_offset(samples: &[PingSample]) -> Option<i64> {
    samples
        .iter()
        .min_by_key(|s| s.rtt())
        .map(PingSample::offset)
}

/// Where the host's playback is now, on the host clock.
pub fn host_position_now(position_ms: u64, playing: bool, at_ms: u64, host_now_ms: u64) -> u64 {
    if playing {
        position_ms + host_now_ms.saturating_sub(at_ms)
    } else {
        position_ms
    }
}

/// What a guest should do about its playback position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Correction {
    None,
    SeekTo(u64),
}

/// Drift beyond 750 ms is fixed with a seek; smaller drift is left alone.
pub fn correction(local_pos_ms: u64, target_pos_ms: u64) -> Correction {
    let drift = local_pos_ms as i64 - target_pos_ms as i64;
    if drift.abs() > DRIFT_LIMIT_MS {
        Correction::SeekTo(target_pos_ms)
    } else {
        Correction::None
    }
}

// ---- tickets ----

/// `slate://party/<ticket>` → the ticket, or the text itself when a bare ticket was pasted.
pub fn parse_ticket(input: &str) -> Option<String> {
    let t = input.trim();
    let t = t.strip_prefix(TICKET_PREFIX).unwrap_or(t);
    let ok = !t.is_empty()
        && t.len() <= 2048
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    ok.then(|| t.to_string())
}

pub fn ticket_link(ticket: &str) -> String {
    format!("{TICKET_PREFIX}{ticket}")
}

// ---- host-side bookkeeping ----

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guest {
    pub id: u64,
    pub name: String,
    pub cohost: bool,
}

#[derive(Debug, Default)]
pub struct Roster {
    guests: Vec<Guest>,
    next_id: u64,
}

impl Roster {
    /// Adds a guest; refuses when the party is full.
    pub fn join(&mut self, name: &str) -> Result<u64, String> {
        if self.guests.len() >= MAX_GUESTS {
            return Err(format!("The party is full ({MAX_GUESTS} guests)."));
        }
        self.next_id += 1;
        self.guests.push(Guest {
            id: self.next_id,
            name: unique_name(&self.guests, &clean_name(name)),
            cohost: false,
        });
        Ok(self.next_id)
    }

    pub fn leave(&mut self, id: u64) -> bool {
        let n = self.guests.len();
        self.guests.retain(|g| g.id != id);
        self.guests.len() != n
    }

    pub fn set_cohost(&mut self, id: u64, on: bool) -> bool {
        self.guests
            .iter_mut()
            .find(|g| g.id == id)
            .map(|g| g.cohost = on)
            .is_some()
    }

    pub fn is_cohost(&self, id: u64) -> bool {
        self.guests.iter().any(|g| g.id == id && g.cohost)
    }

    pub fn cohosts(&self) -> Vec<String> {
        self.guests
            .iter()
            .filter(|g| g.cohost)
            .map(|g| g.name.clone())
            .collect()
    }

    /// (id, name, cohost) in join order, for choosing a successor.
    pub fn successor_input(&self) -> Vec<(u64, String, bool)> {
        self.guests
            .iter()
            .map(|g| (g.id, g.name.clone(), g.cohost))
            .collect()
    }

    pub fn names(&self) -> Vec<String> {
        self.guests.iter().map(|g| g.name.clone()).collect()
    }

    pub fn guests(&self) -> &[Guest] {
        &self.guests
    }

    pub fn len(&self) -> usize {
        self.guests.len()
    }

    pub fn is_empty(&self) -> bool {
        self.guests.is_empty()
    }
}

fn unique_name(existing: &[Guest], name: &str) -> String {
    if !existing.iter().any(|g| g.name == name) {
        return name.to_string();
    }
    (2..)
        .map(|n| format!("{name} {n}"))
        .find(|c| !existing.iter().any(|g| &g.name == c))
        .unwrap_or_else(|| name.to_string())
}

/// Per-guest sliding-window limit on suggestions.
#[derive(Debug, Default)]
pub struct RateLimit {
    times: std::collections::HashMap<u64, Vec<u64>>,
}

impl RateLimit {
    /// Whether guest `id` may suggest at `now_ms`; records the attempt when allowed.
    pub fn allow(&mut self, id: u64, now_ms: u64) -> bool {
        let t = self.times.entry(id).or_default();
        t.retain(|x| now_ms.saturating_sub(*x) < 60_000);
        if t.len() >= SUGGEST_PER_MINUTE {
            return false;
        }
        t.push(now_ms);
        true
    }

    pub fn forget(&mut self, id: u64) {
        self.times.remove(&id);
    }
}

/// Messages one peer may send in [`FLOOD_WINDOW_MS`]; a peer that sends more is disconnected.
/// A well-behaved guest sends a handful a minute (requests are limited separately).
pub const FLOOD_MESSAGES: u32 = 60;
pub const FLOOD_WINDOW_MS: u64 = 10_000;

/// Counts one peer's messages in fixed windows (no allocation, one per connection).
#[derive(Debug, Default)]
pub struct Flood {
    window_start: u64,
    count: u32,
}

impl Flood {
    /// Records a message at `now_ms`; false once the peer is over the limit.
    pub fn allow(&mut self, now_ms: u64) -> bool {
        if now_ms.saturating_sub(self.window_start) >= FLOOD_WINDOW_MS {
            self.window_start = now_ms;
            self.count = 0;
        }
        self.count += 1;
        self.count <= FLOOD_MESSAGES
    }
}

/// A Slate 1.2 host refuses a newer guest with "protocol version 2 is not supported (need 1)".
/// This turns that (and any other refusal) into what a guest should read.
pub fn rejection_text(reason: &str) -> String {
    let need = reason
        .strip_prefix("protocol version ")
        .and_then(|r| r.split_once("(need "))
        .and_then(|(_, n)| n.trim_end_matches(')').trim().parse::<u32>().ok());
    match need {
        Some(v) => host_version_gap(v),
        None => clip(reason, 200),
    }
}

/// The next tracks to announce: the upcoming ids, at most [`PREFETCH_TRACKS`].
pub fn prefetch_list(upcoming: &[u64]) -> Vec<u64> {
    upcoming.iter().copied().take(PREFETCH_TRACKS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip_and_are_compact() {
        let m = HostMsg::Track {
            id: 5,
            position_ms: 1200,
            playing: true,
            at_ms: 99,
        };
        let b = encode(&m).expect("encode");
        assert!(b.len() < 100);
        assert_eq!(decode::<HostMsg>(&b), Ok(m));
        let g = GuestMsg::Suggest { track_id: 7 };
        assert_eq!(decode::<GuestMsg>(&encode(&g).expect("encode")), Ok(g));
    }

    #[test]
    fn oversized_and_malformed_input_is_refused() {
        assert!(decode::<GuestMsg>(&vec![b' '; MAX_MESSAGE_BYTES + 1]).is_err());
        assert!(decode::<GuestMsg>(b"{\"nope\":1}").is_err());
        assert!(decode::<GuestMsg>(b"garbage").is_err());
        assert!(decode::<HostMsg>(b"").is_err());
        let big = HostMsg::Queue {
            next: vec![1; 3000],
            by: vec![],
        };
        assert!(encode(&big).is_err());
    }

    #[test]
    fn validation_rejects_wrong_versions_and_oversized_lists() {
        assert!(validate_guest(&GuestMsg::Hello {
            v: 9,
            name: "x".into(),
            secret: String::new()
        })
        .is_err());
        assert!(validate_guest(&GuestMsg::Hello {
            v: PROTOCOL_VERSION,
            name: "x".into(),
            secret: String::new()
        })
        .is_ok());
        assert!(validate_guest(&GuestMsg::Suggest { track_id: 0 }).is_err());
        assert!(validate_host(&HostMsg::Queue {
            next: vec![1; 11],
            by: vec![]
        })
        .is_err());
        assert!(validate_host(&HostMsg::Queue {
            next: vec![1; 5],
            by: vec![]
        })
        .is_ok());
        let w = HostMsg::Welcome {
            v: PROTOCOL_VERSION + 1,
            host: "h".into(),
            guests: vec![],
            state: PlayState::default(),
            allow_suggest: false,
            you: String::new(),
            vote_skip: false,
        };
        assert!(validate_host(&w).is_err());
    }

    #[test]
    fn names_are_cleaned() {
        assert_eq!(clean_name("  Ann\u{7}  "), "Ann");
        assert_eq!(clean_name(""), "Guest");
        assert_eq!(clean_name(&"é".repeat(99)).chars().count(), MAX_NAME_CHARS);
    }

    #[test]
    fn clock_offset_uses_the_fastest_round_trip() {
        // Host clock is 1000 ms ahead.
        let samples = [
            PingSample {
                sent_ms: 0,
                received_ms: 400,
                host_ms: 1300,
            }, // slow, asymmetric
            PingSample {
                sent_ms: 1000,
                received_ms: 1040,
                host_ms: 2020,
            },
            PingSample {
                sent_ms: 2000,
                received_ms: 2200,
                host_ms: 3100,
            },
        ];
        assert_eq!(best_offset(&samples), Some(1000));
        assert_eq!(best_offset(&[]), None);
        assert_eq!(samples[1].rtt(), 40);
    }

    #[test]
    fn host_position_advances_only_while_playing() {
        assert_eq!(host_position_now(10_000, true, 5_000, 7_500), 12_500);
        assert_eq!(host_position_now(10_000, false, 5_000, 7_500), 10_000);
        assert_eq!(host_position_now(10_000, true, 9_000, 8_000), 10_000);
    }

    #[test]
    fn drift_over_750ms_seeks() {
        assert_eq!(correction(10_000, 10_700), Correction::None);
        assert_eq!(correction(10_000, 9_300), Correction::None);
        assert_eq!(correction(10_000, 10_800), Correction::SeekTo(10_800));
        assert_eq!(correction(12_000, 10_000), Correction::SeekTo(10_000));
    }

    #[test]
    fn tickets_parse_from_links_or_bare() {
        assert_eq!(
            parse_ticket("slate://party/abc-DEF_123"),
            Some("abc-DEF_123".into())
        );
        assert_eq!(parse_ticket("  abc123 "), Some("abc123".into()));
        for bad in [
            "",
            "slate://party/",
            "has space",
            "a/b",
            "slate://other/x",
            "é",
        ] {
            assert_eq!(parse_ticket(bad), None, "{bad}");
        }
        assert_eq!(ticket_link("xyz"), "slate://party/xyz");
        assert_eq!(parse_ticket(&"a".repeat(3000)), None);
    }

    #[test]
    fn the_roster_caps_at_ten_and_names_stay_unique() {
        let mut r = Roster::default();
        let first = r.join("Ann").expect("room");
        let second = r.join("Ann").expect("room");
        assert_ne!(first, second);
        assert_eq!(r.names(), vec!["Ann", "Ann 2"]);
        for i in 0..(MAX_GUESTS - 2) {
            r.join(&format!("g{i}")).expect("room");
        }
        assert_eq!(r.len(), MAX_GUESTS);
        assert!(r.join("late").is_err());
        assert!(r.leave(first));
        assert!(!r.leave(first));
        assert!(r.join("late").is_ok());
    }

    #[test]
    fn suggestions_are_rate_limited_per_guest() {
        let mut l = RateLimit::default();
        for _ in 0..SUGGEST_PER_MINUTE {
            assert!(l.allow(1, 1000));
        }
        assert!(!l.allow(1, 2000));
        assert!(l.allow(2, 2000));
        assert!(l.allow(1, 70_000));
        l.forget(1);
        assert!(l.allow(1, 70_001));
    }

    #[test]
    fn a_flooding_peer_is_cut_off_and_a_normal_one_is_not() {
        let mut f = Flood::default();
        for i in 0..FLOOD_MESSAGES {
            assert!(f.allow(u64::from(i)));
        }
        assert!(!f.allow(100), "one over the limit in the window");
        assert!(f.allow(FLOOD_WINDOW_MS + 200), "a new window starts over");
    }

    #[test]
    fn a_slate_1_2_hosts_refusal_says_which_side_is_older() {
        let t = rejection_text("protocol version 2 is not supported (need 1)");
        assert!(t.contains("older version"), "{t}");
        assert_eq!(
            rejection_text("The party is full (10 guests)."),
            "The party is full (10 guests)."
        );
        assert_eq!(rejection_text(&"x".repeat(500)).len(), 200);
    }

    #[test]
    fn prefetch_is_capped_at_five() {
        assert_eq!(prefetch_list(&[1, 2, 3]), vec![1, 2, 3]);
        assert_eq!(prefetch_list(&[1, 2, 3, 4, 5, 6, 7]).len(), PREFETCH_TRACKS);
    }

    #[test]
    fn a_1_2_guest_is_told_the_host_is_newer_and_a_newer_host_is_told_the_reverse() {
        // Slate 1.2 spoke protocol 1.
        let old_guest = GuestMsg::Hello {
            v: 1,
            name: "x".into(),
            secret: String::new(),
        };
        let e = validate_guest(&old_guest).unwrap_err();
        assert!(e.contains("host is on a newer version"), "{e}");
        let new_guest = GuestMsg::Hello {
            v: PROTOCOL_VERSION + 1,
            name: "x".into(),
            secret: String::new(),
        };
        assert!(validate_guest(&new_guest)
            .unwrap_err()
            .contains("older version"));
        assert!(host_version_gap(PROTOCOL_VERSION + 1).contains("newer version"));
        assert!(host_version_gap(1).contains("older version"));
    }

    #[test]
    fn a_1_2_welcome_still_parses_and_unknown_messages_are_ignored_not_fatal() {
        // A 1.2 host's Welcome has none of the 1.3 fields.
        let old = br#"{"t":"welcome","v":1,"host":"h","guests":[],"state":{"track":null,"position_ms":0,"playing":false,"at_ms":0},"allow_suggest":true}"#;
        let m: HostMsg = decode(old).expect("parse");
        assert!(matches!(m, HostMsg::Welcome { v: 1, .. }));
        assert!(validate_host(&m).unwrap_err().contains("older version"));
        // A future message type decodes to Unknown instead of failing the connection.
        assert_eq!(
            decode::<HostMsg>(br#"{"t":"hologram","x":1}"#),
            Ok(HostMsg::Unknown)
        );
        assert_eq!(
            decode::<GuestMsg>(br#"{"t":"hologram"}"#),
            Ok(GuestMsg::Unknown)
        );
        // And a 1.2 Queue (no `by`) parses.
        assert!(matches!(
            decode::<HostMsg>(br#"{"t":"queue","next":[1,2]}"#),
            Ok(HostMsg::Queue { .. })
        ));
    }

    #[test]
    fn request_lists_fit_in_one_message() {
        let view = ReqView {
            id: u64::MAX,
            guest: "g".repeat(MAX_NAME_CHARS),
            title: clip(&"t".repeat(99), 48),
            artist: clip(&"a".repeat(99), 32),
            quality: "HI_RES_LOSSLESS".into(),
            unavailable: 10,
        };
        let m = HostMsg::Requests {
            pending: vec![view; MAX_REQUEST_VIEWS],
            total: 30,
        };
        assert!(encode(&m).is_ok());
        assert!(validate_host(&HostMsg::Requests {
            pending: vec![],
            total: 0
        })
        .is_ok());
    }

    #[test]
    fn cohosts_are_tracked_in_the_roster() {
        let mut r = Roster::default();
        let a = r.join("A").expect("room");
        let b = r.join("B").expect("room");
        assert!(r.set_cohost(b, true));
        assert!(!r.set_cohost(99, true));
        assert!(r.is_cohost(b) && !r.is_cohost(a));
        assert_eq!(r.cohosts(), vec!["B"]);
        assert_eq!(
            super::super::requests::successor(&r.successor_input()),
            Some(b)
        );
    }
}
