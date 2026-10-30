//! User-made themes: an accent and a background plus optional per-role overrides on top of the
//! derived palette, WCAG contrast checks, "generate from a cover colour", the on-disk list of
//! saved themes and the `.slatetheme` import/export format.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::theme::{self, Derived};

pub const FILE_VERSION: u32 = 1;
/// Largest `.slatetheme` accepted.
pub const MAX_FILE_BYTES: usize = 16 * 1024;
pub const MAX_NAME_CHARS: usize = 40;
pub const MAX_THEMES: usize = 100;

/// Every colour role the editor offers: id, label.
pub const ROLES: [(&str, &str); 9] = [
    ("accent", "Accent"),
    ("accent-hover", "Accent (hover)"),
    ("on-accent", "Text on accent"),
    ("background", "Background"),
    ("surface", "Surface"),
    ("surface-raised", "Raised surface / buttons"),
    ("text", "Text"),
    ("text-muted", "Secondary text"),
    ("danger", "Error"),
];

/// (text role, background role) pairs that must stay readable (WCAG AA body text, 4.5:1).
pub const TEXT_PAIRS: [(&str, &str); 7] = [
    ("text", "background"),
    ("text", "surface"),
    ("text", "surface-raised"),
    ("text-muted", "background"),
    ("text-muted", "surface"),
    ("on-accent", "accent"),
    ("danger", "background"),
];

pub const MIN_CONTRAST: f64 = 4.5;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomTheme {
    pub name: String,
    pub accent: String,
    pub background: String,
    /// Role id → `#RRGGBB`, for roles that differ from the derived colour.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub overrides: BTreeMap<String, String>,
}

/// Role id → `#RRGGBB`, every role present.
pub type Palette = BTreeMap<&'static str, String>;

impl CustomTheme {
    pub fn new(name: &str, accent: &str, background: &str) -> Self {
        Self {
            name: clean_name(name),
            accent: accent.to_string(),
            background: background.to_string(),
            overrides: BTreeMap::new(),
        }
    }

    /// The theme's colours: derived from accent and background, then overridden.
    pub fn palette(&self) -> Palette {
        let d: Derived = theme::derive(&self.accent, &self.background);
        let mut p: Palette = BTreeMap::new();
        p.insert("accent", d.accent);
        p.insert("accent-hover", d.accent_hover);
        p.insert("on-accent", d.on_accent);
        p.insert("background", d.bg_base);
        p.insert("surface", d.bg_surface);
        p.insert("surface-raised", d.bg_button);
        p.insert("text", d.text_primary);
        p.insert("text-muted", d.text_muted);
        p.insert("danger", d.error);
        for (role, _) in ROLES {
            if let Some(v) = self.overrides.get(role) {
                p.insert(role, v.to_ascii_uppercase());
            }
        }
        // The two base colours are fields, not overrides.
        p.insert("accent", self.accent.to_ascii_uppercase());
        p.insert("background", self.background.to_ascii_uppercase());
        p
    }

    /// Sets one role. "accent" and "background" are the base colours; the rest are overrides
    /// (removed again when equal to the derived colour).
    pub fn set_role(&mut self, role: &str, hex: &str) -> Result<(), String> {
        let hex = theme::normalize_hex(hex).ok_or("Enter a colour like #A855F7.")?;
        if !ROLES.iter().any(|(id, _)| *id == role) {
            return Err(format!("unknown colour role {role:?}"));
        }
        match role {
            "accent" => self.accent = hex,
            "background" => self.background = hex,
            _ => {
                self.overrides.insert(role.to_string(), hex);
            }
        }
        // Drop overrides that match the derivation.
        let derived = CustomTheme {
            overrides: BTreeMap::new(),
            ..self.clone()
        }
        .palette();
        self.overrides.retain(|r, v| {
            derived.get(r.as_str()).map(|d| !d.eq_ignore_ascii_case(v)) == Some(true)
        });
        Ok(())
    }

    pub fn reset_role(&mut self, role: &str) {
        self.overrides.remove(role);
    }
}

pub fn clean_name(name: &str) -> String {
    let n: String = name
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect();
    let n = n.trim();
    if n.is_empty() {
        "My theme".into()
    } else {
        n.into()
    }
}

// ---- contrast ----

fn channel(c: u8) -> f64 {
    let v = f64::from(c) / 255.0;
    if v <= 0.03928 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn rgb(hex: &str) -> Option<(u8, u8, u8)> {
    let h = hex.strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some((p(0)?, p(2)?, p(4)?))
}

fn luminance(hex: &str) -> Option<f64> {
    let (r, g, b) = rgb(hex)?;
    Some(0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b))
}

/// WCAG contrast ratio (1–21) of two `#RRGGBB` colours.
pub fn contrast_ratio(a: &str, b: &str) -> Option<f64> {
    let (la, lb) = (luminance(a)?, luminance(b)?);
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    Some((hi + 0.05) / (lo + 0.05))
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContrastWarning {
    pub text_role: &'static str,
    pub on_role: &'static str,
    pub ratio: f64,
}

/// Every text pair below 4.5:1.
pub fn contrast_warnings(p: &Palette) -> Vec<ContrastWarning> {
    TEXT_PAIRS
        .iter()
        .filter_map(|(t, b)| {
            let ratio = contrast_ratio(p.get(t)?, p.get(b)?)?;
            (ratio < MIN_CONTRAST).then_some(ContrastWarning {
                text_role: t,
                on_role: b,
                ratio,
            })
        })
        .collect()
}

pub fn role_label(id: &str) -> &'static str {
    ROLES.iter().find(|(r, _)| *r == id).map_or("", |(_, l)| l)
}

// ---- from a cover colour ----

/// Accent and background for a theme built around `cover` (`#RRGGBB`): the cover's hue at a
/// lively saturation for the accent, a very dark tone of the same hue for the background.
pub fn from_cover_color(cover: &str) -> Option<(String, String)> {
    rgb(cover)?;
    let (h, s, l) = theme::hex_to_hsl(cover);
    let accent = theme::hsl_to_hex(h, s.clamp(45.0, 90.0), l.clamp(50.0, 65.0));
    let background = theme::hsl_to_hex(h, (s * 0.5).clamp(10.0, 35.0), 9.0);
    Some((accent.to_ascii_uppercase(), background.to_ascii_uppercase()))
}

// ---- .slatetheme ----

#[derive(Serialize, Deserialize)]
struct ThemeDoc {
    version: u32,
    name: String,
    accent: String,
    background: String,
    #[serde(default)]
    overrides: BTreeMap<String, String>,
}

pub fn export(t: &CustomTheme) -> String {
    let doc = ThemeDoc {
        version: FILE_VERSION,
        name: t.name.clone(),
        accent: t.accent.clone(),
        background: t.background.clone(),
        overrides: t.overrides.clone(),
    };
    serde_json::to_string_pretty(&doc).unwrap_or_default()
}

/// Parses and validates a `.slatetheme`. Errors are written for the user.
pub fn import(text: &str) -> Result<CustomTheme, String> {
    if text.len() > MAX_FILE_BYTES {
        return Err("That file is too large to be a Slate theme.".into());
    }
    let doc: ThemeDoc = serde_json::from_str(text)
        .map_err(|_| "That is not a valid Slate theme file.".to_string())?;
    if doc.version != FILE_VERSION {
        return Err(format!(
            "This theme uses version {}, but this Slate understands version {FILE_VERSION}.",
            doc.version
        ));
    }
    let bad = |what: &str, v: &str| {
        format!("The theme's {what} colour {v:?} is not a valid #RRGGBB colour.")
    };
    let accent = theme::normalize_hex(&doc.accent).ok_or_else(|| bad("accent", &doc.accent))?;
    let background =
        theme::normalize_hex(&doc.background).ok_or_else(|| bad("background", &doc.background))?;
    let mut t = CustomTheme::new(&doc.name, &accent, &background);
    for (role, hex) in &doc.overrides {
        if !ROLES.iter().any(|(id, _)| id == role) {
            return Err(format!("The theme has an unknown colour role {role:?}."));
        }
        let hex = theme::normalize_hex(hex).ok_or_else(|| bad(role, hex))?;
        t.overrides.insert(role.clone(), hex);
    }
    // accent/background overrides are the base colours themselves.
    t.overrides.remove("accent");
    t.overrides.remove("background");
    Ok(t)
}

// ---- the saved list ----

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Store {
    /// Name of the custom theme in use, if the current look is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
    #[serde(default)]
    pub themes: Vec<CustomTheme>,
}

impl Store {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(tmp, path)
    }

    pub fn get(&self, name: &str) -> Option<&CustomTheme> {
        self.themes.iter().find(|t| t.name == name)
    }

    /// A name not used yet: "Name", "Name 2", …
    pub fn unique_name(&self, wanted: &str) -> String {
        let base = clean_name(wanted);
        if self.get(&base).is_none() {
            return base;
        }
        (2..)
            .map(|n| format!("{base} {n}"))
            .find(|c| self.get(c).is_none())
            .unwrap_or(base)
    }

    /// Adds or replaces (same name). Fails when the list is full.
    pub fn upsert(&mut self, t: CustomTheme) -> Result<(), String> {
        if let Some(slot) = self.themes.iter_mut().find(|x| x.name == t.name) {
            *slot = t;
            return Ok(());
        }
        if self.themes.len() >= MAX_THEMES {
            return Err("Too many saved themes; delete some first.".into());
        }
        self.themes.push(t);
        Ok(())
    }

    pub fn duplicate(&mut self, name: &str) -> Option<String> {
        let mut copy = self.get(name)?.clone();
        copy.name = self.unique_name(&format!("{name} copy"));
        let new = copy.name.clone();
        self.upsert(copy).ok()?;
        Some(new)
    }

    /// Renames, keeping `active` pointing at the same theme. Fails on a taken name.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<String, String> {
        let to = clean_name(to);
        if to != from && self.get(&to).is_some() {
            return Err(format!("There is already a theme called “{to}”."));
        }
        let t = self
            .themes
            .iter_mut()
            .find(|t| t.name == from)
            .ok_or("No such theme.")?;
        t.name = to.clone();
        if self.active.as_deref() == Some(from) {
            self.active = Some(to.clone());
        }
        Ok(to)
    }

    pub fn delete(&mut self, name: &str) -> bool {
        let before = self.themes.len();
        self.themes.retain(|t| t.name != name);
        if self.active.as_deref() == Some(name) {
            self.active = None;
        }
        self.themes.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> CustomTheme {
        CustomTheme::new("Test", "#A855F7", "#130F1A")
    }

    #[test]
    fn palette_has_every_role_and_matches_the_derivation() {
        let p = sample().palette();
        for (id, _) in ROLES {
            assert!(p.contains_key(id), "{id}");
        }
        assert_eq!(p["accent"], "#A855F7");
        assert_eq!(p["background"], "#130F1A");
        let d = theme::derive("#A855F7", "#130F1A");
        assert_eq!(p["text"], d.text_primary);
    }

    #[test]
    fn overrides_win_and_redundant_ones_vanish() {
        let mut t = sample();
        t.set_role("text", "#ff0000").expect("valid");
        assert_eq!(t.palette()["text"], "#FF0000");
        let derived = theme::derive("#A855F7", "#130F1A").text_primary;
        t.set_role("text", &derived).expect("valid");
        assert!(t.overrides.is_empty());
        assert!(t.set_role("text", "red").is_err());
        assert!(t.set_role("nope", "#000000").is_err());
        t.set_role("accent", "#112233").expect("valid");
        assert_eq!(t.palette()["accent"], "#112233");
    }

    #[test]
    fn contrast_ratio_matches_wcag_examples() {
        let r = contrast_ratio("#000000", "#FFFFFF").expect("ratio");
        assert!((r - 21.0).abs() < 0.01);
        let same = contrast_ratio("#777777", "#777777").expect("ratio");
        assert!((same - 1.0).abs() < 1e-9);
        // #767676 on white is the classic 4.54:1 pass.
        assert!(contrast_ratio("#767676", "#FFFFFF").expect("ratio") >= 4.5);
        assert!(contrast_ratio("#777777", "#FFFFFF").expect("ratio") < 4.5);
        assert!(contrast_ratio("nope", "#FFFFFF").is_none());
    }

    #[test]
    fn warnings_name_the_offending_pair() {
        assert!(contrast_warnings(&sample().palette()).is_empty());
        let mut t = sample();
        t.set_role("text", "#2A2335").expect("valid");
        let w = contrast_warnings(&t.palette());
        assert!(w
            .iter()
            .any(|w| w.text_role == "text" && w.on_role == "background"));
    }

    #[test]
    fn cover_colours_make_a_readable_theme() {
        for cover in [
            "#E63946", "#2A9D8F", "#F4A261", "#264653", "#999999", "#000000",
        ] {
            let (a, b) = from_cover_color(cover).expect("theme");
            let t = CustomTheme::new("c", &a, &b);
            assert!(
                contrast_warnings(&t.palette())
                    .iter()
                    .all(|w| w.text_role == "danger" || w.ratio > 3.0),
                "{cover}"
            );
        }
        assert!(from_cover_color("nope").is_none());
    }

    #[test]
    fn export_import_round_trip() {
        let mut t = sample();
        t.set_role("text-muted", "#CCCCCC").expect("valid");
        assert_eq!(import(&export(&t)).expect("round trip"), t);
    }

    #[test]
    fn import_rejects_bad_input_with_clear_errors() {
        assert!(import("not json")
            .expect_err("json")
            .contains("not a valid"));
        assert!(import(&"x".repeat(MAX_FILE_BYTES + 1))
            .expect_err("size")
            .contains("too large"));
        let v2 = r##"{"version":2,"name":"a","accent":"#FFFFFF","background":"#000000"}"##;
        assert!(import(v2).expect_err("version").contains("version 2"));
        let bad = r##"{"version":1,"name":"a","accent":"purple","background":"#000000"}"##;
        assert!(import(bad).expect_err("colour").contains("accent"));
        let role = r##"{"version":1,"name":"a","accent":"#FFFFFF","background":"#000000","overrides":{"x":"#000000"}}"##;
        assert!(import(role)
            .expect_err("role")
            .contains("unknown colour role"));
        let none = r##"{"version":1,"name":"","accent":"#FFF","background":"#000"}"##;
        let t = import(none).expect("short hex and blank name are fine");
        assert_eq!(
            (t.accent.as_str(), t.name.as_str()),
            ("#FFFFFF", "My theme")
        );
    }

    #[test]
    fn names_are_cleaned_and_unique() {
        assert_eq!(clean_name("  hi\u{7}  "), "hi");
        assert_eq!(clean_name(&"a".repeat(100)).chars().count(), MAX_NAME_CHARS);
        let mut s = Store::default();
        s.upsert(sample()).expect("room");
        assert_eq!(s.unique_name("Test"), "Test 2");
        assert_eq!(s.duplicate("Test").as_deref(), Some("Test copy"));
        assert_eq!(s.duplicate("Test").as_deref(), Some("Test copy 2"));
    }

    #[test]
    fn rename_and_delete_keep_active_consistent() {
        let mut s = Store::default();
        s.upsert(sample()).expect("room");
        s.upsert(CustomTheme::new("Other", "#111111", "#222222"))
            .expect("room");
        s.active = Some("Test".into());
        assert!(s.rename("Test", "Other").is_err());
        assert_eq!(s.rename("Test", "Renamed").as_deref(), Ok("Renamed"));
        assert_eq!(s.active.as_deref(), Some("Renamed"));
        assert!(s.delete("Renamed"));
        assert_eq!(s.active, None);
        assert!(!s.delete("Renamed"));
    }

    #[test]
    fn store_round_trips_on_disk_and_survives_garbage() {
        let dir = std::env::temp_dir().join(format!("slate-ct-{}", std::process::id()));
        let path = dir.join("custom_themes.json");
        let mut s = Store::default();
        s.upsert(sample()).expect("room");
        s.save(&path).expect("save");
        assert_eq!(Store::load(&path), s);
        std::fs::write(&path, "{broken").expect("write");
        assert_eq!(Store::load(&path), Store::default());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_list_is_bounded() {
        let mut s = Store::default();
        for i in 0..MAX_THEMES {
            s.upsert(CustomTheme::new(&format!("t{i}"), "#111111", "#222222"))
                .expect("room");
        }
        assert!(s
            .upsert(CustomTheme::new("one more", "#111111", "#222222"))
            .is_err());
        assert!(s
            .upsert(CustomTheme::new("t1", "#333333", "#222222"))
            .is_ok());
    }
}
