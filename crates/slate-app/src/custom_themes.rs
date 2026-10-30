//! Settings → Appearance → Your themes: saved custom themes, the editor with live preview,
//! contrast warnings, "generate from the current cover" and `.slatetheme` import / export.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use slate_core::custom_theme::{self as ct, CustomTheme, Store};
use slate_core::theme::Theme as CoreTheme;
use slate_core::theme_config;
use slint::{Color, ComponentHandle, ModelRc, SharedString, VecModel};

use crate::backend::Backend;
use crate::bootstrap::{AppWindow, RoleRow, SettingsState, Theme};
use crate::themes::color;

/// The custom theme currently styling the app, if the look is a custom one.
static ACTIVE: Mutex<Option<CustomTheme>> = Mutex::new(None);

/// An open editor: the draft, the saved name it edits (None: new) and what to restore on
/// cancel.
struct Draft {
    theme: CustomTheme,
    original: Option<String>,
    selected: String,
    restore: (CoreTheme, Option<CustomTheme>),
}

static DRAFT: Mutex<Option<Draft>> = Mutex::new(None);

fn store_path() -> Option<PathBuf> {
    slate_core::config_dir_for_env().map(|d| d.join("custom_themes.json"))
}

fn load_store() -> Store {
    store_path().map(|p| Store::load(&p)).unwrap_or_default()
}

fn save_store(s: &Store) -> Result<(), String> {
    let p = store_path().ok_or("No config directory")?;
    s.save(&p)
        .map_err(|e| format!("Could not save themes: {e}"))
}

/// Startup: the custom theme in use, provided `theme.json` still describes it.
pub fn init(current: &CoreTheme) {
    let store = load_store();
    let active = store
        .active
        .as_deref()
        .and_then(|n| store.get(n))
        .filter(|t| t.accent == current.accent && t.background == current.bg_base)
        .cloned();
    if let Ok(mut a) = ACTIVE.lock() {
        *a = active;
    }
}

/// The active custom theme's colours over the derived ones. UI thread.
pub fn overlay(w: &AppWindow, t: &CustomTheme) {
    let p = t.palette();
    let g = w.global::<Theme>();
    let c = |role: &str| p.get(role).map(|h| color(h));
    if let Some(v) = c("accent") {
        g.set_accent(v);
    }
    if let Some(v) = c("accent-hover") {
        g.set_accent_hover(v);
    }
    if let Some(v) = c("on-accent") {
        g.set_on_accent(v);
    }
    if let Some(v) = c("background") {
        g.set_background(v);
    }
    if let Some(v) = c("surface") {
        g.set_surface(v);
    }
    if let Some(v) = c("surface-raised") {
        g.set_surface_raised(v);
    }
    if let Some(v) = c("text") {
        g.set_text(v);
    }
    if let Some(v) = c("text-muted") {
        g.set_text_muted(v);
    }
    if let Some(v) = c("danger") {
        g.set_danger(v);
    }
}

/// Called at the end of `themes::apply`.
pub fn apply_active(w: &AppWindow) {
    let draft = DRAFT
        .lock()
        .ok()
        .and_then(|d| d.as_ref().map(|d| d.theme.clone()));
    let active = ACTIVE.lock().ok().and_then(|a| a.clone());
    if let Some(t) = draft.or(active) {
        overlay(w, &t);
    }
}

/// A preset or a picker edit replaces the custom look.
pub fn clear_active() {
    if let Ok(mut a) = ACTIVE.lock() {
        if a.take().is_some() {
            let mut s = load_store();
            s.active = None;
            let _ = save_store(&s);
        }
    }
}

fn hex_of(c: Color) -> String {
    format!("#{:02X}{:02X}{:02X}", c.red(), c.green(), c.blue())
}

fn core_of(t: &CustomTheme) -> CoreTheme {
    CoreTheme {
        name: theme_config::CUSTOM_PRESET.into(),
        accent: t.accent.clone(),
        bg_base: t.background.clone(),
    }
}

/// Pushes the list of saved themes and the editor state to Settings. UI thread.
pub fn sync(w: &AppWindow) {
    let s = w.global::<SettingsState>();
    let store = load_store();
    let active = ACTIVE
        .lock()
        .ok()
        .and_then(|a| a.as_ref().map(|t| t.name.clone()))
        .unwrap_or_default();
    let names: Vec<SharedString> = store.themes.iter().map(|t| t.name.clone().into()).collect();
    s.set_custom_themes(ModelRc::new(VecModel::from(names)));
    s.set_custom_active(active.into());
    let draft = DRAFT.lock().ok();
    let Some(d) = draft.as_deref().and_then(Option::as_ref) else {
        s.set_editing(false);
        return;
    };
    s.set_editing(true);
    s.set_edit_name(d.theme.name.clone().into());
    s.set_edit_role(d.selected.clone().into());
    let p = d.theme.palette();
    let rows: Vec<RoleRow> = ct::ROLES
        .iter()
        .map(|(id, label)| {
            let hex = p.get(id).cloned().unwrap_or_default();
            RoleRow {
                id: (*id).into(),
                label: (*label).into(),
                color: color(&hex),
                hex: hex.into(),
                custom: d.theme.overrides.contains_key(*id),
            }
        })
        .collect();
    s.set_edit_roles(ModelRc::new(VecModel::from(rows)));
    s.set_edit_picker(
        p.get(d.selected.as_str())
            .map(|h| color(h))
            .unwrap_or_default(),
    );
    let warnings: Vec<String> = ct::contrast_warnings(&p)
        .iter()
        .map(|c| {
            format!(
                "{} on {} has contrast {:.1}:1 (needs 4.5:1)",
                ct::role_label(c.text_role),
                ct::role_label(c.on_role).to_lowercase(),
                c.ratio
            )
        })
        .collect();
    s.set_edit_warnings(warnings.join("\n").into());
}

impl Backend {
    fn refresh_custom_ui(&self) {
        let t = self.current_theme_pub();
        self.ui(move |w| {
            crate::themes::apply(&w, &t);
            sync(&w);
        });
        self.reapply_tint();
    }

    fn current_custom(&self) -> Option<CustomTheme> {
        ACTIVE.lock().ok().and_then(|a| a.clone())
    }

    /// Makes `t` the look of the app and remembers it.
    fn use_custom(&self, t: CustomTheme) {
        let mut store = load_store();
        store.active = Some(t.name.clone());
        if let Err(e) = save_store(&store) {
            self.settings_status(e);
        }
        if let Ok(mut a) = ACTIVE.lock() {
            *a = Some(t.clone());
        }
        // Keeps theme.json (and the file watcher) describing the same accent and background.
        self.set_theme_quiet(core_of(&t));
        self.refresh_custom_ui();
    }

    pub(crate) fn custom_theme_action(&self, action: &str) {
        let (verb, arg) = action.split_once(':').unwrap_or((action, ""));
        match verb {
            "theme-new" => {
                let cur = self.current_theme_pub();
                let mut t = CustomTheme::new("My theme", &cur.accent, &cur.bg_base);
                if let Some(a) = self.current_custom() {
                    t.overrides = a.overrides;
                }
                t.name = load_store().unique_name("My theme");
                self.open_editor(t, None);
            }
            "theme-edit" => {
                if let Some(t) = load_store().get(arg).cloned() {
                    self.open_editor(t, Some(arg.to_string()));
                }
            }
            "theme-apply" => {
                if let Some(t) = load_store().get(arg).cloned() {
                    self.use_custom(t);
                }
            }
            "theme-duplicate" => {
                let mut s = load_store();
                if s.duplicate(arg).is_some() {
                    self.report(save_store(&s));
                }
                self.refresh_custom_ui();
            }
            "theme-delete" => {
                let mut s = load_store();
                if s.delete(arg) {
                    if self.current_custom().is_some_and(|t| t.name == arg) {
                        if let Ok(mut a) = ACTIVE.lock() {
                            *a = None;
                        }
                    }
                    self.report(save_store(&s));
                }
                self.refresh_custom_ui();
            }
            "theme-export" => self.export_theme(arg),
            "theme-import" => self.import_theme_dialog(),
            "theme-role" => {
                if let Ok(mut d) = DRAFT.lock() {
                    if let Some(d) = d.as_mut() {
                        d.selected = arg.to_string();
                    }
                }
                self.ui(|w| sync(&w));
            }
            "theme-role-reset" => {
                self.edit_draft(|t| t.reset_role(arg));
            }
            "theme-cover" => self.generate_from_cover(),
            "theme-save" => self.save_draft(),
            "theme-cancel" => self.cancel_draft(),
            _ => {}
        }
    }

    fn report(&self, r: Result<(), String>) {
        if let Err(e) = r {
            self.settings_status(e);
        }
    }

    fn open_editor(&self, t: CustomTheme, original: Option<String>) {
        let restore = (self.current_theme_pub(), self.current_custom());
        if let Ok(mut d) = DRAFT.lock() {
            *d = Some(Draft {
                selected: "accent".into(),
                theme: t,
                original,
                restore,
            });
        }
        self.refresh_custom_ui();
    }

    /// Applies `f` to the draft and shows the result live.
    fn edit_draft(&self, f: impl FnOnce(&mut CustomTheme)) {
        if let Ok(mut d) = DRAFT.lock() {
            if let Some(d) = d.as_mut() {
                f(&mut d.theme);
            }
        }
        self.refresh_custom_ui();
    }

    /// Colour-picker drag (live) for the selected role.
    pub fn edit_color(&self, role: &str, c: Color) {
        let hex = hex_of(c);
        self.edit_draft(|t| {
            let _ = t.set_role(role, &hex);
        });
    }

    pub fn edit_hex(&self, role: &str, text: &str) {
        let mut err = None;
        self.edit_draft(|t| {
            if let Err(e) = t.set_role(role, text) {
                err = Some(e);
            }
        });
        if let Some(e) = err {
            self.ui(move |w| w.set_error(e.into()));
        }
    }

    pub fn edit_name(&self, name: &str) {
        let name = name.to_string();
        if let Ok(mut d) = DRAFT.lock() {
            if let Some(d) = d.as_mut() {
                d.theme.name = name.chars().take(ct::MAX_NAME_CHARS).collect();
            }
        }
    }

    fn generate_from_cover(&self) {
        let Some(px) = self.shared.cover_px.lock().ok().and_then(|p| p.clone()) else {
            self.settings_status("Play a track first; its cover is used for the colours.");
            return;
        };
        let this = self.clone();
        self.rt.spawn(async move {
            let found = tokio::task::spawn_blocking(move || crate::cover_tint::dominant(&px))
                .await
                .ok()
                .flatten();
            let Some(rgb) = found else {
                this.settings_status("Could not read a colour from this cover.");
                return;
            };
            let hex = format!("#{:02X}{:02X}{:02X}", rgb.0, rgb.1, rgb.2);
            match ct::from_cover_color(&hex) {
                Some((accent, background)) => this.edit_draft(|t| {
                    t.accent = accent;
                    t.background = background;
                    t.overrides.clear();
                }),
                None => this.settings_status("Could not read a colour from this cover."),
            }
        });
    }

    fn save_draft(&self) {
        let Some(d) = DRAFT.lock().ok().and_then(|mut d| d.take()) else {
            return;
        };
        let mut store = load_store();
        let mut t = d.theme;
        t.name = ct::clean_name(&t.name);
        if let Some(orig) = &d.original {
            if *orig != t.name {
                if let Err(e) = store.rename(orig, &t.name) {
                    // Name taken: keep editing.
                    if let Ok(mut slot) = DRAFT.lock() {
                        *slot = Some(Draft {
                            theme: t,
                            original: d.original,
                            selected: d.selected,
                            restore: d.restore,
                        });
                    }
                    self.ui(move |w| w.set_error(e.into()));
                    return;
                }
            }
        } else if store.get(&t.name).is_some() {
            t.name = store.unique_name(&t.name);
        }
        if let Err(e) = store.upsert(t.clone()) {
            self.settings_status(e);
        }
        if let Err(e) = save_store(&store) {
            self.settings_status(e);
        }
        self.use_custom(t);
    }

    fn cancel_draft(&self) {
        let Some(d) = DRAFT.lock().ok().and_then(|mut d| d.take()) else {
            return;
        };
        let (core, custom) = d.restore;
        if let Ok(mut a) = ACTIVE.lock() {
            *a = custom;
        }
        self.set_theme_quiet(core);
        self.refresh_custom_ui();
    }

    fn export_theme(&self, name: &str) {
        let Some(t) = load_store().get(name).cloned() else {
            return;
        };
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(file) = rfd::AsyncFileDialog::new()
                .set_title("Export theme")
                .set_file_name(crate::export::named(&t.name, "slatetheme"))
                .add_filter("Slate theme", &["slatetheme"])
                .save_file()
                .await
            else {
                return;
            };
            let path = file.path().to_path_buf();
            let text = ct::export(&t);
            match tokio::fs::write(&path, text).await {
                Ok(()) => this.settings_status(format!("Exported to {}", path.display())),
                Err(e) => this.settings_status(format!("Could not save the theme: {e}")),
            }
        });
    }

    fn import_theme_dialog(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(file) = rfd::AsyncFileDialog::new()
                .set_title("Import theme")
                .add_filter("Slate theme", &["slatetheme"])
                .pick_file()
                .await
            else {
                return;
            };
            this.import_theme_path(file.path().to_path_buf());
        });
    }

    /// From the file picker or a file dropped on the window.
    pub fn import_theme_path(&self, path: PathBuf) {
        let this = self.clone();
        self.rt.spawn(async move {
            match read_limited(&path).await.and_then(|s| ct::import(&s)) {
                Ok(t) => {
                    let mut store = load_store();
                    let mut t = t;
                    t.name = store.unique_name(&t.name);
                    let name = t.name.clone();
                    if let Err(e) = store.upsert(t.clone()) {
                        this.settings_status(e);
                        return;
                    }
                    if let Err(e) = save_store(&store) {
                        this.settings_status(e);
                        return;
                    }
                    this.use_custom(t);
                    this.settings_status(format!("Imported “{name}”"));
                }
                Err(e) => this.settings_status(e),
            }
        });
    }
}

/// Reads at most `MAX_FILE_BYTES + 1` bytes so a huge file is never loaded.
async fn read_limited(path: &Path) -> Result<String, String> {
    use tokio::io::AsyncReadExt;
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|e| format!("Could not open the file: {e}"))?;
    let mut buf = Vec::new();
    file.take(ct::MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut buf)
        .await
        .map_err(|e| format!("Could not read the file: {e}"))?;
    String::from_utf8(buf).map_err(|_| "That is not a valid Slate theme file.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use slate_core::theme;

    #[test]
    fn core_theme_carries_the_base_colours() {
        let t = CustomTheme::new("x", "#111111", "#222222");
        let c = core_of(&t);
        assert_eq!(
            (c.accent.as_str(), c.bg_base.as_str()),
            ("#111111", "#222222")
        );
        assert!(theme::normalize_hex(&c.accent).is_some());
    }

    #[tokio::test]
    async fn oversized_files_are_cut_off_and_rejected() {
        let path =
            std::env::temp_dir().join(format!("slate-big-{}.slatetheme", std::process::id()));
        std::fs::write(&path, "x".repeat(ct::MAX_FILE_BYTES * 4)).expect("write");
        let text = read_limited(&path).await.expect("read");
        assert_eq!(text.len(), ct::MAX_FILE_BYTES + 1);
        assert!(ct::import(&text).is_err());
        let _ = std::fs::remove_file(path);
    }
}
