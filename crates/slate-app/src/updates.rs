//! The "Slate X.Y.Z is available" notice. Looks at Slate's latest GitHub
//! release at most once a day, through the app's proxied HTTP client, and only
//! ever offers a link to the release page; nothing is downloaded or installed.

use std::sync::atomic::{AtomicBool, Ordering};

use slate_core::commands::updates;

use crate::backend::Backend;
use crate::uiprefs::UiPrefs;

const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// One check per run, however often the main screen is entered.
static CHECKED_THIS_RUN: AtomicBool = AtomicBool::new(false);

/// A day has passed since the last attempt (or the clock went backwards).
fn due(prefs: &UiPrefs, now: u64) -> bool {
    prefs.check_updates
        && (prefs.last_update_check > now || now - prefs.last_update_check >= CHECK_INTERVAL_SECS)
}

/// The notice to show, if any: a known newer version the user has not closed.
fn notice(prefs: &UiPrefs, current: &str) -> Option<(String, String)> {
    let (version, url) = prefs.known_update.as_ref()?;
    let newer = updates::is_update_available(current, version);
    (prefs.check_updates && newer && prefs.dismissed_update != *version)
        .then(|| (version.clone(), url.clone()))
}

impl Backend {
    /// Called when the main screen opens.
    pub(crate) fn check_for_update(&self) {
        if CHECKED_THIS_RUN.swap(true, Ordering::SeqCst) {
            return;
        }
        let this = self.clone();
        self.rt.spawn(async move {
            let current = env!("CARGO_PKG_VERSION");
            let mut prefs = UiPrefs::load();
            if !prefs.check_updates {
                return;
            }
            // A notice found earlier stays until it is closed or installed.
            this.show_update(notice(&prefs, current));

            let now = slate_core::now_secs();
            if !due(&prefs, now) {
                return;
            }
            // Record the attempt first: a failing network is not retried
            // until tomorrow either.
            prefs.last_update_check = now;
            if let Err(e) = prefs.save() {
                tracing::debug!("update check: could not save preferences: {e}");
            }
            let Some(state) = this.state() else { return };
            match updates::check_for_update(&state).await {
                Ok(info) => {
                    let mut prefs = UiPrefs::load();
                    prefs.known_update = info
                        .available
                        .then(|| (info.latest.clone(), info.url.clone()));
                    if let Err(e) = prefs.save() {
                        tracing::debug!("update check: could not save preferences: {e}");
                    }
                    this.show_update(notice(&prefs, current));
                }
                Err(e) => tracing::debug!("update check failed: {}", e.log_safe()),
            }
        });
    }

    fn show_update(&self, notice: Option<(String, String)>) {
        let text = notice
            .as_ref()
            .map(|(v, _)| format!("Slate {v} is available"))
            .unwrap_or_default();
        if let Ok(mut slot) = self.shared.update.lock() {
            *slot = notice;
        }
        self.ui(move |w| w.set_update_notice(text.into()));
    }

    pub(crate) fn open_update(&self) {
        let url = self
            .shared
            .update
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|(_, u)| u.clone()));
        let Some(url) = url.filter(|u| updates::is_release_url(u)) else {
            return;
        };
        self.rt.spawn_blocking(move || {
            if let Err(e) = open::that(&url) {
                tracing::warn!("could not open browser: {e}");
            }
        });
    }

    pub(crate) fn dismiss_update(&self) {
        let version = self
            .shared
            .update
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|(v, _)| v.clone()));
        if let Some(version) = version {
            let mut prefs = UiPrefs::load();
            prefs.dismissed_update = version;
            if let Err(e) = prefs.save() {
                tracing::warn!("could not remember the dismissed update: {e}");
            }
        }
        self.show_update(None);
    }

    /// Settings → General toggle. Turning it off hides any notice on screen.
    pub(crate) fn set_check_updates(&self, enabled: bool) -> Result<(), String> {
        let mut prefs = UiPrefs::load();
        prefs.check_updates = enabled;
        let saved = prefs.save().map_err(|e| e.to_string());
        if !enabled {
            self.show_update(None);
        }
        saved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefs(known: Option<(&str, &str)>, dismissed: &str) -> UiPrefs {
        UiPrefs {
            known_update: known.map(|(v, u)| (v.to_string(), u.to_string())),
            dismissed_update: dismissed.to_string(),
            ..UiPrefs::default()
        }
    }

    #[test]
    fn checks_at_most_once_a_day_and_never_when_off() {
        let mut p = UiPrefs::default();
        assert!(due(&p, 1_000_000));
        p.last_update_check = 1_000_000;
        assert!(!due(&p, 1_000_000 + CHECK_INTERVAL_SECS - 1));
        assert!(due(&p, 1_000_000 + CHECK_INTERVAL_SECS));
        // A clock that moved backwards must not block checks for days.
        assert!(due(&p, 500_000));
        p.check_updates = false;
        assert!(!due(&p, 9_000_000));
    }

    #[test]
    fn a_dismissed_version_stays_hidden_but_a_newer_one_shows() {
        let known = Some((
            "0.4.0",
            "https://github.com/legendarylolo318-cloud/slate/releases",
        ));
        assert!(notice(&prefs(known, ""), "0.3.0").is_some());
        assert!(notice(&prefs(known, "0.4.0"), "0.3.0").is_none());
        let newer = Some((
            "0.5.0",
            "https://github.com/legendarylolo318-cloud/slate/releases",
        ));
        assert!(notice(&prefs(newer, "0.4.0"), "0.3.0").is_some());
    }

    #[test]
    fn an_installed_or_older_version_is_not_announced() {
        let url = "https://github.com/legendarylolo318-cloud/slate/releases";
        assert!(notice(&prefs(Some(("0.4.0", url)), ""), "0.4.0").is_none());
        assert!(notice(&prefs(Some(("0.4.0", url)), ""), "0.5.0").is_none());
        assert!(notice(&prefs(Some(("0.5.0-rc.1", url)), ""), "0.3.0").is_none());
        assert!(notice(&prefs(Some(("garbage", url)), ""), "0.3.0").is_none());
        assert!(notice(&prefs(None, ""), "0.3.0").is_none());
    }

    #[test]
    fn turning_the_check_off_hides_a_known_notice() {
        let url = "https://github.com/legendarylolo318-cloud/slate/releases";
        let mut p = prefs(Some(("0.4.0", url)), "");
        p.check_updates = false;
        assert!(notice(&p, "0.3.0").is_none());
    }

    #[test]
    fn old_preference_files_default_to_checking() {
        let p = UiPrefs::parse(r#"{"scale_index": 2}"#);
        assert!(p.check_updates);
        assert_eq!(p.last_update_check, 0);
        assert!(p.known_update.is_none());
    }
}
