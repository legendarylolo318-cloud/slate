//! "Remove duplicates…" on a playlist you own: scan every page (progress shown), preview
//! the copies that would go, then remove them highest index first.

use std::sync::Mutex;
use std::time::Duration;

use slate_core::commands::{library as lib_cmds, metadata};
use slate_core::playlist_tools::{find_duplicates, removal_order, Duplicate};
use slate_core::tidal_api::TidalTrack;

use crate::backend::Backend;
use crate::library::SourceKind;
use crate::models::PAGE_SIZE;

const PAGE_PAUSE: Duration = Duration::from_millis(300);
/// Longest preview list; the rest is summarised.
const PREVIEW_ROWS: usize = 60;

/// The scan result the dialog is asking about: playlist uuid and the copies to remove.
static PENDING: Mutex<Option<(String, Vec<Duplicate>)>> = Mutex::new(None);

fn describe(tracks: &[TidalTrack], dups: &[Duplicate]) -> String {
    let name = |t: &TidalTrack| {
        let artist = t
            .artists
            .as_ref()
            .and_then(|a| a.first())
            .map(|a| a.name.as_str())
            .or_else(|| t.artist.as_ref().map(|a| a.name.as_str()))
            .unwrap_or("");
        format!("{artist} – {}", t.title)
    };
    let mut out = format!(
        "{} duplicate{} will be removed. The first copy of each track stays.\n\n",
        dups.len(),
        if dups.len() == 1 { "" } else { "s" }
    );
    for d in dups.iter().take(PREVIEW_ROWS) {
        if let Some(t) = tracks.get(d.index as usize) {
            out.push_str(&format!(
                "#{}  {}  (same as #{})\n",
                d.index + 1,
                name(t),
                d.kept_index + 1
            ));
        }
    }
    if dups.len() > PREVIEW_ROWS {
        out.push_str(&format!("…and {} more\n", dups.len() - PREVIEW_ROWS));
    }
    out
}

impl Backend {
    /// "Remove duplicates…" button.
    pub fn scan_duplicates(&self) {
        let Some(SourceKind::Playlist(uuid)) = self.lib.active_kind_pub() else {
            return;
        };
        let generation = self.lib.generation_now();
        let this = self.clone();
        self.rt.spawn(async move {
            if let Err(message) = this.scan(&uuid, generation).await {
                this.ui(move |w| {
                    w.set_notice("".into());
                    w.set_error(message.into());
                });
            }
        });
    }

    async fn scan(&self, uuid: &str, generation: u64) -> Result<(), String> {
        let state = self.state().ok_or("Not signed in")?;
        let user = self.user_id().await.ok_or("Could not tell who you are")?;
        let details = metadata::get_playlist_details(&state, uuid.to_string())
            .await
            .map_err(|e| e.log_safe())?;
        if details["creator"]["id"].as_u64() != Some(user) {
            return Err("You can only remove duplicates from playlists you own.".into());
        }
        let total = details["numberOfTracks"].as_u64().unwrap_or(0) as usize;
        let kind = SourceKind::Playlist(uuid.to_string());
        let mut tracks: Vec<TidalTrack> = Vec::with_capacity(total);
        for page in 0..total.div_ceil(PAGE_SIZE) {
            if self.lib.generation_now() != generation {
                return Err("Scan cancelled: the page changed.".into());
            }
            if page > 0 {
                tokio::time::sleep(PAGE_PAUSE).await;
            }
            let (items, _) = self.fetch_page_for_export(&kind, page).await?;
            tracks.extend(items);
            let text = format!("Looking for duplicates… {} of {total} tracks", tracks.len());
            self.ui(move |w| w.set_notice(text.into()));
        }
        if self.lib.generation_now() != generation {
            return Err("Scan cancelled: the page changed.".into());
        }
        let dups = find_duplicates(&tracks);
        if dups.is_empty() {
            self.notify("No duplicates found");
            return Ok(());
        }
        let preview = describe(&tracks, &dups);
        if let Ok(mut p) = PENDING.lock() {
            *p = Some((uuid.to_string(), dups));
        }
        let uuid = uuid.to_string();
        self.ui(move |w| {
            w.set_notice("".into());
            w.set_dialog_kind("dedupe".into());
            w.set_dialog_target(uuid.into());
            w.set_dialog_text(preview.into());
            w.set_dialog_open(true);
        });
        Ok(())
    }

    /// The dialog's "Remove" button.
    pub(crate) async fn run_dedupe(&self, uuid: &str) {
        let pending = PENDING.lock().ok().and_then(|mut p| p.take());
        let (Some(state), Some((planned, dups))) = (self.state(), pending) else {
            return;
        };
        if planned != uuid {
            return;
        }
        let order = removal_order(&dups);
        let total = order.len();
        let mut removed = 0usize;
        for (n, index) in order.into_iter().enumerate() {
            self.ui({
                let text = format!("Removing duplicates… {n} of {total}");
                move |w| w.set_notice(text.into())
            });
            match lib_cmds::remove_track_from_playlist(&state, uuid.to_string(), index).await {
                Ok(()) => removed += 1,
                Err(e) => {
                    let msg = format!(
                        "Removed {removed} of {total} duplicates, then failed: {}",
                        e.log_safe()
                    );
                    self.ui(move |w| {
                        w.set_notice("".into());
                        w.set_error(msg.into());
                    });
                    self.navigate_refresh(crate::pages::Route::Playlist(uuid.to_string()));
                    return;
                }
            }
        }
        self.ui(|w| w.set_notice("".into()));
        self.notify(&format!("Removed {removed} duplicates"));
        self.navigate_refresh(crate::pages::Route::Playlist(uuid.to_string()));
    }
}
