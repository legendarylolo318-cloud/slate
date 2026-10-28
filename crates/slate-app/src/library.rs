//! Library browsing: sidebar playlists and paged track lists.
//!
//! Display is lazy (see `models::LazyTracks`); playback needs the whole list,
//! which `ensure_all` fetches in parallel pages on demand.

use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;

use futures_util::stream::{self, StreamExt};
use slate_core::commands::{auth, library as lib_cmds};
use slate_core::queue::{Source, Track};
use slate_core::tidal_api::TidalTrack;
use slint::{ModelRc, VecModel};

use crate::backend::Backend;
use crate::bootstrap::{PlaylistEntry, TrackRow};
use crate::images::format_time;
use crate::models::{with_tracks, LazyTracks, PAGE_SIZE};

/// Concurrent page requests when loading a whole list for playback.
const BULK_CONCURRENCY: usize = 6;
const PLAYLIST_PAGE: u32 = 50;
const MAX_PLAYLIST_PAGES: u32 = 40;

#[derive(Clone, Debug, PartialEq)]
pub enum SourceKind {
    Loved,
    Playlist(String),
}

#[derive(Clone)]
struct Active {
    kind: SourceKind,
    title: String,
    id: String,
}

#[derive(Default)]
pub struct LibState {
    generation: AtomicU64,
    active: Mutex<Option<Active>>,
    tracks: Mutex<BTreeMap<usize, Track>>,
    total: AtomicUsize,
    user_id: Mutex<Option<u64>>,
}

fn to_track(t: &TidalTrack) -> Option<Track> {
    serde_json::to_value(t)
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
}

fn row_of(t: &Track) -> TrackRow {
    let version = t
        .extra
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let title = if version.is_empty() {
        t.title().to_string()
    } else {
        format!("{} ({version})", t.title())
    };
    let album = t
        .extra
        .get("album")
        .and_then(|a| a.get("title"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    TrackRow {
        id: t.id.to_string().into(),
        title: title.into(),
        artist: t.artist_name().into(),
        album: album.into(),
        duration: format_time(t.duration_secs()).into(),
        explicit: t.extra.get("explicit").and_then(|v| v.as_bool()) == Some(true),
        loaded: true,
    }
}

impl Backend {
    // ---------- sidebar ----------

    async fn user_id(&self) -> Option<u64> {
        if let Some(id) = self.lib.user_id.lock().ok().and_then(|g| *g) {
            return Some(id);
        }
        let state = self.state()?;
        match auth::get_session_user_id(&state).await {
            Ok(id) => {
                if let Ok(mut g) = self.lib.user_id.lock() {
                    *g = Some(id);
                }
                Some(id)
            }
            Err(e) => {
                tracing::warn!("could not resolve user id: {}", e.log_safe());
                None
            }
        }
    }

    /// Fills the sidebar. Called whenever the main screen is entered.
    pub fn load_library(&self) {
        let this = self.clone();
        self.ui(|w| w.set_playlists_loading(true));
        self.rt.spawn(async move {
            let (Some(state), Some(uid)) = (this.state(), this.user_id().await) else {
                this.ui(|w| w.set_playlists_loading(false));
                return;
            };
            let mut entries: Vec<PlaylistEntry> = Vec::new();
            for page in 0..MAX_PLAYLIST_PAGES {
                let offset = page * PLAYLIST_PAGE;
                match lib_cmds::get_all_playlists(
                    &state,
                    this.app.clone(),
                    uid,
                    offset,
                    PLAYLIST_PAGE,
                    "DATE".into(),
                    "DESC".into(),
                )
                .await
                {
                    Ok(p) => {
                        let got = p.items.len();
                        entries.extend(p.items.into_iter().map(|pl| {
                            PlaylistEntry {
                                subtitle: pl
                                    .number_of_tracks
                                    .map(|n| n.to_string())
                                    .unwrap_or_default()
                                    .into(),
                                uuid: pl.uuid.into(),
                                title: pl.title.into(),
                            }
                        }));
                        if got == 0 || entries.len() >= p.total_number_of_items as usize {
                            break;
                        }
                    }
                    Err(e) => {
                        let msg = format!("Could not load playlists: {}", e.log_safe());
                        this.ui(move |w| w.set_error(msg.into()));
                        break;
                    }
                }
            }
            this.ui(move |w| {
                w.set_playlists(ModelRc::new(VecModel::from(entries)));
                w.set_playlists_loading(false);
            });
        });
    }

    // ---------- opening a list ----------

    pub fn open_loved(&self) {
        self.open_source(SourceKind::Loved, "Loved Tracks".into(), "loved".into());
    }

    pub fn open_playlist(&self, uuid: &str) {
        let uuid = uuid.to_string();
        // The title is already in the sidebar model; read it on the UI thread.
        let this = self.clone();
        self.ui(move |w| {
            use slint::Model;
            let title = w
                .get_playlists()
                .iter()
                .find(|p| p.uuid == uuid.as_str())
                .map(|p| p.title.to_string())
                .unwrap_or_else(|| "Playlist".into());
            this.open_source(SourceKind::Playlist(uuid.clone()), title, uuid);
        });
    }

    fn open_source(&self, kind: SourceKind, title: String, id: String) {
        let generation = self.lib.generation.fetch_add(1, Ordering::SeqCst) + 1;
        if let Ok(mut t) = self.lib.tracks.lock() {
            t.clear();
        }
        self.lib.total.store(0, Ordering::Relaxed);
        if let Ok(mut a) = self.lib.active.lock() {
            *a = Some(Active {
                kind: kind.clone(),
                title: title.clone(),
                id: id.clone(),
            });
        }

        let this = self.clone();
        let (page_title, selected) = (title.clone(), id.clone());
        self.ui(move |w| {
            let for_fetch = this.clone();
            let model = LazyTracks::new(Box::new(move |page| {
                for_fetch.request_page(generation, page)
            }));
            w.set_tracks(ModelRc::from(Rc::new(model)));
            w.set_has_page(true);
            w.set_page_title(page_title.into());
            w.set_page_subtitle("".into());
            w.set_page_loading(true);
            w.set_selected_source(selected.into());
        });

        // Page 0 tells us the total; fill it before publishing the total so the
        // list does not ask for it a second time.
        let this = self.clone();
        self.rt.spawn(async move {
            match this.fetch_page(&kind, 0).await {
                Ok((tracks, total)) => {
                    if this.lib.generation.load(Ordering::SeqCst) != generation {
                        return;
                    }
                    let rows = this.remember(0, &tracks);
                    this.lib.total.store(total, Ordering::Relaxed);
                    this.ui(move |w| {
                        with_tracks(&w, |m| {
                            m.fill(0, rows);
                            m.set_total(total);
                        });
                        w.set_page_subtitle(format!("{total} tracks").into());
                        w.set_page_loading(false);
                    });
                }
                Err(msg) => this.ui(move |w| {
                    w.set_page_loading(false);
                    w.set_error(msg.into());
                }),
            }
        });
    }

    /// Called from the UI thread when the list scrolls onto an unloaded page.
    fn request_page(&self, generation: u64, page: usize) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(kind) = this.active_kind() else {
                return;
            };
            match this.fetch_page(&kind, page).await {
                Ok((tracks, _)) => {
                    if this.lib.generation.load(Ordering::SeqCst) != generation {
                        return;
                    }
                    let rows = this.remember(page, &tracks);
                    this.ui(move |w| {
                        with_tracks(&w, |m| m.fill(page * PAGE_SIZE, rows));
                    });
                }
                Err(msg) => {
                    tracing::warn!("page {page} failed: {msg}");
                    this.ui(move |w| {
                        with_tracks(&w, |m| m.forget_request(page));
                    });
                }
            }
        });
    }

    fn active_kind(&self) -> Option<SourceKind> {
        self.lib
            .active
            .lock()
            .ok()?
            .as_ref()
            .map(|a| a.kind.clone())
    }

    /// Stores fetched tracks for playback and returns their display rows.
    fn remember(&self, page: usize, tracks: &[TidalTrack]) -> Vec<TrackRow> {
        let mut rows = Vec::with_capacity(tracks.len());
        if let Ok(mut cache) = self.lib.tracks.lock() {
            for (i, t) in tracks.iter().enumerate() {
                if let Some(track) = to_track(t) {
                    rows.push(row_of(&track));
                    cache.insert(page * PAGE_SIZE + i, track);
                }
            }
        }
        rows
    }

    async fn fetch_page(
        &self,
        kind: &SourceKind,
        page: usize,
    ) -> Result<(Vec<TidalTrack>, usize), String> {
        let state = self.state().ok_or("The core is still starting.")?;
        let offset = u32::try_from(page * PAGE_SIZE).unwrap_or(u32::MAX);
        let limit = PAGE_SIZE as u32;
        let result = match kind {
            SourceKind::Loved => {
                let uid = self
                    .user_id()
                    .await
                    .ok_or("Could not resolve your account.")?;
                lib_cmds::get_favorite_tracks(
                    &state,
                    self.app.clone(),
                    uid,
                    offset,
                    limit,
                    "DATE".into(),
                    "DESC".into(),
                )
                .await
            }
            SourceKind::Playlist(uuid) => {
                lib_cmds::get_playlist_tracks_page(
                    &state,
                    self.app.clone(),
                    uuid.clone(),
                    offset,
                    limit,
                    None,
                    None,
                )
                .await
            }
        };
        result
            .map(|p| (p.items, p.total_number_of_items as usize))
            .map_err(|e| format!("Could not load tracks: {}", e.log_safe()))
    }

    // ---------- playback from a list ----------

    /// Loads every page not yet cached and returns the list in order.
    async fn ensure_all(&self) -> Option<(Vec<Track>, Source)> {
        let active = self.lib.active.lock().ok()?.clone()?;
        let generation = self.lib.generation.load(Ordering::SeqCst);
        let total = self.lib.total.load(Ordering::Relaxed);
        let pages = total.div_ceil(PAGE_SIZE);
        let missing: Vec<usize> = {
            let cache = self.lib.tracks.lock().ok()?;
            (0..pages)
                .filter(|p| !cache.contains_key(&(p * PAGE_SIZE)))
                .collect()
        };
        let results = stream::iter(missing)
            .map(|p| {
                let kind = active.kind.clone();
                async move { (p, self.fetch_page(&kind, p).await) }
            })
            .buffer_unordered(BULK_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;
        if self.lib.generation.load(Ordering::SeqCst) != generation {
            return None;
        }
        for (p, res) in results {
            match res {
                Ok((tracks, _)) => {
                    self.remember(p, &tracks);
                }
                Err(msg) => {
                    self.ui(move |w| w.set_error(msg.into()));
                    return None;
                }
            }
        }
        let all: Vec<Track> = self.lib.tracks.lock().ok()?.values().cloned().collect();
        let (kind, id) = match &active.kind {
            SourceKind::Loved => ("favorites", serde_json::Value::from("loved")),
            SourceKind::Playlist(u) => ("playlist", serde_json::Value::from(u.as_str())),
        };
        let _ = &active.id;
        let source = Source {
            kind: kind.into(),
            id,
            name: active.title,
            image: None,
            subtitle: None,
            mix_type: None,
            tracks: all.clone(),
        };
        Some((all, source))
    }

    pub fn play_row(&self, index: usize) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(ctl) = this.controller() else { return };
            let Some(track) = this
                .lib
                .tracks
                .lock()
                .ok()
                .and_then(|c| c.get(&index).cloned())
            else {
                return;
            };
            let Some((all, source)) = this.ensure_all().await else {
                return;
            };
            let outcome = ctl.play_from_source(track, all, Some(source), false).await;
            this.apply(outcome).await;
        });
    }

    pub fn play_all(&self, shuffle: bool) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(ctl) = this.controller() else { return };
            let Some((all, source)) = this.ensure_all().await else {
                return;
            };
            let outcome = ctl.play_all(all, Some(source), false, shuffle).await;
            this.apply(outcome).await;
        });
    }
}
