//! Exporting a playlist or Loved Tracks as CSV or M3U.
//!
//! The text builders are pure; `Backend::export_list` adds the save dialog,
//! the one-page-at-a-time fetching of whatever is not loaded yet, and progress.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::Value;
use slate_core::queue::Track;

use crate::backend::Backend;
use crate::library::SourceKind;
use crate::models::PAGE_SIZE;

/// Pause between page requests, so a long list does not hit TIDAL in a burst.
const PAGE_PAUSE: Duration = Duration::from_millis(300);

/// One export at a time.
static EXPORTING: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Csv,
    M3u,
}

impl Format {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "csv" => Some(Self::Csv),
            "m3u" => Some(Self::M3u),
            _ => None,
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::M3u => "m3u",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Csv => "CSV",
            Self::M3u => "M3U playlist",
        }
    }

    fn render(self, tracks: &[Track]) -> String {
        match self {
            Self::Csv => csv(tracks),
            Self::M3u => m3u(tracks),
        }
    }
}

fn track_url(id: u64) -> String {
    format!("https://tidal.com/track/{id}")
}

fn title_of(t: &Track) -> String {
    match t.extra.get("version").and_then(Value::as_str) {
        Some(v) if !v.is_empty() => format!("{} ({v})", t.title()),
        _ => t.title().to_string(),
    }
}

fn artists_of(t: &Track) -> Vec<String> {
    let names: Vec<String> = t
        .extra
        .get("artists")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if names.is_empty() {
        vec![t.artist_name().to_string()]
    } else {
        names
    }
}

fn album_of(t: &Track) -> &str {
    t.extra
        .get("album")
        .and_then(|a| a.get("title"))
        .and_then(Value::as_str)
        .unwrap_or("")
}

fn seconds_of(t: &Track) -> u64 {
    t.duration_secs().max(0.0).round() as u64
}

/// One CSV field: quoted when it holds a comma, quote, CR or LF, with quotes doubled.
fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// RFC 4180 rows with CRLF line ends. Starts with a byte-order mark, which is
/// what makes Excel read the accents and other non-Latin text correctly.
pub fn csv(tracks: &[Track]) -> String {
    let mut out = String::from("\u{feff}Title,Artists,Album,Duration (seconds),ISRC,TIDAL URL\r\n");
    for t in tracks {
        let isrc = t.extra.get("isrc").and_then(Value::as_str).unwrap_or("");
        let fields = [
            title_of(t),
            artists_of(t).join("; "),
            album_of(t).to_string(),
            seconds_of(t).to_string(),
            isrc.to_string(),
            track_url(t.id),
        ];
        let row: Vec<String> = fields.iter().map(|f| csv_field(f)).collect();
        out.push_str(&row.join(","));
        out.push_str("\r\n");
    }
    out
}

/// A line of an M3U must not break across lines.
fn one_line(s: &str) -> String {
    s.split(['\r', '\n']).collect::<Vec<_>>().join(" ")
}

/// Extended M3U (UTF-8): an `#EXTINF` line, then the track's TIDAL URL.
pub fn m3u(tracks: &[Track]) -> String {
    let mut out = String::from("#EXTM3U\n");
    for t in tracks {
        let label = format!("{} - {}", artists_of(t).join(", "), title_of(t));
        out.push_str(&format!(
            "#EXTINF:{},{}\n{}\n",
            seconds_of(t),
            one_line(&label),
            track_url(t.id)
        ));
    }
    out
}

/// A file name that is valid on Linux and Windows.
pub fn file_name(title: &str, format: Format) -> String {
    named(title, format.extension())
}

/// `title` made into a file name with extension `ext`.
pub fn named(title: &str, ext: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cut: String = cleaned.trim().chars().take(100).collect();
    // Windows drops a trailing dot or space, so a name may not end in one.
    let mut stem = cut.trim_end_matches(['.', ' ']).trim_start().to_string();
    let reserved = ["CON", "PRN", "AUX", "NUL"];
    let upper = stem.to_ascii_uppercase();
    let base = upper.split('.').next().unwrap_or("");
    let numbered = (base.starts_with("COM") || base.starts_with("LPT"))
        && base.len() == 4
        && base.ends_with(|c: char| c.is_ascii_digit());
    if stem.is_empty() || reserved.contains(&base) || numbered {
        stem = format!("Slate {stem}").trim().to_string();
    }
    format!("{stem}.{ext}")
}

impl Backend {
    /// "Export…" on a playlist or Loved Tracks page.
    pub fn export_list(&self, format: &str) {
        let Some(format) = Format::parse(format) else {
            return;
        };
        let Some(active) = self.lib.active_for_export() else {
            return;
        };
        let (kind, title) = active;
        if !matches!(kind, SourceKind::Loved | SourceKind::Playlist(_)) {
            return;
        }
        // Taken now, before the dialog: by the time it closes the user may be
        // on another page, whose rows must not end up in this file.
        let generation = self.lib.generation_now();
        let total = self.lib.total_now();
        if EXPORTING.swap(true, Ordering::SeqCst) {
            self.notify("An export is already running");
            return;
        }
        let this = self.clone();
        self.rt.spawn(async move {
            let result = this
                .run_export(format, kind, title, generation, total)
                .await;
            EXPORTING.store(false, Ordering::SeqCst);
            match result {
                Ok(Some(message)) => this.notify(&message),
                Ok(None) => this.ui(|w| w.set_notice("".into())),
                Err(message) => this.ui(move |w| {
                    w.set_notice("".into());
                    w.set_error(message.into());
                }),
            }
        });
    }

    /// `Ok(None)`: the user cancelled the dialog.
    async fn run_export(
        &self,
        format: Format,
        kind: SourceKind,
        title: String,
        generation: u64,
        total: usize,
    ) -> Result<Option<String>, String> {
        let name = file_name(&title, format);
        let Some(file) = rfd::AsyncFileDialog::new()
            .set_title("Export")
            .set_file_name(&name)
            .add_filter(format.label(), &[format.extension()])
            .save_file()
            .await
        else {
            return Ok(None);
        };
        let path = with_extension(file.path(), format);
        let mut pages: BTreeMap<usize, Vec<Track>> = BTreeMap::new();
        let page_count = total.div_ceil(PAGE_SIZE);
        let mut fetched_any = false;
        for page in 0..page_count {
            if self.lib.generation_now() != generation {
                return Err("Export cancelled: the page changed.".into());
            }
            let wanted = PAGE_SIZE.min(total - page * PAGE_SIZE);
            if let Some(loaded) = self.loaded_page(page, wanted) {
                pages.insert(page, loaded);
            } else {
                if fetched_any {
                    tokio::time::sleep(PAGE_PAUSE).await;
                }
                fetched_any = true;
                let (items, _) = self.fetch_page_for_export(&kind, page).await?;
                if self.lib.generation_now() != generation {
                    return Err("Export cancelled: the page changed.".into());
                }
                pages.insert(
                    page,
                    items.iter().filter_map(crate::library::to_track).collect(),
                );
            }
            let done: usize = pages.values().map(Vec::len).sum();
            let text = format!("Exporting… {done} of {total} tracks");
            self.ui(move |w| w.set_notice(text.into()));
        }
        let tracks: Vec<Track> = pages.into_values().flatten().collect();
        let body = format.render(&tracks);
        let count = tracks.len();
        let written = write_file(path.clone(), body).await;
        written.map_err(|e| format!("Could not save {}: {e}", path.display()))?;
        Ok(Some(format!(
            "Exported {count} tracks to {}",
            path.display()
        )))
    }
}

/// Some save dialogs (portals in particular) return the name as typed, so a
/// missing extension is added.
fn with_extension(path: &Path, format: Format) -> std::path::PathBuf {
    if path.extension().is_some() {
        path.to_path_buf()
    } else {
        path.with_extension(format.extension())
    }
}

async fn write_file(path: std::path::PathBuf, body: String) -> std::io::Result<()> {
    tokio::task::spawn_blocking(move || write_atomically(&path, body.as_bytes()))
        .await
        .map_err(std::io::Error::other)?
}

/// Writes next to the target and renames, so a failure never leaves half a file
/// where the user's previous export was.
fn write_atomically(path: &Path, body: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp-slate");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn track(id: u64, v: Value) -> Track {
        let mut t = Track::new(id);
        if let Value::Object(m) = v {
            t.extra = m;
        }
        t
    }

    #[test]
    fn csv_quotes_commas_quotes_and_newlines() {
        let t = track(
            7,
            json!({
                "title": "Hello, \"World\"",
                "artists": [{"name": "A, B"}, {"name": "C"}],
                "album": {"title": "Line1\nLine2"},
                "duration": 215,
                "isrc": "USRC17607839",
            }),
        );
        let out = csv(&[t]);
        let row = out.lines().nth(1).map(str::to_string);
        assert!(
            out.starts_with("\u{feff}Title,Artists,Album,Duration (seconds),ISRC,TIDAL URL\r\n")
        );
        // The embedded newline lives inside quotes, so the logical row spans two lines.
        assert!(out.contains("\"Hello, \"\"World\"\"\",\"A, B; C\",\"Line1\nLine2\",215,USRC17607839,https://tidal.com/track/7\r\n"));
        assert!(row.is_some());
    }

    #[test]
    fn csv_keeps_unicode_and_blank_isrc() {
        let t = track(
            1,
            json!({"title": "Beyoncé — Ça va 東京", "artist": {"name": "Björk"}, "duration": 61.4}),
        );
        let out = csv(&[t]);
        assert!(out.contains("Beyoncé — Ça va 東京,Björk,,61,,https://tidal.com/track/1\r\n"));
    }

    #[test]
    fn version_is_part_of_the_title() {
        let t = track(
            2,
            json!({"title": "Song", "version": "Live", "artist": {"name": "X"}}),
        );
        assert!(csv(std::slice::from_ref(&t)).contains("Song (Live),X,"));
        assert!(m3u(&[t]).contains(",X - Song (Live)\n"));
    }

    #[test]
    fn m3u_has_extinf_then_url_and_flattens_newlines() {
        let a = track(
            10,
            json!({"title": "One", "artist": {"name": "Ann"}, "duration": 180}),
        );
        let b = track(
            11,
            json!({"title": "Two\nlines", "artists": [{"name": "Bo"}, {"name": "Cy"}], "duration": 59.6}),
        );
        assert_eq!(
            m3u(&[a, b]),
            "#EXTM3U\n#EXTINF:180,Ann - One\nhttps://tidal.com/track/10\n#EXTINF:60,Bo, Cy - Two lines\nhttps://tidal.com/track/11\n"
        );
    }

    #[test]
    fn empty_lists_still_produce_valid_files() {
        assert_eq!(m3u(&[]), "#EXTM3U\n");
        assert!(csv(&[]).ends_with("TIDAL URL\r\n"));
    }

    #[test]
    fn file_names_are_valid_on_every_platform() {
        assert_eq!(file_name("Road trip", Format::Csv), "Road trip.csv");
        assert_eq!(file_name("AC/DC: Live?", Format::M3u), "AC_DC_ Live_.m3u");
        assert_eq!(file_name("", Format::Csv), "Slate.csv");
        assert_eq!(file_name("con", Format::Csv), "Slate con.csv");
        assert_eq!(file_name("COM1", Format::M3u), "Slate COM1.m3u");
        assert_eq!(file_name("Mix...", Format::M3u), "Mix.m3u");
        assert!(file_name(&"x".repeat(300), Format::Csv).len() <= 104);
        assert_eq!(file_name("Mix . ", Format::Csv), "Mix.csv");
        let long = format!("{} tail", "x".repeat(99));
        assert_eq!(
            file_name(&long, Format::Csv),
            format!("{}.csv", "x".repeat(99))
        );
    }

    #[test]
    fn a_name_typed_without_an_extension_gets_one() {
        assert_eq!(
            with_extension(Path::new("/tmp/list"), Format::M3u),
            Path::new("/tmp/list.m3u")
        );
        assert_eq!(
            with_extension(Path::new("/tmp/list.txt"), Format::Csv),
            Path::new("/tmp/list.txt")
        );
    }

    #[test]
    fn format_names_parse() {
        assert_eq!(Format::parse("csv"), Some(Format::Csv));
        assert_eq!(Format::parse("m3u"), Some(Format::M3u));
        assert_eq!(Format::parse("pdf"), None);
    }

    #[test]
    fn writing_replaces_the_file_and_leaves_no_temp_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("list.csv");
        std::fs::write(&path, "old").expect("seed");
        write_atomically(&path, b"new").expect("write");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "new");
        assert_eq!(std::fs::read_dir(dir.path()).expect("dir").count(), 1);
    }
}
