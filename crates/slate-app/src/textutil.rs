//! Small text helpers shared by user-typed or server-provided strings that end up in the UI
//! or in a rendered image.

/// True for the bidirectional embeddings, overrides, isolates and marks that can make text
/// read in a different order than it was typed (the same set the party chat strips).
pub fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}'
    )
}

/// `s` without control characters and bidi controls, runs of whitespace folded to one space.
pub fn clean_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if is_bidi_control(c) || (c.is_control() && !c.is_whitespace()) {
            continue;
        }
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bidi_overrides_and_controls_go() {
        assert_eq!(
            clean_line("a \u{202E}gnp\u{202C}\u{2067}x\u{2069}\u{200F}b"),
            "a gnpxb"
        );
        assert_eq!(clean_line("  one\n two\t\u{7}three  "), "one two three");
        assert_eq!(clean_line("\u{202E}\u{2066}"), "");
    }
}

/// "47 min" / "1 h 05 min": a length for a notice.
pub fn minutes_text(secs: f32) -> String {
    let m = (secs.max(0.0) / 60.0).round() as u32;
    if m >= 60 {
        format!("{} h {:02} min", m / 60, m % 60)
    } else {
        format!("{m} min")
    }
}

#[cfg(test)]
mod minutes_tests {
    use super::minutes_text;

    #[test]
    fn lengths_read_as_minutes_or_hours() {
        assert_eq!(minutes_text(47.0 * 60.0 + 20.0), "47 min");
        assert_eq!(minutes_text(3900.0), "1 h 05 min");
        assert_eq!(minutes_text(-5.0), "0 min");
    }
}
