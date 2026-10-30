//! Settings → Integrations → Discord, and the per-track context the presence needs: which
//! playlist is playing, and whether it must stay off Discord.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use slate_core::commands::{metadata, utility};
use slate_core::discord_template::{self, Fields};
use slate_core::queue::Track;

use crate::backend::Backend;
use crate::bootstrap::SettingsState;
use slint::ComponentHandle;

/// Whether a playlist is public, remembered for the session (looked up only while the
/// "hide private playlists" option is on).
static PUBLIC: Mutex<Option<HashMap<String, bool>>> = Mutex::new(None);
/// Latest track change wins when several arrive while the playlist is looked up.
static TICKET: AtomicU64 = AtomicU64::new(0);

pub(crate) fn next_ticket() -> u64 {
    TICKET.fetch_add(1, Ordering::SeqCst) + 1
}

pub(crate) fn is_latest(ticket: u64) -> bool {
    TICKET.load(Ordering::SeqCst) == ticket
}

/// The presence's `{quality}` text.
pub fn quality_text(tier: i32, atmos: bool) -> String {
    let base = match tier {
        2 => "Hi-Res Lossless",
        1 => "Lossless",
        _ => "",
    };
    match (base.is_empty(), atmos) {
        (_, false) => base.to_string(),
        (true, true) => "Dolby Atmos".into(),
        (false, true) => format!("{base} · Dolby Atmos"),
    }
}

/// Whether a playlist must stay off Discord, given the options and what is known about it.
pub fn hide_playlist(
    hidden_list: &[String],
    uuid: &str,
    hide_private: bool,
    public: Option<bool>,
) -> bool {
    hidden_list.iter().any(|u| u == uuid) || (hide_private && public != Some(true))
}

impl Backend {
    /// (playlist name, hidden) for the track that is starting, from the queue's source.
    pub(crate) async fn discord_context(&self) -> (String, bool) {
        let (Some(state), Some(ctl)) = (self.state(), self.controller()) else {
            return (String::new(), false);
        };
        let source = ctl.with_queue(|q| q.playback_source.clone()).await;
        let Some(src) = source.filter(|s| s.kind == "playlist") else {
            return (String::new(), false);
        };
        let uuid = src.id.as_str().unwrap_or_default().to_string();
        let prefs = utility::get_discord_prefs(&state);
        let known = PUBLIC
            .lock()
            .ok()
            .and_then(|m| m.as_ref()?.get(&uuid).copied());
        let public = match known {
            Some(p) => Some(p),
            None if prefs.hide_private => {
                let looked = metadata::get_playlist_details(&state, uuid.clone())
                    .await
                    .ok()
                    .and_then(|d| d["publicPlaylist"].as_bool());
                if let (Some(p), Ok(mut m)) = (looked, PUBLIC.lock()) {
                    m.get_or_insert_with(HashMap::new).insert(uuid.clone(), p);
                }
                looked
            }
            None => None,
        };
        let hidden = hide_playlist(&prefs.hidden_playlists, &uuid, prefs.hide_private, public);
        (src.name, hidden)
    }

    /// Year of a track's album, for `{year}`.
    pub(crate) fn year_of(track: &Track) -> String {
        track
            .extra
            .get("album")
            .and_then(|a| a.get("releaseDate"))
            .and_then(|v| v.as_str())
            .and_then(|d| d.get(..4))
            .unwrap_or("")
            .to_string()
    }

    // ---- settings ----

    pub(crate) fn discord_template_edited(&self, which: &str, text: &str) {
        let (which, text) = (which.to_string(), text.to_string());
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            let cur = utility::get_discord_prefs(&state);
            let (d, s) = if which == "details" {
                (text, cur.state)
            } else {
                (cur.details, text)
            };
            if let Err(e) = utility::set_discord_templates(&state, d, s) {
                this.settings_status(format!("Could not save: {}", e.log_safe()));
            }
            this.ui(|w| refresh_preview(&w));
        });
    }

    pub(crate) fn discord_reset_templates(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            let _ = utility::set_discord_templates(&state, String::new(), String::new());
            this.ui(move |w| {
                let s = w.global::<SettingsState>();
                s.set_discord_details("".into());
                s.set_discord_state("".into());
                refresh_preview(&w);
            });
        });
    }

    pub(crate) fn discord_hide_private(&self, on: bool) -> Result<(), String> {
        let state = self.state().ok_or("Not ready")?;
        utility::set_discord_hide_private(&state, on).map_err(|e| e.log_safe())
    }

    /// Right-click a playlist → "Hide from Discord" / "Show on Discord".
    pub fn toggle_playlist_discord_hidden(&self, uuid: &str) {
        let uuid = uuid.to_string();
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            let now_hidden = !utility::get_discord_prefs(&state)
                .hidden_playlists
                .contains(&uuid);
            match utility::set_discord_playlist_hidden(&state, &uuid, now_hidden) {
                Ok(()) => this.notify(if now_hidden {
                    "This playlist is now hidden from Discord"
                } else {
                    "This playlist can show on Discord again"
                }),
                Err(e) => this.settings_status(format!("Could not save: {}", e.log_safe())),
            }
        });
    }
}

/// Live preview with an example track.
pub(crate) fn refresh_preview(w: &crate::bootstrap::AppWindow) {
    let s = w.global::<SettingsState>();
    let f = Fields {
        title: "Halo",
        artist: "Beyoncé",
        album: "I Am... Sasha Fierce",
        quality: "Hi-Res Lossless",
        playlist: "Road Trip",
        year: "2008",
    };
    let (d, st) = discord_template::lines(
        &discord_template::clean_template(&s.get_discord_details()),
        &discord_template::clean_template(&s.get_discord_state()),
        &f,
    );
    s.set_discord_preview(format!("{}\n{}", d.unwrap_or_default(), st.unwrap_or_default()).into());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_text_names_the_tiers() {
        assert_eq!(quality_text(0, false), "");
        assert_eq!(quality_text(1, false), "Lossless");
        assert_eq!(quality_text(2, true), "Hi-Res Lossless · Dolby Atmos");
        assert_eq!(quality_text(0, true), "Dolby Atmos");
    }

    #[test]
    fn hiding_rules() {
        let list = vec!["abc".to_string()];
        assert!(hide_playlist(&list, "abc", false, Some(true)));
        assert!(!hide_playlist(&list, "xyz", false, None));
        assert!(!hide_playlist(&list, "xyz", true, Some(true)));
        assert!(hide_playlist(&list, "xyz", true, Some(false)));
        // Unknown visibility is treated as private.
        assert!(hide_playlist(&list, "xyz", true, None));
    }

    #[test]
    fn tickets_only_let_the_latest_through() {
        let a = next_ticket();
        let b = next_ticket();
        assert!(!is_latest(a));
        assert!(is_latest(b));
    }
}
