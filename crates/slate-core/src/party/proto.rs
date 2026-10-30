//! Listening-party wire protocol and the rules around it. Pure: no sockets here.
//!
//! Everyone plays from their own TIDAL account; only playback state travels. The host is
//! authoritative and sends a message only when something changes. Every message from a peer is
//! untrusted input: it is size-limited, versioned and validated before use.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;
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
    },
    Guests {
        guests: Vec<String>,
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
    Leave,
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

/// Validates a guest message beyond its shape.
pub fn validate_guest(m: &GuestMsg) -> Result<(), String> {
    match m {
        GuestMsg::Hello { v, .. } if *v != PROTOCOL_VERSION => Err(format!(
            "protocol version {v} is not supported (need {PROTOCOL_VERSION})"
        )),
        GuestMsg::Suggest { track_id } if *track_id == 0 => Err("bad track id".into()),
        _ => Ok(()),
    }
}

/// Validates a host message beyond its shape.
pub fn validate_host(m: &HostMsg) -> Result<(), String> {
    match m {
        HostMsg::Welcome { v, .. } if *v != PROTOCOL_VERSION => Err(format!(
            "the host speaks protocol version {v}; this Slate speaks {PROTOCOL_VERSION}"
        )),
        HostMsg::Queue { next } if next.len() > PREFETCH_TRACKS * 2 => Err("queue too long".into()),
        HostMsg::Welcome { guests, .. } | HostMsg::Guests { guests }
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
        });
        Ok(self.next_id)
    }

    pub fn leave(&mut self, id: u64) -> bool {
        let n = self.guests.len();
        self.guests.retain(|g| g.id != id);
        self.guests.len() != n
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
        assert!(decode::<GuestMsg>(b"{\"t\":\"nope\"}").is_err());
        assert!(decode::<GuestMsg>(b"garbage").is_err());
        assert!(decode::<HostMsg>(b"").is_err());
        let big = HostMsg::Queue {
            next: vec![1; 3000],
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
            v: 1,
            name: "x".into(),
            secret: String::new()
        })
        .is_ok());
        assert!(validate_guest(&GuestMsg::Suggest { track_id: 0 }).is_err());
        assert!(validate_host(&HostMsg::Queue { next: vec![1; 11] }).is_err());
        assert!(validate_host(&HostMsg::Queue { next: vec![1; 5] }).is_ok());
        let w = HostMsg::Welcome {
            v: 2,
            host: "h".into(),
            guests: vec![],
            state: PlayState::default(),
            allow_suggest: false,
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
    fn prefetch_is_capped_at_five() {
        assert_eq!(prefetch_list(&[1, 2, 3]), vec![1, 2, 3]);
        assert_eq!(prefetch_list(&[1, 2, 3, 4, 5, 6, 7]).len(), PREFETCH_TRACKS);
    }
}
