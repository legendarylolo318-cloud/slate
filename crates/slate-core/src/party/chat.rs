//! Party text chat: short plain-text messages, a per-person rate limit, a mute list for the
//! host and the last 100 messages kept in memory only (never written anywhere). Pure; the
//! transport is in `net`. Guest id 0 is the host.

use std::collections::{HashMap, HashSet, VecDeque};

pub const MAX_CHARS: usize = 200;
/// One person may send this many messages ...
pub const PER_WINDOW: usize = 5;
/// ... in this many milliseconds.
pub const WINDOW_MS: u64 = 10_000;
pub const MAX_KEPT: usize = 100;

/// A message as shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub from: String,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Nothing left after cleaning.
    Empty,
    /// Over 5 per 10 s.
    TooFast,
    /// The host muted this guest.
    Muted,
}

/// Plain text only: control characters (newlines, escapes…) removed, surrounding space
/// trimmed, cut to [`MAX_CHARS`] characters. `None` when nothing is left.
pub fn clean(text: &str) -> Option<String> {
    let s: String = text
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_CHARS * 4)
        .collect();
    let s: String = s.trim().chars().take(MAX_CHARS).collect();
    let s = s.trim_end().to_string();
    (!s.is_empty()).then_some(s)
}

#[derive(Debug, Default)]
pub struct Chat {
    log: VecDeque<Line>,
    times: HashMap<u64, Vec<u64>>,
    muted: HashSet<u64>,
}

impl Chat {
    /// Accepts a message from `guest` (named `from`) at `now_ms`, or says why not.
    pub fn post(
        &mut self,
        guest: u64,
        from: &str,
        text: &str,
        now_ms: u64,
    ) -> Result<Line, Refused> {
        if self.muted.contains(&guest) {
            return Err(Refused::Muted);
        }
        let text = clean(text).ok_or(Refused::Empty)?;
        let t = self.times.entry(guest).or_default();
        t.retain(|x| now_ms.saturating_sub(*x) < WINDOW_MS);
        if t.len() >= PER_WINDOW {
            return Err(Refused::TooFast);
        }
        t.push(now_ms);
        let line = Line {
            from: from.to_string(),
            text,
        };
        self.log.push_back(line.clone());
        while self.log.len() > MAX_KEPT {
            self.log.pop_front();
        }
        Ok(line)
    }

    pub fn set_muted(&mut self, guest: u64, muted: bool) {
        if muted {
            self.muted.insert(guest);
        } else {
            self.muted.remove(&guest);
        }
    }

    pub fn is_muted(&self, guest: u64) -> bool {
        self.muted.contains(&guest)
    }

    /// A guest left: their rate history and mute go with them (ids are never reused).
    pub fn forget(&mut self, guest: u64) {
        self.times.remove(&guest);
        self.muted.remove(&guest);
    }

    pub fn lines(&self) -> impl Iterator<Item = &Line> {
        self.log.iter()
    }

    /// The newest `n` messages, oldest first (sent to someone who joins late).
    pub fn recent(&self, n: usize) -> Vec<Line> {
        let skip = self.log.len().saturating_sub(n);
        self.log.iter().skip(skip).cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.log.len()
    }

    pub fn is_empty(&self) -> bool {
        self.log.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_plain_short_and_trimmed() {
        assert_eq!(clean("  hi\nthere\t\u{7}  ").as_deref(), Some("hithere"));
        assert_eq!(clean("\n\r\u{1b}[31m x").as_deref(), Some("[31m x"));
        assert_eq!(clean("   \n "), None);
        let long = "é".repeat(500);
        assert_eq!(clean(&long).map(|s| s.chars().count()), Some(MAX_CHARS));
        // Cut at a character, never in the middle of one.
        assert!(clean(&"😀".repeat(300)).is_some_and(|s| s.chars().all(|c| c == '😀')));
    }

    #[test]
    fn five_messages_per_ten_seconds_per_person() {
        let mut c = Chat::default();
        for i in 0..PER_WINDOW {
            assert!(c.post(1, "Ann", "hi", i as u64).is_ok());
        }
        assert_eq!(c.post(1, "Ann", "hi", 100), Err(Refused::TooFast));
        // Someone else is not affected, and the window moves on.
        assert!(c.post(2, "Bob", "hi", 100).is_ok());
        assert!(c.post(1, "Ann", "hi", WINDOW_MS + 10).is_ok());
    }

    #[test]
    fn refused_messages_are_not_kept_or_counted() {
        let mut c = Chat::default();
        assert_eq!(c.post(1, "Ann", " \n", 0), Err(Refused::Empty));
        assert!(c.is_empty());
        for _ in 0..PER_WINDOW {
            let _ = c.post(1, "Ann", "x", 0);
        }
        let n = c.len();
        assert_eq!(c.post(1, "Ann", "y", 1), Err(Refused::TooFast));
        assert_eq!(c.len(), n);
    }

    #[test]
    fn only_the_last_hundred_are_kept() {
        let mut c = Chat::default();
        for i in 0..250u64 {
            // Spread over time and people so the rate limit stays out of the way.
            assert!(c.post(i, "G", &format!("m{i}"), i * 20_000).is_ok());
        }
        assert_eq!(c.len(), MAX_KEPT);
        assert_eq!(c.lines().next().map(|l| l.text.as_str()), Some("m150"));
        assert_eq!(c.lines().last().map(|l| l.text.as_str()), Some("m249"));
    }

    #[test]
    fn the_newest_messages_come_back_oldest_first() {
        let mut c = Chat::default();
        for i in 0..30u64 {
            assert!(c.post(i, "G", &format!("m{i}"), i * 20_000).is_ok());
        }
        let r = c.recent(5);
        assert_eq!(r.len(), 5);
        assert_eq!(r[0].text, "m25");
        assert_eq!(r[4].text, "m29");
        assert_eq!(c.recent(500).len(), 30);
        assert!(Chat::default().recent(5).is_empty());
    }

    #[test]
    fn a_muted_guest_is_refused_until_unmuted_or_gone() {
        let mut c = Chat::default();
        c.set_muted(3, true);
        assert!(c.is_muted(3));
        assert_eq!(c.post(3, "Cy", "hello", 0), Err(Refused::Muted));
        assert!(c.post(4, "Di", "hello", 0).is_ok());
        c.set_muted(3, false);
        assert!(c.post(3, "Cy", "hello", 1).is_ok());
        c.set_muted(3, true);
        c.forget(3);
        assert!(!c.is_muted(3));
    }
}
