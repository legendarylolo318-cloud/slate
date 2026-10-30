//! "Other versions": explicit / clean, remasters, live, Dolby Atmos… of a track (right-click a
//! row) and of an album (button on the album page). Found by searching the primary artist plus
//! the title with version noise removed.

use std::sync::Mutex;

use serde_json::Value;
use slate_core::commands::{library as lib_cmds, metadata, versions};
use slate_core::queue::Track;
use slint::{ModelRc, SharedString, VecModel};

use crate::backend::Backend;
use crate::bootstrap::VersionRow;
use crate::library::SourceKind;
use crate::pages::Route;

/// What the open dialog is about.
struct Ctx {
    /// Row of the open playlist the track sits in, when the page is an owned playlist.
    playlist_row: Option<(String, usize)>,
    /// Position in the queue, when asked from the queue panel.
    queue_index: Option<usize>,
    album: bool,
    items: Vec<Value>,
}

static CTX: Mutex<Option<Ctx>> = Mutex::new(None);

/// One line under the title: year · quality · duration.
pub fn detail_line(v: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(y) = v["year"].as_i64() {
        parts.push(y.to_string());
    }
    if let Some(q) = v["quality"].as_str().filter(|q| !q.is_empty()) {
        parts.push(q.to_string());
    }
    if v["atmos"].as_bool() == Some(true) {
        parts.push("Dolby Atmos".into());
    }
    if v["explicit"].as_bool() == Some(true) {
        parts.push("E".into());
    }
    if let Some(d) = v["duration_seconds"].as_u64().filter(|d| *d > 0) {
        parts.push(format!("{}:{:02}", d / 60, d % 60));
    }
    parts.join(" · ")
}

impl Backend {
    /// Right-click → "Other versions…" on a track.
    pub(crate) fn show_track_versions(
        &self,
        track: Track,
        playlist_row: Option<usize>,
        queue_index: Option<usize>,
    ) {
        let playlist_row = match (self.lib.active_kind_pub(), playlist_row) {
            (Some(SourceKind::Playlist(u)), Some(i)) => Some((u, i)),
            _ => None,
        };
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            this.ui(|w| w.set_notice("Looking for other versions…".into()));
            match versions::track_versions(&state, track.id).await {
                Ok(items) if items.is_empty() => this.notify("No other versions found"),
                Ok(items) => {
                    let title = format!("Other versions of “{}”", track.title());
                    this.open_versions(
                        Ctx {
                            playlist_row,
                            queue_index,
                            album: false,
                            items,
                        },
                        title,
                    );
                }
                Err(e) => {
                    let msg = format!("Could not search for versions: {}", e.log_safe());
                    this.ui(move |w| {
                        w.set_notice("".into());
                        w.set_error(msg.into());
                    });
                }
            }
        });
    }

    /// "Other versions of this album" on an album page.
    pub fn show_album_versions(&self) {
        let Some(SourceKind::Album(id)) = self.lib.active_kind_pub() else {
            return;
        };
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            this.ui(|w| w.set_notice("Looking for other versions…".into()));
            match versions::album_versions(&state, id).await {
                Ok(items) if items.is_empty() => {
                    this.notify("No other versions of this album found")
                }
                Ok(items) => this.open_versions(
                    Ctx {
                        playlist_row: None,
                        queue_index: None,
                        album: true,
                        items,
                    },
                    "Other versions of this album".into(),
                ),
                Err(e) => {
                    let msg = format!("Could not search for versions: {}", e.log_safe());
                    this.ui(move |w| {
                        w.set_notice("".into());
                        w.set_error(msg.into());
                    });
                }
            }
        });
    }

    fn open_versions(&self, ctx: Ctx, title: String) {
        let rows: Vec<(String, String, String, String)> = ctx
            .items
            .iter()
            .map(|v| {
                (
                    v["id"].as_u64().unwrap_or(0).to_string(),
                    v["title"].as_str().unwrap_or("").to_string(),
                    v["label"].as_str().unwrap_or("").to_string(),
                    detail_line(v),
                )
            })
            .collect();
        let (can_replace_playlist, can_replace_queue, album) = (
            ctx.playlist_row.is_some(),
            ctx.queue_index.is_some(),
            ctx.album,
        );
        if let Ok(mut c) = CTX.lock() {
            *c = Some(ctx);
        }
        self.ui(move |w| {
            let model: Vec<VersionRow> = rows
                .into_iter()
                .map(|(id, title, label, detail)| VersionRow {
                    id: id.into(),
                    title: title.into(),
                    label: label.into(),
                    detail: detail.into(),
                })
                .collect();
            w.set_notice("".into());
            w.set_versions_title(SharedString::from(title));
            w.set_versions_rows(ModelRc::new(VecModel::from(model)));
            w.set_versions_album(album);
            w.set_versions_replace_playlist(can_replace_playlist);
            w.set_versions_replace_queue(can_replace_queue);
            w.set_versions_open(true);
        });
    }

    pub fn versions_close(&self) {
        if let Ok(mut c) = CTX.lock() {
            *c = None;
        }
    }

    /// A button on a version row: "play", "queue", "replace-playlist", "replace-queue", "open".
    pub fn version_action(&self, index: usize, action: &str) {
        let picked = CTX.lock().ok().and_then(|c| {
            let c = c.as_ref()?;
            let id = c.items.get(index)?["id"].as_u64()?;
            Some((id, c.playlist_row.clone(), c.queue_index, c.album))
        });
        let Some((id, playlist_row, queue_index, album)) = picked else {
            return;
        };
        let (action, this) = (action.to_string(), self.clone());
        self.versions_close();
        self.ui(|w| w.set_versions_open(false));
        if album {
            this.navigate(Route::Album(id));
            return;
        }
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            let track = match metadata::get_track(&state, id).await {
                Ok(v) => serde_json::from_value::<Track>(v).ok(),
                Err(e) => {
                    let msg = format!("Could not load that version: {}", e.log_safe());
                    this.ui(move |w| w.set_error(msg.into()));
                    return;
                }
            };
            let Some(track) = track else { return };
            match action.as_str() {
                "play" => {
                    if let Some(ctl) = this.controller() {
                        let outcome = ctl
                            .play_from_source(track.clone(), vec![track], None, false)
                            .await;
                        this.apply(outcome).await;
                    }
                }
                "queue" => this.track_action(track, None, "queue"),
                "replace-queue" => {
                    let (Some(ctl), Some(at)) = (this.controller(), queue_index) else {
                        return;
                    };
                    ctl.remove_from_queue(at).await;
                    ctl.add_to_queue(track, None).await;
                    let last = ctl.with_queue(|q| q.manual.len() + q.queue.len()).await;
                    if last > 0 {
                        let _ = ctl.move_in_queue(last - 1, at.min(last - 1)).await;
                    }
                    this.refresh_queue_info().await;
                    this.save().await;
                    this.notify("Replaced in the queue");
                }
                "replace-playlist" => {
                    let Some((playlist, row)) = playlist_row else {
                        return;
                    };
                    this.replace_in_playlist(&state, &playlist, this.real_index(row), track.id)
                        .await;
                }
                _ => {}
            }
        });
    }

    /// Puts `new_id` where the track at `at` is, then removes the old one. The playlist is
    /// edited through the paced client: add, move into place, remove.
    async fn replace_in_playlist(
        &self,
        state: &slate_core::AppState,
        uuid: &str,
        at: usize,
        new_id: u64,
    ) {
        let result = async {
            let before = lib_cmds::get_playlist_tracks(state, self.app.clone(), uuid.to_string())
                .await
                .map_err(|e| e.log_safe())?
                .len();
            lib_cmds::add_track_to_playlist(state, uuid.to_string(), new_id)
                .await
                .map_err(|e| e.log_safe())?;
            // The new track is at index `before`; bring it in front of the old one, then drop
            // the old one (which has moved down by one).
            if before != at {
                lib_cmds::move_playlist_item(state, uuid.to_string(), before as u32, at as u32)
                    .await
                    .map_err(|e| e.log_safe())?;
            }
            lib_cmds::remove_track_from_playlist(state, uuid.to_string(), at as u32 + 1)
                .await
                .map_err(|e| e.log_safe())
        }
        .await;
        match result {
            Ok(()) => {
                self.notify("Replaced in the playlist");
                self.navigate_refresh(Route::Playlist(uuid.to_string()));
            }
            Err(e) => {
                let msg = format!("Could not replace the track: {e}");
                self.ui(move |w| w.set_error(msg.into()));
                self.navigate_refresh(Route::Playlist(uuid.to_string()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detail_lines_join_what_is_known() {
        let v = json!({ "year": 2011, "quality": "Lossless", "atmos": false, "explicit": true, "duration_seconds": 245 });
        assert_eq!(detail_line(&v), "2011 · Lossless · E · 4:05");
        let atmos = json!({ "year": null, "quality": "", "atmos": true, "explicit": false, "duration_seconds": 0 });
        assert_eq!(detail_line(&atmos), "Dolby Atmos");
        assert_eq!(detail_line(&json!({})), "");
    }
}
