//! Theme glue: derives the palette in core, pushes it into the Slint `Theme`
//! global, and keeps `theme.json` in sync (including external edits).

use std::sync::{Arc, Mutex};

use slate_core::theme::{self, Theme as CoreTheme};
use slate_core::theme_config::{self, ThemeFile};
use slint::{Color, ComponentHandle, ModelRc, VecModel};

use crate::backend::Backend;
use crate::bootstrap::{AppWindow, PresetTile, SettingsState, Theme};

pub fn color(hex: &str) -> Color {
    let h = hex.trim_start_matches('#');
    let p = |i: usize| u8::from_str_radix(h.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0);
    Color::from_rgb_u8(p(0), p(2), p(4))
}

/// The theme file as saved, or the default preset when there is none / it is invalid.
pub fn load_saved() -> CoreTheme {
    let t = load_saved_inner();
    crate::custom_themes::init(&t);
    t
}

fn load_saved_inner() -> CoreTheme {
    match theme_config::theme_file_get() {
        Ok(Some(file)) => theme::resolve(&file).unwrap_or_else(|| {
            tracing::warn!("theme.json did not resolve to a theme; using the default");
            theme::default_theme()
        }),
        Ok(None) => theme::default_theme(),
        Err(e) => {
            tracing::warn!("theme.json unreadable ({e}); using the default");
            theme::default_theme()
        }
    }
}

/// Writes `t` into the Slint globals.
pub fn apply(w: &AppWindow, t: &CoreTheme) {
    let d = theme::derive(&t.accent, &t.bg_base);
    let g = w.global::<Theme>();
    g.set_dark(d.is_dark);
    g.set_accent(color(&d.accent));
    g.set_accent_hover(color(&d.accent_hover));
    g.set_on_accent(color(&d.on_accent));
    g.set_background(color(&d.bg_base));
    g.set_surface(color(&d.bg_surface));
    g.set_surface_raised(color(&d.bg_button));
    g.set_text(color(&d.text_primary));
    g.set_text_muted(color(&d.text_muted));
    g.set_danger(color(&d.error));

    let s = w.global::<SettingsState>();
    s.set_theme_name(t.name.clone().into());
    s.set_accent_color(color(&t.accent));
    s.set_bg_color(color(&t.bg_base));
    s.set_accent_hex(t.accent.clone().into());
    s.set_bg_hex(t.bg_base.clone().into());
    let tiles: Vec<PresetTile> = theme::PRESETS
        .iter()
        .map(|(n, a, b)| PresetTile {
            name: (*n).into(),
            accent: color(a),
            bg: color(b),
        })
        .collect();
    s.set_presets(ModelRc::new(VecModel::from(tiles)));
    crate::custom_themes::apply_active(w);
}

fn hex_of(c: Color) -> String {
    format!("#{:02X}{:02X}{:02X}", c.red(), c.green(), c.blue())
}

impl Backend {
    pub(crate) fn current_theme_pub(&self) -> CoreTheme {
        self.current_theme()
    }

    fn current_theme(&self) -> CoreTheme {
        self.theme
            .lock()
            .map(|t| t.clone())
            .unwrap_or_else(|_| theme::default_theme())
    }

    /// A preset, picker or external edit: whatever custom theme was in use is left behind.
    fn set_theme(&self, t: CoreTheme, save: bool) {
        crate::custom_themes::clear_active();
        self.set_theme_inner(t, save);
    }

    /// Applies and saves `t` while keeping the active custom theme (it owns these colours).
    pub(crate) fn set_theme_quiet(&self, t: CoreTheme) {
        self.set_theme_inner(t, true);
    }

    fn set_theme_inner(&self, t: CoreTheme, save: bool) {
        if let Ok(mut cur) = self.theme.lock() {
            *cur = t.clone();
        }
        let shown = t.clone();
        self.ui(move |w| apply(&w, &shown));
        // Text colours changed, so the tint may need another lightness.
        self.reapply_tint();
        if save {
            self.save_theme(&t);
        }
    }

    fn save_theme(&self, t: &CoreTheme) {
        let file = theme::to_file(t);
        self.rt.spawn_blocking(move || {
            if let Err(e) = theme_config::theme_file_set(file) {
                tracing::warn!("could not save theme.json: {e}");
            }
        });
    }

    pub fn theme_preset(&self, index: usize) {
        if let Some((name, ..)) = theme::PRESETS.get(index) {
            if let Some(t) = theme::preset(name) {
                self.set_theme(t, true);
            }
        }
    }

    /// `target` 0 = accent, 1 = background. Live while dragging; saved on release.
    pub fn theme_color(&self, target: i32, c: Color, save: bool) {
        self.set_custom(target, hex_of(c), save);
    }

    pub fn theme_hex(&self, target: i32, text: &str) {
        match theme::normalize_hex(text) {
            Some(hex) => self.set_custom(target, hex, true),
            None => self.ui(|w| w.set_error("Enter a colour like #A855F7.".into())),
        }
    }

    fn set_custom(&self, target: i32, hex: String, save: bool) {
        let mut t = self.current_theme();
        if target == 0 {
            t.accent = hex;
        } else {
            t.bg_base = hex;
        }
        // Editing a preset's colours makes it custom; `to_file` decides on save.
        if !theme::PRESETS
            .iter()
            .any(|(n, a, b)| *n == t.name && *a == t.accent && *b == t.bg_base)
        {
            t.name = "Custom".into();
        }
        self.set_theme(t, save);
    }

    /// `theme.json` changed on disk (or was deleted).
    pub fn theme_file_changed(&self, payload: serde_json::Value) {
        if payload.is_null() {
            // Deleted: put the live theme back, as the original does.
            self.save_theme(&self.current_theme());
            return;
        }
        let Ok(file) = serde_json::from_value::<ThemeFile>(payload) else {
            return;
        };
        match theme::resolve(&file) {
            Some(t) if t != self.current_theme() => self.set_theme(t, false),
            Some(_) => {}
            None => tracing::warn!("ignored an invalid theme.json edit"),
        }
    }

    pub fn start_theme_watcher(&self) {
        let watcher = theme_config::spawn_theme_watcher(self.app.clone());
        if let Ok(mut slot) = self.theme_watcher.lock() {
            *slot = watcher;
        }
    }
}

pub type SharedTheme = Arc<Mutex<CoreTheme>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_into_colours() {
        let c = color("#A855F7");
        assert_eq!((c.red(), c.green(), c.blue()), (0xA8, 0x55, 0xF7));
        assert_eq!(hex_of(c), "#A855F7");
    }
}
