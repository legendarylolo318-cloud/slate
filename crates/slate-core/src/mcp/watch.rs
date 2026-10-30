//! Playback events for subscribed agents: `track_skipped`, `track_replayed`, `queue_low`.
//! A small state machine fed by the player's own track-change, position and queue updates; it
//! is created when a client subscribes and dropped when the last one leaves, so it costs
//! nothing otherwise.

use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    TrackSkipped {
        track_id: u64,
        position_secs: u32,
    },
    TrackReplayed {
        track_id: u64,
    },
    /// Two tracks remain in the queue (fired once per drop).
    QueueLow {
        remaining: usize,
    },
}

impl Event {
    pub fn name(&self) -> &'static str {
        match self {
            Self::TrackSkipped { .. } => "track_skipped",
            Self::TrackReplayed { .. } => "track_replayed",
            Self::QueueLow { .. } => "queue_low",
        }
    }
}

/// A track skipped before 30 s, or before half of a track shorter than a minute.
pub fn is_skip(position_secs: u32, duration_secs: u32) -> bool {
    if duration_secs < 60 {
        u64::from(position_secs) * 2 < u64::from(duration_secs)
    } else {
        position_secs < 30
    }
}

#[derive(Debug, Default)]
pub struct Watch {
    current: Option<(u64, u32)>, // id, duration
    position: u32,
    played: HashSet<u64>,
    /// Furthest point reached in the current track.
    furthest: u32,
    low_fired: bool,
}

impl Watch {
    pub fn new() -> Self {
        Self::default()
    }

    /// The position of the playing track (seconds), as reported by the player.
    pub fn position(&mut self, secs: u32) {
        self.position = secs;
        self.furthest = self.furthest.max(secs);
    }

    /// A seek: returns `TrackReplayed` for a jump back to the start after more than half.
    pub fn seeked(&mut self, to_secs: u32) -> Option<Event> {
        let (id, dur) = self.current?;
        let replay = to_secs <= 2 && dur > 0 && self.furthest * 2 > dur;
        self.position = to_secs;
        if replay {
            self.furthest = to_secs;
            return Some(Event::TrackReplayed { track_id: id });
        }
        None
    }

    /// A new track started. `skipped`: the user moved on rather than the track ending.
    pub fn track_changed(&mut self, new_id: u64, new_duration: u32, skipped: bool) -> Vec<Event> {
        let mut out = Vec::new();
        if let Some((old, old_dur)) = self.current {
            if skipped && is_skip(self.position, old_dur) {
                out.push(Event::TrackSkipped {
                    track_id: old,
                    position_secs: self.position,
                });
            }
        }
        if !self.played.insert(new_id) {
            out.push(Event::TrackReplayed { track_id: new_id });
        }
        self.current = Some((new_id, new_duration));
        self.position = 0;
        self.furthest = 0;
        out
    }

    /// The queue length changed. Fires `QueueLow` once when it falls to two, not in repeat-all.
    pub fn queue_len(&mut self, len: usize, repeat_all: bool) -> Option<Event> {
        if len > 2 {
            self.low_fired = false;
            return None;
        }
        if len == 2 && !self.low_fired && !repeat_all {
            self.low_fired = true;
            return Some(Event::QueueLow { remaining: len });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_thresholds() {
        assert!(is_skip(29, 200));
        assert!(!is_skip(30, 200));
        assert!(is_skip(10, 50));
        assert!(!is_skip(25, 50));
        assert!(is_skip(0, 0) || !is_skip(0, 0)); // degenerate durations do not panic
    }

    #[test]
    fn skipping_early_is_reported_with_the_position() {
        let mut w = Watch::new();
        assert!(w.track_changed(1, 200, false).is_empty());
        w.position(12);
        let ev = w.track_changed(2, 180, true);
        assert_eq!(
            ev,
            vec![Event::TrackSkipped {
                track_id: 1,
                position_secs: 12
            }]
        );
        // Natural end after a full listen: nothing.
        w.position(179);
        assert!(w.track_changed(3, 180, false).is_empty());
        // A late skip is not a skip.
        w.position(100);
        assert!(w.track_changed(4, 180, true).is_empty());
    }

    #[test]
    fn replays_by_new_play_and_by_seek() {
        let mut w = Watch::new();
        w.track_changed(1, 200, false);
        w.position(150);
        w.track_changed(2, 100, false);
        let ev = w.track_changed(1, 200, false);
        assert_eq!(ev, vec![Event::TrackReplayed { track_id: 1 }]);
        // Seek back to the start after more than half.
        w.position(120);
        assert_eq!(w.seeked(0), Some(Event::TrackReplayed { track_id: 1 }));
        // Not after less than half.
        w.track_changed(9, 200, false);
        w.position(50);
        assert_eq!(w.seeked(0), None);
        // Seeking elsewhere is not a replay.
        w.position(150);
        assert_eq!(w.seeked(60), None);
    }

    #[test]
    fn queue_low_fires_once_per_drop_and_not_on_repeat_all() {
        let mut w = Watch::new();
        assert_eq!(w.queue_len(5, false), None);
        assert_eq!(w.queue_len(3, false), None);
        assert_eq!(
            w.queue_len(2, false),
            Some(Event::QueueLow { remaining: 2 })
        );
        assert_eq!(w.queue_len(2, false), None);
        assert_eq!(w.queue_len(1, false), None);
        assert_eq!(w.queue_len(0, false), None);
        // Refilled, then low again.
        assert_eq!(w.queue_len(8, false), None);
        assert_eq!(
            w.queue_len(2, false),
            Some(Event::QueueLow { remaining: 2 })
        );
        let mut r = Watch::new();
        assert_eq!(r.queue_len(2, true), None);
        assert_eq!(Event::QueueLow { remaining: 2 }.name(), "queue_low");
    }
}
