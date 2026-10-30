//! Rebindable shortcuts: the live keymap, key-event resolution for the window, the capture
//! flow in Settings → Shortcuts, and pushing the bindings to the sheet and global keys.

use std::sync::Mutex;

use slate_core::keymap::{self, Combo, Keymap, Scope, ACTIONS};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::backend::Backend;
use crate::bootstrap::{AppWindow, Shortcut, ShortcutRow, Shortcuts};
use crate::uiprefs::UiPrefs;

static KEYMAP: Mutex<Keymap> = Mutex::new(Keymap::new());

pub fn init(prefs: &UiPrefs) {
    if let Ok(mut k) = KEYMAP.lock() {
        *k = Keymap::from_overrides(&prefs.keymap);
    }
}

pub fn combo(id: &str) -> Option<Combo> {
    KEYMAP.lock().ok()?.combo(id)
}

/// The in-app action for a key event, or "".
pub fn resolve(text: &str, ctrl: bool, alt: bool, shift: bool, meta: bool) -> String {
    let Some(c) = Combo::from_event(text, ctrl, alt, shift, meta) else {
        return String::new();
    };
    KEYMAP
        .lock()
        .ok()
        .and_then(|k| k.resolve(&c).map(str::to_string))
        .unwrap_or_default()
}

/// Rows for Settings → Shortcuts.
pub fn rows() -> Vec<(String, String, String, bool, bool)> {
    let Ok(k) = KEYMAP.lock() else { return vec![] };
    ACTIONS
        .iter()
        .map(|a| {
            (
                a.id.to_string(),
                a.label.to_string(),
                k.combo(a.id).map(|c| c.display()).unwrap_or_default(),
                k.is_custom(a.id),
                a.scope == Scope::Global,
            )
        })
        .collect()
}

/// Lines for the `?` sheet: every in-app binding plus the fixed Esc.
pub fn sheet() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = rows()
        .into_iter()
        .filter(|r| !r.4)
        .map(|r| (r.2, r.1))
        .collect();
    out.push(("Esc".into(), "Leave full screen / close this sheet".into()));
    out
}

/// Pushes the bindings to the window. UI thread.
pub fn apply_ui(w: &AppWindow) {
    let model: Vec<ShortcutRow> = rows()
        .into_iter()
        .map(|(id, label, combo, custom, global)| ShortcutRow {
            id: id.into(),
            label: label.into(),
            combo: combo.into(),
            custom,
            global,
        })
        .collect();
    w.global::<crate::bootstrap::SettingsState>()
        .set_shortcut_rows(ModelRc::new(VecModel::from(model)));
    w.global::<crate::bootstrap::SettingsState>()
        .set_global_shortcuts_hint(crate::platform::global_keys::describe().into());
    let list: Vec<Shortcut> = sheet()
        .into_iter()
        .map(|(k, a)| Shortcut {
            keys: SharedString::from(k),
            action: SharedString::from(a),
        })
        .collect();
    w.global::<Shortcuts>()
        .set_list(ModelRc::new(VecModel::from(list)));
}

impl Backend {
    fn save_keymap(&self) {
        let overrides = KEYMAP
            .lock()
            .map(|k| k.overrides().clone())
            .unwrap_or_default();
        let mut prefs = UiPrefs::load();
        prefs.keymap = overrides;
        if let Err(e) = prefs.save() {
            tracing::warn!("could not save shortcuts: {e}");
        }
    }

    /// Settings click: wait for the next combo for `id`.
    pub(crate) fn begin_capture(&self, id: &str) {
        let id = id.to_string();
        self.ui(move |w| {
            let s = w.global::<crate::bootstrap::SettingsState>();
            s.set_capture_warning("".into());
            s.set_capturing(id.into());
        });
    }

    /// A key arrived while recording.
    pub fn capture_key(&self, text: &str, ctrl: bool, alt: bool, shift: bool, meta: bool) {
        let Some(combo) = Combo::from_event(text, ctrl, alt, shift, meta) else {
            return; // a modifier on its own: keep waiting
        };
        let this = self.clone();
        self.ui(move |w| {
            let s = w.global::<crate::bootstrap::SettingsState>();
            let id = s.get_capturing().to_string();
            let result = KEYMAP
                .lock()
                .map_err(|_| "Shortcuts are unavailable".to_string())
                .and_then(|mut k| {
                    k.set(&id, &combo).map_err(|c| {
                        format!(
                            "{} is already used by “{}”",
                            combo.display(),
                            c.action.label
                        )
                    })
                });
            match result {
                Ok(()) => {
                    s.set_capturing("".into());
                    s.set_capture_warning("".into());
                    this.save_keymap();
                    apply_ui(&w);
                    if keymap::action(&id).is_some_and(|a| a.scope == Scope::Global) {
                        this.reload_global_shortcuts();
                    }
                }
                Err(msg) => s.set_capture_warning(msg.into()),
            }
        });
    }

    pub(crate) fn reset_shortcut(&self, id: Option<&str>) {
        if let Ok(mut k) = KEYMAP.lock() {
            match id {
                Some(id) => k.reset(id),
                None => k.reset_all(),
            }
        }
        self.save_keymap();
        self.ui(|w| apply_ui(&w));
        self.reload_global_shortcuts();
    }

    /// Re-registers the system-wide shortcuts with the current bindings.
    fn reload_global_shortcuts(&self) {
        if UiPrefs::load().global_shortcuts {
            self.set_global_shortcuts(true);
        }
    }
}
