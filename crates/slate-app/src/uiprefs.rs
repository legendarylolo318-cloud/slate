//! App-only preferences that are not part of the core settings: interface scale
//! and theme mode. Plain JSON next to the other config files; nothing secret.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Index → scale factor; 0 means "let the toolkit decide".
pub const SCALES: [f32; 6] = [0.0, 1.0, 1.25, 1.5, 1.75, 2.0];

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
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
        p
    }

    pub fn save(&self) -> std::io::Result<()> {
        let Some(p) = path() else { return Ok(()) };
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(p, serde_json::to_string_pretty(self)?)
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
