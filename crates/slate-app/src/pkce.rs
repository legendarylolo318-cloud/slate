//! Browser (PKCE) sign-in. TIDAL serves Lossless / Max quality only to this
//! login; the device-code login is capped at 320 kbps AAC.
//!
//! The user signs in in their browser, then pastes the address TIDAL ends on
//! (it carries a one-time `code`); Slate exchanges it for tokens.

use std::sync::Mutex;

use slate_core::commands::auth;

use crate::backend::Backend;
use crate::bootstrap::{Screen, SettingsState};
use slint::ComponentHandle;

/// (code verifier, client unique key) of the sign-in in progress.
pub type PendingPkce = Mutex<Option<(String, String)>>;

/// The `code` from a pasted redirect address, or the input itself when it is
/// just the bare code.
pub fn extract_code(pasted: &str) -> Option<String> {
    let s = pasted.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(i) = s.find("code=") {
        let tail = &s[i + 5..];
        let end = tail.find(['&', '#', ' ']).unwrap_or(tail.len());
        let code = &tail[..end];
        return (!code.is_empty()).then(|| code.to_string());
    }
    // A bare code: no URL characters.
    (!s.contains(['/', '?', ':', ' '])).then(|| s.to_string())
}

impl Backend {
    fn pkce_status(&self, text: impl Into<String>) {
        let text = text.into();
        self.ui(move |w| w.global::<SettingsState>().set_pkce_status(text.into()));
    }

    pub fn start_pkce(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            let params = match auth::start_pkce_browser_login() {
                Ok(p) => p,
                Err(e) => return this.pkce_status(format!("Browser sign-in unavailable: {}", e.log_safe())),
            };
            if let Ok(mut p) = this.pkce.lock() {
                *p = Some((params.code_verifier, params.client_unique_key));
            }
            let url = params.authorize_url;
            let shown = url.clone();
            this.ui(move |w| {
                let g = w.global::<SettingsState>();
                g.set_pkce_pending(true);
                g.set_pkce_input("".into());
                g.set_pkce_url(shown.into());
            });
            this.pkce_status(
                "Sign in in your browser (use Open browser or Copy link if it did not open). When it \\
                 finishes the page may look blank or show an error: copy its address from the \\
                 address bar and paste it here.",
            );
            this.open_in_browser(url);
        });
    }

    fn open_in_browser(&self, url: String) {
        let this = self.clone();
        self.rt.spawn_blocking(move || {
            if let Err(e) = open::that(&url) {
                tracing::warn!("could not open browser: {e}");
                this.pkce_status("Could not open a browser. Press Copy link and open it yourself.");
            }
        });
    }

    async fn pkce_url(&self) -> Option<String> {
        let url = self
            .read_ui(|w| w.global::<SettingsState>().get_pkce_url().to_string())
            .await;
        (!url.is_empty()).then_some(url)
    }

    pub async fn open_pkce_url(&self) {
        match self.pkce_url().await {
            Some(url) => self.open_in_browser(url),
            None => self.pkce_status("Start the browser sign-in first."),
        }
    }

    pub async fn copy_pkce_url(&self) {
        let Some(url) = self.pkce_url().await else {
            return self.pkce_status("Start the browser sign-in first.");
        };
        let result = tokio::task::spawn_blocking(move || {
            arboard::Clipboard::new().and_then(|mut c| c.set_text(url))
        })
        .await;
        match result {
            Ok(Ok(())) => self.pkce_status("Link copied. Paste it into your browser and sign in."),
            _ => self.pkce_status("Could not access the clipboard."),
        }
    }

    pub async fn finish_pkce(&self) {
        let pasted = self
            .read_ui(|w| w.global::<SettingsState>().get_pkce_input().to_string())
            .await;
        let Some(code) = extract_code(&pasted) else {
            return self
                .pkce_status("That does not contain a sign-in code. Paste the full address.");
        };
        let Some((verifier, key)) = self.pkce.lock().ok().and_then(|mut p| p.take()) else {
            return self.pkce_status("Start the browser sign-in first.");
        };
        match auth::complete_pkce_browser_login(self.app.clone(), code, verifier, key).await {
            Ok(_) => {
                self.ui(|w| {
                    let g = w.global::<SettingsState>();
                    g.set_pkce_pending(false);
                    g.set_pkce_input("".into());
                    g.set_pkce_status(
                        "Signed in. Lossless and Max quality are now available.".into(),
                    );
                    g.set_auth_max(true);
                });
                if self.in_main.load(std::sync::atomic::Ordering::Relaxed) {
                    self.notify("Browser sign-in complete: Lossless / Max unlocked");
                } else {
                    self.ui(|w| w.set_screen(Screen::Main));
                    self.enter_main();
                }
            }
            Err(e) => {
                // The one-time code is spent; a new attempt needs a new browser round trip.
                self.ui(|w| w.global::<SettingsState>().set_pkce_pending(false));
                self.pkce_status(format!(
                    "Sign-in failed: {}. Press the button to try again.",
                    e.log_safe()
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_the_code_from_pasted_addresses() {
        assert_eq!(
            extract_code("https://tidal.com/android/login/auth?code=abc.DEF-123&state=x"),
            Some("abc.DEF-123".into())
        );
        assert_eq!(
            extract_code("https://tidal.com/android/login/auth?foo=1&code=zzz"),
            Some("zzz".into())
        );
        assert_eq!(extract_code("  rawcode123  "), Some("rawcode123".into()));
    }

    #[test]
    fn rejects_input_without_a_code() {
        assert_eq!(extract_code(""), None);
        assert_eq!(extract_code("https://tidal.com/android/login/auth"), None);
        assert_eq!(extract_code("https://tidal.com/x?code="), None);
        assert_eq!(extract_code("two words"), None);
    }
}
