//! "Other versions" of a track or album: the title with version noise stripped, a label for
//! what kind of version a title is, and grouping of search hits around the original.

use crate::list_filter::fold;

/// Words that mark a bracketed or dashed suffix as version noise.
const NOISE: &[&str] = &[
    "remaster",
    "remastered",
    "remix",
    "live",
    "version",
    "edit",
    "mix",
    "mono",
    "stereo",
    "acoustic",
    "demo",
    "instrumental",
    "radio",
    "single",
    "album",
    "deluxe",
    "expanded",
    "anniversary",
    "bonus",
    "explicit",
    "clean",
    "extended",
    "original",
    "session",
    "sessions",
    "unplugged",
    "reprise",
    "feat",
    "featuring",
    "ft",
    "with",
    "atmos",
    "dolby",
    "digital",
    "edition",
    "restored",
    "reissue",
    "rerecorded",
    "re-recorded",
    "taylor's",
    "from",
    "mix",
    "cut",
    "club",
    "dub",
    "karaoke",
    "cover",
    "tribute",
];

fn is_year(w: &str) -> bool {
    w.len() == 4 && w.chars().all(|c| c.is_ascii_digit())
}

fn noisy(segment: &str) -> bool {
    let f = fold(segment);
    let words: Vec<&str> = f
        .split(|c: char| !c.is_alphanumeric() && c != '\'' && c != '-')
        .filter(|w| !w.is_empty())
        .collect();
    !words.is_empty()
        && words
            .iter()
            .any(|w| NOISE.contains(w) || w.starts_with("remaster") || w.starts_with("live"))
        && words.iter().all(|w| {
            NOISE.contains(w)
                || is_year(w)
                || w.starts_with("remaster")
                || w.starts_with("live")
                || w.len() <= 3
                || w.chars().all(|c| c.is_ascii_digit() || c == '-')
        })
        || (words
            .first()
            .is_some_and(|w| matches!(*w, "feat" | "featuring" | "ft" | "with")))
}

/// A " - …" suffix is version information as soon as it names a kind of version
/// ("- Live at Wembley", "- 2011 Remaster"), whatever else it says.
fn noisy_suffix(segment: &str) -> bool {
    fold(segment)
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        // "with" and "from" also start ordinary titles ("Song - With You").
        .filter(|w| !matches!(*w, "with" | "from"))
        .any(|w| NOISE.contains(&w) || w.starts_with("remaster") || w.starts_with("live"))
}

/// The names in an artist credit: "Beyoncé, Jay-Z" and "Beyoncé & Jay-Z" → beyonce, jay-z.
fn credited(s: &str) -> Vec<String> {
    let mut f = format!(" {} ", fold(s));
    for sep in [
        " feat. ",
        " feat ",
        " ft. ",
        " featuring ",
        " with ",
        " x ",
        " and ",
    ] {
        f = f.replace(sep, ",");
    }
    f.split([',', '&', ';', '/'])
        .map(|n| n.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|n| !n.is_empty())
        .collect()
}

/// Whether two artist credits share an artist ("Beyoncé" and "Beyoncé, Jay-Z"; not "Ann" and
/// "Annie" or "Ann Peebles"). An empty credit matches nothing.
fn same_artist(a: &str, b: &str) -> bool {
    let theirs = credited(b);
    credited(a).iter().any(|n| theirs.contains(n))
}

/// "Halo (Remastered 2011) - Live (feat. X)" → "halo": the folded title of the song itself.
pub fn base_title(title: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    let mut seg = String::new();
    for c in title.chars() {
        match c {
            '(' | '[' => {
                if depth == 0 {
                    seg.clear();
                }
                depth += 1;
            }
            ')' | ']' if depth > 0 => {
                depth -= 1;
                if depth == 0 && !noisy(&seg) {
                    out.push(' ');
                    out.push_str(&seg);
                    out.push(' ');
                }
            }
            _ if depth > 0 => seg.push(c),
            _ => out.push(c),
        }
    }
    // A trailing " - something" that is noise ("- Remastered 2011", "- Live at …").
    if let Some(i) = out.rfind(" - ") {
        if noisy_suffix(&out[i + 3..]) {
            out.truncate(i);
        }
    }
    fold(out.trim())
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// What kind of version a title describes, for the label in the list.
pub fn kind_label(title: &str, version: Option<&str>, explicit: bool, atmos: bool) -> String {
    let all = fold(&format!("{title} {}", version.unwrap_or("")));
    let mut parts: Vec<&str> = Vec::new();
    let has = |w: &str| all.split(|c: char| !c.is_alphanumeric()).any(|x| x == w);
    if has("live") || all.contains("unplugged") {
        parts.push("Live");
    }
    if all.contains("remaster") {
        parts.push("Remaster");
    }
    if has("remix") {
        parts.push("Remix");
    }
    if has("acoustic") {
        parts.push("Acoustic");
    }
    if has("instrumental") {
        parts.push("Instrumental");
    }
    if has("demo") {
        parts.push("Demo");
    }
    if has("mono") {
        parts.push("Mono");
    }
    if has("deluxe") || has("expanded") {
        parts.push("Deluxe");
    }
    if has("edit") || has("radio") {
        parts.push("Edit");
    }
    if atmos {
        parts.push("Dolby Atmos");
    }
    if parts.is_empty() {
        parts.push(if explicit { "Explicit" } else { "Original" });
    } else if explicit {
        parts.push("Explicit");
    }
    parts.join(" · ")
}

/// One candidate for "other versions".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: u64,
    pub title: String,
    pub version: Option<String>,
    pub artist: String,
    pub year: Option<i32>,
    pub explicit: bool,
    pub atmos: bool,
    /// 0 none, 1 Lossless, 2 Hi-Res.
    pub quality: i32,
    pub duration_secs: u32,
}

/// Candidates that are versions of (`title`, `artist`) but not the track `exclude_id` itself,
/// best first: same artist, base title equal; ordered by year then label.
pub fn others(title: &str, artist: &str, exclude_id: u64, hits: Vec<Candidate>) -> Vec<Candidate> {
    let base = base_title(title);
    if base.is_empty() {
        return Vec::new();
    }
    let mut v: Vec<Candidate> = hits
        .into_iter()
        .filter(|c| c.id != exclude_id)
        .filter(|c| base_title(&c.title) == base)
        .filter(|c| same_artist(&c.artist, artist))
        .collect();
    v.sort_by_key(|c| (c.year.unwrap_or(i32::MAX), c.duration_secs, c.id));
    let mut seen = std::collections::HashSet::new();
    v.retain(|c| seen.insert(c.id));
    v
}

/// The same for albums (title, artist, id).
pub fn other_albums(
    title: &str,
    artist: &str,
    exclude_id: u64,
    hits: Vec<Candidate>,
) -> Vec<Candidate> {
    others(title, artist, exclude_id, hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_is_stripped_from_titles() {
        for (input, want) in [
            ("Halo (Remastered 2011)", "halo"),
            ("Halo - Remastered 2011", "halo"),
            ("Halo - Live at Wembley", "halo"),
            ("Halo [Live]", "halo"),
            ("Halo (feat. Jay-Z)", "halo"),
            ("Halo (Radio Edit) (feat. X)", "halo"),
            ("Halo - Single Version", "halo"),
            ("Beyoncé – Halo", "beyonce – halo"),
            ("Heroes", "heroes"),
            ("Heroes (2017 Remaster)", "heroes"),
        ] {
            assert_eq!(base_title(input), want, "{input}");
        }
    }

    #[test]
    fn meaningful_brackets_are_kept() {
        assert_eq!(
            base_title("(I Can't Get No) Satisfaction"),
            "i can't get no satisfaction"
        );
        assert_eq!(base_title("Song (Part 2)"), "song part 2");
        assert_eq!(base_title("Hurt - 2009"), "hurt - 2009");
    }

    #[test]
    fn labels() {
        assert_eq!(kind_label("Halo", None, false, false), "Original");
        assert_eq!(kind_label("Halo", None, true, false), "Explicit");
        assert_eq!(kind_label("Halo - Live", None, false, false), "Live");
        assert_eq!(
            kind_label("Halo (2011 Remaster)", None, true, false),
            "Remaster · Explicit"
        );
        assert_eq!(
            kind_label("Halo", Some("Dolby Atmos"), false, true),
            "Dolby Atmos"
        );
        assert_eq!(kind_label("Halo", Some("Radio Edit"), false, false), "Edit");
    }

    fn cand(id: u64, title: &str, artist: &str, year: i32) -> Candidate {
        Candidate {
            id,
            title: title.into(),
            version: None,
            artist: artist.into(),
            year: Some(year),
            explicit: false,
            atmos: false,
            quality: 1,
            duration_secs: 200,
        }
    }

    #[test]
    fn others_are_the_same_song_by_the_same_artist() {
        let hits = vec![
            cand(1, "Halo", "Beyoncé", 2008),
            cand(2, "Halo - Live", "Beyoncé", 2010),
            cand(3, "Halo (Remastered 2011)", "Beyoncé", 2011),
            cand(4, "Halo", "Texas", 1997),
            cand(5, "Halo Effect", "Beyoncé", 2008),
            cand(2, "Halo - Live", "Beyoncé", 2010),
        ];
        let ids: Vec<u64> = others("Halo", "Beyoncé", 1, hits)
            .iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(ids, vec![2, 3]);
    }

    #[test]
    fn unrelated_artists_and_titles_do_not_match() {
        let hits = vec![
            cand(1, "Halo", "", 2008),
            cand(2, "Halo", "Annie", 2008),
            cand(3, "Halo - With You", "Ann", 2008),
            cand(4, "Halo", "Ann Peebles", 2008),
        ];
        assert!(same_artist("Beyoncé & Jay-Z", "beyonce"));
        assert!(same_artist("X feat. Beyoncé", "Beyoncé"));
        let ids: Vec<u64> = others("Halo", "Ann", 9, hits)
            .iter()
            .map(|c| c.id)
            .collect();
        assert!(ids.is_empty(), "{ids:?}");
        assert!(others("(Live)", "Ann", 9, vec![cand(5, "[Live]", "Ann", 1)]).is_empty());
        assert_eq!(base_title("Halo - With You"), "halo - with you");
    }

    #[test]
    fn featured_artists_still_match() {
        let hits = vec![cand(7, "Halo", "Beyoncé, Jay-Z", 2009)];
        assert_eq!(others("Halo", "Beyoncé", 1, hits).len(), 1);
    }
}
