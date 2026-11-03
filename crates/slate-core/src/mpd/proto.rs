//! MPD text protocol pieces that need no player: tokenizing, search filters, response lines.

use super::Song;

pub const ACK_NOT_LIST: u32 = 1;
pub const ACK_ARG: u32 = 2;
pub const ACK_PASSWORD: u32 = 3;
pub const ACK_PERMISSION: u32 = 4;
pub const ACK_UNKNOWN: u32 = 5;
pub const ACK_NO_EXIST: u32 = 50;
pub const ACK_SYSTEM: u32 = 52;

/// Longest line (one command) a client may send.
pub const MAX_LINE: usize = 64 * 1024;
/// Most commands in one command list, and its total size.
pub const MAX_LIST_COMMANDS: usize = 1000;
pub const MAX_LIST_BYTES: usize = 1024 * 1024;

/// Splits a command line into words. Words are separated by spaces or tabs; a double-quoted
/// word may hold spaces and `\"` / `\\` escapes. `None` for an unterminated quote.
pub fn tokenize(line: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c == ' ' || c == '\t' {
            chars.next();
            continue;
        }
        let mut word = String::new();
        if c == '"' {
            chars.next();
            loop {
                match chars.next() {
                    None => return None,
                    Some('"') => break,
                    Some('\\') => word.push(chars.next()?),
                    Some(ch) => word.push(ch),
                }
            }
        } else {
            while let Some(&ch) = chars.peek() {
                if ch == ' ' || ch == '\t' {
                    break;
                }
                word.push(ch);
                chars.next();
            }
        }
        out.push(word);
    }
    Some(out)
}

/// `ACK [code@index] {command} message`.
pub fn ack(code: u32, index: usize, command: &str, msg: &str) -> String {
    let msg: String = msg.chars().filter(|c| !c.is_control()).collect();
    format!("ACK [{code}@{index}] {{{command}}} {msg}\n")
}

/// A value safe to put after `key: ` on a line.
pub fn clean(v: &str) -> String {
    v.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

pub fn kv(out: &mut String, key: &str, value: impl std::fmt::Display) {
    use std::fmt::Write;
    let _ = writeln!(out, "{key}: {}", clean(&value.to_string()));
}

/// The song block of `currentsong`, `playlistinfo`, `search`…; `pos` adds `Pos` and `Id`.
pub fn song_block(out: &mut String, s: &Song, pos: Option<usize>) {
    kv(out, "file", s.uri());
    if !s.artist.is_empty() {
        kv(out, "Artist", &s.artist);
    }
    let album_artist = if s.album_artist.is_empty() {
        &s.artist
    } else {
        &s.album_artist
    };
    if !album_artist.is_empty() {
        kv(out, "AlbumArtist", album_artist);
    }
    kv(out, "Title", &s.title);
    if !s.album.is_empty() {
        kv(out, "Album", &s.album);
    }
    if let Some(t) = s.track {
        kv(out, "Track", t);
    }
    if !s.date.is_empty() {
        kv(out, "Date", &s.date);
    }
    kv(out, "Time", s.duration.round() as u64);
    kv(out, "duration", format!("{:.3}", s.duration));
    if let Some(p) = pos {
        kv(out, "Pos", p);
        kv(out, "Id", s.id);
    }
}

/// The track id in `tidal://track/123`, `tidal:track:123`, `tidal:123` or a bare number.
pub fn track_id_of(uri: &str) -> Option<u64> {
    let uri = uri.trim();
    let tail = uri.rsplit(['/', ':']).next()?;
    if uri.contains(':') && !uri.starts_with("tidal") {
        return None;
    }
    tail.parse().ok()
}

/// A position range: `N` or `START:END` (end exclusive, either side optional).
pub fn parse_range(arg: &str, len: usize) -> Option<std::ops::Range<usize>> {
    match arg.split_once(':') {
        None => {
            let n: usize = arg.parse().ok()?;
            (n < len).then(|| n..n + 1)
        }
        Some((a, b)) => {
            let start = if a.is_empty() { 0 } else { a.parse().ok()? };
            let end = if b.is_empty() {
                len
            } else {
                b.parse::<usize>().ok()?.min(len)
            };
            (start <= end && start <= len).then_some(start..end)
        }
    }
}

/// Search terms: (tag, value) pairs, lowercased tag.
pub type Terms = Vec<(String, String)>;

/// Terms of `search`/`find` arguments: old style `TYPE WHAT [TYPE WHAT …]`, or one filter
/// expression `(Artist == 'x')` / `((any contains 'a') AND (Album == 'b'))`. Negations and
/// regular expressions are treated as plain matches. Trailing `sort`/`window`/`group`
/// options are ignored.
pub fn parse_terms(args: &[String]) -> Terms {
    let Some(first) = args.first() else {
        return Vec::new();
    };
    if first.starts_with('(') {
        return parse_expression(first);
    }
    let mut terms = Vec::new();
    let mut i = 0;
    while i + 1 < args.len() {
        let tag = args[i].to_lowercase();
        if matches!(tag.as_str(), "sort" | "window" | "group") {
            break;
        }
        terms.push((tag, args[i + 1].clone()));
        i += 2;
    }
    terms
}

fn parse_expression(s: &str) -> Terms {
    let chars: Vec<char> = s.chars().collect();
    let mut terms = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '(' {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        let tag_start = j;
        while j < chars.len() && !chars[j].is_whitespace() && chars[j] != '(' && chars[j] != ')' {
            j += 1;
        }
        let tag: String = chars[tag_start..j].iter().collect();
        if tag.is_empty() || tag.starts_with('!') {
            i += 1;
            continue;
        }
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        while j < chars.len() && !chars[j].is_whitespace() {
            j += 1;
        }
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        if j < chars.len() && (chars[j] == '\'' || chars[j] == '"') {
            let quote = chars[j];
            j += 1;
            let mut value = String::new();
            while j < chars.len() && chars[j] != quote {
                if chars[j] == '\\' && j + 1 < chars.len() {
                    j += 1;
                }
                value.push(chars[j]);
                j += 1;
            }
            terms.push((tag.to_lowercase(), value));
            i = j;
        } else {
            i += 1;
        }
    }
    terms
}

/// The words to ask TIDAL for: every term's value once.
pub fn query_text(terms: &Terms) -> String {
    let mut seen: Vec<&str> = Vec::new();
    for (_, v) in terms {
        let v = v.trim();
        if !v.is_empty() && !seen.contains(&v) {
            seen.push(v);
        }
    }
    seen.join(" ")
}

fn fold(s: &str) -> String {
    s.to_lowercase()
}

/// Whether `song` satisfies every term. `exact` (`find`) wants equality, `search` a substring.
/// Tags Slate does not carry (genre, composer…) match everything.
pub fn matches(song: &Song, terms: &Terms, exact: bool) -> bool {
    terms.iter().all(|(tag, want)| {
        let want = fold(want);
        let has = |field: &str| {
            let f = fold(field);
            if exact {
                f == want
            } else {
                f.contains(&want)
            }
        };
        match tag.as_str() {
            "artist" => has(&song.artist),
            "albumartist" => has(if song.album_artist.is_empty() {
                &song.artist
            } else {
                &song.album_artist
            }),
            "album" => has(&song.album),
            "title" => has(&song.title),
            "file" => has(&song.uri()),
            "any" => has(&song.title) || has(&song.artist) || has(&song.album) || has(&song.uri()),
            _ => true,
        }
    })
}

/// Constant-time equality for secrets of any length.
pub fn secret_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        diff |= usize::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0));
    }
    diff == 0
}

/// `image/jpeg`, `image/png` or `image/webp` by the first bytes.
pub fn image_mime(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        "image/png"
    } else if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else {
        "image/jpeg"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }

    fn song(title: &str, artist: &str, album: &str) -> Song {
        Song {
            id: 1,
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
            ..Song::default()
        }
    }

    #[test]
    fn words_quotes_and_escapes() {
        assert_eq!(tokenize("play 3").expect("words"), s(&["play", "3"]));
        assert_eq!(
            tokenize(r#"find artist "Sigur Rós" title "a \"b\" c\\d""#).unwrap(),
            s(&["find", "artist", "Sigur Rós", "title", r#"a "b" c\d"#])
        );
        assert_eq!(tokenize("   ping\t").expect("words"), s(&["ping"]));
        assert_eq!(tokenize(r#"add """#).expect("words"), s(&["add", ""]));
        assert!(tokenize(r#"add "open"#).is_none());
        assert!(tokenize(r#"add "x\"#).is_none());
        assert_eq!(tokenize("").expect("words"), Vec::<String>::new());
    }

    #[test]
    fn errors_are_one_clean_line() {
        assert_eq!(
            ack(5, 0, "foo", "unknown command \"foo\"\nOK"),
            "ACK [5@0] {foo} unknown command \"foo\"OK\n"
        );
    }

    #[test]
    fn values_never_break_the_line_format() {
        let mut out = String::new();
        kv(&mut out, "Title", "a\nOK\nb");
        assert_eq!(out, "Title: a OK b\n");
    }

    #[test]
    fn song_uris_round_trip() {
        let s = Song {
            id: 440427618,
            ..Song::default()
        };
        assert_eq!(track_id_of(&s.uri()), Some(440427618));
        assert_eq!(track_id_of("tidal:track:7"), Some(7));
        assert_eq!(track_id_of("tidal:8"), Some(8));
        assert_eq!(track_id_of("9"), Some(9));
        assert_eq!(track_id_of("file:///music/a.flac"), None);
        assert_eq!(track_id_of("tidal://track/x"), None);
        assert_eq!(track_id_of(""), None);
    }

    #[test]
    fn ranges() {
        assert_eq!(parse_range("2", 5), Some(2..3));
        assert_eq!(parse_range("5", 5), None);
        assert_eq!(parse_range("1:3", 5), Some(1..3));
        assert_eq!(parse_range("3:", 5), Some(3..5));
        assert_eq!(parse_range(":2", 5), Some(0..2));
        assert_eq!(parse_range("1:99", 5), Some(1..5));
        assert_eq!(parse_range("4:2", 5), None);
        assert_eq!(parse_range("x", 5), None);
    }

    #[test]
    fn old_style_and_expression_terms() {
        assert_eq!(
            parse_terms(&s(&["any", "Daft Punk", "album", "Discovery"])),
            vec![
                ("any".into(), "Daft Punk".into()),
                ("album".into(), "Discovery".into())
            ]
        );
        assert_eq!(
            parse_terms(&s(&["(Artist == 'Daft Punk')"])),
            vec![("artist".into(), "Daft Punk".into())]
        );
        assert_eq!(
            parse_terms(&s(&[
                r#"((any contains "it\"s") AND (Album == 'Discovery'))"#,
                "sort",
                "Title"
            ])),
            vec![
                ("any".into(), "it\"s".into()),
                ("album".into(), "Discovery".into())
            ]
        );
        assert!(parse_terms(&s(&[])).is_empty());
        assert_eq!(
            parse_terms(&s(&["title", "x", "sort", "Artist"])),
            vec![("title".into(), "x".into())]
        );
    }

    #[test]
    fn query_text_is_each_value_once() {
        let t = vec![
            ("artist".to_string(), "Daft Punk".to_string()),
            ("any".to_string(), "Daft Punk".to_string()),
            ("album".to_string(), "Discovery".to_string()),
        ];
        assert_eq!(query_text(&t), "Daft Punk Discovery");
    }

    #[test]
    fn search_is_a_substring_find_is_equality() {
        let a = song("One More Time", "Daft Punk", "Discovery");
        let t = vec![("artist".to_string(), "daft".to_string())];
        assert!(matches(&a, &t, false));
        assert!(!matches(&a, &t, true));
        let t = vec![("artist".to_string(), "DAFT PUNK".to_string())];
        assert!(matches(&a, &t, true));
        let t = vec![("any".to_string(), "discov".to_string())];
        assert!(matches(&a, &t, false));
        let t = vec![("genre".to_string(), "house".to_string())];
        assert!(matches(&a, &t, true), "unknown tags match everything");
        let t = vec![
            ("artist".to_string(), "daft".to_string()),
            ("album".to_string(), "homework".to_string()),
        ];
        assert!(!matches(&a, &t, false));
    }

    #[test]
    fn secrets_compare_in_full() {
        assert!(secret_eq("hunter2", "hunter2"));
        assert!(!secret_eq("hunter2", "hunter3"));
        assert!(!secret_eq("hunter", "hunter2"));
        assert!(!secret_eq("", "x"));
        assert!(secret_eq("", ""));
    }

    #[test]
    fn mime_by_magic() {
        assert_eq!(image_mime(&[0x89, b'P', b'N', b'G', 1]), "image/png");
        assert_eq!(image_mime(b"RIFF\0\0\0\0WEBPVP8 "), "image/webp");
        assert_eq!(image_mime(&[0xFF, 0xD8, 0xFF]), "image/jpeg");
    }

    #[test]
    fn song_block_has_position_and_id() {
        let mut out = String::new();
        let s = Song {
            id: 7,
            title: "T".into(),
            artist: "A".into(),
            duration: 200.4,
            ..Song::default()
        };
        song_block(&mut out, &s, Some(3));
        assert!(out.starts_with("file: tidal://track/7\n"));
        assert!(out.contains("Time: 200\n") && out.contains("duration: 200.400\n"));
        assert!(out.ends_with("Pos: 3\nId: 7\n"));
    }
}
