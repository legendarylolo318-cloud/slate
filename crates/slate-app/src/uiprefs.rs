//! App-only preferences that are not part of the core settings: interface scale
//! and theme mode. Plain JSON next to the other config files; nothing secret.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Index → scale factor; 0 means "let the toolkit decide".
pub const SCALES: [f32; 6] = [0.0, 1.0, 1.25, 1.5, 1.75, 2.0];

/// Player bar size: Small, Normal, Large, Extra large.
pub const BAR_SCALES: [f32; 4] = [0.85, 1.0, 1.2, 1.4];
/// Lyrics size: Small, Normal, Large, Extra large.
pub const LYRICS_SCALES: [f32; 4] = [0.85, 1.0, 1.25, 1.5];
/// "Normal" in both lists.
const DEFAULT_SIZE: usize = 1;

fn default_true() -> bool {
    true
}

fn default_size() -> usize {
    DEFAULT_SIZE
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UiPrefs {
    #[serde(default)]
    pub scale_index: usize,
    /// Renderer that worked after a startup fallback; the next launch starts
    /// there instead of failing over again (see `bootstrap::run`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renderer: Option<String>,
    /// Renderer being started and not yet confirmed. Still set at the next
    /// launch means that renderer crashed the process, so it is skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renderer_pending: Option<String>,
    /// Index into `BAR_SCALES`.
    #[serde(default = "default_size")]
    pub bar_size: usize,
    /// Index into `LYRICS_SCALES`.
    #[serde(default = "default_size")]
    pub lyrics_size: usize,
    /// System-wide media shortcuts (off by default).
    #[serde(default)]
    pub global_shortcuts: bool,
    /// Tint the player bar and full-screen player with the cover's colour.
    #[serde(default)]
    pub cover_colors: bool,
    /// Look for a newer Slate release once a day (on by default).
    #[serde(default = "default_true")]
    pub check_updates: bool,
    /// Unix time of the last check, successful or not.
    #[serde(default)]
    pub last_update_check: u64,
    /// The newest release found and not yet installed: version, then link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_update: Option<(String, String)>,
    /// The version whose notice the user closed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub dismissed_update: String,
    /// Rebound shortcuts: action id → combo (`slate_core::keymap`).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub keymap: std::collections::BTreeMap<String, String>,
    /// Sort order per library list (`sort::List::key`).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub sorts: std::collections::BTreeMap<String, SortPref>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SortPref {
    pub order: String,
    pub desc: bool,
}

impl Default for UiPrefs {
    fn default() -> Self {
        Self {
            scale_index: 0,
            renderer: None,
            renderer_pending: None,
            bar_size: DEFAULT_SIZE,
            lyrics_size: DEFAULT_SIZE,
            global_shortcuts: false,
            cover_colors: false,
            check_updates: true,
            last_update_check: 0,
            known_update: None,
            dismissed_update: String::new(),
            keymap: Default::default(),
            sorts: Default::default(),
        }
    }
}

fn path() -> Option<PathBuf> {
    slate_core::config_dir_for_env().map(|d| d.join("ui.json"))
}

impl UiPrefs {
    pub fn load() -> Self {
        path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|s| Self::parse(&s))
            .unwrap_or_default()
    }

    pub fn parse(s: &str) -> Self {
        let mut p: Self = serde_json::from_str(s).unwrap_or_default();
        if p.scale_index >= SCALES.len() {
            p.scale_index = 0;
        }
        if p.bar_size >= BAR_SCALES.len() {
            p.bar_size = DEFAULT_SIZE;
        }
        if p.lyrics_size >= LYRICS_SCALES.len() {
            p.lyrics_size = DEFAULT_SIZE;
        }
        p
    }

    pub fn save(&self) -> std::io::Result<()> {
        let Some(p) = path() else { return Ok(()) };
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(p, serde_json::to_string_pretty(self)?)
    }

    pub fn bar_scale(&self) -> f32 {
        BAR_SCALES.get(self.bar_size).copied().unwrap_or(1.0)
    }

    pub fn lyrics_scale(&self) -> f32 {
        LYRICS_SCALES.get(self.lyrics_size).copied().unwrap_or(1.0)
    }

    pub fn scale_factor(&self) -> Option<f32> {
        SCALES.get(self.scale_index).copied().filter(|s| *s > 0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembered_renderer_round_trips_and_is_optional() {
        let p = UiPrefs::parse(r#"{"scale_index":2,"renderer":"software"}"#);
        assert_eq!(p.renderer.as_deref(), Some("software"));
        let json = serde_json::to_string(&UiPrefs::default()).unwrap();
        assert!(!json.contains("renderer"), "{json}");
        assert_eq!(UiPrefs::parse(&serde_json::to_string(&p).unwrap()), p);
    }

    #[test]
    fn sizes_default_to_normal_and_clamp() {
        let p = UiPrefs::parse("{}");
        assert_eq!((p.bar_scale(), p.lyrics_scale()), (1.0, 1.0));
        let p = UiPrefs::parse(r#"{"bar_size":3,"lyrics_size":0}"#);
        assert_eq!((p.bar_scale(), p.lyrics_scale()), (1.4, 0.85));
        let p = UiPrefs::parse(r#"{"bar_size":9,"lyrics_size":9}"#);
        assert_eq!((p.bar_size, p.lyrics_size), (1, 1));
    }

    #[test]
    fn defaults_and_clamping() {
        assert_eq!(UiPrefs::parse("{}"), UiPrefs::default());
        assert_eq!(UiPrefs::parse("not json"), UiPrefs::default());
        assert_eq!(UiPrefs::parse(r#"{"scale_index":99}"#).scale_index, 0);
    }

    #[test]
    fn scale_factor_maps_index() {
        assert_eq!(
            UiPrefs {
                scale_index: 0,
                ..UiPrefs::default()
            }
            .scale_factor(),
            None
        );
        assert_eq!(
            UiPrefs {
                scale_index: 3,
                ..UiPrefs::default()
            }
            .scale_factor(),
            Some(1.5)
        );
    }
}
