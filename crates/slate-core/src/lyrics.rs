//! Synced-lyrics (LRC) parsing and timing.
//!
//! Ported from the original app's `lib/lrc.ts`: `[mm:ss.xx] text` lines, several
//! timestamps per line allowed, metadata tags (`[ar:..]`) ignored.

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub time: f32,
    pub text: String,
}

/// Parses LRC text into lines sorted by time. Lines without a timestamp are skipped.
pub fn parse_lrc(src: &str) -> Vec<Line> {
    let mut out = Vec::new();
    for raw in src.lines() {
        let mut rest = raw.trim();
        let mut times = Vec::new();
        while let Some(stripped) = rest.strip_prefix('[') {
            let Some(end) = stripped.find(']') else { break };
            match parse_stamp(&stripped[..end]) {
                Some(t) => times.push(t),
                // `[ar:Artist]`-style tag, or garbage: stop consuming stamps.
                None => break,
            }
            rest = stripped[end + 1..].trim_start();
        }
        for t in times {
            out.push(Line {
                time: t,
                text: rest.to_string(),
            });
        }
    }
    out.sort_by(|a, b| a.time.total_cmp(&b.time));
    out
}

fn parse_stamp(s: &str) -> Option<f32> {
    let (min, sec) = s.split_once(':')?;
    let min: u32 = min.trim().parse().ok()?;
    let sec: f32 = sec.trim().replace(',', ".").parse().ok()?;
    (sec >= 0.0).then_some(min as f32 * 60.0 + sec)
}

/// Index of the line being sung at `pos` seconds, or `None` before the first line.
pub fn current_index(lines: &[Line], pos: f32) -> Option<usize> {
    lines.partition_point(|l| l.time <= pos).checked_sub(1)
}

/// Plain (unsynced) lyrics as display lines.
pub fn plain_lines(src: &str) -> Vec<String> {
    src.lines().map(|l| l.trim_end().to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_and_multi_stamps() {
        let l = parse_lrc("[00:12.50] Hello\n[01:02.00][00:30.25]Chorus\n");
        assert_eq!(l.len(), 3);
        assert_eq!(
            l[0],
            Line {
                time: 12.5,
                text: "Hello".into()
            }
        );
        assert_eq!(l[1].time, 30.25);
        assert_eq!(l[2].time, 62.0);
        assert_eq!(l[2].text, "Chorus");
    }

    #[test]
    fn ignores_metadata_and_untimed_lines() {
        let l = parse_lrc("[ar:Someone]\n[ti:Song]\nno stamp here\n[00:01.00]ok");
        assert_eq!(
            l,
            vec![Line {
                time: 1.0,
                text: "ok".into()
            }]
        );
    }

    #[test]
    fn accepts_comma_fractions_and_empty_text() {
        let l = parse_lrc("[00:05,5]\n[00:06.0]x");
        assert_eq!(l[0].time, 5.5);
        assert_eq!(l[0].text, "");
    }

    #[test]
    fn rejects_malformed_stamps() {
        assert!(parse_lrc("[xx:yy]nope").is_empty());
        assert!(parse_lrc("[00:-1.0]neg").is_empty());
    }

    #[test]
    fn finds_current_line() {
        let l = parse_lrc("[00:10.00]a\n[00:20.00]b\n[00:30.00]c");
        assert_eq!(current_index(&l, 5.0), None);
        assert_eq!(current_index(&l, 10.0), Some(0));
        assert_eq!(current_index(&l, 25.0), Some(1));
        assert_eq!(current_index(&l, 999.0), Some(2));
        assert_eq!(current_index(&[], 1.0), None);
    }

    #[test]
    fn plain_lines_keep_blank_separators() {
        assert_eq!(plain_lines("a\n\nb  "), vec!["a", "", "b"]);
    }
}
