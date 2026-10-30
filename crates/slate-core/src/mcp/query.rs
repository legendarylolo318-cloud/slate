//! `play_query`: turn "the new Kendrick album" or "that Daft Punk song from Tron" into one
//! best match with a confidence, plus a few alternatives. Pure scoring; the search itself goes
//! through the normal client.

use crate::list_filter::fold;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Track,
    Album,
    Artist,
    Playlist,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Track => "track",
            Self::Album => "album",
            Self::Artist => "artist",
            Self::Playlist => "playlist",
        }
    }
}

/// What the words of a query say beyond the name.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Intent {
    /// Search text with filler and kind words removed.
    pub text: String,
    pub wants: Option<Kind>,
    /// "new", "latest", "newest": prefer recent releases.
    pub recent: bool,
}

const FILLER: &[&str] = &[
    "the", "a", "an", "that", "this", "some", "play", "put", "on", "me", "please", "by", "from",
    "of", "for", "to", "i", "want", "hear", "listen", "my", "song", "tune", "track",
];

pub fn parse(query: &str) -> Intent {
    let mut intent = Intent::default();
    let mut words: Vec<String> = Vec::new();
    for w in fold(query).split_whitespace() {
        match w {
            "new" | "latest" | "newest" | "recent" => intent.recent = true,
            "album" | "albums" | "lp" => intent.wants = Some(Kind::Album),
            "playlist" | "playlists" => intent.wants = Some(Kind::Playlist),
            "artist" | "band" | "discography" => intent.wants = Some(Kind::Artist),
            "song" | "tune" | "track" => intent.wants = intent.wants.or(Some(Kind::Track)),
            _ => {}
        }
        if !FILLER.contains(&w)
            && !matches!(
                w,
                "new"
                    | "latest"
                    | "newest"
                    | "recent"
                    | "album"
                    | "albums"
                    | "lp"
                    | "playlist"
                    | "playlists"
                    | "artist"
                    | "band"
                    | "discography"
            )
        {
            words.push(w.to_string());
        }
    }
    intent.text = words.join(" ");
    intent
}

/// One search hit, reduced to what scoring needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub kind: Kind,
    pub id: String,
    pub title: String,
    /// Primary artist (or owner for a playlist).
    pub artist: String,
    /// Further text that can match: album title, etc.
    pub extra: String,
    /// 0–100, TIDAL popularity if known.
    pub popularity: u32,
    /// Release year, if known.
    pub year: Option<i32>,
}

fn words(s: &str) -> Vec<String> {
    fold(s)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// Fraction of `needles` that appear as whole words in `hay`.
fn overlap(needles: &[String], hay: &[String]) -> f64 {
    if needles.is_empty() {
        return 0.0;
    }
    let hit = needles.iter().filter(|n| hay.contains(n)).count();
    hit as f64 / needles.len() as f64
}

/// Similarity of two short strings by character bigrams (Dice), forgiving of typos.
fn fuzzy(a: &str, b: &str) -> f64 {
    let (a, b) = (fold(a), fold(b));
    let grams = |s: &str| -> Vec<(char, char)> {
        let c: Vec<char> = s.chars().collect();
        c.windows(2).map(|w| (w[0], w[1])).collect()
    };
    let (ga, gb) = (grams(&a), grams(&b));
    if ga.is_empty() || gb.is_empty() {
        return f64::from(u8::from(a == b && !a.is_empty()));
    }
    let mut used = vec![false; gb.len()];
    let mut shared = 0usize;
    for g in &ga {
        if let Some(i) = gb.iter().enumerate().position(|(i, h)| !used[i] && h == g) {
            used[i] = true;
            shared += 1;
        }
    }
    2.0 * shared as f64 / (ga.len() + gb.len()) as f64
}

/// 0–1 score of how well `hit` answers the query. `this_year` drives "new".
pub fn score(intent: &Intent, hit: &Hit, this_year: i32) -> f64 {
    let q = words(&intent.text);
    if q.is_empty() {
        return 0.0;
    }
    let title = words(&hit.title);
    let artist = words(&hit.artist);
    let extra = words(&hit.extra);
    let all: Vec<String> = title.iter().chain(&artist).chain(&extra).cloned().collect();

    // How much of the query is explained by the hit at all.
    let coverage = overlap(&q, &all);
    // Exact title wins; otherwise the best fuzzy title/title+artist.
    let exact = fold(&hit.title) == fold(&intent.text);
    let title_sim = fuzzy(&hit.title, &intent.text);
    let combo_sim = fuzzy(&format!("{} {}", hit.artist, hit.title), &intent.text);
    let name = if exact {
        1.0
    } else {
        title_sim.max(combo_sim).max(overlap(&q, &title) * 0.9)
    };
    let mut s = 0.55 * coverage + 0.35 * name + 0.10 * (f64::from(hit.popularity.min(100)) / 100.0);
    if let Some(k) = intent.wants {
        s += if hit.kind == k { 0.08 } else { -0.12 };
    }
    if intent.recent {
        if let Some(y) = hit.year {
            s += match this_year - y {
                ..=0 => 0.12,
                1 => 0.08,
                2 => 0.03,
                _ => -0.05,
            };
        }
    }
    s.clamp(0.0, 1.0)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Ranked {
    pub hit: Hit,
    pub score: f64,
}

/// Hits best first. Ties keep search order.
pub fn rank(intent: &Intent, hits: Vec<Hit>, this_year: i32) -> Vec<Ranked> {
    let mut v: Vec<Ranked> = hits
        .into_iter()
        .map(|hit| Ranked {
            score: score(intent, &hit, this_year),
            hit,
        })
        .collect();
    v.sort_by(|a, b| b.score.total_cmp(&a.score));
    v
}

/// Confidence in the top result: its score, reduced when a runner-up is almost as good.
pub fn confidence(ranked: &[Ranked]) -> f64 {
    let Some(top) = ranked.first() else {
        return 0.0;
    };
    let gap = ranked.get(1).map_or(1.0, |second| top.score - second.score);
    let ambiguity = if gap < 0.03 {
        0.75
    } else if gap < 0.08 {
        0.9
    } else {
        1.0
    };
    (top.score * ambiguity).clamp(0.0, 1.0)
}

pub const PLAY_THRESHOLD: f64 = 0.7;

pub fn should_play(confidence: f64, play_anyway: bool) -> bool {
    play_anyway || confidence >= PLAY_THRESHOLD
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(
        kind: Kind,
        id: &str,
        title: &str,
        artist: &str,
        extra: &str,
        pop: u32,
        year: i32,
    ) -> Hit {
        Hit {
            kind,
            id: id.into(),
            title: title.into(),
            artist: artist.into(),
            extra: extra.into(),
            popularity: pop,
            year: Some(year),
        }
    }

    #[test]
    fn parsing_pulls_out_kind_and_recency() {
        let i = parse("the new Kendrick album");
        assert_eq!(i.text, "kendrick");
        assert_eq!(i.wants, Some(Kind::Album));
        assert!(i.recent);
        let i = parse("that Daft Punk song from Tron");
        assert_eq!(i.text, "daft punk tron");
        assert_eq!(i.wants, Some(Kind::Track));
        assert!(!i.recent);
        let i = parse("lofi playlist");
        assert_eq!((i.text.as_str(), i.wants), ("lofi", Some(Kind::Playlist)));
        assert_eq!(parse("  ").text, "");
    }

    #[test]
    fn an_exact_track_beats_a_cover() {
        let i = parse("Halo Beyoncé");
        let r = rank(
            &i,
            vec![
                hit(
                    Kind::Track,
                    "2",
                    "Halo (Cover)",
                    "Some Band",
                    "Covers",
                    20,
                    2015,
                ),
                hit(
                    Kind::Track,
                    "1",
                    "Halo",
                    "Beyoncé",
                    "I Am... Sasha Fierce",
                    85,
                    2008,
                ),
            ],
            2026,
        );
        assert_eq!(r[0].hit.id, "1");
        assert!(confidence(&r) >= PLAY_THRESHOLD, "{}", confidence(&r));
    }

    #[test]
    fn new_prefers_recent_releases_of_the_artist() {
        let i = parse("the new Kendrick album");
        let r = rank(
            &i,
            vec![
                hit(
                    Kind::Album,
                    "old",
                    "good kid, m.A.A.d city",
                    "Kendrick Lamar",
                    "",
                    80,
                    2012,
                ),
                hit(Kind::Album, "new", "GNX", "Kendrick Lamar", "", 80, 2025),
                hit(Kind::Track, "t", "Kendrick Anthem", "Someone", "", 10, 2025),
            ],
            2025,
        );
        assert_eq!(r[0].hit.id, "new");
    }

    #[test]
    fn typos_still_match() {
        let i = parse("daft punk discovry");
        let r = rank(
            &i,
            vec![hit(
                Kind::Album,
                "d",
                "Discovery",
                "Daft Punk",
                "",
                70,
                2001,
            )],
            2025,
        );
        assert!(r[0].score > 0.5, "{}", r[0].score);
    }

    #[test]
    fn nonsense_is_not_confident() {
        let i = parse("zzzz qqqq");
        let r = rank(
            &i,
            vec![hit(Kind::Track, "1", "Halo", "Beyoncé", "", 90, 2008)],
            2025,
        );
        assert!(confidence(&r) < PLAY_THRESHOLD);
        assert_eq!(confidence(&[]), 0.0);
        assert_eq!(
            score(
                &parse(""),
                &hit(Kind::Track, "1", "x", "y", "", 0, 2000),
                2025
            ),
            0.0
        );
    }

    #[test]
    fn near_ties_lower_the_confidence() {
        let i = parse("halo");
        let a = hit(Kind::Track, "1", "Halo", "Beyoncé", "", 80, 2008);
        let b = hit(Kind::Track, "2", "Halo", "Texas", "", 80, 2008);
        let tied = rank(&i, vec![a.clone(), b], 2025);
        let alone = rank(&i, vec![a], 2025);
        assert!(confidence(&tied) < confidence(&alone));
    }

    #[test]
    fn play_gate() {
        assert!(should_play(0.7, false));
        assert!(!should_play(0.69, false));
        assert!(should_play(0.1, true));
    }
}
