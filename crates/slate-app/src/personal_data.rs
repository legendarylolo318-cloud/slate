//! Settings → "Export / import personal data": the lyrics timing offsets and the personal
//! tags in one JSON file, so they can travel to another computer by hand. No network.
//!
//! Import merges and never removes anything: an offset in the file replaces the local
//! one for that track, tags are added to what a track already has. Text from the file is
//! cleaned like tags typed in the app (no control or bidirectional-override characters).

use serde::{Deserialize, Serialize};

use crate::backend::Backend;
use crate::tags::Snapshot;
use crate::textutil::clean_line;

const SCHEMA: &str = "slate-personal-data";
const VERSION: u32 = 1;
/// A file bigger than this is not ours.
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TAG_TRACKS: usize = 20_000;
const MAX_OFFSETS: usize = 10_000;
const MAX_TEXT: usize = 200;

#[derive(Debug, Serialize, Deserialize)]
struct File {
    schema: String,
    version: u32,
    #[serde(default)]
    exported: u64,
    #[serde(default)]
    lyrics_offsets: Vec<OffsetEntry>,
    #[serde(default)]
    tags: Vec<TagEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct OffsetEntry {
    track: u64,
    ms: i32,
}

#[derive(Debug, Serialize, Deserialize)]
struct TagEntry {
    track: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    artist: String,
    #[serde(default)]
    artist_id: u64,
    #[serde(default)]
    album: String,
    #[serde(default)]
    album_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cover: Option<String>,
    #[serde(default)]
    duration: u32,
    #[serde(default)]
    tags: Vec<String>,
}

/// What a file holds after cleaning.
#[derive(Debug, Default, PartialEq)]
pub struct Imported {
    pub offsets: Vec<(u64, i32)>,
    pub tags: Vec<(u64, Snapshot, Vec<String>)>,
}

/// The file's text for the given data.
pub fn build(
    offsets: impl IntoIterator<Item = (u64, i32)>,
    tags: impl IntoIterator<Item = (u64, Snapshot, Vec<String>)>,
    now: u64,
) -> String {
    let file = File {
        schema: SCHEMA.into(),
        version: VERSION,
        exported: now,
        lyrics_offsets: offsets
            .into_iter()
            .map(|(track, ms)| OffsetEntry { track, ms })
            .collect(),
        tags: tags
            .into_iter()
            .map(|(track, s, tags)| TagEntry {
                track,
                title: s.title,
                artist: s.artist,
                artist_id: s.artist_id,
                album: s.album,
                album_id: s.album_id,
                cover: s.cover,
                duration: s.duration,
                tags,
            })
            .collect(),
    };
    serde_json::to_string_pretty(&file).unwrap_or_default()
}

fn text(s: &str) -> String {
    clean_line(s).chars().take(MAX_TEXT).collect()
}

/// A cover id is a UUID-like token used to build an image address: nothing else passes.
fn cover(c: Option<String>) -> Option<String> {
    c.filter(|c| {
        !c.is_empty()
            && c.len() <= 64
            && c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
    })
}

/// Reads and cleans a file. Errors are sentences for the user.
pub fn parse(raw: &str) -> Result<Imported, String> {
    let file: File = serde_json::from_str(raw)
        .map_err(|_| "This is not a Slate personal data file.".to_string())?;
    if file.schema != SCHEMA {
        return Err("This is not a Slate personal data file.".into());
    }
    if file.version > VERSION {
        return Err("This file was made by a newer Slate. Update Slate to read it.".into());
    }
    let mut out = Imported::default();
    for o in file.lyrics_offsets.into_iter().take(MAX_OFFSETS) {
        let ms = o.ms.clamp(
            -slate_core::lyrics_offset::MAX_MS,
            slate_core::lyrics_offset::MAX_MS,
        );
        if ms != 0 {
            out.offsets.push((o.track, ms));
        }
    }
    for t in file.tags.into_iter().take(MAX_TAG_TRACKS) {
        let tags: Vec<String> = t
            .tags
            .iter()
            .filter_map(|x| crate::tags::normalize(x))
            .collect();
        if tags.is_empty() {
            continue;
        }
        let snap = Snapshot {
            title: text(&t.title),
            artist: text(&t.artist),
            artist_id: t.artist_id,
            album: text(&t.album),
            album_id: t.album_id,
            cover: cover(t.cover),
            duration: t.duration,
        };
        out.tags.push((t.track, snap, tags));
    }
    Ok(out)
}

impl Backend {
    pub(crate) fn personal_data_export(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(file) = rfd::AsyncFileDialog::new()
                .set_title("Export personal data")
                .set_file_name("slate-personal-data.json")
                .add_filter("JSON", &["json"])
                .save_file()
                .await
            else {
                return;
            };
            let path = file.path().to_path_buf();
            let msg = tokio::task::spawn_blocking(move || export_to(&path))
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            this.settings_status(match msg {
                Ok(m) | Err(m) => m,
            });
        });
    }

    pub(crate) fn personal_data_import(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(file) = rfd::AsyncFileDialog::new()
                .set_title("Import personal data")
                .add_filter("JSON", &["json"])
                .pick_file()
                .await
            else {
                return;
            };
            let path = file.path().to_path_buf();
            let read = tokio::task::spawn_blocking(move || read_file(&path))
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            let msg = match read {
                Ok(imp) => this.apply_import(imp).await,
                Err(m) => m,
            };
            this.settings_status(msg);
        });
    }

    async fn apply_import(&self, imp: Imported) -> String {
        let n_offsets = imp.offsets.len();
        if let Some(dir) = slate_core::config_dir_for_env() {
            let path = slate_core::lyrics_offset::path_in(&dir);
            let offsets = imp.offsets;
            let _ = tokio::task::spawn_blocking(move || {
                if !offsets.is_empty() {
                    slate_core::lyrics_offset::update_saved(&path, |all| {
                        for (id, ms) in offsets {
                            all.set(id, ms);
                        }
                    });
                }
            })
            .await;
        }
        let (tracks, tags) = self.tags_import(imp.tags);
        // The offset of the track on screen may have changed.
        self.reload_lyric_offset();
        format!(
            "Imported {n_offsets} lyrics {} and {tags} new {} on {tracks} new {}",
            if n_offsets == 1 { "offset" } else { "offsets" },
            if tags == 1 { "tag" } else { "tags" },
            if tracks == 1 { "track" } else { "tracks" },
        )
    }
}

fn export_to(path: &std::path::Path) -> Result<String, String> {
    let dir = slate_core::config_dir_for_env().ok_or("No profile folder.")?;
    let offsets = slate_core::lyrics_offset::load(&slate_core::lyrics_offset::path_in(&dir));
    let n = offsets.iter().count();
    let tags = crate::tags::export_entries();
    let t = tags.len();
    let body = build(offsets.iter(), tags, slate_core::now_secs());
    slate_core::write_atomic(path, body.as_bytes())
        .map_err(|e| format!("Could not save the file: {e}"))?;
    Ok(format!(
        "Exported {n} lyrics offsets and {t} tagged tracks to {}",
        path.display()
    ))
}

fn read_file(path: &std::path::Path) -> Result<Imported, String> {
    let len = std::fs::metadata(path)
        .map_err(|e| format!("Could not read the file: {e}"))?
        .len();
    if len > MAX_FILE_BYTES {
        return Err("This file is too big to be a Slate personal data file.".into());
    }
    let raw = std::fs::read_to_string(path).map_err(|e| format!("Could not read the file: {e}"))?;
    parse(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(title: &str) -> Snapshot {
        Snapshot {
            title: title.into(),
            artist: "Artist".into(),
            artist_id: 5,
            album: "Album".into(),
            album_id: 6,
            cover: Some("ab12-cd".into()),
            duration: 200,
        }
    }

    #[test]
    fn a_file_round_trips() {
        let text = build(
            [(1, 300), (2, -150)],
            [(
                9,
                snap("Song"),
                vec!["Jazz".to_string(), "Late".to_string()],
            )],
            1,
        );
        let got = parse(&text).expect("parses");
        assert_eq!(got.offsets, vec![(1, 300), (2, -150)]);
        assert_eq!(got.tags.len(), 1);
        assert_eq!(got.tags[0].0, 9);
        assert_eq!(got.tags[0].1, snap("Song"));
        assert_eq!(got.tags[0].2, vec!["Jazz", "Late"]);
    }

    #[test]
    fn hostile_text_is_cleaned_and_values_are_bounded() {
        // JSON escapes, so the source itself holds no bidi characters.
        let raw = concat!(
            r#"{"schema":"slate-personal-data","version":1,"#,
            r#""lyrics_offsets":[{"track":1,"ms":99999999},{"track":2,"ms":0}],"#,
            r#""tags":[{"track":3,"title":"a\u202Eb\u0007c","cover":"../../etc/passwd","#,
            r#""tags":["x\u202Ey\n","  ","\u2066"]}]}"#
        );
        let got = parse(raw).expect("parses");
        assert_eq!(got.offsets, vec![(1, slate_core::lyrics_offset::MAX_MS)]);
        assert_eq!(got.tags.len(), 1);
        let (_, s, tags) = &got.tags[0];
        assert_eq!(s.title, "abc");
        assert_eq!(s.cover, None, "a path is not a cover id");
        assert_eq!(tags, &vec!["xy".to_string()]);
    }

    #[test]
    fn other_files_and_newer_versions_are_refused() {
        assert!(parse("not json").is_err());
        assert!(parse(r#"{"schema":"other","version":1}"#).is_err());
        let e = parse(r#"{"schema":"slate-personal-data","version":2}"#).unwrap_err();
        assert!(e.contains("newer"), "{e}");
        // Missing sections are fine: an empty export.
        assert_eq!(
            parse(r#"{"schema":"slate-personal-data","version":1}"#),
            Ok(Imported::default())
        );
    }

    #[test]
    fn untagged_entries_are_dropped() {
        let raw = r#"{"schema":"slate-personal-data","version":1,"tags":[{"track":1,"tags":[]}]}"#;
        assert!(parse(raw).unwrap().tags.is_empty());
    }
}
