//! Reading a library export from another service (Spotify, Apple Music) and judging search
//! results against it. Pure: the app reads the files, does the searching and creates the
//! playlists.
//!
//! Everything read here is hostile. Sizes, counts and nesting are bounded, text goes through
//! the caller's `clean` (the app's `clean_line`), every pass is linear in the file, and a file
//! that is too big or not what it claims is refused with a sentence.

use std::collections::HashMap;

use serde::Deserialize;

/// Largest file read at all.
pub const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
pub const MAX_PLAYLISTS: usize = 200;
/// Tracks over all playlists (a track in two playlists counts twice).
pub const MAX_TRACKS: usize = 20_000;
/// XML nodes read from an Apple `Library.xml` (a 20,000-track library is about 600,000).
pub const MAX_XML_NODES: u32 = 2_000_000;
/// Longest title or artist kept.
const MAX_FIELD_CHARS: usize = 300;
/// Playlist names are cut here.
const MAX_NAME_CHARS: usize = 100;
/// Duration agreement for a confident match.
pub const DURATION_TOLERANCE_SECS: f32 = 3.0;

/// One track as the other service names it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Want {
    pub title: String,
    pub artist: String,
    pub isrc: Option<String>,
    pub duration_secs: Option<f32>,
}

impl Want {
    /// What two copies of the same wanted track share, so each is searched once.
    pub fn key(&self) -> String {
        match &self.isrc {
            Some(i) => format!("i:{i}"),
            None => format!(
                "t:{}|{}|{}",
                fold(&self.title),
                fold(&self.artist),
                self.duration_secs.map_or(0, |d| d.round() as i64)
            ),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportPlaylist {
    pub name: String,
    pub tracks: Vec<Want>,
}

/// Where an export came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    SpotifyLibrary,
    SpotifyPlaylists,
    Csv,
    AppleLibraryXml,
}

type Clean<'a> = &'a dyn Fn(&str) -> String;

fn field(s: &str, clean: Clean) -> String {
    let cut: String = s.chars().take(MAX_FIELD_CHARS * 4).collect();
    clean(&cut).chars().take(MAX_FIELD_CHARS).collect()
}

fn name(s: &str, clean: Clean, fallback: &str) -> String {
    let n: String = clean(&s.chars().take(MAX_NAME_CHARS * 4).collect::<String>())
        .chars()
        .take(MAX_NAME_CHARS)
        .collect();
    if n.is_empty() {
        fallback.to_string()
    } else {
        n
    }
}

/// Valid ISRCs are 12 letters and digits (hyphens as printed are dropped).
fn isrc(s: &str) -> Option<String> {
    let t: String = s
        .chars()
        .filter(|c| *c != '-' && !c.is_whitespace())
        .collect();
    (t.len() == 12 && t.chars().all(|c| c.is_ascii_alphanumeric())).then(|| t.to_ascii_uppercase())
}

/// A caps check shared by every reader: refuse, with a sentence, past the limits.
pub fn check_caps(playlists: &[ImportPlaylist]) -> Result<(), String> {
    if playlists.len() > MAX_PLAYLISTS {
        return Err(format!(
            "That export holds more than {MAX_PLAYLISTS} playlists. Slate imports at most {MAX_PLAYLISTS} at a time; split the export and import it in parts."
        ));
    }
    let tracks: usize = playlists.iter().map(|p| p.tracks.len()).sum();
    if tracks > MAX_TRACKS {
        return Err(format!(
            "That export holds more than {MAX_TRACKS} tracks. Slate imports at most {MAX_TRACKS} at a time; split the export and import it in parts."
        ));
    }
    Ok(())
}

// ---------- Spotify account export (JSON) ----------

#[derive(Deserialize, Default)]
struct SpLibrary {
    #[serde(default)]
    tracks: Vec<SpLibTrack>,
}

#[derive(Deserialize, Default)]
struct SpLibTrack {
    #[serde(default)]
    artist: Option<String>,
    #[serde(default)]
    track: Option<String>,
}

#[derive(Deserialize, Default)]
struct SpPlaylists {
    #[serde(default)]
    playlists: Vec<SpPlaylist>,
}

#[derive(Deserialize, Default)]
struct SpPlaylist {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    items: Vec<SpItem>,
}

#[derive(Deserialize, Default)]
struct SpItem {
    #[serde(default)]
    track: Option<SpTrack>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SpTrack {
    #[serde(default)]
    track_name: Option<String>,
    #[serde(default)]
    artist_name: Option<String>,
}

/// What kind of Spotify file this is, from its top-level keys.
pub fn spotify_kind(text: &str) -> Option<Kind> {
    #[derive(Deserialize)]
    struct Probe {
        #[serde(default)]
        tracks: Option<serde::de::IgnoredAny>,
        #[serde(default)]
        playlists: Option<serde::de::IgnoredAny>,
    }
    let p: Probe = serde_json::from_str(text).ok()?;
    if p.playlists.is_some() {
        Some(Kind::SpotifyPlaylists)
    } else if p.tracks.is_some() {
        Some(Kind::SpotifyLibrary)
    } else {
        None
    }
}

/// `YourLibrary.json`: the liked songs, as one playlist named `liked_name`.
pub fn parse_spotify_library(
    text: &str,
    clean: Clean,
    liked_name: &str,
) -> Result<Vec<ImportPlaylist>, String> {
    let lib: SpLibrary = serde_json::from_str(text)
        .map_err(|_| "That is not a Spotify YourLibrary.json (its JSON could not be read).")?;
    let tracks: Vec<Want> = lib
        .tracks
        .iter()
        .filter_map(|t| {
            let title = field(t.track.as_deref()?, clean);
            (!title.is_empty()).then(|| Want {
                title,
                artist: field(t.artist.as_deref().unwrap_or(""), clean),
                ..Want::default()
            })
        })
        .collect();
    let out = if tracks.is_empty() {
        Vec::new()
    } else {
        vec![ImportPlaylist {
            name: name(liked_name, clean, "Liked Songs"),
            tracks,
        }]
    };
    check_caps(&out)?;
    Ok(out)
}

/// `Playlist1.json` and its numbered siblings.
pub fn parse_spotify_playlists(text: &str, clean: Clean) -> Result<Vec<ImportPlaylist>, String> {
    let p: SpPlaylists = serde_json::from_str(text)
        .map_err(|_| "That is not a Spotify Playlist file (its JSON could not be read).")?;
    if p.playlists.len() > MAX_PLAYLISTS {
        return check_caps(&vec![ImportPlaylist::default(); p.playlists.len()])
            .map(|()| Vec::new());
    }
    let out: Vec<ImportPlaylist> = p
        .playlists
        .iter()
        .map(|pl| ImportPlaylist {
            name: name(pl.name.as_deref().unwrap_or(""), clean, "Imported playlist"),
            tracks: pl
                .items
                .iter()
                .filter_map(|i| {
                    let t = i.track.as_ref()?;
                    let title = field(t.track_name.as_deref()?, clean);
                    (!title.is_empty()).then(|| Want {
                        title,
                        artist: field(t.artist_name.as_deref().unwrap_or(""), clean),
                        ..Want::default()
                    })
                })
                .collect(),
        })
        .filter(|p| !p.tracks.is_empty())
        .collect();
    check_caps(&out)?;
    Ok(out)
}

// ---------- CSV: Exportify and Apple's privacy export ----------

/// RFC 4180 records; quoted fields hold commas, doubled quotes and line breaks. Stops with an
/// error once `max_records` have been read, so a hostile file cannot make a huge list.
fn csv_records(text: &str, max_records: usize) -> Result<Vec<Vec<String>>, String> {
    let mut records: Vec<Vec<String>> = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    let push = |record: &mut Vec<String>, records: &mut Vec<Vec<String>>| -> Result<(), String> {
        let r = std::mem::take(record);
        if r.iter().any(|f| !f.trim().is_empty()) {
            records.push(r);
        }
        if records.len() > max_records {
            Err("That file has more rows than Slate imports at once.".to_string())
        } else {
            Ok(())
        }
    };
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
                push(&mut record, &mut records)?;
            }
            _ => field.push(c),
        }
    }
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        push(&mut record, &mut records)?;
    }
    Ok(records)
}

/// Milliseconds or seconds, by the column's own name and size: Exportify and Apple write ms,
/// some Apple files seconds.
fn duration_of(raw: &str, ms_column: bool) -> Option<f32> {
    let v: f64 = raw.trim().parse().ok()?;
    if !v.is_finite() || v <= 0.0 {
        return None;
    }
    let secs = if ms_column || v > 20_000.0 {
        v / 1000.0
    } else {
        v
    };
    (secs < 24.0 * 3600.0).then_some(secs as f32)
}

fn cell(row: &[String], col: Option<usize>) -> &str {
    col.and_then(|i| row.get(i))
        .map(String::as_str)
        .unwrap_or("")
}

/// An Exportify playlist CSV or Apple's privacy-export CSV. Columns are found by name; a
/// "Playlist Name" column splits the rows into playlists, otherwise they are one playlist
/// named `file_name`.
pub fn parse_csv(text: &str, clean: Clean, file_name: &str) -> Result<Vec<ImportPlaylist>, String> {
    let text = text.trim_start_matches('\u{feff}');
    let records = csv_records(text, MAX_TRACKS + 1)?;
    let Some((header, rows)) = records.split_first() else {
        return Err("That CSV is empty.".into());
    };
    let names: Vec<String> = header.iter().map(|h| h.trim().to_lowercase()).collect();
    let find = |wanted: &[&str]| names.iter().position(|n| wanted.contains(&n.as_str()));
    let title = find(&[
        "track name",
        "title",
        "track title",
        "name",
        "song name",
        "track",
    ]);
    let Some(title) = title else {
        return Err("That CSV has no track title column (Track Name or Title), so it is not a Spotify or Apple Music export.".into());
    };
    let artist = find(&[
        "artist name(s)",
        "artist name",
        "artist",
        "artists",
        "artist names",
        "performer",
    ]);
    let isrc_col = find(&["isrc"]);
    let dur_col = find(&[
        "duration (ms)",
        "track duration (ms)",
        "track duration",
        "duration",
        "total time",
        "duration ms",
        "length",
    ]);
    let ms = dur_col.is_some_and(|i| {
        names
            .get(i)
            .is_some_and(|n| n.contains("ms") || n == "total time")
    });
    let list_col = find(&["playlist name", "playlist", "playlist title"]);

    let mut order: Vec<String> = Vec::new();
    let mut lists: HashMap<String, Vec<Want>> = HashMap::new();
    for row in rows {
        let t = field(cell(row, Some(title)), clean);
        if t.is_empty() {
            continue;
        }
        // "A, B" or "A; B": the first artist searches best.
        let a = field(
            cell(row, artist).split([';', ',']).next().unwrap_or(""),
            clean,
        );
        let want = Want {
            title: t,
            artist: a,
            isrc: isrc(cell(row, isrc_col)),
            duration_secs: duration_of(cell(row, dur_col), ms),
        };
        let list = match list_col {
            Some(_) => name(cell(row, list_col), clean, file_name),
            None => name(file_name, clean, "Imported playlist"),
        };
        if !lists.contains_key(&list) {
            if order.len() >= MAX_PLAYLISTS {
                return check_caps(&vec![ImportPlaylist::default(); MAX_PLAYLISTS + 1])
                    .map(|()| Vec::new());
            }
            order.push(list.clone());
        }
        lists.entry(list).or_default().push(want);
    }
    let out: Vec<ImportPlaylist> = order
        .into_iter()
        .filter_map(|n| {
            lists
                .remove(&n)
                .map(|tracks| ImportPlaylist { name: n, tracks })
        })
        .collect();
    if out.is_empty() {
        return Err("That CSV holds no tracks.".into());
    }
    check_caps(&out)?;
    Ok(out)
}

// ---------- Apple Music Library.xml (a property list) ----------

type Pairs<'a, 'i> = Vec<(String, roxmltree::Node<'a, 'i>)>;

/// The key/value pairs of a plist `<dict>` (key, value, key, value …).
fn dict<'a, 'i>(node: roxmltree::Node<'a, 'i>) -> Pairs<'a, 'i> {
    let mut out = Vec::new();
    let mut key: Option<String> = None;
    for c in node.children().filter(|c| c.is_element()) {
        if c.tag_name().name() == "key" {
            key = Some(c.text().unwrap_or("").to_string());
        } else if let Some(k) = key.take() {
            out.push((k, c));
        }
    }
    out
}

fn get<'a, 'i>(d: &Pairs<'a, 'i>, k: &str) -> Option<roxmltree::Node<'a, 'i>> {
    d.iter().find(|(n, _)| n == k).map(|(_, v)| *v)
}

/// Apple's `Library.xml`. The DTD at the top is skipped, never fetched: roxmltree reads no
/// external entity and expands internal ones within its own limits, and the node count is
/// capped.
pub fn parse_apple_library(text: &str, clean: Clean) -> Result<Vec<ImportPlaylist>, String> {
    let opts = roxmltree::ParsingOptions {
        allow_dtd: true,
        nodes_limit: MAX_XML_NODES,
    };
    let doc = roxmltree::Document::parse_with_options(text, opts).map_err(|e| match e {
        roxmltree::Error::NodesLimitReached => {
            "That Library.xml is larger than Slate reads (too many entries).".to_string()
        }
        _ => "That is not an Apple Music Library.xml (the XML could not be read).".to_string(),
    })?;
    let plist = doc.root_element();
    if plist.tag_name().name() != "plist" {
        return Err(
            "That XML is not a property list, so it is not an Apple Music Library.xml.".into(),
        );
    }
    let Some(top) = plist
        .children()
        .find(|n| n.is_element() && n.tag_name().name() == "dict")
    else {
        return Err("That property list has no library in it.".into());
    };
    let top = dict(top);
    let text_of = |n: roxmltree::Node<'_, '_>| n.text().unwrap_or("").to_string();

    // Track id → track, then playlists refer to ids. One pass each, no search per item.
    let mut tracks: HashMap<String, Want> = HashMap::new();
    if let Some(t) = get(&top, "Tracks") {
        for (id, node) in dict(t) {
            let d = dict(node);
            let title = get(&d, "Name")
                .map(|n| field(&text_of(n), clean))
                .unwrap_or_default();
            if title.is_empty() {
                continue;
            }
            let artist = get(&d, "Artist")
                .map(|n| field(&text_of(n), clean))
                .unwrap_or_default();
            let duration_secs = get(&d, "Total Time")
                .and_then(|n| text_of(n).trim().parse::<f64>().ok())
                .filter(|ms| ms.is_finite() && *ms > 0.0 && *ms < 24.0 * 3600.0 * 1000.0)
                .map(|ms| (ms / 1000.0) as f32);
            tracks.insert(
                id,
                Want {
                    title,
                    artist,
                    isrc: None,
                    duration_secs,
                },
            );
        }
    }
    let mut out: Vec<ImportPlaylist> = Vec::new();
    if let Some(list) = get(&top, "Playlists") {
        for p in list
            .children()
            .filter(|c| c.is_element() && c.tag_name().name() == "dict")
        {
            let d = dict(p);
            // Built-in lists (the whole library, Music, Podcasts…) and folders are not playlists.
            let flag = |k: &str| get(&d, k).is_some_and(|n| n.tag_name().name() == "true");
            if flag("Master") || flag("Folder") || get(&d, "Distinguished Kind").is_some() {
                continue;
            }
            let pname = get(&d, "Name").map(text_of).unwrap_or_default();
            let mut items = Vec::new();
            if let Some(arr) = get(&d, "Playlist Items") {
                for it in arr
                    .children()
                    .filter(|c| c.is_element() && c.tag_name().name() == "dict")
                {
                    let id = get(&dict(it), "Track ID").map(text_of).unwrap_or_default();
                    if let Some(w) = tracks.get(id.trim()) {
                        items.push(w.clone());
                    }
                }
            }
            if !items.is_empty() {
                if out.len() >= MAX_PLAYLISTS {
                    return check_caps(&vec![ImportPlaylist::default(); MAX_PLAYLISTS + 1])
                        .map(|()| Vec::new());
                }
                out.push(ImportPlaylist {
                    name: name(&pname, clean, "Imported playlist"),
                    tracks: items,
                });
                // The cap is checked as it grows, so a hostile file cannot build a huge list first.
                let so_far: usize = out.iter().map(|p| p.tracks.len()).sum();
                if so_far > MAX_TRACKS {
                    return check_caps(&out).map(|()| Vec::new());
                }
            }
        }
    }
    if out.is_empty() {
        return Err("That Library.xml has no playlists with tracks in them.".into());
    }
    check_caps(&out)?;
    Ok(out)
}

// ---------- judging search results ----------

/// A search result, reduced to what the judgement needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub id: u64,
    pub title: String,
    pub artists: Vec<String>,
    pub duration_secs: f32,
    pub isrc: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Same recording: ISRC equal, or title, artist and duration (±3 s) agree.
    Matched(u64),
    /// Probably it, but something differs (another length, a looser title): to be reviewed.
    Uncertain(u64),
    NotFound,
}

/// Case, accents and everything in brackets removed ("(Remastered 2011)", "[Live]"), and
/// "feat." tails cut; letters and digits only.
pub fn fold(s: &str) -> String {
    let mut depth = 0u32;
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth > 0 => {}
            c if c.is_alphanumeric() => out.push_str(&crate::list_filter::fold(&c.to_string())),
            _ => out.push(' '),
        }
    }
    let mut words: Vec<&str> = out.split_whitespace().collect();
    if let Some(i) = words
        .iter()
        .position(|w| matches!(*w, "feat" | "ft" | "featuring"))
    {
        words.truncate(i);
    }
    words.join(" ")
}

/// Whether `want_title` names `cand_title`: equal once folded.
fn title_equal(a: &str, b: &str) -> bool {
    let (a, b) = (fold(a), fold(b));
    !a.is_empty() && a == b
}

/// Looser: one folded title begins with the other ("Africa" / "Africa - 2014 Remaster").
fn title_close(a: &str, b: &str) -> bool {
    let (a, b) = (fold(a), fold(b));
    !a.is_empty() && !b.is_empty() && (a.starts_with(&b) || b.starts_with(&a))
}

fn artist_ok(want: &str, cand: &[String]) -> bool {
    let w = fold(want);
    // No artist in the export (a bare title list): nothing to disagree with.
    w.is_empty()
        || cand.iter().any(|a| {
            let a = fold(a);
            !a.is_empty() && (a == w || w.contains(&a) || a.contains(&w))
        })
}

/// Chooses among `cands` (a search's results, best first) for `want`. Linear in `cands`.
pub fn judge(want: &Want, cands: &[Candidate]) -> Verdict {
    if let Some(w) = &want.isrc {
        if let Some(c) = cands
            .iter()
            .find(|c| c.isrc.as_deref().and_then(isrc).as_ref() == Some(w))
        {
            return Verdict::Matched(c.id);
        }
    }
    let dur_ok = |c: &Candidate| match want.duration_secs {
        Some(d) => (d - c.duration_secs).abs() <= DURATION_TOLERANCE_SECS,
        None => true,
    };
    // Confident: the exact (folded) title, the artist, and the length when it is known.
    // Without a length (Spotify's account export has none) only an exact title is confident,
    // so an unknown length cannot tip a loose title into a match.
    if let Some(c) = cands.iter().find(|c| {
        title_equal(&want.title, &c.title) && artist_ok(&want.artist, &c.artists) && dur_ok(c)
    }) {
        return Verdict::Matched(c.id);
    }
    // Review: the artist agrees and the title is the same or close, but the length is off,
    // or the title is only a prefix.
    if let Some(c) = cands
        .iter()
        .find(|c| title_close(&want.title, &c.title) && artist_ok(&want.artist, &c.artists))
    {
        return Verdict::Uncertain(c.id);
    }
    Verdict::NotFound
}

/// Review counts for one playlist.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub matched: usize,
    pub uncertain: usize,
    pub not_found: usize,
}

pub fn count(verdicts: &[Verdict]) -> Counts {
    let mut c = Counts::default();
    for v in verdicts {
        match v {
            Verdict::Matched(_) => c.matched += 1,
            Verdict::Uncertain(_) => c.uncertain += 1,
            Verdict::NotFound => c.not_found += 1,
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn cand(id: u64, title: &str, artist: &str, secs: f32, isrc: Option<&str>) -> Candidate {
        Candidate {
            id,
            title: title.into(),
            artists: vec![artist.into()],
            duration_secs: secs,
            isrc: isrc.map(Into::into),
        }
    }

    fn want(title: &str, artist: &str, secs: Option<f32>, isrc: Option<&str>) -> Want {
        Want {
            title: title.into(),
            artist: artist.into(),
            duration_secs: secs,
            isrc: isrc.and_then(isrc_fn),
        }
    }

    fn isrc_fn(s: &str) -> Option<String> {
        isrc(s)
    }

    #[test]
    fn spotify_library_and_playlists_are_read() {
        let lib = r#"{"tracks":[{"artist":"Toto","album":"IV","track":"Africa","uri":"spotify:track:1"},{"artist":"","track":""}],"albums":[{"x":1}]}"#;
        assert_eq!(spotify_kind(lib), Some(Kind::SpotifyLibrary));
        let l = parse_spotify_library(lib, &clean, "Liked Songs").unwrap();
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].name, "Liked Songs");
        assert_eq!(
            l[0].tracks,
            vec![Want {
                title: "Africa".into(),
                artist: "Toto".into(),
                ..Want::default()
            }]
        );

        let pl = r#"{"playlists":[{"name":"Road trip","lastModifiedDate":"2024-01-01","items":[
            {"track":{"trackName":"Africa","artistName":"Toto","albumName":"IV","trackUri":"u"},"episode":null,"localTrack":null},
            {"track":null,"episode":{"name":"a podcast"}},
            {"localTrack":{"x":1}}]},{"name":"Empty","items":[]}]}"#;
        assert_eq!(spotify_kind(pl), Some(Kind::SpotifyPlaylists));
        let p = parse_spotify_playlists(pl, &clean).unwrap();
        assert_eq!(p.len(), 1, "an empty playlist is dropped");
        assert_eq!(p[0].name, "Road trip");
        assert_eq!(
            p[0].tracks.len(),
            1,
            "episodes and local files are not tracks"
        );
        assert_eq!(spotify_kind("{\"other\":1}"), None);
        assert_eq!(spotify_kind("not json"), None);
    }

    #[test]
    fn exportify_csv_has_isrc_duration_and_artists() {
        let csv = "Track URI,Track Name,Album Name,Artist Name(s),Release Date,Duration (ms),ISRC\n\
                   spotify:track:1,\"Africa, Remastered\",IV,\"Toto, Someone\",1982,295000,USSM18200001\n\
                   spotify:track:2,Hold the Line,Toto,Toto,1978,,\n";
        let p = parse_csv(csv, &clean, "My list").unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].name, "My list");
        let t = &p[0].tracks;
        assert_eq!(
            (t[0].title.as_str(), t[0].artist.as_str()),
            ("Africa, Remastered", "Toto")
        );
        assert_eq!(t[0].isrc.as_deref(), Some("USSM18200001"));
        assert_eq!(t[0].duration_secs, Some(295.0));
        assert_eq!(t[1].duration_secs, None);
        assert_eq!(t[1].isrc, None);
    }

    #[test]
    fn apple_privacy_csv_splits_playlists_by_name() {
        let csv = "Playlist Name,Title,Artist,Track Duration\nRun,Song A,X,200000\nRun,Song B,Y,180000\nChill,Song C,Z,210000\n";
        let p = parse_csv(csv, &clean, "file").unwrap();
        assert_eq!(
            p.iter()
                .map(|p| (p.name.as_str(), p.tracks.len()))
                .collect::<Vec<_>>(),
            vec![("Run", 2), ("Chill", 1)]
        );
        assert_eq!(
            p[0].tracks[0].duration_secs,
            Some(200.0),
            "a big number is milliseconds"
        );
    }

    #[test]
    fn csv_without_a_title_column_is_refused_with_a_sentence() {
        let e = parse_csv("a,b\n1,2\n", &clean, "f").unwrap_err();
        assert!(e.contains("no track title column"), "{e}");
        assert!(parse_csv("", &clean, "f").is_err());
        assert!(parse_csv("Title,Artist\n", &clean, "f")
            .unwrap_err()
            .contains("no tracks"));
    }

    const APPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Tracks</key><dict>
 <key>101</key><dict><key>Track ID</key><integer>101</integer><key>Name</key><string>Africa</string><key>Artist</key><string>Toto</string><key>Total Time</key><integer>295000</integer></dict>
 <key>102</key><dict><key>Track ID</key><integer>102</integer><key>Name</key><string>Rock &amp; Roll</string><key>Artist</key><string>Y</string></dict>
</dict>
<key>Playlists</key><array>
 <dict><key>Name</key><string>Library</string><key>Master</key><true/><key>Playlist Items</key><array><dict><key>Track ID</key><integer>101</integer></dict></array></dict>
 <dict><key>Name</key><string>Mine</string><key>Playlist Items</key><array><dict><key>Track ID</key><integer>101</integer></dict><dict><key>Track ID</key><integer>102</integer></dict><dict><key>Track ID</key><integer>999</integer></dict></array></dict>
 <dict><key>Name</key><string>Music</string><key>Distinguished Kind</key><integer>4</integer><key>Playlist Items</key><array><dict><key>Track ID</key><integer>101</integer></dict></array></dict>
</array></dict></plist>"#;

    #[test]
    fn apple_library_xml_gives_user_playlists_only() {
        let p = parse_apple_library(APPLE, &clean).unwrap();
        assert_eq!(
            p.len(),
            1,
            "the master list and the built-in Music list are not playlists"
        );
        assert_eq!(p[0].name, "Mine");
        assert_eq!(p[0].tracks.len(), 2, "an id with no track is skipped");
        assert_eq!(
            p[0].tracks[0],
            Want {
                title: "Africa".into(),
                artist: "Toto".into(),
                isrc: None,
                duration_secs: Some(295.0)
            }
        );
        assert_eq!(
            p[0].tracks[1].title, "Rock & Roll",
            "predefined entities are read"
        );
    }

    #[test]
    fn apple_xml_never_fetches_and_expands_nothing_dangerous() {
        // An external entity must neither be read from disk nor over the network.
        let xxe = r#"<?xml version="1.0"?>
<!DOCTYPE plist [ <!ENTITY x SYSTEM "file:///etc/passwd"> ]>
<plist><dict><key>Tracks</key><dict><key>1</key><dict><key>Name</key><string>&x;</string></dict></dict>
<key>Playlists</key><array><dict><key>Name</key><string>p</string><key>Playlist Items</key><array><dict><key>Track ID</key><integer>1</integer></dict></array></dict></array></dict></plist>"#;
        match parse_apple_library(xxe, &clean) {
            Err(_) => {}
            Ok(p) => assert!(
                !p.iter()
                    .flat_map(|p| &p.tracks)
                    .any(|t| t.title.contains("root:")),
                "the file was read"
            ),
        }
        // A billion-laughs expansion is refused, quickly.
        let mut bomb =
            String::from("<?xml version=\"1.0\"?><!DOCTYPE plist [<!ENTITY a0 \"aaaaaaaaaa\">");
        for i in 1..10 {
            bomb.push_str(&format!(
                "<!ENTITY a{i} \"{}\">",
                format!("&a{};", i - 1).repeat(10)
            ));
        }
        bomb.push_str("]><plist><dict><key>k</key><string>&a9;</string></dict></plist>");
        let t = std::time::Instant::now();
        let _ = parse_apple_library(&bomb, &clean);
        assert!(
            t.elapsed().as_secs() < 3,
            "entity expansion must be bounded"
        );
        assert!(parse_apple_library("<notplist/>", &clean)
            .unwrap_err()
            .contains("not a property list"));
        assert!(parse_apple_library("garbage", &clean).is_err());
    }

    #[test]
    fn too_many_nodes_is_refused_with_a_sentence() {
        let mut x = String::from("<plist><dict><key>Tracks</key><dict>");
        for i in 0..(MAX_XML_NODES / 2) {
            x.push_str(&format!("<key>{i}</key><dict></dict>"));
        }
        x.push_str("</dict></dict></plist>");
        let e = parse_apple_library(&x, &clean).unwrap_err();
        assert!(e.contains("larger than Slate reads"), "{e}");
    }

    #[test]
    fn caps_refuse_with_a_sentence() {
        let many = |n: usize, per: usize| -> Vec<ImportPlaylist> {
            (0..n)
                .map(|i| ImportPlaylist {
                    name: format!("p{i}"),
                    tracks: vec![Want::default(); per],
                })
                .collect()
        };
        assert!(check_caps(&many(200, 100)).is_ok());
        assert!(check_caps(&many(201, 1))
            .unwrap_err()
            .contains("more than 200 playlists"));
        assert!(check_caps(&many(2, 10_001))
            .unwrap_err()
            .contains("more than 20000 tracks"));
        // A CSV over the row cap stops while reading, not after.
        let mut csv = String::from("Title,Artist\n");
        for i in 0..(MAX_TRACKS + 5) {
            csv.push_str(&format!("t{i},a\n"));
        }
        assert!(parse_csv(&csv, &clean, "f").is_err());
    }

    #[test]
    fn a_large_export_is_read_in_linear_time() {
        let mut items = String::new();
        for i in 0..MAX_TRACKS {
            items.push_str(&format!(
                r#"{{"track":{{"trackName":"Song {i}","artistName":"A {i}"}}}},"#
            ));
        }
        items.pop();
        let json = format!(r#"{{"playlists":[{{"name":"Big","items":[{items}]}}]}}"#);
        let t = std::time::Instant::now();
        let p = parse_spotify_playlists(&json, &clean).unwrap();
        assert_eq!(p[0].tracks.len(), MAX_TRACKS);
        assert!(t.elapsed().as_secs() < 5, "{:?}", t.elapsed());
    }

    #[test]
    fn hostile_text_is_cleaned_and_cut() {
        let strip = |s: &str| {
            s.chars()
                .filter(|c| !c.is_control() && *c != '\u{202e}')
                .collect::<String>()
        };
        let json = serde_json::json!({"playlists":[{"name":"n".repeat(1000),"items":[{"track":{
            "trackName":"A\u{7}b\u{202e}c","artistName":"x".repeat(5000)}}]}]})
        .to_string();
        let p = parse_spotify_playlists(&json, &strip).unwrap();
        assert_eq!(p[0].name.chars().count(), MAX_NAME_CHARS);
        assert_eq!(p[0].tracks[0].title, "Abc");
        assert_eq!(p[0].tracks[0].artist.chars().count(), MAX_FIELD_CHARS);
    }

    #[test]
    fn deeply_nested_json_is_refused_not_a_crash() {
        let deep = format!(
            "{{\"playlists\":{}{}}}",
            "[".repeat(100_000),
            "]".repeat(100_000)
        );
        assert!(parse_spotify_playlists(&deep, &clean).is_err());
    }

    #[test]
    fn isrc_decides_first() {
        let w = want("Africa", "Toto", Some(295.0), Some("US-SM1-82000-01"));
        // Wrong title, wrong length, same ISRC: it is the recording.
        let cands = [
            cand(1, "Africa (Live)", "Toto", 400.0, None),
            cand(2, "Other name", "X", 10.0, Some("ussm18200001")),
        ];
        assert_eq!(judge(&w, &cands), Verdict::Matched(2));
    }

    #[test]
    fn title_artist_and_duration_within_three_seconds_match() {
        let w = want("Africa", "Toto", Some(295.0), None);
        assert_eq!(
            judge(&w, &[cand(1, "Africa", "Toto", 297.9, None)]),
            Verdict::Matched(1)
        );
        assert_eq!(
            judge(&w, &[cand(1, "Africa", "Toto", 298.5, None)]),
            Verdict::Uncertain(1),
            "3.5 s off is for review"
        );
        assert_eq!(
            judge(
                &w,
                &[cand(1, "Africa (2014 Remaster)", "TOTO", 296.0, None)]
            ),
            Verdict::Matched(1),
            "brackets and case are ignored"
        );
        assert_eq!(
            judge(&w, &[cand(1, "Africa", "Someone Else", 295.0, None)]),
            Verdict::NotFound
        );
        assert_eq!(
            judge(&w, &[cand(1, "Rosanna", "Toto", 295.0, None)]),
            Verdict::NotFound
        );
        assert_eq!(judge(&w, &[]), Verdict::NotFound);
    }

    #[test]
    fn an_unknown_length_matches_only_an_exact_title() {
        let w = want("Africa", "Toto", None, None);
        assert_eq!(
            judge(&w, &[cand(1, "Africa", "Toto", 295.0, None)]),
            Verdict::Matched(1)
        );
        assert_eq!(
            judge(
                &w,
                &[cand(1, "Africa - Live in Paris", "Toto", 400.0, None)]
            ),
            Verdict::Uncertain(1)
        );
    }

    #[test]
    fn feat_tails_and_accents_fold_away() {
        assert_eq!(fold("Beyoncé (feat. Jay-Z) [Live]"), "beyonce");
        assert_eq!(fold("Song feat. Someone"), "song");
        assert_eq!(fold("  A  &  B "), "a b");
        assert_eq!(fold("(((("), "");
        assert!(title_equal("Déjà Vu", "deja vu"));
    }

    #[test]
    fn an_exported_artist_list_is_matched_against_any_credited_artist() {
        let w = want("Song", "B", None, None);
        let c = Candidate {
            id: 1,
            title: "Song".into(),
            artists: vec!["A".into(), "B".into()],
            duration_secs: 1.0,
            isrc: None,
        };
        assert_eq!(judge(&w, &[c]), Verdict::Matched(1));
    }

    #[test]
    fn keys_group_the_same_track_and_counts_add_up() {
        let a = want("Africa", "Toto", Some(295.2), None);
        let b = want("Africa (Remastered)", "TOTO", Some(294.8), None);
        assert_eq!(a.key(), b.key(), "search once");
        let i1 = want("x", "y", None, Some("USSM18200001"));
        assert_eq!(i1.key(), "i:USSM18200001");
        let c = count(&[
            Verdict::Matched(1),
            Verdict::Matched(2),
            Verdict::Uncertain(3),
            Verdict::NotFound,
        ]);
        assert_eq!(
            c,
            Counts {
                matched: 2,
                uncertain: 1,
                not_found: 1
            }
        );
    }

    #[test]
    fn isrc_text_is_validated() {
        assert_eq!(isrc("US-SM1-82000-01").as_deref(), Some("USSM18200001"));
        assert_eq!(isrc("short"), None);
        assert_eq!(isrc("US SM1 82000 0!"), None);
        assert_eq!(isrc(""), None);
    }
}
