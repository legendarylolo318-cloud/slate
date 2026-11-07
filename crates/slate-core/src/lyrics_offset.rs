//! Per-track lyrics timing offsets ("lyrics are 300 ms late"). One small JSON file in the
//! config directory, read when lyrics are shown and written when the offset is nudged;
//! nothing is kept in memory or touched on disk otherwise.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One nudge.
pub const STEP_MS: i32 = 100;
/// The offset never goes beyond this either way.
pub const MAX_MS: i32 = 10_000;
/// Oldest entries are forgotten past this many tracks.
pub const MAX_ENTRIES: usize = 500;

/// Track id → offset in milliseconds. Positive: lyrics come later; negative: earlier.
/// Kept as a list, most recently changed last, so the oldest can be dropped.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offsets {
    entries: Vec<(u64, i32)>,
}

impl Offsets {
    /// Unreadable or malformed input is an empty set, never an error.
    pub fn parse(text: &str) -> Self {
        let mut o: Self = serde_json::from_str(text).unwrap_or_default();
        o.entries.retain(|(_, ms)| *ms != 0 && ms.abs() <= MAX_MS);
        // A hand-edited file can name a track twice; the later (newer) entry wins.
        let mut seen = std::collections::HashSet::new();
        let mut keep: Vec<(u64, i32)> = Vec::with_capacity(o.entries.len());
        for e in o.entries.into_iter().rev() {
            if seen.insert(e.0) {
                keep.push(e);
            }
        }
        keep.reverse();
        o.entries = keep;
        let excess = o.entries.len().saturating_sub(MAX_ENTRIES);
        o.entries.drain(..excess);
        o
    }

    pub fn get(&self, track_id: u64) -> i32 {
        self.entries
            .iter()
            .rfind(|(id, _)| *id == track_id)
            .map_or(0, |(_, ms)| *ms)
    }

    /// Moves the offset of `track_id` by `delta_ms` (clamped to ±[`MAX_MS`]); returns
    /// the new value. Zero removes the entry.
    pub fn nudge(&mut self, track_id: u64, delta_ms: i32) -> i32 {
        let ms = (self.get(track_id) + delta_ms).clamp(-MAX_MS, MAX_MS);
        self.entries.retain(|(id, _)| *id != track_id);
        if ms != 0 {
            self.entries.push((track_id, ms));
            let excess = self.entries.len().saturating_sub(MAX_ENTRIES);
            self.entries.drain(..excess);
        }
        ms
    }

    /// Every offset, oldest change first.
    pub fn iter(&self) -> impl Iterator<Item = (u64, i32)> + '_ {
        self.entries.iter().copied()
    }

    /// Sets the offset of `track_id` (clamped to ±[`MAX_MS`]); zero removes the entry.
    pub fn set(&mut self, track_id: u64, ms: i32) {
        let ms = ms.clamp(-MAX_MS, MAX_MS);
        self.entries.retain(|(id, _)| *id != track_id);
        if ms != 0 {
            self.entries.push((track_id, ms));
            let excess = self.entries.len().saturating_sub(MAX_ENTRIES);
            self.entries.drain(..excess);
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }
}

/// The file inside the config directory `dir`.
pub fn path_in(dir: &Path) -> PathBuf {
    dir.join("lyrics-offsets.json")
}

pub fn load(path: &Path) -> Offsets {
    std::fs::read_to_string(path)
        .map(|t| Offsets::parse(&t))
        .unwrap_or_default()
}

pub fn save(path: &Path, offsets: &Offsets) -> io::Result<()> {
    crate::write_atomic(path, offsets.to_json().as_bytes())
}

/// Nudges the saved offset of `track_id` and hands the new value to `then`, all under
/// one process-wide lock: two quick nudges each read, change and write the file, and
/// unserialised the second could read before the first wrote (a lost nudge) or report
/// its value after the first's newer one. Blocking file I/O: call off the UI thread.
pub fn nudge_saved(path: &Path, track_id: u64, delta_ms: i32, then: impl FnOnce(i32)) {
    let ms = update_saved(path, |all| all.nudge(track_id, delta_ms));
    then(ms);
}

/// Reads the saved offsets, lets `f` change them and writes them back, all under the
/// one process-wide lock `nudge_saved` uses. Blocking file I/O: off the UI thread.
pub fn update_saved<R>(path: &Path, f: impl FnOnce(&mut Offsets) -> R) -> R {
    static FILE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _held = FILE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut all = load(path);
    let r = f(&mut all);
    if let Err(e) = save(path, &all) {
        log::warn!("could not save the lyrics offsets: {e}");
    }
    r
}

/// The position to look the current line up at: lyrics shifted later by `offset_ms`
/// are found by looking earlier in the track.
pub fn lookup_position(pos_secs: f32, offset_ms: i32) -> f32 {
    (pos_secs - offset_ms as f32 / 1000.0).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nudging_adds_up_and_zero_forgets() {
        let mut o = Offsets::default();
        assert_eq!(o.nudge(7, STEP_MS), 100);
        assert_eq!(o.nudge(7, STEP_MS), 200);
        assert_eq!(o.get(7), 200);
        assert_eq!(o.nudge(7, -200), 0);
        assert_eq!(o.get(7), 0);
        assert_eq!(o.to_json(), r#"{"entries":[]}"#);
    }

    #[test]
    fn the_offset_is_bounded() {
        let mut o = Offsets::default();
        for _ in 0..200 {
            o.nudge(1, STEP_MS);
        }
        assert_eq!(o.get(1), MAX_MS);
        for _ in 0..400 {
            o.nudge(1, -STEP_MS);
        }
        assert_eq!(o.get(1), -MAX_MS);
    }

    #[test]
    fn the_oldest_entries_are_dropped() {
        let mut o = Offsets::default();
        for id in 0..(MAX_ENTRIES as u64 + 10) {
            o.nudge(id, 100);
        }
        assert_eq!(o.get(0), 0);
        assert_eq!(o.get(MAX_ENTRIES as u64 + 9), 100);
        assert_eq!(o.entries.len(), MAX_ENTRIES);
    }

    #[test]
    fn garbage_is_an_empty_set_and_out_of_range_entries_are_dropped() {
        assert_eq!(Offsets::parse("not json"), Offsets::default());
        let o = Offsets::parse(r#"{"entries":[[1,100],[2,0],[3,99999]]}"#);
        assert_eq!(o.get(1), 100);
        assert_eq!(o.get(2), 0);
        assert_eq!(o.get(3), 0);
    }

    #[test]
    fn saved_offsets_come_back() {
        let d = tempfile::tempdir().expect("tempdir");
        let p = path_in(d.path());
        assert_eq!(load(&p), Offsets::default(), "no file yet");
        let mut o = Offsets::default();
        o.nudge(42, -300);
        save(&p, &o).expect("save");
        assert_eq!(load(&p).get(42), -300);
    }

    #[test]
    fn a_track_named_twice_keeps_its_newer_entry_once() {
        let o = Offsets::parse(r#"{"entries":[[1,100],[2,300],[1,-200]]}"#);
        assert_eq!(o.get(1), -200);
        assert_eq!(o.entries, vec![(2, 300), (1, -200)]);
    }

    #[test]
    fn concurrent_nudges_are_never_lost() {
        let d = tempfile::tempdir().expect("tempdir");
        let p = std::sync::Arc::new(path_in(d.path()));
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let threads: Vec<_> = (0..16)
            .map(|_| {
                let (p, seen) = (p.clone(), seen.clone());
                std::thread::spawn(move || {
                    nudge_saved(&p, 9, STEP_MS, |ms| seen.lock().unwrap().push(ms))
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(load(&p).get(9), 16 * STEP_MS);
        let seen = seen.lock().unwrap();
        assert!(
            seen.windows(2).all(|w| w[0] < w[1]),
            "reported in order: {seen:?}"
        );
    }

    #[test]
    fn a_later_offset_looks_earlier_in_the_track() {
        assert_eq!(lookup_position(10.0, 500), 9.5);
        assert_eq!(lookup_position(10.0, -500), 10.5);
        assert_eq!(lookup_position(0.2, 500), 0.0);
    }

    #[test]
    fn set_replaces_clamps_and_zero_removes() {
        let mut o = Offsets::default();
        o.set(1, 300);
        o.set(2, 99_999);
        o.set(1, -50);
        assert_eq!(o.get(1), -50);
        assert_eq!(o.get(2), MAX_MS);
        assert_eq!(o.iter().collect::<Vec<_>>(), vec![(2, MAX_MS), (1, -50)]);
        o.set(2, 0);
        assert_eq!(o.get(2), 0);
    }
}
