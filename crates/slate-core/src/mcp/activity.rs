//! The agent activity log: every mutating call (who, what, result) with enough to undo the
//! reversible ones. Bounded to 500 entries; lives only while the server runs.

use std::collections::VecDeque;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const MAX_ENTRIES: usize = 500;

/// How to put a change back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Undo {
    /// Remove tracks the agent queued.
    RemoveFromQueue { track_ids: Vec<u64> },
    /// Put back tracks the agent removed from the queue (with the position they had).
    RestoreQueue { track_ids: Vec<u64>, at: usize },
    /// Move a queue item back.
    MoveQueueItem { track_id: u64, to: usize },
    /// Remove tracks the agent added to a playlist.
    RemoveFromPlaylist {
        playlist: String,
        track_ids: Vec<u64>,
        /// Length of the playlist before the add: TIDAL appends and skips tracks already in
        /// it, so only entries from here on were added. Absent in older logs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_index: Option<u32>,
    },
    /// Add tracks the agent removed back to a playlist (at the end, as TIDAL has no position add).
    AddToPlaylist {
        playlist: String,
        track_ids: Vec<u64>,
    },
    /// Restore a playlist's original order.
    ReorderPlaylist { playlist: String, order: Vec<u64> },
    /// Delete a playlist the agent created (asks the user first).
    DeleteCreatedPlaylist { playlist: String },
    /// Undo a favourite change.
    SetFavorite { kind: String, id: u64, on: bool },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: u64,
    /// Unix seconds.
    pub time: u64,
    pub client: String,
    pub tool: String,
    pub summary: String,
    pub result: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo: Option<Undo>,
    #[serde(default)]
    pub undone: bool,
    /// True for changes that cannot be undone (playlist deletion).
    #[serde(default)]
    pub not_undoable: bool,
}

#[derive(Debug, Default)]
pub struct Log {
    entries: VecDeque<Entry>,
    next_id: u64,
}

impl Log {
    pub fn load(path: &Path) -> Self {
        let entries: VecDeque<Entry> = std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let next_id = entries.back().map_or(1, |e| e.id + 1);
        let mut log = Self { entries, next_id };
        log.trim();
        log
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string(&self.entries)?)?;
        std::fs::rename(tmp, path)
    }

    fn trim(&mut self) {
        while self.entries.len() > MAX_ENTRIES {
            self.entries.pop_front();
        }
    }

    /// Records a call and returns its id.
    pub fn push(
        &mut self,
        time: u64,
        client: &str,
        tool: &str,
        summary: &str,
        result: &str,
        undo: Option<Undo>,
        not_undoable: bool,
    ) -> u64 {
        let id = self.next_id.max(1);
        self.next_id = id + 1;
        let clip = |s: &str, n: usize| -> String { s.chars().take(n).collect() };
        self.entries.push_back(Entry {
            id,
            time,
            client: clip(client, 60),
            tool: clip(tool, 60),
            summary: clip(summary, 300),
            result: clip(result, 300),
            undo,
            undone: false,
            not_undoable,
        });
        self.trim();
        id
    }

    /// Newest first.
    pub fn recent(&self, limit: usize, offset: usize) -> Vec<Entry> {
        self.entries
            .iter()
            .rev()
            .skip(offset)
            .take(limit)
            .cloned()
            .collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The undo for an entry that can still be undone.
    pub fn undo_for(&self, id: u64) -> Result<Undo, String> {
        let e = self
            .entries
            .iter()
            .find(|e| e.id == id)
            .ok_or("That entry is no longer in the log.")?;
        if e.undone {
            return Err("Already undone.".into());
        }
        if e.not_undoable {
            return Err("Deleting a playlist cannot be undone.".into());
        }
        e.undo
            .clone()
            .ok_or_else(|| "This change cannot be undone.".into())
    }

    pub fn mark_undone(&mut self, id: u64) {
        if let Some(e) = self.entries.iter_mut().find(|e| e.id == id) {
            e.undone = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push(log: &mut Log, n: u64, undo: Option<Undo>) -> u64 {
        log.push(
            n,
            "Hermes",
            "add_to_queue",
            "Queued 1 track",
            "ok",
            undo,
            false,
        )
    }

    #[test]
    fn the_log_keeps_only_the_last_500() {
        let mut log = Log::default();
        for n in 0..(MAX_ENTRIES as u64 + 40) {
            push(&mut log, n, None);
        }
        assert_eq!(log.len(), MAX_ENTRIES);
        let newest = log.recent(1, 0);
        assert_eq!(newest[0].time, MAX_ENTRIES as u64 + 39);
        assert_eq!(log.recent(1000, 0).last().map(|e| e.time), Some(40));
    }

    #[test]
    fn recent_is_newest_first_and_pageable() {
        let mut log = Log::default();
        for n in 0..5 {
            push(&mut log, n, None);
        }
        let times: Vec<u64> = log.recent(2, 1).iter().map(|e| e.time).collect();
        assert_eq!(times, vec![3, 2]);
    }

    #[test]
    fn undo_rules() {
        let mut log = Log::default();
        let a = push(
            &mut log,
            1,
            Some(Undo::RemoveFromQueue { track_ids: vec![7] }),
        );
        let b = push(&mut log, 2, None);
        let c = log.push(3, "x", "delete_playlist", "Deleted", "ok", None, true);
        assert!(matches!(log.undo_for(a), Ok(Undo::RemoveFromQueue { .. })));
        assert!(log.undo_for(b).is_err());
        assert!(log
            .undo_for(c)
            .expect_err("not undoable")
            .contains("cannot be undone"));
        log.mark_undone(a);
        assert!(log.undo_for(a).expect_err("twice").contains("Already"));
        assert!(log.undo_for(999).is_err());
    }

    #[test]
    fn text_is_clipped_and_ids_increase() {
        let mut log = Log::default();
        let a = log.push(
            1,
            &"c".repeat(500),
            "t",
            &"s".repeat(2000),
            "r",
            None,
            false,
        );
        let b = push(&mut log, 2, None);
        assert!(b > a);
        let e = &log.recent(2, 0)[1];
        assert_eq!(e.client.chars().count(), 60);
        assert_eq!(e.summary.chars().count(), 300);
    }

    #[test]
    fn disk_round_trip_and_garbage() {
        let dir = std::env::temp_dir().join(format!("slate-act-{}", std::process::id()));
        let path = dir.join("agent_activity.json");
        let mut log = Log::default();
        push(
            &mut log,
            1,
            Some(Undo::SetFavorite {
                kind: "track".into(),
                id: 3,
                on: false,
            }),
        );
        log.save(&path).expect("save");
        let back = Log::load(&path);
        assert_eq!(back.recent(10, 0), log.recent(10, 0));
        let mut back = back;
        let id = push(&mut back, 2, None);
        assert_eq!(id, 2);
        std::fs::write(&path, "nope").expect("write");
        assert!(Log::load(&path).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
