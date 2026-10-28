//! The "write log files" preference. Read before `AppState` exists, so it
//! lives in a tiny plaintext sidecar rather than the encrypted settings.
//! (The logger itself is set up by the frontend.)

use std::path::Path;

/// Read the logging toggle from the plaintext sidecar file at
/// `~/.config/slate/logging.toggle`. The file must contain literally
/// `"true"` or `"false"` (trailing whitespace/newlines are trimmed).
///
/// Defaults to `true` on any failure (missing file, unreadable, or
/// any content other than the literal string `"false"`). This runs
/// before `AppState` exists, so it intentionally
/// reads only this small sidecar — **not** the encrypted `Settings` struct.
pub fn read_logging_preference(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return true;
    };
    text.trim() != "false"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp_toggle(contents: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        f
    }

    #[test]
    fn probe_returns_true_when_file_missing() {
        let path = std::path::PathBuf::from("/nonexistent/slate-logging.toggle");
        assert!(read_logging_preference(&path));
    }

    #[test]
    fn probe_returns_true_when_contents_garbage() {
        let f = tmp_toggle("not valid stuff");
        assert!(read_logging_preference(f.path()));
    }

    #[test]
    fn probe_returns_false_when_explicitly_disabled() {
        let f = tmp_toggle("false");
        assert!(!read_logging_preference(f.path()));
    }

    #[test]
    fn probe_returns_true_when_explicitly_enabled() {
        let f = tmp_toggle("true");
        assert!(read_logging_preference(f.path()));
    }

    #[test]
    fn probe_returns_false_when_explicitly_disabled_with_trailing_newline() {
        let f = tmp_toggle("false\n");
        assert!(!read_logging_preference(f.path()));
    }

    #[test]
    fn probe_returns_true_when_contents_empty() {
        let f = tmp_toggle("");
        assert!(read_logging_preference(f.path()));
    }
}
