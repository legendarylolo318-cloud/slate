//! Favourites (hearts), add-to-playlist and playlist folders.

use std::collections::HashSet;

use serde_json::Value;
use slate_core::commands::library as lib_cmds;
use slate_core::queue::Track;

use crate::backend::Backend;
use crate::library::SourceKind;
use crate::pages::CardData;

#[derive(Default)]
pub struct Favs {
    pub tracks: HashSet<u64>,
    pub albums: HashSet<u64>,
    pub artists: HashSet<u64>,
    pub playlists: HashSet<String>,
}

/// Folders and playlists found in a folder listing.
#[derive(Debug, Default, PartialEq)]
pub struct FolderItems {
    /// (folder id, name, item count)
    pub folders: Vec<(String, String, u64)>,
    pub playlists: Vec<CardData>,
}

/// Defensive parse of the folder endpoint: the response nests entities under
/// `data` and identifies them by TRN (`trn:folder:ID`, `trn:playlist:UUID`).
pub fn parse_folder_items(v: &Value) -> FolderItems {
    let mut out = FolderItems::default();
    let Some(items) = v.get("items").and_then(Value::as_array) else {
        return out;
    };
    for item in items {
        let d = item.get("data").unwrap_or(item);
        let trn = d.get("trn").and_then(Value::as_str).unwrap_or("");
        let name = d
            .get("name")
            .or_else(|| d.get("title"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if let Some(id) = trn.strip_prefix("trn:folder:") {
            let n = d
                .get("totalNumberOfItems")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            out.folders.push((id.to_string(), name, n));
        } else if let Some(uuid) = d
            .get("uuid")
            .and_then(Value::as_str)
            .or_else(|| trn.strip_prefix("trn:playlist:"))
        {
            out.playlists.push(CardData {
                kind: "playlist",
                id: uuid.to_string(),
                title: name,
                subtitle: d
                    .get("numberOfTracks")
                    .and_then(Value::as_u64)
                    .map(|n| format!("{n} tracks"))
                    .unwrap_or_default(),
                image: d
                    .get("squareImage")
                    .or_else(|| d.get("image"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
        }
    }
    out
}

impl Backend {
    pub fn load_favorites(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            let (Some(state), Some(uid)) = (this.state(), this.user_id().await) else {
                return;
            };
            match lib_cmds::get_all_favorite_ids(&state, uid).await {
                Ok(ids) => {
                    if let Ok(mut f) = this.favs.lock() {
                        *f = Favs {
                            tracks: ids.tracks.into_iter().collect(),
                            albums: ids.albums.into_iter().collect(),
                            artists: ids.artists.into_iter().collect(),
                            playlists: ids.playlists.into_iter().collect(),
                        };
                    }
                    this.refresh_favorite_ui();
                }
                Err(e) => tracing::warn!("favourites unavailable: {}", e.log_safe()),
            }
        });
    }

    pub(crate) fn is_favorite_track(&self, id: u64) -> bool {
        self.favs.lock().is_ok_and(|f| f.tracks.contains(&id))
    }

    /// Pushes favourite state for the current track and page to the window.
    pub(crate) fn refresh_favorite_ui(&self) {
        let (cur, kind, artist) = (
            self.current_track_id(),
            self.lib.active_kind_pub(),
            self.lib.artist_id(),
        );
        let can_export = matches!(kind, Some(SourceKind::Loved | SourceKind::Playlist(_)));
        let can_dedupe = matches!(kind, Some(SourceKind::Playlist(_)));
        let (track_fav, page_fav, can, hero_fav) = {
            let Ok(f) = self.favs.lock() else { return };
            let track_fav = cur.is_some_and(|c| f.tracks.contains(&c));
            let (page_fav, can) = match &kind {
                Some(SourceKind::Album(a)) => (f.albums.contains(a), true),
                Some(SourceKind::Playlist(u)) => (f.playlists.contains(u), true),
                _ => (false, false),
            };
            (
                track_fav,
                page_fav,
                can,
                artist.is_some_and(|a| f.artists.contains(&a)),
            )
        };
        self.ui(move |w| {
            w.set_track_favorite(track_fav);
            w.set_page_favorite(page_fav);
            w.set_can_favorite(can);
            w.set_can_export(can_export);
            w.set_can_dedupe(can_dedupe);
            w.set_hero_favorite(hero_fav);
        });
    }

    pub fn toggle_track_favorite(&self, track_id: u64) {
        let this = self.clone();
        self.rt.spawn(async move {
            let (Some(state), Some(uid)) = (this.state(), this.user_id().await) else {
                return;
            };
            let now = !this.is_favorite_track(track_id);
            let result = if now {
                lib_cmds::add_favorite_track(&state, uid, track_id).await
            } else {
                lib_cmds::remove_favorite_track(&state, uid, track_id).await
            };
            match result {
                Ok(()) => {
                    if let Ok(mut f) = this.favs.lock() {
                        if now {
                            f.tracks.insert(track_id);
                        } else {
                            f.tracks.remove(&track_id);
                        }
                    }
                    let id = track_id.to_string();
                    this.ui(move |w| {
                        crate::models::with_tracks(&w, |m| m.set_favorite(&id, now));
                    });
                    this.refresh_favorite_ui();
                    this.notify(if now {
                        "Added to Loved Tracks"
                    } else {
                        "Removed from Loved Tracks"
                    });
                }
                Err(e) => {
                    let msg = format!("Could not update Loved Tracks: {}", e.log_safe());
                    this.ui(move |w| w.set_error(msg.into()));
                }
            }
        });
    }

    pub fn toggle_current_favorite(&self) {
        if let Some(id) = self.current_track_id() {
            self.toggle_track_favorite(id);
        }
    }

    pub fn toggle_page_favorite(&self) {
        let Some(kind) = self.lib.active_kind_pub() else {
            return;
        };
        let this = self.clone();
        self.rt.spawn(async move {
            let (Some(state), Some(uid)) = (this.state(), this.user_id().await) else {
                return;
            };
            let result = match &kind {
                SourceKind::Album(id) => {
                    let now = !this.favs.lock().is_ok_and(|f| f.albums.contains(id));
                    let r = if now {
                        lib_cmds::add_favorite_album(&state, uid, *id).await
                    } else {
                        lib_cmds::remove_favorite_album(&state, uid, *id).await
                    };
                    r.map(|()| {
                        if let Ok(mut f) = this.favs.lock() {
                            if now {
                                f.albums.insert(*id);
                            } else {
                                f.albums.remove(id);
                            }
                        }
                    })
                }
                SourceKind::Playlist(u) => {
                    let now = !this.favs.lock().is_ok_and(|f| f.playlists.contains(u));
                    let r = if now {
                        lib_cmds::add_favorite_playlist(&state, uid, u.clone()).await
                    } else {
                        lib_cmds::remove_favorite_playlist(&state, uid, u.clone()).await
                    };
                    r.map(|()| {
                        if let Ok(mut f) = this.favs.lock() {
                            if now {
                                f.playlists.insert(u.clone());
                            } else {
                                f.playlists.remove(u);
                            }
                        }
                    })
                }
                _ => return,
            };
            match result {
                Ok(()) => this.refresh_favorite_ui(),
                Err(e) => {
                    let msg = format!("Could not save: {}", e.log_safe());
                    this.ui(move |w| w.set_error(msg.into()));
                }
            }
        });
    }

    pub fn toggle_hero_favorite(&self) {
        let Some(id) = self.lib.artist_id() else {
            return;
        };
        let this = self.clone();
        self.rt.spawn(async move {
            let (Some(state), Some(uid)) = (this.state(), this.user_id().await) else {
                return;
            };
            let now = !this.favs.lock().is_ok_and(|f| f.artists.contains(&id));
            let r = if now {
                lib_cmds::add_favorite_artist(&state, uid, id).await
            } else {
                lib_cmds::remove_favorite_artist(&state, uid, id).await
            };
            match r {
                Ok(()) => {
                    if let Ok(mut f) = this.favs.lock() {
                        if now {
                            f.artists.insert(id);
                        } else {
                            f.artists.remove(&id);
                        }
                    }
                    this.refresh_favorite_ui();
                }
                Err(e) => {
                    let msg = format!("Could not update follow: {}", e.log_safe());
                    this.ui(move |w| w.set_error(msg.into()));
                }
            }
        });
    }

    // ---------- playlists ----------

    fn picker_track(&self, target: i32) -> Option<Track> {
        match target {
            -2 => self.current_track_id().map(Track::new),
            i if i >= 0 => self.cached_track(usize::try_from(i).ok()?),
            _ => None,
        }
    }

    pub fn add_to_playlist(&self, target: i32, uuid: &str) {
        let Some(track) = self.picker_track(target) else {
            return;
        };
        let uuid = uuid.to_string();
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            match lib_cmds::add_track_to_playlist(&state, uuid, track.id).await {
                Ok(()) => {
                    this.notify("Added to playlist");
                    this.load_library();
                }
                Err(e) => {
                    let msg = format!("Could not add to playlist: {}", e.log_safe());
                    this.ui(move |w| w.set_error(msg.into()));
                }
            }
        });
    }

    pub fn create_playlist(&self, target: i32, name: &str) {
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        let track = self.picker_track(target);
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            match lib_cmds::create_playlist(&state, name, String::new(), "UNLISTED".into()).await {
                Ok(p) => {
                    if let Some(t) = track {
                        if let Err(e) = lib_cmds::add_track_to_playlist(&state, p.uuid, t.id).await
                        {
                            tracing::warn!("new playlist created but add failed: {}", e.log_safe());
                        }
                    }
                    this.notify("Playlist created");
                    this.load_library();
                }
                Err(e) => {
                    let msg = format!("Could not create playlist: {}", e.log_safe());
                    this.ui(move |w| w.set_error(msg.into()));
                }
            }
        });
    }

    // ---------- management ----------

    /// Rename / delete / move from the sidebar dialog. `target` is a playlist
    /// uuid, or `folder:ID` for a folder.
    pub fn dialog_accepted(&self, kind: &str, target: &str, text: &str) {
        let (kind, target, text) = (
            kind.to_string(),
            target.to_string(),
            text.trim().to_string(),
        );
        let this = self.clone();
        self.rt.spawn(async move {
            let (Some(state), Some(uid)) = (this.state(), this.user_id().await) else {
                return;
            };
            let folder = target.strip_prefix("folder:").map(str::to_string);
            let result: Result<&str, String> = match (kind.as_str(), &folder) {
                ("rename", Some(id)) => {
                    lib_cmds::rename_playlist_folder(&state, format!("trn:folder:{id}"), text)
                        .await
                        .map(|()| "Folder renamed")
                        .map_err(|e| e.log_safe())
                }
                ("rename", None) => rename_playlist(&state, &target, &text)
                    .await
                    .map(|()| "Playlist renamed"),
                ("delete", Some(id)) => {
                    lib_cmds::delete_playlist_folder(&state, format!("trn:folder:{id}"))
                        .await
                        .map(|()| "Folder deleted")
                        .map_err(|e| e.log_safe())
                }
                ("artist", None) => {
                    if let Ok(id) = target.parse() {
                        this.navigate(crate::pages::Route::Artist(id));
                    }
                    return;
                }
                ("compare", None) => return this.run_compare(&target, &text).await,
                ("dedupe", None) => return this.run_dedupe(&target).await,
                ("delete", None) => lib_cmds::delete_playlist(&state, uid, target.clone())
                    .await
                    .map(|()| "Playlist deleted")
                    .map_err(|e| e.log_safe()),
                ("move", None) => {
                    let dest = text.strip_prefix("folder:").unwrap_or(&text).to_string();
                    lib_cmds::move_playlist_to_folder(
                        &state,
                        dest,
                        format!("trn:playlist:{target}"),
                    )
                    .await
                    .map(|()| "Moved")
                    .map_err(|e| e.log_safe())
                }
                _ => return,
            };
            match result {
                Ok(msg) => {
                    this.notify(msg);
                    this.load_library();
                    // A deleted page must not stay on screen.
                    if kind == "delete" {
                        this.navigate(crate::pages::Route::Home);
                    }
                }
                Err(e) => {
                    let msg = format!("Could not {kind}: {e}");
                    this.ui(move |w| w.set_error(msg.into()));
                }
            }
        });
    }

    /// Removes row `index` from the playlist being shown.
    pub fn remove_from_playlist(&self, index: usize) {
        let Some(SourceKind::Playlist(uuid)) = self.lib.active_kind_pub() else {
            self.notify("Open one of your playlists to remove tracks from it.");
            return;
        };
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            let idx = u32::try_from(index).unwrap_or(u32::MAX);
            match lib_cmds::remove_track_from_playlist(&state, uuid.clone(), idx).await {
                Ok(()) => {
                    this.notify("Removed from playlist");
                    this.navigate_refresh(crate::pages::Route::Playlist(uuid));
                }
                Err(e) => {
                    let msg = format!("Could not remove: {}", e.log_safe());
                    this.ui(move |w| w.set_error(msg.into()));
                }
            }
        });
    }

    // ---------- folders ----------

    pub fn create_folder(&self, name: &str) {
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            match lib_cmds::create_playlist_folder(&state, "root".into(), name, String::new()).await
            {
                Ok(_) => {
                    this.notify("Folder created");
                    this.load_library();
                }
                Err(e) => {
                    let msg = format!("Could not create folder: {}", e.log_safe());
                    this.ui(move |w| w.set_error(msg.into()));
                }
            }
        });
    }

    /// Folders at the top level, for the sidebar.
    pub(crate) async fn fetch_folders(&self) -> Vec<(String, String, u64)> {
        let Some(state) = self.state() else {
            return Vec::new();
        };
        match self.folder_listing(&state, "root", Some("FOLDER")).await {
            Ok(v) => {
                let parsed = parse_folder_items(&v);
                tracing::debug!(folders = parsed.folders.len(), "folder listing parsed");
                parsed.folders
            }
            Err(e) => {
                tracing::debug!("folders unavailable: {}", e.log_safe());
                Vec::new()
            }
        }
    }

    /// Contents of one folder as playlist cards.
    pub(crate) async fn fetch_folder_playlists(&self, id: &str) -> Result<Vec<CardData>, String> {
        let state = self.state().ok_or("The core is still starting.")?;
        let v = self
            .folder_listing(&state, id, None)
            .await
            .map_err(|e| e.log_safe())?;
        Ok(parse_folder_items(&v).playlists)
    }

    /// One folder's whole listing: the endpoint returns at most 50 items per call
    /// and a `cursor` for the next page.
    async fn folder_listing(
        &self,
        state: &slate_core::AppState,
        id: &str,
        include_only: Option<&str>,
    ) -> Result<Value, slate_core::SlateError> {
        const MAX_PAGES: usize = 40;
        let mut items = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut v = lib_cmds::get_playlist_folders(
                state,
                self.app.clone(),
                id.to_string(),
                include_only.map(str::to_string),
                0,
                50,
                "DATE_UPDATED".into(),
                "DESC".into(),
                cursor.take(),
            )
            .await?;
            if let Some(Value::Array(page)) = v.get_mut("items").map(Value::take) {
                items.extend(page);
            }
            match v.get("cursor").and_then(Value::as_str) {
                Some(next) if !next.is_empty() => cursor = Some(next.to_string()),
                _ => break,
            }
        }
        Ok(serde_json::json!({ "items": items }))
    }
}

/// Keeps the playlist's description and visibility while changing its title.
async fn rename_playlist(
    state: &slate_core::AppState,
    uuid: &str,
    title: &str,
) -> Result<(), String> {
    let details = slate_core::commands::metadata::get_playlist_details(state, uuid.to_string())
        .await
        .map_err(|e| e.log_safe())?;
    let description = details["description"].as_str().unwrap_or("").to_string();
    let access = details["publicPlaylist"].as_bool().map_or("UNLISTED", |p| {
        if p {
            "PUBLIC"
        } else {
            "UNLISTED"
        }
    });
    lib_cmds::update_playlist(
        state,
        uuid.to_string(),
        title.to_string(),
        description,
        access.into(),
    )
    .await
    .map(|_| ())
    .map_err(|e| e.log_safe())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_folders_and_playlists_by_trn() {
        let v = json!({"items":[
            {"data":{"trn":"trn:folder:f1","name":"Gym","totalNumberOfItems":4}},
            {"data":{"trn":"trn:playlist:p1","name":"Run","numberOfTracks":12,"squareImage":"a-b"}},
            {"trn":"trn:playlist:p2","title":"Flat"},
            {"data":{"unknown":true}}
        ]});
        let p = parse_folder_items(&v);
        assert_eq!(p.folders, vec![("f1".to_string(), "Gym".to_string(), 4)]);
        assert_eq!(p.playlists.len(), 2);
        assert_eq!(p.playlists[0].subtitle, "12 tracks");
        assert_eq!(p.playlists[1].id, "p2");
    }

    #[test]
    fn empty_or_odd_responses_yield_nothing() {
        assert_eq!(parse_folder_items(&json!({})), FolderItems::default());
        assert_eq!(
            parse_folder_items(&json!({"items": 3})),
            FolderItems::default()
        );
    }
}
