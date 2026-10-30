//! Theme presets and colour derivation, ported from the original app's
//! `lib/theme.ts`. File I/O and validation of `theme.json` live in
//! `theme_config`; this module owns what a theme *means*.

use crate::theme_config::{ThemeCustom, ThemeFile, CUSTOM_PRESET, THEME_FILE_VERSION};

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub name: String,
    pub accent: String,
    pub bg_base: String,
}

/// The 15 built-in presets, in the original order. `Violet Night` is the default.
pub const PRESETS: [(&str, &str, &str); 15] = [
    ("Violet Night", "#A855F7", "#130F1A"),
    ("Cyberpunk", "#FCE300", "#18180C"),
    ("Forest", "#22C55E", "#0E1410"),
    ("Ocean", "#3B82F6", "#0E1118"),
    ("Midnight Cyan", "#00FFFF", "#121212"),
    ("Sakura", "#F9A8D4", "#140F12"),
    ("Rose", "#F43F5E", "#140E0F"),
    ("Ember", "#F97316", "#151010"),
    ("Copper", "#E8915A", "#12100E"),
    ("Noir", "#FFFFFF", "#020202"),
    ("Daylight", "#2563EB", "#F5F3EE"),
    ("Snowfall", "#0891B2", "#F8FAFC"),
    ("Paper", "#7C3AED", "#FAF5FF"),
    ("Meadow", "#16A34A", "#F2F6EE"),
    ("Blossom", "#E11D48", "#FEF3F2"),
];

pub fn preset(name: &str) -> Option<Theme> {
    PRESETS
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(n, a, b)| Theme {
            name: (*n).into(),
            accent: (*a).into(),
            bg_base: (*b).into(),
        })
}

pub fn default_theme() -> Theme {
    let (n, a, b) = PRESETS[0];
    Theme {
        name: n.into(),
        accent: a.into(),
        bg_base: b.into(),
    }
}

/// `#RGB` / `#RRGGBB` → uppercase `#RRGGBB`.
pub fn normalize_hex(input: &str) -> Option<String> {
    let digits = input.trim().strip_prefix('#')?;
    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    match digits.len() {
        3 => Some(format!(
            "#{}",
            digits
                .chars()
                .flat_map(|c| [c, c])
                .collect::<String>()
                .to_uppercase()
        )),
        6 => Some(format!("#{}", digits.to_uppercase())),
        _ => None,
    }
}

/// Resolves a (validated) theme file to a [`Theme`]. A named preset wins over the
/// stored colours, exactly as in the original.
pub fn resolve(file: &ThemeFile) -> Option<Theme> {
    if file.version.is_some_and(|v| v != THEME_FILE_VERSION) {
        return None;
    }
    let accent = normalize_hex(&file.custom.accent)?;
    let bg_base = normalize_hex(&file.custom.background)?;
    if file.preset == CUSTOM_PRESET {
        return Some(Theme {
            name: "Custom".into(),
            accent,
            bg_base,
        });
    }
    preset(&file.preset)
}

/// A theme whose name and colours match a preset is saved as that preset;
/// anything else is saved as `custom`.
pub fn to_file(theme: &Theme) -> ThemeFile {
    let accent = normalize_hex(&theme.accent).unwrap_or_else(|| theme.accent.to_uppercase());
    let bg = normalize_hex(&theme.bg_base).unwrap_or_else(|| theme.bg_base.to_uppercase());
    let is_preset = PRESETS.iter().any(|(n, a, b)| {
        *n == theme.name && a.eq_ignore_ascii_case(&accent) && b.eq_ignore_ascii_case(&bg)
    });
    ThemeFile {
        version: Some(THEME_FILE_VERSION),
        preset: if is_preset {
            theme.name.clone()
        } else {
            CUSTOM_PRESET.into()
        },
        custom: ThemeCustom {
            accent,
            background: bg,
        },
    }
}

// ---- derivation ----

pub(crate) fn hex_to_hsl(hex: &str) -> (f64, f64, f64) {
    let h = hex.trim_start_matches('#');
    let p = |i: usize| {
        u8::from_str_radix(h.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0) as f64 / 255.0
    };
    let (r, g, b) = (p(0), p(2), p(4));
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.0;
    let (mut s, mut hue) = (0.0, 0.0);
    if max != min {
        let d = max - min;
        s = if l > 0.5 {
            d / (2.0 - max - min)
        } else {
            d / (max + min)
        };
        hue = if max == r {
            ((g - b) / d + if g < b { 6.0 } else { 0.0 }) / 6.0
        } else if max == g {
            ((b - r) / d + 2.0) / 6.0
        } else {
            ((r - g) / d + 4.0) / 6.0
        };
    }
    (hue * 360.0, s * 100.0, l * 100.0)
}

pub(crate) fn hsl_to_hex(h: f64, s: f64, l: f64) -> String {
    let (sn, ln) = (s / 100.0, l / 100.0);
    let c = (1.0 - (2.0 * ln - 1.0).abs()) * sn;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = ln - c / 2.0;
    let (r, g, b) = match h {
        h if h < 60.0 => (c, x, 0.0),
        h if h < 120.0 => (x, c, 0.0),
        h if h < 180.0 => (0.0, c, x),
        h if h < 240.0 => (0.0, x, c),
        h if h < 300.0 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let to = |v: f64| format!("{:02x}", ((v + m) * 255.0).round() as u8);
    format!("#{}{}{}", to(r), to(g), to(b))
}

fn shift(hex: &str, delta: f64) -> String {
    let (h, s, l) = hex_to_hsl(hex);
    hsl_to_hex(h, s, (l + delta).clamp(0.0, 100.0))
}

fn scale(hex: &str, factor: f64) -> String {
    let (h, s, l) = hex_to_hsl(hex);
    hsl_to_hex(h, s, (l * factor).clamp(0.0, 100.0))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Derived {
    pub is_dark: bool,
    pub bg_base: String,
    pub bg_surface: String,
    pub bg_surface_hover: String,
    pub bg_elevated: String,
    pub bg_sidebar: String,
    pub bg_overlay: String,
    pub bg_inset: String,
    pub bg_inset_hover: String,
    pub bg_button: String,
    pub bg_button_hover: String,
    pub accent: String,
    pub accent_hover: String,
    pub on_accent: String,
    pub text_primary: String,
    pub text_secondary: String,
    pub text_muted: String,
    pub text_faint: String,
    pub text_disabled: String,
    pub success: String,
    pub error: String,
    pub warning: String,
}

pub fn derive(accent: &str, bg_base: &str) -> Derived {
    let (_, _, bg_l) = hex_to_hsl(bg_base);
    let dark = bg_l < 50.0;
    let dir = if dark { 1.0 } else { -1.0 };
    let (primary, secondary, muted, faint, disabled) = if dark {
        ("#ffffff", "#b3b3b3", "#a6a6a6", "#666666", "#535353")
    } else {
        ("#111111", "#444444", "#666666", "#666666", "#aaaaaa")
    };
    Derived {
        is_dark: dark,
        bg_base: bg_base.into(),
        bg_surface: shift(bg_base, 4.0 * dir),
        bg_surface_hover: shift(bg_base, 9.0 * dir),
        bg_elevated: shift(bg_base, 2.5 * dir),
        bg_sidebar: shift(bg_base, -2.0),
        bg_overlay: shift(bg_base, -3.0),
        bg_inset: shift(bg_base, 7.0 * dir),
        bg_inset_hover: shift(bg_base, 11.0 * dir),
        bg_button: shift(bg_base, 14.0 * dir),
        bg_button_hover: shift(bg_base, 19.0 * dir),
        accent: accent.into(),
        accent_hover: scale(accent, 0.88),
        on_accent: if dark { "#000000" } else { "#ffffff" }.into(),
        text_primary: primary.into(),
        text_secondary: secondary.into(),
        text_muted: muted.into(),
        text_faint: faint.into(),
        text_disabled: disabled.into(),
        success: "#1ed760".into(),
        error: "#ff6666".into(),
        warning: "#ffa726".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme_config::{validate, PRESET_NAMES};

    #[test]
    fn preset_table_matches_the_validated_names_in_order() {
        let names: Vec<&str> = PRESETS.iter().map(|p| p.0).collect();
        assert_eq!(names, PRESET_NAMES);
    }

    #[test]
    fn every_preset_round_trips_through_its_file() {
        for (name, ..) in PRESETS {
            let t = preset(name).expect("preset");
            let f = to_file(&t);
            assert_eq!(f.preset, name);
            assert!(validate(&f).is_ok());
            assert_eq!(resolve(&f), Some(t));
        }
    }

    #[test]
    fn edited_preset_colours_become_custom() {
        let mut t = preset("Ocean").expect("preset");
        t.accent = "#123456".into();
        assert_eq!(to_file(&t).preset, CUSTOM_PRESET);
    }

    #[test]
    fn custom_files_resolve_to_their_colours_and_bad_ones_fail() {
        let f = ThemeFile {
            version: Some(1),
            preset: CUSTOM_PRESET.into(),
            custom: ThemeCustom {
                accent: "#abc".into(),
                background: "#000".into(),
            },
        };
        let t = resolve(&f).expect("custom");
        assert_eq!(
            (t.accent.as_str(), t.bg_base.as_str()),
            ("#AABBCC", "#000000")
        );
        let bad = ThemeFile {
            version: Some(2),
            ..f.clone()
        };
        assert_eq!(resolve(&bad), None);
        let unknown = ThemeFile {
            preset: "Nope".into(),
            ..f
        };
        assert_eq!(resolve(&unknown), None);
    }

    #[test]
    fn hex_normalisation() {
        assert_eq!(normalize_hex(" #a1b "), Some("#AA11BB".into()));
        assert_eq!(normalize_hex("a1b"), None);
        assert_eq!(normalize_hex("#12345"), None);
        assert_eq!(normalize_hex("#GGGGGG"), None);
    }

    #[test]
    fn derivation_follows_the_original_rules() {
        let d = derive("#A855F7", "#130F1A");
        assert!(d.is_dark);
        assert_eq!(d.on_accent, "#000000");
        assert_eq!(d.text_primary, "#ffffff");
        // surfaces get lighter on dark themes, darker on light ones
        let (_, _, base) = hex_to_hsl("#130F1A");
        let (_, _, surf) = hex_to_hsl(&d.bg_surface);
        assert!(surf > base);
        let l = derive("#2563EB", "#F5F3EE");
        assert!(!l.is_dark);
        assert_eq!(l.on_accent, "#ffffff");
        let (_, _, lbase) = hex_to_hsl("#F5F3EE");
        let (_, _, lsurf) = hex_to_hsl(&l.bg_surface);
        assert!(lsurf < lbase);
        // sidebar is always darker than the base
        let (_, _, side) = hex_to_hsl(&l.bg_sidebar);
        assert!(side < lbase);
    }

    #[test]
    fn hsl_round_trip_is_stable() {
        for hex in ["#A855F7", "#00FFFF", "#F5F3EE", "#020202"] {
            let (h, s, l) = hex_to_hsl(hex);
            assert_eq!(hsl_to_hex(h, s, l).to_uppercase(), hex);
        }
    }
}
