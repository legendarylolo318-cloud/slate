//! Personal tags on tracks. They live in `tags.json` next to `ui.json`, with a snapshot of each
//! tagged track (title, artist, album, duration) so a tag page opens without any API call.
//! The file is read the first time a tag feature is used and written off the UI thread,
//! debounced, after changes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use slate_core::queue::Track;
use slate_core::tidal_api::TidalTrack;
use slint::{Image, ModelRc, VecModel};

use crate::backend::Backend;
use crate::bootstrap::{Choice, PlaylistEntry};
use crate::textutil::clean_line;

/// Longest tag, in characters.
pub const MAX_TAG: usize = 40;
/// Most suggestions shown under the field.
const SUGGESTIONS: usize = 6;
/// A write waits this long for further edits.
const DEBOUNCE: Duration = Duration::from_millis(700);

/// What is remembered about a tagged track.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub title: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub artist_id: u64,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub album_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<String>,
    #[serde(default)]
    pub duration: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Entry {
    #[serde(flatten)]
    snap: Snapshot,
    tags: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TagStore {
    #[serde(default)]
    tracks: BTreeMap<u64, Entry>,
}

/// A tag as typed, made safe to store: no control or bidi characters, one line, at most
/// [`MAX_TAG`] characters. `None` when nothing is left.
pub fn normalize(raw: &str) -> Option<String> {
    let s: String = clean_line(raw).chars().take(MAX_TAG).collect();
    let s = s.trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn same(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

impl TagStore {
    pub fn parse(s: &str) -> Self {
        let mut st: Self = serde_json::from_str(s).unwrap_or_default();
        // A hand-edited or damaged file must not break the lookups below.
        for e in st.tracks.values_mut() {
            let mut kept: Vec<String> = Vec::new();
            for t in e.tags.drain(..) {
                if let Some(t) = normalize(&t) {
                    if !kept.iter().any(|k| same(k, &t)) {
                        kept.push(t);
                    }
                }
            }
            e.tags = kept;
        }
        st.tracks.retain(|_, e| !e.tags.is_empty());
        st
    }

    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .map(|s| Self::parse(&s))
            .unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }

    /// Writes through a temporary file so a crash cannot leave half a file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, self.to_json())?;
        std::fs::rename(&tmp, path)
    }

    /// Tags `id` (with a fresh snapshot). `false` when it already had the tag.
    pub fn add(&mut self, id: u64, snap: Snapshot, tag: &str) -> bool {
        let Some(tag) = normalize(tag) else { return false };
        let known = self.canonical(&tag);
        let e = self.tracks.entry(id).or_default();
        e.snap = snap;
        if e.tags.iter().any(|t| same(t, &tag)) {
            return false;
        }
        e.tags.push(known.unwrap_or(tag));
        true
    }

    /// The spelling the tag already has elsewhere, so "Jazz" and "jazz" are one tag.
    fn canonical(&self, tag: &str) -> Option<String> {
        self.tracks
            .values()
            .flat_map(|e| &e.tags)
            .find(|t| same(t, tag))
            .cloned()
    }

    pub fn remove(&mut self, id: u64, tag: &str) -> bool {
        let Some(e) = self.tracks.get_mut(&id) else { return false };
        let before = e.tags.len();
        e.tags.retain(|t| !same(t, tag));
        let changed = e.tags.len() != before;
        if e.tags.is_empty() {
            self.tracks.remove(&id);
        }
        changed
    }

    pub fn tags_of(&self, id: u64) -> &[String] {
        self.tracks.get(&id).map_or(&[], |e| e.tags.as_slice())
    }

    /// Every tag with the number of tracks that carry it, sorted by name.
    pub fn counts(&self) -> Vec<(String, usize)> {
        let mut m: BTreeMap<String, (String, usize)> = BTreeMap::new();
        for t in self.tracks.values().flat_map(|e| &e.tags) {
            m.entry(t.to_lowercase()).or_insert_with(|| (t.clone(), 0)).1 += 1;
        }
        m.into_values().collect()
    }

    /// Tags starting with `prefix` (case-insensitive), most used first, then by name.
    pub fn suggest(&self, prefix: &str, skip: &[String]) -> Vec<String> {
        let p = prefix.trim().to_lowercase();
        let mut v: Vec<(String, usize)> = self
            .counts()
            .into_iter()
            .filter(|(t, _)| t.to_lowercase().starts_with(&p) && !skip.iter().any(|s| same(s, t)))
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
        v.into_iter().map(|(t, _)| t).take(SUGGESTIONS).collect()
    }

    /// Tracks carrying `tag`, in the order they were first tagged (by id: stable).
    pub fn tracks_for(&self, tag: &str) -> Vec<(u64, &Snapshot)> {
        self.tracks
            .iter()
            .filter(|(_, e)| e.tags.iter().any(|t| same(t, tag)))
            .map(|(id, e)| (*id, &e.snap))
            .collect()
    }

    /// Renames `from` to `to`; a tag that already exists absorbs it. Returns how many tracks
    /// changed, or `None` when `to` is not a usable name.
    pub fn rename(&mut self, from: &str, to: &str) -> Option<usize> {
        let to = normalize(to)?;
        let spelled = self.canonical(&to).filter(|c| !same(c, from)).unwrap_or(to);
        let mut n = 0;
        for e in self.tracks.values_mut() {
            if let Some(i) = e.tags.iter().position(|t| same(t, from)) {
                e.tags.remove(i);
                if !e.tags.iter().any(|t| same(t, &spelled)) {
                    e.tags.push(spelled.clone());
                }
                n += 1;
            }
        }
        Some(n)
    }

    /// Removes `tag` everywhere; returns how many tracks had it.
    pub fn delete(&mut self, tag: &str) -> usize {
        let mut n = 0;
        for e in self.tracks.values_mut() {
            let before = e.tags.len();
            e.tags.retain(|t| !same(t, tag));
            n += usize::from(e.tags.len() != before);
        }
        self.tracks.retain(|_, e| !e.tags.is_empty());
        n
    }
}

impl Snapshot {
    pub fn of(t: &Track) -> Self {
        let id_of = |k: &str| {
            t.extra
                .get(k)
                .and_then(|a| a.get("id"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
        };
        Self {
            title: clean_line(t.title()),
            artist: clean_line(t.artist_name()),
            artist_id: id_of("artist"),
            album: clean_line(
                t.extra
                    .get("album")
                    .and_then(|a| a.get("title"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
            ),
            album_id: id_of("album"),
            cover: t.cover_id().map(str::to_string),
            duration: t.duration_secs() as u32,
        }
    }

    /// A track the pages can show and play, built without any API call.
    pub fn to_track(&self, id: u64) -> Option<TidalTrack> {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "title": self.title,
            "duration": self.duration,
            "artist": { "id": self.artist_id, "name": self.artist },
            "artists": [{ "id": self.artist_id, "name": self.artist, "type": "MAIN" }],
            "album": { "id": self.album_id, "title": self.album, "cover": self.cover },
        }))
        .ok()
    }
}

// ---------- the process-wide store ----------

static STORE: Mutex<Option<TagStore>> = Mutex::new(None);
static WRITE_GEN: AtomicU64 = AtomicU64::new(0);
/// The sidebar list is open: changes refresh it.
static SIDEBAR_OPEN: AtomicBool = AtomicBool::new(false);
/// The tracks the open Tags dialog edits.
static TARGET: Mutex<Vec<(u64, Snapshot)>> = Mutex::new(Vec::new());

fn path() -> Option<PathBuf> {
    slate_core::config_dir_for_env().map(|d| d.join("tags.json"))
}

/// The store, read from disk on first use.
fn store() -> MutexGuard<'static, Option<TagStore>> {
    let mut g = STORE.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        *g = Some(path().map(|p| TagStore::load(&p)).unwrap_or_default());
    }
    g
}

fn with<R>(f: impl FnOnce(&mut TagStore) -> R) -> R {
    let mut g = store();
    f(g.as_mut().expect("loaded above"))
}

/// Sidebar entry of a tag.
fn entry((name, n): &(String, usize)) -> PlaylistEntry {
    PlaylistEntry {
        uuid: format!("tag:{name}").into(),
        title: name.clone().into(),
        subtitle: format!("{n} {}", if *n == 1 { "track" } else { "tracks" }).into(),
        folder: false,
        cover: Image::default(),
    }
}

impl Backend {
    /// Writes the file after the edits have been quiet for a moment, on a blocking thread.
    fn tags_changed(&self) {
        let gen = WRITE_GEN.fetch_add(1, Ordering::AcqRel) + 1;
        let this = self.clone();
        self.rt.spawn(async move {
            tokio::time::sleep(DEBOUNCE).await;
            if WRITE_GEN.load(Ordering::Acquire) != gen {
                return;
            }
            let snapshot = with(|s| s.clone());
            let saved = tokio::task::spawn_blocking(move || match path() {
                Some(p) => snapshot.save(&p),
                None => Ok(()),
            })
            .await;
            if !matches!(saved, Ok(Ok(()))) {
                this.ui(|w| w.set_error("Could not save your tags.".into()));
            }
        });
        if SIDEBAR_OPEN.load(Ordering::Relaxed) {
            self.tags_fill_sidebar();
        }
    }

    fn tags_fill_sidebar(&self) {
        let counts = with(|s| s.counts());
        self.ui(move |w| {
            let rows: Vec<PlaylistEntry> = counts.iter().map(entry).collect();
            w.set_tag_lists(ModelRc::new(VecModel::from(rows)));
        });
    }

    /// The sidebar's Tags entry was opened or closed; the file is read on the first open.
    pub fn tags_sidebar_toggled(&self, open: bool) {
        SIDEBAR_OPEN.store(open, Ordering::Relaxed);
        if open {
            let this = self.clone();
            self.rt.spawn(async move { this.tags_fill_sidebar() });
        } else {
            self.ui(|w| w.set_tag_lists(ModelRc::default()));
        }
    }

    pub(crate) fn tags_open_tag(&self, name: &str) {
        self.navigate(crate::pages::Route::Tag(name.to_string()));
    }

    /// The tracks of `tag` for the track page.
    pub(crate) fn tag_tracks(&self, tag: &str) -> (String, Vec<TidalTrack>) {
        with(|s| {
            let name = s
                .counts()
                .into_iter()
                .map(|(t, _)| t)
                .find(|t| same(t, tag))
                .unwrap_or_else(|| tag.to_string());
            let tracks = s
                .tracks_for(tag)
                .into_iter()
                .filter_map(|(id, snap)| snap.to_track(id))
                .collect();
            (name, tracks)
        })
    }

    /// "Tags…" on one or more tracks.
    pub(crate) fn tags_open_dialog(&self, tracks: &[Track]) {
        let target: Vec<(u64, Snapshot)> = tracks
            .iter()
            .filter(|t| !t.is_video())
            .map(|t| (t.id, Snapshot::of(t)))
            .collect();
        if target.is_empty() {
            self.notify("Nothing to tag here");
            return;
        }
        *TARGET.lock().unwrap_or_else(|e| e.into_inner()) = target;
        let this = self.clone();
        self.rt.spawn(async move {
            this.tags_refresh_dialog("");
            this.ui(|w| {
                w.set_dialog_text("".into());
                w.set_dialog_target("".into());
                w.set_dialog_kind("tags".into());
                w.set_dialog_open(true);
            });
        });
    }

    /// Current tags of the targets and the suggestions for what is typed.
    fn tags_refresh_dialog(&self, typed: &str) {
        let target: Vec<u64> = TARGET
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(id, _)| *id)
            .collect();
        let n = target.len();
        let (current, suggestions) = with(|s| {
            let mut counts: BTreeMap<String, (String, usize)> = BTreeMap::new();
            for id in &target {
                for t in s.tags_of(*id) {
                    counts.entry(t.to_lowercase()).or_insert_with(|| (t.clone(), 0)).1 += 1;
                }
            }
            let current: Vec<(String, usize)> = counts.into_values().collect();
            // A tag only some of the targets have is still offered, to complete the set.
            let full: Vec<String> = current
                .iter()
                .filter(|(_, c)| *c == n)
                .map(|(t, _)| t.clone())
                .collect();
            (current, s.suggest(typed, &full))
        });
        let tags: Vec<Choice> = current
            .into_iter()
            .map(|(t, c)| Choice {
                title: if n > 1 && c < n { format!("{t} ({c}/{n})") } else { t.clone() }.into(),
                id: t.into(),
            })
            .collect();
        let sugg: Vec<Choice> = suggestions
            .into_iter()
            .map(|t| Choice { title: t.clone().into(), id: t.into() })
            .collect();
        self.ui(move |w| {
            w.set_dialog_tags(ModelRc::new(VecModel::from(tags)));
            w.set_dialog_choices(ModelRc::new(VecModel::from(sugg)));
            w.set_dialog_count(n as i32);
        });
    }

    /// Dialog buttons: "type" (the field changed), "add" a tag, "remove" a tag.
    pub fn tag_dialog_action(&self, kind: &str, text: &str) {
        let (kind, text) = (kind.to_string(), text.to_string());
        let this = self.clone();
        self.rt.spawn(async move {
            let target = TARGET.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let mut changed = false;
            match kind.as_str() {
                "add" => {
                    changed = with(|s| {
                        target
                            .iter()
                            .fold(false, |c, (id, snap)| s.add(*id, snap.clone(), &text) | c)
                    });
                    this.ui(|w| w.set_dialog_text("".into()));
                    this.tags_refresh_dialog("");
                }
                "remove" => {
                    changed = with(|s| target.iter().fold(false, |c, (id, _)| s.remove(*id, &text) | c));
                    let typed = this.read_ui(|w| w.get_dialog_text().to_string()).await;
                    this.tags_refresh_dialog(&typed);
                }
                _ => this.tags_refresh_dialog(&text),
            }
            if changed {
                this.tags_changed();
            }
        });
    }

    /// Rename or delete from the sidebar (`target` is "tag:<name>").
    pub(crate) fn tag_manage(&self, kind: &str, name: &str, text: &str) {
        let active = matches!(
            self.lib.active_kind_pub(),
            Some(crate::library::SourceKind::Tag(t)) if same(&t, name)
        );
        let (msg, reopen) = match kind {
            "rename" => match with(|s| s.rename(name, text)) {
                Some(_) => ("Tag renamed", normalize(text)),
                None => ("That is not a usable tag name", None),
            },
            _ => {
                with(|s| s.delete(name));
                ("Tag deleted", None)
            }
        };
        self.tags_changed();
        self.notify(msg);
        if active {
            match reopen {
                Some(new) => self.tags_open_tag(&new),
                None => self.navigate(crate::pages::Route::Home),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(title: &str) -> Snapshot {
        Snapshot { title: title.into(), artist: "A".into(), duration: 100, ..Snapshot::default() }
    }

    #[test]
    fn normalize_strips_bidi_controls_and_limits_length() {
        assert_eq!(normalize("  jazz \u{202E}gnp\u{202C}  ").as_deref(), Some("jazz gnp"));
        assert_eq!(normalize("\u{202E}\u{2066}  \n"), None);
        assert_eq!(normalize(&"x".repeat(100)).unwrap().chars().count(), MAX_TAG);
    }

    #[test]
    fn tags_are_case_insensitive_and_keep_the_first_spelling() {
        let mut s = TagStore::default();
        assert!(s.add(1, snap("a"), "Jazz"));
        assert!(!s.add(1, snap("a"), "jazz"));
        assert!(s.add(2, snap("b"), "JAZZ"));
        assert_eq!(s.counts(), vec![("Jazz".to_string(), 2)]);
        assert_eq!(s.tags_of(2), &["Jazz".to_string()]);
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("tags.json");
        let mut s = TagStore::default();
        s.add(5, snap("Song"), "chill");
        s.add(5, snap("Song"), "night");
        s.save(&p).unwrap();
        assert!(!p.with_extension("json.tmp").exists());
        assert_eq!(TagStore::load(&p), s);
        assert_eq!(TagStore::load(&dir.path().join("none.json")), TagStore::default());
    }

    #[test]
    fn damaged_files_load_empty_and_bad_tags_are_dropped() {
        assert_eq!(TagStore::parse("{not json"), TagStore::default());
        let s = TagStore::parse(r#"{"tracks":{"1":{"title":"t","tags":["ok","OK","\u202e","  "]}}}"#);
        assert_eq!(s.tags_of(1), &["ok".to_string()]);
    }

    #[test]
    fn rename_merges_into_an_existing_tag() {
        let mut s = TagStore::default();
        s.add(1, snap("a"), "road");
        s.add(2, snap("b"), "drive");
        s.add(2, snap("b"), "road");
        assert_eq!(s.rename("road", "Drive"), Some(2));
        assert_eq!(s.counts(), vec![("drive".to_string(), 2)]);
        assert_eq!(s.tags_of(2), &["drive".to_string()]);
        assert_eq!(s.rename("drive", "\u{202E}"), None);
    }

    #[test]
    fn delete_forgets_tracks_without_tags() {
        let mut s = TagStore::default();
        s.add(1, snap("a"), "x");
        s.add(2, snap("b"), "x");
        s.add(2, snap("b"), "y");
        assert_eq!(s.delete("X"), 2);
        assert!(s.tracks_for("x").is_empty());
        assert_eq!(s.counts(), vec![("y".to_string(), 1)]);
        assert!(s.remove(2, "y"));
        assert_eq!(s, TagStore::default());
    }

    #[test]
    fn suggestions_are_prefix_matches_by_use() {
        let mut s = TagStore::default();
        for id in 1..=3 {
            s.add(id, snap("t"), "rock");
        }
        s.add(1, snap("t"), "road trip");
        s.add(1, snap("t"), "jazz");
        assert_eq!(s.suggest("ro", &[]), vec!["rock".to_string(), "road trip".to_string()]);
        assert_eq!(s.suggest("ro", &["rock".to_string()]), vec!["road trip".to_string()]);
        assert_eq!(s.suggest("", &[]).len(), 3);
    }

    #[test]
    fn snapshots_become_playable_tracks() {
        let t = Snapshot { title: "T".into(), artist: "Ar".into(), artist_id: 9, album: "Al".into(), album_id: 8, cover: Some("c-d".into()), duration: 61 }
            .to_track(77)
            .unwrap();
        assert_eq!((t.id, t.title.as_str(), t.duration), (77, "T", 61));
        assert_eq!(t.album.unwrap().id, 8);
    }
}
