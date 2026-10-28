//! `tidal://` and tidal.com links → in-app destinations.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeepLink {
    Track(u64),
    Album(u64),
    Artist(u64),
    Video(u64),
    Playlist(String),
    Mix(String),
}

fn is_uuid(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

fn is_mix_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Parses a link, ignoring everything that is not a plain content reference.
/// Accepts `tidal://album/1`, `https://tidal.com/browse/album/1`,
/// `https://listen.tidal.com/track/1`, and similar.
pub fn parse(link: &str) -> Option<DeepLink> {
    let link = link.trim();
    let rest = link
        .strip_prefix("tidal://")
        .or_else(|| link.strip_prefix("https://tidal.com/"))
        .or_else(|| link.strip_prefix("https://listen.tidal.com/"))
        .or_else(|| link.strip_prefix("https://www.tidal.com/"))?;
    let path = rest.split(['?', '#']).next().unwrap_or("");
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let (i, kind) = segs.iter().enumerate().find(|(_, s)| {
        matches!(
            **s,
            "track" | "album" | "artist" | "video" | "playlist" | "mix"
        )
    })?;
    let id = segs.get(i + 1)?;
    let num = || id.parse::<u64>().ok();
    match *kind {
        "track" => num().map(DeepLink::Track),
        "album" => num().map(DeepLink::Album),
        "artist" => num().map(DeepLink::Artist),
        "video" => num().map(DeepLink::Video),
        "playlist" => is_uuid(id).then(|| DeepLink::Playlist((*id).to_string())),
        "mix" => is_mix_id(id).then(|| DeepLink::Mix((*id).to_string())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_custom_scheme_and_web_links() {
        assert_eq!(parse("tidal://album/123"), Some(DeepLink::Album(123)));
        assert_eq!(parse("tidal://track/9?x=1"), Some(DeepLink::Track(9)));
        assert_eq!(
            parse("https://tidal.com/browse/artist/77"),
            Some(DeepLink::Artist(77))
        );
        assert_eq!(
            parse("https://listen.tidal.com/video/5"),
            Some(DeepLink::Video(5))
        );
        assert_eq!(
            parse("https://tidal.com/playlist/0a1b2c3d-1111-2222-3333-444455556666"),
            Some(DeepLink::Playlist(
                "0a1b2c3d-1111-2222-3333-444455556666".into()
            ))
        );
        assert_eq!(
            parse("tidal://mix/0123abcDEF"),
            Some(DeepLink::Mix("0123abcDEF".into()))
        );
    }

    #[test]
    fn rejects_everything_else() {
        assert_eq!(parse("tidal://album/abc"), None);
        assert_eq!(parse("tidal://settings"), None);
        assert_eq!(parse("https://evil.example/album/1"), None);
        assert_eq!(parse("tidal://playlist/not a uuid!"), None);
        assert_eq!(parse("javascript:alert(1)"), None);
        assert_eq!(parse(""), None);
    }
}
