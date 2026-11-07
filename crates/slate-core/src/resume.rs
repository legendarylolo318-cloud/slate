//! Resume points for long tracks (DJ mixes, live sets, audiobooks): where you left off.
//!
//! Pure logic and the file format; the app decides when to read and write. The store is
//! at most [`MAX_ENTRIES`] tracks, the least recently left one dropped first.

use serde::{Deserialize, Serialize};

/// Only tracks at least this long (seconds) get a resume point.
pub const MIN_TRACK_SECS: f32 = 20.0 * 60.0;
/// Leaving earlier than this into a track remembers nothing.
pub const MIN_POSITION_SECS: f32 = 60.0;
/// Leaving within this many seconds of the end counts as having finished it.
pub const END_MARGIN_SECS: f32 = 60.0;
/// Most tracks remembered.
pub const MAX_ENTRIES: usize = 500;
/// Name of the encrypted state file.
pub const FILE: &str = "resume.json";

/// Whether the track is long enough to be tracked at all.
pub fn is_long(duration: f32) -> bool {
    duration >= MIN_TRACK_SECS
}

/// Track id → seconds, most recently left last.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumePoints {
    entries: Vec<(u64, u32)>,
}

impl ResumePoints {
    /// Unreadable or malformed input is an empty store, never an error.
    pub fn parse(text: &str) -> Self {
        let mut r: Self = serde_json::from_str(text).unwrap_or_default();
        // A hand-edited file can name a track twice; the later entry wins.
        let mut seen = std::collections::HashSet::new();
        let mut keep: Vec<(u64, u32)> = Vec::with_capacity(r.entries.len());
        for e in r.entries.into_iter().rev() {
            if e.1 > 0 && seen.insert(e.0) {
                keep.push(e);
            }
        }
        keep.reverse();
        let excess = keep.len().saturating_sub(MAX_ENTRIES);
        keep.drain(..excess);
        r.entries = keep;
        r
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The saved position of `id` if it still makes sense for a track of `duration`.
    pub fn get(&self, id: u64, duration: f32) -> Option<u32> {
        let secs = self.entries.iter().rfind(|(i, _)| *i == id)?.1;
        (is_long(duration) && (secs as f32) + END_MARGIN_SECS < duration).then_some(secs)
    }

    /// The track `id` (`duration` long) was left at `position`. Returns whether the
    /// store changed. Past a minute and not within a minute of the end the position is
    /// kept; near the end the track counts as finished and its point is dropped; in the
    /// first minute an older point is left alone.
    pub fn leave(&mut self, id: u64, duration: f32, position: f32) -> bool {
        if !is_long(duration) || !position.is_finite() {
            return false;
        }
        if duration - position <= END_MARGIN_SECS {
            return self.forget(id);
        }
        if position <= MIN_POSITION_SECS {
            return false;
        }
        let secs = position as u32;
        if self.entries.last() == Some(&(id, secs)) {
            return false;
        }
        self.entries.retain(|(i, _)| *i != id);
        self.entries.push((id, secs));
        let excess = self.entries.len().saturating_sub(MAX_ENTRIES);
        self.entries.drain(..excess);
        true
    }

    /// Drops the point of `id`; whether there was one.
    pub fn forget(&mut self, id: u64) -> bool {
        let before = self.entries.len();
        self.entries.retain(|(i, _)| *i != id);
        self.entries.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: f32 = 3600.0;

    #[test]
    fn a_long_track_left_midway_is_remembered() {
        let mut r = ResumePoints::default();
        assert!(r.leave(1, HOUR, 754.4));
        assert_eq!(r.get(1, HOUR), Some(754));
        assert!(!r.leave(1, HOUR, 754.9), "same second: no change");
    }

    #[test]
    fn short_tracks_never_get_a_point() {
        let mut r = ResumePoints::default();
        assert!(!r.leave(1, 19.0 * 60.0, 600.0));
        assert!(r.is_empty());
        r.leave(2, HOUR, 600.0);
        assert_eq!(r.get(2, 19.0 * 60.0), None, "a duration that changed");
    }

    #[test]
    fn the_first_minute_and_the_last_minute_do_not_count() {
        let mut r = ResumePoints::default();
        assert!(!r.leave(1, HOUR, 60.0));
        assert!(!r.leave(1, HOUR, 10.0));
        assert!(!r.leave(1, HOUR, HOUR - 60.0));
        assert!(r.is_empty());
        assert!(r.leave(1, HOUR, 61.0));
        assert!(r.leave(1, HOUR, HOUR - 61.0));
        assert_eq!(r.get(1, HOUR), Some(3539));
    }

    #[test]
    fn finishing_a_track_drops_its_point_and_an_early_skip_keeps_it() {
        let mut r = ResumePoints::default();
        r.leave(1, HOUR, 900.0);
        assert!(
            !r.leave(1, HOUR, 5.0),
            "skipped at once after starting again"
        );
        assert_eq!(r.get(1, HOUR), Some(900));
        assert!(r.leave(1, HOUR, HOUR - 2.0));
        assert_eq!(r.get(1, HOUR), None);
    }

    #[test]
    fn the_least_recently_left_track_goes_first() {
        let mut r = ResumePoints::default();
        for id in 0..(MAX_ENTRIES as u64 + 20) {
            r.leave(id, HOUR, 300.0);
        }
        assert_eq!(r.len(), MAX_ENTRIES);
        assert_eq!(r.get(0, HOUR), None);
        assert_eq!(r.get(MAX_ENTRIES as u64 + 19, HOUR), Some(300));
        // Leaving track 20 again makes it the newest, so it outlives later arrivals.
        r.leave(20, HOUR, 400.0);
        for id in 1000..1019 {
            r.leave(id, HOUR, 300.0);
        }
        assert_eq!(r.get(20, HOUR), Some(400));
        assert_eq!(r.get(21, HOUR), None);
    }

    #[test]
    fn a_point_near_the_end_is_not_offered() {
        let mut r = ResumePoints::parse(r#"{"entries":[[1,3590]]}"#);
        assert_eq!(r.get(1, HOUR), None);
        assert!(r.forget(1));
        assert!(!r.forget(1));
    }

    #[test]
    fn garbage_and_duplicates_are_cleaned_up() {
        assert_eq!(ResumePoints::parse("not json"), ResumePoints::default());
        let r = ResumePoints::parse(r#"{"entries":[[1,100],[2,0],[1,200]]}"#);
        assert_eq!(r.get(1, HOUR), Some(200));
        assert_eq!(r.get(2, HOUR), None);
        assert_eq!(r.len(), 1);
        let again = ResumePoints::parse(&r.to_json());
        assert_eq!(again, r);
    }
}
