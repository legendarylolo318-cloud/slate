//! Named snapshots of the play queue, kept on disk. Each snapshot is two small files: a header
//! (`<id>.meta.json`: name, time, track count, length) and the tracks (`<id>.tracks.json`), so
//! listing the snapshots reads headers only and the tracks are read when one is restored.
//! Nothing is loaded until the list is opened.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::queue::Track;

pub const MAX_SNAPSHOTS: usize = 50;
pub const MAX_TRACKS: usize = 1000;
pub const MAX_NAME_CHARS: usize = 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    pub id: String,
    pub name: String,
    /// Unix seconds.
    pub saved_at: u64,
    pub count: usize,
    pub duration_secs: u32,
}

impl Meta {
    /// "Road trip · 24 tracks · 1 h 32 min" for the list.
    pub fn label(&self) -> String {
        let (h, m) = (self.duration_secs / 3600, self.duration_secs % 3600 / 60);
        let len = if h > 0 {
            format!("{h} h {m} min")
        } else {
            format!("{m} min")
        };
        let n = self.count;
        format!(
            "{} · {n} track{} · {len}",
            self.name,
            if n == 1 { "" } else { "s" }
        )
    }
}

/// A snapshot name: control characters dropped, trimmed, at most 60 characters, never empty.
pub fn clean_name(raw: &str) -> String {
    let n: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect();
    let n = n.trim();
    if n.is_empty() {
        "Queue".into()
    } else {
        n.into()
    }
}

/// `YYYY-MM-DD` (UTC) for a Unix time: the default name of a snapshot or saved playlist.
pub fn ymd(secs: u64) -> String {
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// How a snapshot is put back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Restore {
    /// The upcoming queue becomes the snapshot.
    Replace,
    /// The snapshot is added after what is already queued.
    Append,
}

pub struct Store {
    dir: PathBuf,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_hexdigit())
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn meta_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.meta.json"))
    }

    fn tracks_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.tracks.json"))
    }

    fn write(path: &Path, data: &[u8]) -> io::Result<()> {
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, data)?;
        std::fs::rename(&tmp, path)
    }

    /// Saves `tracks` (at most [`MAX_TRACKS`]) under `name`.
    pub fn save(&self, name: &str, tracks: &[Track], now: u64) -> Result<Meta, String> {
        if tracks.is_empty() {
            return Err("The queue is empty.".into());
        }
        if self.list().len() >= MAX_SNAPSHOTS {
            return Err(format!(
                "You can keep {MAX_SNAPSHOTS} snapshots. Delete one first."
            ));
        }
        let tracks = &tracks[..tracks.len().min(MAX_TRACKS)];
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let meta = Meta {
            id: uuid::Uuid::new_v4().simple().to_string(),
            name: clean_name(name),
            saved_at: now,
            count: tracks.len(),
            duration_secs: tracks.iter().map(|t| t.duration_secs() as u32).sum(),
        };
        let body = serde_json::to_vec(tracks).map_err(|e| e.to_string())?;
        // Tracks first: a header without tracks would list a snapshot that cannot be restored.
        Self::write(&self.tracks_path(&meta.id), &body).map_err(|e| e.to_string())?;
        let head = serde_json::to_vec(&meta).map_err(|e| e.to_string())?;
        Self::write(&self.meta_path(&meta.id), &head).map_err(|e| e.to_string())?;
        Ok(meta)
    }

    /// All snapshots, newest first. Reads headers only; unreadable files are skipped.
    pub fn list(&self) -> Vec<Meta> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut v: Vec<Meta> = rd
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".meta.json"))
            .filter_map(|e| std::fs::read(e.path()).ok())
            .filter_map(|b| serde_json::from_slice::<Meta>(&b).ok())
            .filter(|m| valid_id(&m.id))
            .collect();
        v.sort_by(|a, b| {
            b.saved_at
                .cmp(&a.saved_at)
                .then_with(|| a.name.cmp(&b.name))
        });
        v
    }

    pub fn load(&self, id: &str) -> Result<Vec<Track>, String> {
        if !valid_id(id) {
            return Err("No such snapshot.".into());
        }
        let b = std::fs::read(self.tracks_path(id)).map_err(|_| "No such snapshot.".to_string())?;
        serde_json::from_slice(&b).map_err(|_| "That snapshot is damaged.".to_string())
    }

    pub fn rename(&self, id: &str, name: &str) -> Result<(), String> {
        if !valid_id(id) {
            return Err("No such snapshot.".into());
        }
        let b = std::fs::read(self.meta_path(id)).map_err(|_| "No such snapshot.".to_string())?;
        let mut m: Meta =
            serde_json::from_slice(&b).map_err(|_| "That snapshot is damaged.".to_string())?;
        m.name = clean_name(name);
        let head = serde_json::to_vec(&m).map_err(|e| e.to_string())?;
        Self::write(&self.meta_path(id), &head).map_err(|e| e.to_string())
    }

    pub fn delete(&self, id: &str) -> Result<(), String> {
        if !valid_id(id) {
            return Err("No such snapshot.".into());
        }
        // Header first, so a half-deleted snapshot disappears from the list.
        let head = std::fs::remove_file(self.meta_path(id));
        let _ = std::fs::remove_file(self.tracks_path(id));
        head.map_err(|_| "No such snapshot.".to_string())
    }
}

/// The queue after restoring `snapshot` over `current_upcoming`.
pub fn restored(mode: Restore, current_upcoming: Vec<Track>, snapshot: Vec<Track>) -> Vec<Track> {
    match mode {
        Restore::Replace => snapshot,
        Restore::Append => {
            let mut v = current_upcoming;
            v.extend(snapshot);
            v
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: u64, secs: u32) -> Track {
        let mut t = Track::new(id);
        t.extra.insert("title".into(), format!("T{id}").into());
        t.extra.insert("duration".into(), secs.into());
        t
    }

    fn ids(v: &[Track]) -> Vec<u64> {
        v.iter().map(|t| t.id).collect()
    }

    #[test]
    fn a_saved_queue_comes_back_whole_and_in_order() {
        let d = tempfile::tempdir().expect("dir");
        let s = Store::new(d.path().join("snaps"));
        let q = vec![track(3, 100), track(1, 200), track(2, 300)];
        let m = s.save("  Road trip \u{7}", &q, 1000).expect("save");
        assert_eq!(
            (m.name.as_str(), m.count, m.duration_secs),
            ("Road trip", 3, 600)
        );
        assert_eq!(m.label(), "Road trip · 3 tracks · 10 min");
        assert_eq!(ids(&s.load(&m.id).expect("load")), vec![3, 1, 2]);
        // Track data (the JSON the queue needs) survives.
        assert_eq!(s.load(&m.id).expect("load")[0].title(), "T3");
    }

    #[test]
    fn listing_reads_headers_newest_first_and_skips_junk() {
        let d = tempfile::tempdir().expect("dir");
        let s = Store::new(d.path());
        assert!(s.list().is_empty(), "no directory yet");
        let a = s.save("old", &[track(1, 60)], 10).expect("save");
        let b = s.save("new", &[track(2, 60)], 20).expect("save");
        std::fs::write(d.path().join("zzz.meta.json"), b"not json").expect("junk");
        std::fs::write(d.path().join("notes.txt"), b"x").expect("junk");
        let l = s.list();
        assert_eq!(
            l.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
            vec![b.id, a.id]
        );
    }

    #[test]
    fn rename_and_delete() {
        let d = tempfile::tempdir().expect("dir");
        let s = Store::new(d.path());
        let m = s.save("one", &[track(1, 60)], 1).expect("save");
        s.rename(&m.id, "  two  ").expect("rename");
        assert_eq!(s.list()[0].name, "two");
        s.rename(&m.id, "   ").expect("rename");
        assert_eq!(s.list()[0].name, "Queue", "never empty");
        s.delete(&m.id).expect("delete");
        assert!(s.list().is_empty());
        assert!(s.load(&m.id).is_err());
        assert!(s.delete(&m.id).is_err());
    }

    #[test]
    fn ids_cannot_escape_the_folder() {
        let d = tempfile::tempdir().expect("dir");
        let s = Store::new(d.path());
        for bad in ["", "../x", "a/b", "..", "zz", &"a".repeat(65)] {
            assert!(s.load(bad).is_err(), "{bad}");
            assert!(s.rename(bad, "x").is_err(), "{bad}");
            assert!(s.delete(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn empty_queues_and_too_many_snapshots_are_refused() {
        let d = tempfile::tempdir().expect("dir");
        let s = Store::new(d.path());
        assert!(s.save("x", &[], 1).is_err());
        for i in 0..MAX_SNAPSHOTS {
            s.save(&format!("s{i}"), &[track(1, 1)], i as u64)
                .expect("room");
        }
        let e = s.save("one more", &[track(1, 1)], 99).unwrap_err();
        assert!(e.contains("Delete one first"), "{e}");
    }

    #[test]
    fn very_long_queues_are_capped() {
        let d = tempfile::tempdir().expect("dir");
        let s = Store::new(d.path());
        let q: Vec<Track> = (1..=(MAX_TRACKS as u64 + 50))
            .map(|i| track(i, 1))
            .collect();
        let m = s.save("big", &q, 1).expect("save");
        assert_eq!(m.count, MAX_TRACKS);
        assert_eq!(s.load(&m.id).expect("load").len(), MAX_TRACKS);
    }

    #[test]
    fn restore_replaces_or_appends() {
        let cur = vec![track(1, 1), track(2, 1)];
        let snap = vec![track(8, 1), track(9, 1)];
        assert_eq!(
            ids(&restored(Restore::Replace, cur.clone(), snap.clone())),
            vec![8, 9]
        );
        assert_eq!(ids(&restored(Restore::Append, cur, snap)), vec![1, 2, 8, 9]);
    }

    #[test]
    fn dates_are_civil_utc() {
        assert_eq!(ymd(0), "1970-01-01");
        assert_eq!(ymd(1_700_000_000), "2023-11-14");
        assert_eq!(ymd(951_782_400), "2000-02-29");
        assert_eq!(ymd(1_793_404_800), "2026-10-31");
    }

    #[test]
    fn names_are_cleaned() {
        assert_eq!(clean_name(""), "Queue");
        assert_eq!(clean_name(&"é".repeat(99)).chars().count(), MAX_NAME_CHARS);
    }
}
