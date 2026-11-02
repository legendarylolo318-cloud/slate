//! A few facts about the genre the DJ is playing, for the DJ page. Static and small: no
//! network, nothing loaded until the page asks. The DJ's query ("deep house 2000s") is
//! matched against keywords; the most specific genre wins.

/// One genre: the words that name it in a query, how to show it, and its facts.
pub struct Genre {
    pub name: &'static str,
    keywords: &'static [&'static str],
    pub facts: &'static [&'static str],
}

const GENRES: &[Genre] = &[
    Genre {
        name: "Deep house",
        keywords: &["deep house"],
        facts: &[
            "Deep house grew out of Chicago and New Jersey in the mid-1980s, slowing house down and adding jazz and soul chords.",
            "Larry Heard's 1986 \"Can You Feel It\" (as Mr. Fingers) is often named as its starting point.",
        ],
    },
    Genre {
        name: "Afro house",
        keywords: &["afro house", "afrohouse", "afro-house"],
        facts: &[
            "Afro house took shape in South Africa in the 2000s, mixing house with Southern African rhythms and vocals.",
            "South Africa is one of the largest house music markets in the world.",
        ],
    },
    Genre {
        name: "Tech house",
        keywords: &["tech house", "tech-house"],
        facts: &["Tech house sits between techno and house: techno's stripped-back sound with house's groove."],
    },
    Genre {
        name: "House",
        keywords: &["house"],
        facts: &[
            "House is named after the Warehouse, the Chicago club where Frankie Knuckles played in the late 1970s and early 1980s.",
            "Most house runs at 118-130 BPM with a kick drum on every beat (\"four on the floor\").",
        ],
    },
    Genre {
        name: "Techno",
        keywords: &["techno"],
        facts: &[
            "Techno began in Detroit in the mid-1980s with Juan Atkins, Derrick May and Kevin Saunderson, the \"Belleville Three\".",
            "Berlin became its second home after the Wall fell in 1989.",
        ],
    },
    Genre {
        name: "Drum & bass",
        keywords: &["drum and bass", "drum & bass", "drum n bass", "dnb", "d&b", "jungle"],
        facts: &[
            "Drum & bass came out of the UK's jungle scene in the early 1990s, usually at 160-180 BPM.",
            "Much of it is built on chopped-up breakbeats, above all the \"Amen break\" from a 1969 Winstons B-side.",
        ],
    },
    Genre {
        name: "UK garage",
        keywords: &["uk garage", "garage", "2-step", "2 step", "speed garage"],
        facts: &["UK garage grew from US garage house in mid-1990s London; its 2-step form drops the kick from every beat."],
    },
    Genre {
        name: "Disco",
        keywords: &["disco", "nu-disco", "nu disco"],
        facts: &[
            "Disco peaked in the late 1970s; its DJs pioneered beat-matching two records to keep a floor dancing.",
            "The 12-inch single was popularised by disco, giving longer, louder mixes for clubs.",
        ],
    },
    Genre {
        name: "Ambient",
        keywords: &["ambient", "chillout", "chill out", "downtempo"],
        facts: &["Brian Eno coined \"ambient music\" with Ambient 1: Music for Airports (1978), music \"as ignorable as it is interesting\"."],
    },
    Genre {
        name: "Lo-fi",
        keywords: &["lo-fi", "lofi", "lo fi", "chillhop"],
        facts: &["Lo-fi hip hop's dusty sound draws on J Dilla and Nujabes; 24-hour \"beats to study to\" streams made it a study staple."],
    },
    Genre {
        name: "Jazz",
        keywords: &["jazz", "bebop", "swing"],
        facts: &[
            "Jazz grew from New Orleans in the early 1900s, from blues, ragtime and brass band music.",
            "Miles Davis's Kind of Blue (1959) is among the best-selling jazz records ever.",
        ],
    },
    Genre {
        name: "Hip hop",
        keywords: &["hip hop", "hip-hop", "rap", "boom bap", "trap"],
        facts: &["Hip hop is usually dated to a 1973 party in the Bronx where DJ Kool Herc looped the breaks of funk records."],
    },
    Genre {
        name: "Trance",
        keywords: &["trance"],
        facts: &["Trance came out of Germany in the early 1990s, known for long build-ups and breakdowns at around 130-140 BPM."],
    },
    Genre {
        name: "Dubstep",
        keywords: &["dubstep"],
        facts: &["Dubstep started in South London in the early 2000s, built around half-time rhythms and heavy sub-bass at ~140 BPM."],
    },
    Genre {
        name: "Funk",
        keywords: &["funk"],
        facts: &["Funk puts the groove on \"the one\": the first beat of the bar, a principle James Brown made famous in the 1960s."],
    },
    Genre {
        name: "Soul",
        keywords: &["soul", "motown", "r&b", "rnb"],
        facts: &["Motown, founded in Detroit in 1959 by Berry Gordy, had over 100 top-ten US hits in its first decade."],
    },
    Genre {
        name: "Reggae",
        keywords: &["reggae", "dub", "dancehall"],
        facts: &["Reggae came out of Jamaica in the late 1960s; dub producers like King Tubby turned the mixing desk into an instrument."],
    },
    Genre {
        name: "Rock",
        keywords: &["rock", "punk", "grunge", "indie"],
        facts: &["Rock and roll grew from blues, country and R&B in the US in the late 1940s and early 1950s."],
    },
    Genre {
        name: "Metal",
        keywords: &["metal"],
        facts: &["Black Sabbath's 1970 debut is often named the first heavy metal album."],
    },
    Genre {
        name: "Classical",
        keywords: &["classical", "orchestral", "baroque", "piano"],
        facts: &["A modern symphony orchestra has around 80-100 players in four sections: strings, woodwind, brass and percussion."],
    },
];

/// The genre a DJ query is about, if it names one we know. When several match
/// ("deep house" also contains "house") the longest keyword wins.
pub fn lookup(query: &str) -> Option<&'static Genre> {
    let q = query.to_lowercase();
    GENRES
        .iter()
        .filter_map(|g| {
            g.keywords
                .iter()
                .filter(|k| contains_word(&q, k))
                .map(|k| k.len())
                .max()
                .map(|len| (len, g))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, g)| g)
}

/// `needle` in `hay` on word boundaries ("rap" is not in "trap" or "scrap").
fn contains_word(hay: &str, needle: &str) -> bool {
    let boundary = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
    hay.match_indices(needle).any(|(i, _)| {
        boundary(hay[..i].chars().next_back()) && boundary(hay[i + needle.len()..].chars().next())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_most_specific_genre_wins() {
        assert_eq!(
            lookup("Deep House 2000s classics").map(|g| g.name),
            Some("Deep house")
        );
        assert_eq!(lookup("soulful house").map(|g| g.name), Some("House"));
        assert_eq!(
            lookup("afro-house vocals").map(|g| g.name),
            Some("Afro house")
        );
        assert_eq!(
            lookup("liquid drum and bass").map(|g| g.name),
            Some("Drum & bass")
        );
    }

    #[test]
    fn words_match_whole_words_only() {
        assert_eq!(lookup("trap").map(|g| g.name), Some("Hip hop"));
        assert!(lookup("scrapbook").is_none());
        assert_eq!(lookup("dub techno").map(|g| g.name), Some("Techno"));
    }

    #[test]
    fn an_unknown_genre_has_no_facts() {
        assert!(lookup("polka from mars").is_none());
        assert!(lookup("").is_none());
    }

    #[test]
    fn every_genre_has_facts_and_keywords() {
        for g in GENRES {
            assert!(!g.facts.is_empty() && !g.keywords.is_empty(), "{}", g.name);
        }
    }
}
