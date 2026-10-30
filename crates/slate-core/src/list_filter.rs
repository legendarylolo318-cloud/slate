//! Filtering a loaded track list: case- and diacritic-insensitive text match plus the
//! quality chips (All / Lossless / Hi-Res / Atmos). Pure; the UI keeps the result as a
//! list of source indices.

/// Lowercases and strips the diacritics of Latin text: "Beyoncé" → "beyonce".
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match base_letter(c) {
            Some(b) => out.push_str(b),
            None => out.extend(c.to_lowercase()),
        }
    }
    out
}

fn base_letter(c: char) -> Option<&'static str> {
    const TABLE: &[(&str, &str)] = &[
        ("àáâãäåāăąǎ", "a"),
        ("ÀÁÂÃÄÅĀĂĄǍ", "a"),
        ("çćĉċč", "c"),
        ("ÇĆĈĊČ", "c"),
        ("ďđ", "d"),
        ("ĎĐ", "d"),
        ("èéêëēĕėęě", "e"),
        ("ÈÉÊËĒĔĖĘĚ", "e"),
        ("ĝğġģ", "g"),
        ("ĜĞĠĢ", "g"),
        ("ĥħ", "h"),
        ("ĤĦ", "h"),
        ("ìíîïĩīĭįıǐ", "i"),
        ("ÌÍÎÏĨĪĬĮİǏ", "i"),
        ("ĵ", "j"),
        ("Ĵ", "j"),
        ("ķ", "k"),
        ("Ķ", "k"),
        ("ĺļľŀł", "l"),
        ("ĹĻĽĿŁ", "l"),
        ("ñńņňŉ", "n"),
        ("ÑŃŅŇ", "n"),
        ("òóôõöøōŏőǒ", "o"),
        ("ÒÓÔÕÖØŌŎŐǑ", "o"),
        ("ŕŗř", "r"),
        ("ŔŖŘ", "r"),
        ("śŝşšș", "s"),
        ("ŚŜŞŠȘ", "s"),
        ("ţťŧț", "t"),
        ("ŢŤŦȚ", "t"),
        ("ùúûüũūŭůűųǔ", "u"),
        ("ÙÚÛÜŨŪŬŮŰŲǓ", "u"),
        ("ŵ", "w"),
        ("Ŵ", "w"),
        ("ýÿŷ", "y"),
        ("ÝŸŶ", "y"),
        ("źżž", "z"),
        ("ŹŻŽ", "z"),
    ];
    match c {
        'ß' => return Some("ss"),
        'æ' | 'Æ' => return Some("ae"),
        'œ' | 'Œ' => return Some("oe"),
        _ => {}
    }
    if c.is_ascii() {
        return None;
    }
    TABLE
        .iter()
        .find(|(set, _)| set.contains(c))
        .map(|(_, base)| *base)
}

/// The quality chips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QualityFilter {
    #[default]
    All,
    Lossless,
    HiRes,
    Atmos,
}

impl QualityFilter {
    pub fn from_index(i: i32) -> Self {
        match i {
            1 => Self::Lossless,
            2 => Self::HiRes,
            3 => Self::Atmos,
            _ => Self::All,
        }
    }

    /// `tier` as in `quality::Badges`: 0 none, 1 Lossless, 2 Hi-Res.
    pub fn accepts(self, tier: i32, atmos: bool) -> bool {
        match self {
            Self::All => true,
            Self::Lossless => tier >= 1,
            Self::HiRes => tier >= 2,
            Self::Atmos => atmos,
        }
    }
}

/// Every whitespace-separated word of `query` (already folded) must occur in the title,
/// the artists or the album.
pub fn text_matches(query: &str, title: &str, artists: &str, album: &str) -> bool {
    let mut words = query.split_whitespace().peekable();
    if words.peek().is_none() {
        return true;
    }
    let hay = format!("{} {} {}", fold(title), fold(artists), fold(album));
    words.all(|w| hay.contains(w))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_case_and_diacritics() {
        assert_eq!(fold("Beyoncé"), "beyonce");
        assert_eq!(fold("Björk — Jóga"), "bjork — joga");
        assert_eq!(fold("Straße Æon Łódź"), "strasse aeon lodz");
        assert_eq!(fold("東京"), "東京");
        assert_eq!(fold(""), "");
    }

    #[test]
    fn every_word_must_match_somewhere() {
        assert!(text_matches("bjork joga", "Jóga", "Björk", "Homogenic"));
        assert!(text_matches("homo", "Jóga", "Björk", "Homogenic"));
        assert!(!text_matches("bjork army", "Jóga", "Björk", "Homogenic"));
        assert!(text_matches("", "x", "y", "z"));
        assert!(text_matches("   ", "x", "y", "z"));
    }

    #[test]
    fn quality_chips() {
        use QualityFilter::*;
        assert!(All.accepts(0, false));
        assert!(!Lossless.accepts(0, false));
        assert!(Lossless.accepts(1, false) && Lossless.accepts(2, false));
        assert!(!HiRes.accepts(1, false) && HiRes.accepts(2, false));
        assert!(Atmos.accepts(0, true) && !Atmos.accepts(2, false));
        assert_eq!(QualityFilter::from_index(3), Atmos);
        assert_eq!(QualityFilter::from_index(9), All);
    }
}
