//! Rebindable keyboard shortcuts: the action list with defaults, key combos (parse, format,
//! capture from a key event), conflict detection and overrides. No UI here.

use std::collections::BTreeMap;

/// Where a shortcut is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Only while a Slate window has focus.
    App,
    /// System-wide media keys.
    Global,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Action {
    pub id: &'static str,
    pub label: &'static str,
    pub default: &'static str,
    pub scope: Scope,
}

const fn app(id: &'static str, label: &'static str, default: &'static str) -> Action {
    Action {
        id,
        label,
        default,
        scope: Scope::App,
    }
}

const fn global(id: &'static str, label: &'static str, default: &'static str) -> Action {
    Action {
        id,
        label,
        default,
        scope: Scope::Global,
    }
}

pub const ACTIONS: &[Action] = &[
    app("play-pause", "Play / pause", "Space"),
    app("seek-back", "Seek back 5 s", "Left"),
    app("seek-forward", "Seek forward 5 s", "Right"),
    app("previous", "Previous track", "Ctrl+Left"),
    app("next", "Next track", "Ctrl+Right"),
    app("mute", "Mute", "M"),
    app("shuffle", "Shuffle", "S"),
    app("repeat", "Repeat (off, all, one)", "R"),
    app("queue", "Queue panel", "Q"),
    app("lyrics", "Lyrics panel", "L"),
    app("fullscreen", "Full-screen player", "F"),
    app("search", "Search", "/"),
    app("shortcuts", "Show the shortcut sheet", "?"),
    app("filter", "Filter this page", "Ctrl+F"),
    global("g-play-pause", "Play / pause (global)", "Ctrl+Alt+Space"),
    global("g-next", "Next track (global)", "Ctrl+Alt+Right"),
    global("g-previous", "Previous track (global)", "Ctrl+Alt+Left"),
];

pub fn action(id: &str) -> Option<&'static Action> {
    ACTIONS.iter().find(|a| a.id == id)
}

/// A key plus modifiers. `key` is a name from [`key_name`] or one lowercase character.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Combo {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
    pub key: String,
}

const NAMED: &[(&str, &str)] = &[
    ("Space", " "),
    ("Left", "\u{F702}"),
    ("Right", "\u{F703}"),
    ("Up", "\u{F700}"),
    ("Down", "\u{F701}"),
    ("Enter", "\n"),
    ("Tab", "\t"),
    ("Backspace", "\u{8}"),
    ("Delete", "\u{7f}"),
    ("Home", "\u{F729}"),
    ("End", "\u{F72B}"),
    ("PageUp", "\u{F72C}"),
    ("PageDown", "\u{F72D}"),
];

/// Slint puts modifier keys themselves in `text` when they are pressed alone.
fn is_modifier_key(text: &str) -> bool {
    matches!(
        text,
        "\u{10}"
            | "\u{11}"
            | "\u{12}"
            | "\u{13}"
            | "\u{14}"
            | "\u{15}"
            | "\u{16}"
            | "\u{17}"
            | "\u{18}"
    )
}

/// Normalised key name for the text of a Slint key event; `None` for modifier-only or
/// non-printable keys we do not support (and Escape, which is never bindable).
pub fn key_name(text: &str) -> Option<String> {
    if text.is_empty() || is_modifier_key(text) || text == "\u{1b}" {
        return None;
    }
    if let Some((name, _)) = NAMED.iter().find(|(_, t)| *t == text) {
        return Some((*name).to_string());
    }
    // F1..F12 are U+F704..U+F70F.
    let mut chars = text.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    if let 0xF704..=0xF70F = c as u32 {
        return Some(format!("F{}", c as u32 - 0xF704 + 1));
    }
    if c.is_control() || (0xF700..=0xF8FF).contains(&(c as u32)) {
        return None;
    }
    Some(c.to_lowercase().collect())
}

impl Combo {
    /// From a key event. Shift is dropped for symbols (`?` already means Shift+/) and kept
    /// for letters, named keys and with other modifiers.
    pub fn from_event(text: &str, ctrl: bool, alt: bool, shift: bool, meta: bool) -> Option<Self> {
        let key = key_name(text)?;
        let single = key.chars().count() == 1;
        let letter = key.chars().next().is_some_and(char::is_alphabetic);
        let shift = if single && !letter { false } else { shift };
        // Plain uppercase letters arrive as text "S" with shift held.
        Some(Self {
            ctrl,
            alt,
            shift,
            meta,
            key,
        })
    }

    pub fn parse(s: &str) -> Option<Self> {
        let mut c = Self {
            ctrl: false,
            alt: false,
            shift: false,
            meta: false,
            key: String::new(),
        };
        // The key "+" is written "Ctrl++" or just "+".
        let (mods, key) = if s == "+" {
            ("", "+")
        } else if let Some(rest) = s.strip_suffix("++") {
            (rest, "+")
        } else {
            match s.rsplit_once('+') {
                Some((m, k)) => (m, k),
                None => ("", s),
            }
        };
        c.key = canonical_key(key)?;
        for m in mods.split('+').filter(|m| !m.is_empty()) {
            match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => c.ctrl = true,
                "alt" => c.alt = true,
                "shift" => c.shift = true,
                "meta" | "super" | "cmd" => c.meta = true,
                _ => return None,
            }
        }
        Some(c)
    }

    /// "Ctrl+Alt+Space".
    pub fn display(&self) -> String {
        let mut out = String::new();
        for (on, name) in [
            (self.ctrl, "Ctrl"),
            (self.alt, "Alt"),
            (self.shift, "Shift"),
            (self.meta, "Meta"),
        ] {
            if on {
                out.push_str(name);
                out.push('+');
            }
        }
        if self.key.chars().count() == 1 {
            out.extend(self.key.chars().flat_map(char::to_uppercase));
        } else {
            out.push_str(&self.key);
        }
        out
    }

    /// XDG GlobalShortcuts portal trigger ("CTRL+ALT+space").
    pub fn to_xdg(&self) -> String {
        let mut out = String::new();
        for (on, name) in [
            (self.ctrl, "CTRL+"),
            (self.alt, "ALT+"),
            (self.shift, "SHIFT+"),
            (self.meta, "LOGO+"),
        ] {
            if on {
                out.push_str(name);
            }
        }
        out.push_str(&match self.key.as_str() {
            "Space" => "space".to_string(),
            "Left" | "Right" | "Up" | "Down" | "Home" | "End" | "Enter" | "Tab" | "Backspace"
            | "Delete" => self.key.clone(),
            "PageUp" => "Page_Up".into(),
            "PageDown" => "Page_Down".into(),
            k => k.to_string(),
        });
        out
    }
}

fn canonical_key(p: &str) -> Option<String> {
    if let Some((name, _)) = NAMED.iter().find(|(n, _)| n.eq_ignore_ascii_case(p)) {
        return Some((*name).to_string());
    }
    let lower = p.to_ascii_lowercase();
    if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
        if (1..=12).contains(&n) {
            return Some(format!("F{n}"));
        }
    }
    let mut chars = p.chars();
    let c = chars.next()?;
    if chars.next().is_none() && !c.is_control() {
        return Some(c.to_lowercase().collect());
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub action: &'static Action,
}

/// Defaults plus the user's overrides (action id → combo string).
#[derive(Debug, Default, Clone)]
pub struct Keymap {
    overrides: BTreeMap<String, String>,
}

impl Keymap {
    pub const fn new() -> Self {
        Self {
            overrides: BTreeMap::new(),
        }
    }

    /// Unknown ids and unparsable combos are dropped.
    pub fn from_overrides(map: &BTreeMap<String, String>) -> Self {
        let overrides = map
            .iter()
            .filter(|(id, v)| action(id).is_some() && Combo::parse(v).is_some())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        Self { overrides }
    }

    pub fn overrides(&self) -> &BTreeMap<String, String> {
        &self.overrides
    }

    pub fn combo(&self, id: &str) -> Option<Combo> {
        let a = action(id)?;
        let s = self.overrides.get(id).map_or(a.default, String::as_str);
        Combo::parse(s)
    }

    pub fn is_custom(&self, id: &str) -> bool {
        self.overrides.contains_key(id)
    }

    /// The action in the same scope that `combo` is bound to, if any.
    pub fn find(&self, scope: Scope, combo: &Combo) -> Option<&'static Action> {
        ACTIONS
            .iter()
            .filter(|a| a.scope == scope)
            .find(|a| self.combo(a.id).as_ref() == Some(combo))
    }

    /// Binds `id` to `combo`, unless another action in the scope already uses it.
    pub fn set(&mut self, id: &str, combo: &Combo) -> Result<(), Conflict> {
        let a = action(id).ok_or(Conflict {
            action: &ACTIONS[0],
        })?;
        if let Some(other) = self.find(a.scope, combo) {
            if other.id != id {
                return Err(Conflict { action: other });
            }
        }
        if Combo::parse(a.default).as_ref() == Some(combo) {
            self.overrides.remove(id);
        } else {
            self.overrides.insert(id.to_string(), combo.display());
        }
        Ok(())
    }

    pub fn reset(&mut self, id: &str) {
        self.overrides.remove(id);
    }

    pub fn reset_all(&mut self) {
        self.overrides.clear();
    }

    /// The in-app action id for a key event.
    pub fn resolve(&self, combo: &Combo) -> Option<&'static str> {
        self.find(Scope::App, combo).map(|a| a.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(s: &str) -> Combo {
        Combo::parse(s).expect("combo")
    }

    #[test]
    fn every_default_parses_and_is_unique_in_its_scope() {
        let km = Keymap::new();
        for a in ACTIONS {
            let combo = km.combo(a.id).expect(a.id);
            assert_eq!(km.find(a.scope, &combo).map(|x| x.id), Some(a.id));
        }
    }

    #[test]
    fn parse_and_display_round_trip() {
        for s in [
            "Ctrl+Alt+Space",
            "Ctrl+Left",
            "M",
            "/",
            "?",
            "Ctrl+F",
            "Shift+F5",
            "Ctrl++",
        ] {
            assert_eq!(c(s).display(), s);
        }
        assert!(Combo::parse("Ctrl+").is_none());
        assert!(Combo::parse("").is_none());
        assert!(Combo::parse("Ctrl+Banana").is_none());
        assert_eq!(c("ctrl+space"), c("Ctrl+Space"));
    }

    #[test]
    fn events_become_combos() {
        assert_eq!(
            Combo::from_event(" ", false, false, false, false),
            Some(c("Space"))
        );
        assert_eq!(
            Combo::from_event("S", false, false, true, false),
            Some(c("Shift+S"))
        );
        // "?" arrives with shift held, but means the symbol itself.
        assert_eq!(
            Combo::from_event("?", false, false, true, false),
            Some(c("?"))
        );
        assert_eq!(
            Combo::from_event("\u{F703}", true, false, false, false),
            Some(c("Ctrl+Right"))
        );
        assert_eq!(
            Combo::from_event("\u{F704}", false, false, false, false),
            Some(c("F1"))
        );
        assert!(Combo::from_event("\u{11}", true, false, false, false).is_none());
        assert!(Combo::from_event("\u{1b}", false, false, false, false).is_none());
        assert!(Combo::from_event("", false, false, false, false).is_none());
    }

    #[test]
    fn conflicts_name_the_other_action() {
        let mut km = Keymap::new();
        let err = km.set("mute", &c("S")).expect_err("S is shuffle");
        assert_eq!(err.action.id, "shuffle");
        assert!(!km.is_custom("mute"));
        // Rebinding to its own current combo is fine.
        assert!(km.set("mute", &c("M")).is_ok());
    }

    #[test]
    fn scopes_do_not_clash() {
        let mut km = Keymap::new();
        assert!(km.set("g-play-pause", &c("Space")).is_ok());
        assert_eq!(km.resolve(&c("Space")), Some("play-pause"));
    }

    #[test]
    fn rebind_resolve_and_reset() {
        let mut km = Keymap::new();
        km.set("mute", &c("Ctrl+M")).expect("free");
        assert_eq!(km.resolve(&c("Ctrl+M")), Some("mute"));
        assert_eq!(km.resolve(&c("M")), None);
        assert!(km.is_custom("mute"));
        km.reset("mute");
        assert_eq!(km.resolve(&c("M")), Some("mute"));
        km.set("mute", &c("Ctrl+M")).expect("free");
        km.reset_all();
        assert!(km.overrides().is_empty());
    }

    #[test]
    fn setting_the_default_clears_the_override() {
        let mut km = Keymap::new();
        km.set("mute", &c("Ctrl+M")).expect("free");
        km.set("mute", &c("M")).expect("back");
        assert!(!km.is_custom("mute"));
    }

    #[test]
    fn bad_stored_overrides_are_dropped() {
        let mut m = BTreeMap::new();
        m.insert("mute".to_string(), "Ctrl+M".to_string());
        m.insert("nope".to_string(), "X".to_string());
        m.insert("queue".to_string(), "Ctrl+".to_string());
        let km = Keymap::from_overrides(&m);
        assert_eq!(km.overrides().len(), 1);
        assert_eq!(km.combo("queue"), Some(c("Q")));
    }

    #[test]
    fn xdg_triggers() {
        assert_eq!(c("Ctrl+Alt+Space").to_xdg(), "CTRL+ALT+space");
        assert_eq!(c("Ctrl+Alt+Right").to_xdg(), "CTRL+ALT+Right");
    }
}
