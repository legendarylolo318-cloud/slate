//! Emoji reactions on the current track: a fixed small set, a per-guest rate limit and a
//! count per emoji that starts again with every track. Pure; the transport is in `net`.

use std::collections::HashMap;

/// What can be sent. A message carries the index, never the text, so a peer cannot put
/// anything else on the screen. Appending is compatible; reordering is not.
pub const EMOJI: [&str; 6] = ["👍", "❤️", "🔥", "😂", "🎉", "👏"];

/// One guest may react this often ...
pub const REACTIONS_PER_WINDOW: usize = 5;
/// ... in this many milliseconds.
pub const WINDOW_MS: u64 = 10_000;

/// The emoji for an index a peer sent, if it is one we know. A newer Slate with more
/// emoji has its extra ones ignored here, not treated as an error.
pub fn emoji(index: u8) -> Option<&'static str> {
    EMOJI.get(usize::from(index)).copied()
}

/// Rate limit per guest plus the counts for the playing track.
#[derive(Debug, Default)]
pub struct Reactions {
    counts: [u32; EMOJI.len()],
    times: HashMap<u64, Vec<u64>>,
}

impl Reactions {
    /// Records a reaction from `guest` (the host uses its own id) at `now_ms`. False when
    /// the index is unknown or the guest is over the limit; nothing is counted then.
    pub fn add(&mut self, guest: u64, index: u8, now_ms: u64) -> bool {
        let Some(slot) = self.counts.get_mut(usize::from(index)) else {
            return false;
        };
        let t = self.times.entry(guest).or_default();
        t.retain(|x| now_ms.saturating_sub(*x) < WINDOW_MS);
        if t.len() >= REACTIONS_PER_WINDOW {
            return false;
        }
        t.push(now_ms);
        *slot = slot.saturating_add(1);
        true
    }

    /// A new track: the counts start from zero (the rate limit carries on).
    pub fn new_track(&mut self) {
        self.counts = [0; EMOJI.len()];
    }

    pub fn forget(&mut self, guest: u64) {
        self.times.remove(&guest);
    }

    /// Emoji with a non-zero count, in the order of [`EMOJI`].
    pub fn tally(&self) -> Vec<(u8, u32)> {
        self.counts
            .iter()
            .enumerate()
            .filter(|(_, n)| **n > 0)
            .map(|(i, n)| (i as u8, *n))
            .collect()
    }
}

/// "🔥 3  👍 1" for the host's tray.
pub fn tally_text(tally: &[(u8, u32)]) -> String {
    tally
        .iter()
        .filter_map(|(i, n)| Some(format!("{} {n}", emoji(*i)?)))
        .collect::<Vec<_>>()
        .join("  ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_indexes_only() {
        assert_eq!(emoji(0), Some("👍"));
        assert_eq!(emoji(EMOJI.len() as u8), None);
        assert_eq!(emoji(255), None);
    }

    #[test]
    fn counts_per_emoji_and_reset_per_track() {
        let mut r = Reactions::default();
        assert!(r.add(1, 2, 0));
        assert!(r.add(2, 2, 10));
        assert!(r.add(1, 0, 20));
        assert_eq!(r.tally(), vec![(0, 1), (2, 2)]);
        r.new_track();
        assert!(r.tally().is_empty());
    }

    #[test]
    fn a_guest_is_limited_per_window_and_others_are_not() {
        let mut r = Reactions::default();
        for i in 0..REACTIONS_PER_WINDOW {
            assert!(r.add(1, 0, i as u64), "{i}");
        }
        assert!(!r.add(1, 0, 100), "over the limit");
        assert!(r.add(2, 0, 100), "another guest");
        assert!(r.add(1, 0, WINDOW_MS + 100), "the window moved on");
        assert_eq!(r.tally(), vec![(0, REACTIONS_PER_WINDOW as u32 + 2)]);
    }

    #[test]
    fn an_unknown_index_counts_nothing_and_uses_no_allowance() {
        let mut r = Reactions::default();
        for _ in 0..20 {
            assert!(!r.add(1, 200, 0));
        }
        assert!(r.add(1, 0, 0));
        assert_eq!(r.tally(), vec![(0, 1)]);
    }

    #[test]
    fn the_limit_carries_over_a_new_track() {
        let mut r = Reactions::default();
        for _ in 0..REACTIONS_PER_WINDOW {
            r.add(1, 0, 0);
        }
        r.new_track();
        assert!(!r.add(1, 0, 1));
    }

    #[test]
    fn tally_text_reads_naturally() {
        assert_eq!(tally_text(&[(2, 3), (0, 1)]), "🔥 3  👍 1");
        assert_eq!(tally_text(&[]), "");
    }
}
