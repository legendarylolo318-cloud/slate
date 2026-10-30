//! Importing a CSV or M3U file as a new playlist.
//!
//! The parsers are pure. Rows that carry a TIDAL track link (Slate's own
//! exports, or any file with `tidal.com/track/…` links) need no request; other
//! rows are looked up by artist and title, one search at a time with a pause
//! between them, and the import stops at the first rate limit.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use slate_core::commands::{library as lib_cmds, search};

use crate::backend::Backend;

/// Pause between requests, as for export.
const PAUSE: Duration = Duration::from_millis(300);
/// Tracks added per request.
const BATCH: usize = 50;
/// Larger files are refused rather than read into memory.
const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// One import at a time.
static IMPORTING: AtomicBool = AtomicBool::new(false);

/// A track named by a file: its TIDAL id when the file has one, otherwise
/// the artist and title to search for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    pub id: Option<u64>,
    pub title: String,
    pub artist: String,
}

impl Entry {
    fn usable(&self) -> bool {
        self.id.is_some() || !self.title.trim().is_empty()
    }
}

/// The track id in a TIDAL link (`tidal.com/track/1`, `/browse/track/1`,
/// `listen.tidal.com/track/1?u`), if `s` contains one.
pub fn tidal_track_id(s: &str) -> Option<u64> {
    let at = s.find("tidal.com/")?;
    let rest = &s[at + "tidal.com/".len()..];
    let rest = rest.strip_prefix("browse/").unwrap_or(rest);
    let digits: String = rest
        .strip_prefix("track/")?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok().filter(|id| *id > 0)
}

/// Picks the parser from the content: M3U when it says so or is a list of
/// links, CSV otherwise.
pub fn parse(text: &str) -> Vec<Entry> {
    let text = text.trim_start_matches('\u{feff}');
    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let looks_m3u = first.trim_start().starts_with('#')
        || (tidal_track_id(first).is_some() && !first.contains(','));
    if looks_m3u {
        parse_m3u(text)
    } else {
        parse_csv(text)
    }
}

/// Extended or plain M3U. `#EXTINF:secs,Artist - Title` names the next entry,
/// which is used for a search when its location is not a TIDAL link.
pub fn parse_m3u(text: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut pending: Option<(String, String)> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(info) = line.strip_prefix("#EXTINF:") {
            let label = info.split_once(',').map_or("", |(_, l)| l).trim();
            pending = Some(match label.split_once(" - ") {
                Some((artist, title)) => (artist.trim().to_string(), title.trim().to_string()),
                None => (String::new(), label.to_string()),
            });
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let (artist, title) = pending.take().unwrap_or_else(|| {
            // A bare location: a file name is the best guess at a title.
            let stem = line
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(line)
                .rsplit_once('.')
                .map_or(line, |(s, _)| s);
            (String::new(), stem.to_string())
        });
        let entry = Entry {
            id: tidal_track_id(line),
            title,
            artist,
        };
        if entry.usable() {
            out.push(entry);
        }
    }
    out
}

/// RFC 4180 records: quoted fields may hold commas, doubled quotes and line
/// breaks.
fn csv_records(text: &str) -> Vec<Vec<String>> {
    let mut records = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    field.push('"');
                    chars.next();
                }
                '"' => quoted = false,
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => quoted = true,
            ',' => record.push(std::mem::take(&mut field)),
            '\r' => {}
            '\n' => {
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
            }
            _ => field.push(c),
        }
    }
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    }
    records.retain(|r| r.iter().any(|f| !f.trim().is_empty()));
    records
}

/// A CSV with a header row. Title and artist columns are found by name
/// (Slate's "Title"/"Artists", Spotify exports' "Track Name"/"Artist Name(s)",
/// and similar); any field holding a TIDAL track link gives the id.
pub fn parse_csv(text: &str) -> Vec<Entry> {
    let records = csv_records(text);
    let Some((header, rows)) = records.split_first() else {
        return Vec::new();
    };
    let names: Vec<String> = header.iter().map(|h| h.trim().to_lowercase()).collect();
    let find = |wanted: &[&str]| names.iter().position(|n| wanted.contains(&n.as_str()));
    let title_col = find(&[
        "title",
        "track name",
        "track",
        "name",
        "song",
        "track title",
    ]);
    let artist_col = find(&[
        "artists",
        "artist",
        "artist name(s)",
        "artist name",
        "artist names",
        "performer",
    ]);
    let header_has_link = header.iter().any(|h| tidal_track_id(h).is_some());
    // No recognisable header: every row is data, read as title, artist.
    let (title_col, artist_col, rows) = match (title_col, artist_col) {
        (None, None) => (Some(0), Some(1), &records[..]),
        _ if header_has_link => (title_col, artist_col, &records[..]),
        _ => (title_col, artist_col, rows),
    };
    let cell = |row: &[String], col: Option<usize>| {
        col.and_then(|i| row.get(i))
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    };
    rows.iter()
        .map(|row| Entry {
            id: row.iter().find_map(|f| tidal_track_id(f)),
            title: cell(row, title_col),
            // "A; B" (Slate) or "A, B" (Spotify): the first artist searches best.
            artist: cell(row, artist_col)
                .split([';', ','])
                .next()
                .unwrap_or("")
                .trim()
                .to_string(),
        })
        .filter(Entry::usable)
        .collect()
}

/// A playlist name from the file name.
fn playlist_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Imported playlist")
        .to_string()
}

/// Loose title match for choosing among search results: case and anything
/// in brackets ("(Remastered)") are ignored.
fn same_title(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        let s = s.split(['(', '[']).next().unwrap_or(s);
        s.trim().to_lowercase()
    };
    let (a, b) = (norm(a), norm(b));
    !a.is_empty() && (a == b || a.starts_with(&b) || b.starts_with(&a))
}

impl Backend {
    /// "Import from file…" in the new-playlist dialog.
    pub fn import_playlist(&self) {
        if IMPORTING.swap(true, Ordering::SeqCst) {
            self.notify("An import is already running");
            return;
        }
        let this = self.clone();
        self.rt.spawn(async move {
            let result = this.run_import().await;
            IMPORTING.store(false, Ordering::SeqCst);
            match result {
                Ok(Some(message)) => {
                    this.notify(&message);
                    this.load_library();
                }
                Ok(None) => this.ui(|w| w.set_notice("".into())),
                Err(message) => this.ui(move |w| {
                    w.set_notice("".into());
                    w.set_error(message.into());
                }),
            }
        });
    }

    /// `Ok(None)`: the user cancelled the dialog.
    async fn run_import(&self) -> Result<Option<String>, String> {
        let Some(file) = rfd::AsyncFileDialog::new()
            .set_title("Import playlist")
            .add_filter("Playlist (CSV, M3U)", &["csv", "m3u", "m3u8", "txt"])
            .pick_file()
            .await
        else {
            return Ok(None);
        };
        let path = file.path().to_path_buf();
        let name = playlist_name(&path);
        let text = read_text(path).await?;
        let entries = parse(&text);
        if entries.is_empty() {
            return Err(
                "No tracks found in that file. Use a CSV with a title column \
                        or an M3U playlist."
                    .into(),
            );
        }
        let state = self.state().ok_or("The core is still starting.")?;

        // Resolve ids: links first (free), then one search per remaining row.
        let total = entries.len();
        let mut ids: Vec<u64> = Vec::with_capacity(total);
        let mut searched_any = false;
        for (i, entry) in entries.iter().enumerate() {
            if let Some(id) = entry.id {
                ids.push(id);
                continue;
            }
            if searched_any {
                tokio::time::sleep(PAUSE).await;
            }
            searched_any = true;
            let text = format!("Importing… finding track {} of {total}", i + 1);
            self.ui(move |w| w.set_notice(text.into()));
            let query = format!("{} {}", entry.artist, entry.title);
            match search::search_tidal(&state, query.trim().to_string(), 5).await {
                Ok(found) => {
                    let pick = found
                        .tracks
                        .iter()
                        .find(|t| same_title(&t.title, &entry.title))
                        .or_else(|| found.tracks.first());
                    if let Some(t) = pick {
                        ids.push(t.id);
                    }
                }
                Err(e) if e.is_rate_limited() => {
                    return Err(format!(
                        "TIDAL is limiting requests; the import stopped after {i} of {total} \
                         tracks. Try again in a few minutes."
                    ));
                }
                Err(e) => tracing::debug!("import: search failed: {}", e.log_safe()),
            }
        }
        let mut seen = std::collections::HashSet::new();
        ids.retain(|id| seen.insert(*id));
        if ids.is_empty() {
            return Err("None of the tracks in that file were found on TIDAL.".into());
        }

        let playlist =
            lib_cmds::create_playlist(&state, name.clone(), String::new(), "PRIVATE".into())
                .await
                .map_err(|e| format!("Could not create the playlist: {}", e.log_safe()))?;

        let mut added = 0usize;
        for (n, batch) in ids.chunks(BATCH).enumerate() {
            if n > 0 {
                tokio::time::sleep(PAUSE).await;
            }
            let text = format!("Importing… adding {} of {} tracks", added, ids.len());
            self.ui(move |w| w.set_notice(text.into()));
            match lib_cmds::add_tracks_to_playlist(&state, playlist.uuid.clone(), batch.to_vec())
                .await
            {
                Ok(()) => added += batch.len(),
                Err(e) if e.is_rate_limited() => {
                    return Err(format!(
                        "TIDAL is limiting requests; \"{name}\" was created with {added} of \
                         {} tracks.",
                        ids.len()
                    ));
                }
                // One unavailable track fails its whole batch; add that
                // batch's tracks one by one so the rest still land.
                Err(_) => {
                    for id in batch {
                        tokio::time::sleep(PAUSE).await;
                        match lib_cmds::add_tracks_to_playlist(
                            &state,
                            playlist.uuid.clone(),
                            vec![*id],
                        )
                        .await
                        {
                            Ok(()) => added += 1,
                            Err(e) if e.is_rate_limited() => {
                                return Err(format!(
                                    "TIDAL is limiting requests; \"{name}\" was created with \
                                     {added} of {} tracks.",
                                    ids.len()
                                ));
                            }
                            Err(_) => {}
                        }
                    }
                }
            }
        }
        Ok(Some(format!(
            "Imported {added} of {total} tracks into \"{name}\""
        )))
    }
}

async fn read_text(path: std::path::PathBuf) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        let len = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
        if len > MAX_FILE_BYTES {
            return Err("That file is too large to be a playlist (over 10 MB).".to_string());
        }
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        // Not UTF-8 (an old Latin-1 M3U): keep what can be read.
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| format!("Could not read the file: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.title.as_str()).collect()
    }

    #[test]
    fn tidal_links_give_ids() {
        assert_eq!(
            tidal_track_id("https://tidal.com/track/77646169"),
            Some(77646169)
        );
        assert_eq!(
            tidal_track_id("https://tidal.com/browse/track/12?u"),
            Some(12)
        );
        assert_eq!(tidal_track_id("https://listen.tidal.com/track/5"), Some(5));
        assert_eq!(tidal_track_id("https://tidal.com/album/5"), None);
        assert_eq!(tidal_track_id("https://example.com/track/5"), None);
        assert_eq!(tidal_track_id("tidal.com/track/"), None);
    }

    #[test]
    fn slates_own_csv_round_trips() {
        let mut t = slate_core::queue::Track::new(7);
        if let serde_json::Value::Object(m) = serde_json::json!({
            "title": "Hello, \"World\"",
            "artists": [{"name": "A, B"}, {"name": "C"}],
            "album": {"title": "Line1\nLine2"},
            "duration": 215,
        }) {
            t.extra = m;
        }
        let file = crate::export::csv(&[t]);
        let entries = parse(&file);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, Some(7));
        assert_eq!(entries[0].title, "Hello, \"World\"");
    }

    #[test]
    fn slates_own_m3u_round_trips() {
        let mut t = slate_core::queue::Track::new(10);
        if let serde_json::Value::Object(m) =
            serde_json::json!({"title": "One", "artist": {"name": "Ann"}, "duration": 180})
        {
            t.extra = m;
        }
        let entries = parse(&crate::export::m3u(&[t]));
        assert_eq!(
            entries,
            vec![Entry {
                id: Some(10),
                title: "One".into(),
                artist: "Ann".into()
            }]
        );
    }

    #[test]
    fn spotify_style_csv_is_searched_by_artist_and_title() {
        let file = "Track URI,Track Name,Artist Name(s),Album Name\r\n\
                    spotify:track:x,Thunderstruck,\"AC/DC\",The Razors Edge\r\n\
                    spotify:track:y,Livin' On A Prayer,\"Bon Jovi, Someone\",Slippery When Wet\r\n";
        let entries = parse(file);
        assert_eq!(
            titles(&entries),
            vec!["Thunderstruck", "Livin' On A Prayer"]
        );
        assert_eq!(entries[1].artist, "Bon Jovi");
        assert!(entries.iter().all(|e| e.id.is_none()));
    }

    #[test]
    fn m3u_without_tidal_links_uses_extinf_or_the_file_name() {
        let file = "#EXTM3U\n#EXTINF:240,Europe - The Final Countdown\n/music/x.flac\n\
                    C:\\Music\\Eye of the Tiger.mp3\n";
        let entries = parse(file);
        assert_eq!(entries[0].artist, "Europe");
        assert_eq!(entries[0].title, "The Final Countdown");
        assert_eq!(entries[1].title, "Eye of the Tiger");
        assert!(entries.iter().all(|e| e.id.is_none()));
    }

    #[test]
    fn a_plain_list_of_links_is_read_as_m3u() {
        let entries = parse("https://tidal.com/track/1\nhttps://tidal.com/track/2\n");
        let ids: Vec<_> = entries.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![Some(1), Some(2)]);
    }

    #[test]
    fn headerless_csv_is_title_then_artist() {
        let entries = parse("Song A,Artist A\nSong B,Artist B\n");
        assert_eq!(titles(&entries), vec!["Song A", "Song B"]);
        assert_eq!(entries[1].artist, "Artist B");
    }

    #[test]
    fn empty_and_junk_files_give_nothing() {
        assert!(parse("").is_empty());
        assert!(parse("Title,Artists\r\n").is_empty());
        assert!(parse("#EXTM3U\n").is_empty());
        assert!(parse("Title,Artists\r\n,\r\n").is_empty());
    }

    #[test]
    fn titles_match_loosely() {
        assert!(same_title("Thunderstruck", "thunderstruck"));
        assert!(same_title("Thunderstruck (Remastered)", "Thunderstruck"));
        assert!(!same_title("Back In Black", "Thunderstruck"));
        assert!(!same_title("", ""));
    }

    #[test]
    fn playlist_names_come_from_the_file() {
        assert_eq!(playlist_name(Path::new("/x/Road trip.csv")), "Road trip");
        assert_eq!(playlist_name(Path::new("/x/.csv")), ".csv");
        assert_eq!(playlist_name(Path::new("/")), "Imported playlist");
    }
}
