//! Custom Discord Rich Presence text: `{title}`, `{artist}`, `{album}`, `{quality}`,
//! `{playlist}` and `{year}` placeholders, clamped to what Discord accepts.

/// Discord shows at most 128 characters in `details` and `state`, and needs at least 2.
pub const MAX_CHARS: usize = 128;
pub const MIN_CHARS: usize = 2;
/// Longest template the settings accept.
pub const MAX_TEMPLATE_CHARS: usize = 200;

pub const DEFAULT_DETAILS: &str = "{title}";
pub const DEFAULT_STATE: &str = "by {artist} on {album}";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fields<'a> {
    pub title: &'a str,
    pub artist: &'a str,
    pub album: &'a str,
    pub quality: &'a str,
    pub playlist: &'a str,
    pub year: &'a str,
}

/// Replaces the placeholders in one pass (so a value that contains `{artist}` is left alone).
pub fn render(template: &str, f: &Fields) -> String {
    let mut out = String::with_capacity(template.len() + 32);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[open..]);
            rest = "";
            break;
        };
        let value = match &after[..close] {
            "title" => Some(f.title),
            "artist" => Some(f.artist),
            "album" => Some(f.album),
            "quality" => Some(f.quality),
            "playlist" => Some(f.playlist),
            "year" => Some(f.year),
            _ => None,
        };
        match value {
            Some(v) => out.push_str(v),
            None => out.push_str(&rest[open..open + close + 2]),
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Tidies a rendered line for Discord: collapses whitespace, cuts to 128 characters with an
/// ellipsis, and drops it (None) when it is shorter than 2 characters.
pub fn clamp(line: &str) -> Option<String> {
    let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
    let n = line.chars().count();
    if n < MIN_CHARS {
        return None;
    }
    if n <= MAX_CHARS {
        return Some(line);
    }
    let mut cut: String = line.chars().take(MAX_CHARS - 1).collect();
    cut.push('…');
    Some(cut)
}

/// Template as the user typed it, limited in length; blank means "use the default".
pub fn clean_template(t: &str) -> String {
    t.chars()
        .filter(|c| !c.is_control())
        .take(MAX_TEMPLATE_CHARS)
        .collect::<String>()
        .trim()
        .to_string()
}

/// (details, state) for the presence. A blank template uses the default; when `{artist}` or
/// `{album}` are empty the default state still reads sensibly.
pub fn lines(details: &str, state: &str, f: &Fields) -> (Option<String>, Option<String>) {
    let details_t = if details.trim().is_empty() {
        DEFAULT_DETAILS
    } else {
        details
    };
    let state_line = if state.trim().is_empty() {
        match (f.artist.is_empty(), f.album.is_empty()) {
            (true, _) => f.album.to_string(),
            (false, true) => format!("by {}", f.artist),
            (false, false) => render(DEFAULT_STATE, f),
        }
    } else {
        render(state, f)
    };
    (clamp(&render(details_t, f)), clamp(&state_line))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f() -> Fields<'static> {
        Fields {
            title: "Halo",
            artist: "Beyoncé",
            album: "I Am... Sasha Fierce",
            quality: "Hi-Res Lossless",
            playlist: "Road Trip",
            year: "2008",
        }
    }

    #[test]
    fn every_placeholder_is_replaced() {
        let t = "{title}|{artist}|{album}|{quality}|{playlist}|{year}";
        assert_eq!(
            render(t, &f()),
            "Halo|Beyoncé|I Am... Sasha Fierce|Hi-Res Lossless|Road Trip|2008"
        );
    }

    #[test]
    fn unknown_and_broken_placeholders_stay_as_typed() {
        assert_eq!(render("{nope} {title", &f()), "{nope} {title");
        assert_eq!(render("}{title}{", &f()), "}Halo{");
        assert_eq!(render("", &f()), "");
    }

    #[test]
    fn values_containing_placeholders_are_not_expanded_again() {
        let mut x = f();
        x.title = "{artist}";
        assert_eq!(render("{title}", &x), "{artist}");
    }

    #[test]
    fn clamp_truncates_and_drops_tiny_lines() {
        assert_eq!(clamp("  a  "), None);
        assert_eq!(clamp(""), None);
        assert_eq!(clamp("ok"), Some("ok".into()));
        assert_eq!(clamp("a   b\n c").as_deref(), Some("a b c"));
        let long = "é".repeat(300);
        let c = clamp(&long).expect("long");
        assert_eq!(c.chars().count(), MAX_CHARS);
        assert!(c.ends_with('…'));
        assert_eq!(
            clamp(&"x".repeat(MAX_CHARS)).map(|s| s.len()),
            Some(MAX_CHARS)
        );
    }

    #[test]
    fn defaults_apply_when_blank() {
        let (d, s) = lines("", "", &f());
        assert_eq!(d.as_deref(), Some("Halo"));
        assert_eq!(s.as_deref(), Some("by Beyoncé on I Am... Sasha Fierce"));
        let mut x = f();
        x.album = "";
        assert_eq!(lines("", "", &x).1.as_deref(), Some("by Beyoncé"));
        x.artist = "";
        x.album = "Alb";
        assert_eq!(lines("", "", &x).1.as_deref(), Some("Alb"));
    }

    #[test]
    fn custom_templates_win() {
        let (d, s) = lines("{artist} – {title}", "{playlist} ({year})", &f());
        assert_eq!(d.as_deref(), Some("Beyoncé – Halo"));
        assert_eq!(s.as_deref(), Some("Road Trip (2008)"));
        // An empty playlist leaves a line that is too short: omitted.
        let mut x = f();
        x.playlist = "";
        x.year = "";
        assert_eq!(lines("{title}", "{playlist}", &x).1, None);
    }

    #[test]
    fn templates_are_cleaned() {
        assert_eq!(clean_template("  {title}\u{7}  "), "{title}");
        assert_eq!(
            clean_template(&"a".repeat(500)).chars().count(),
            MAX_TEMPLATE_CHARS
        );
    }
}
