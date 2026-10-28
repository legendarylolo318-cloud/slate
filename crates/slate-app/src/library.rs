//! Library browsing: sidebar playlists and paged track lists.
//!
//! Display is lazy (see `models::LazyTracks`); playback needs the whole list,
//! which `ensure_all` fetches in parallel pages on demand.

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::stream::{self, StreamExt};
use slate_core::commands::{auth, library as lib_cmds, pages};
use slate_core::queue::{Source, Track};
use slate_core::tidal_api::TidalTrack;
use slint::{Image, Model, ModelRc, VecModel};

use crate::backend::Backend;
use crate::bootstrap::{AppWindow, PlaylistEntry};
use crate::images::{format_time, tidal_image_url};
use crate::models::{with_tracks, LazyTracks, RowData, PAGE_SIZE};

/// Concurrent page requests when loading a whole list for playback.
const BULK_CONCURRENCY: usize = 6;
const PLAYLIST_PAGE: u32 = 50;
const MAX_PLAYLIST_PAGES: u32 = 40;

#[derive(Clone, Debug, PartialEq)]
pub enum SourceKind {
    Loved,
    Playlist(String),
    Album(u64),
    Mix(String),
}

/// What the page header shows.
pub struct Header {
    pub label: &'static str,
    pub title: String,
    pub subtitle: String,
    pub cover: Option<String>,
    pub loved: bool,
}

#[derive(Clone)]
struct Active {
    kind: SourceKind,
    title: String,
}

#[derive(Default)]
pub struct LibState {
    generation: AtomicU64,
    active: Mutex<Option<Active>>,
    tracks: Mutex<BTreeMap<usize, Track>>,
    total: AtomicUsize,
    user_id: Mutex<Option<u64>>,
    /// Playlist uuid → TIDAL image id, for the page header.
    playlist_images: Mutex<HashMap<String, Option<String>>>,
    /// Mix items arrive in one response; paging serves slices of it.
    static_tracks: Mutex<Vec<TidalTrack>>,
    /// Artist page "Play top tracks" list.
    pub hero_tracks: Mutex<Vec<Track>>,
    cards_gen: AtomicU64,
    artist: Mutex<Option<u64>>,
}

fn to_track(t: &TidalTrack) -> Option<Track> {
    serde_json::to_value(t)
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
}

fn row_of(t: &Track) -> RowData {
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
    RowData {
        id: t.id.to_string(),
        title,
        artist: t.artist_name().to_string(),
        album: album.to_string(),
        duration: format_time(t.duration_secs()),
        explicit: t.extra.get("explicit").and_then(|v| v.as_bool()) == Some(true),
        favorite: false,
    }
}

/// Sets the thumbnail of sidebar row `i`, unless the list changed underneath.
fn set_playlist_cover(w: &AppWindow, i: usize, uuid: &str, cover: Image) {
    let model = w.get_playlists();
    let Some(list) = model.as_any().downcast_ref::<VecModel<PlaylistEntry>>() else {
        return;
    };
    if let Some(mut entry) = list.row_data(i) {
        if entry.uuid == uuid {
            entry.cover = cover;
            list.set_row_data(i, entry);
        }
    }
}

impl LibState {
    pub fn active_kind_pub(&self) -> Option<SourceKind> {
        self.active.lock().ok()?.as_ref().map(|a| a.kind.clone())
    }

    pub fn artist_id(&self) -> Option<u64> {
        self.artist.lock().ok().and_then(|a| *a)
    }

    pub fn set_artist(&self, id: Option<u64>) {
        if let Ok(mut a) = self.artist.lock() {
            *a = id;
        }
    }

    /// Invalidates in-flight card-page work and returns the new generation.
    pub fn bump_cards(&self) -> u64 {
        self.cards_gen.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn cards_generation(&self) -> u64 {
        self.cards_gen.load(Ordering::SeqCst)
    }
}

impl Backend {
    pub(crate) fn cached_track(&self, index: usize) -> Option<Track> {
        self.lib.tracks.lock().ok()?.get(&index).cloned()
    }

    // ---------- sidebar ----------

    pub(crate) async fn user_id(&self) -> Option<u64> {
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
            // (uuid, title, track count, image id): plain data, so it can cross threads.
            let mut data: Vec<(String, String, String, Option<String>)> = Vec::new();
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
                        data.extend(p.items.into_iter().map(|pl| {
                            let count = pl
                                .number_of_tracks
                                .map(|n| format!("{n} tracks"))
                                .unwrap_or_default();
                            (pl.uuid, pl.title, count, pl.image)
                        }));
                        if got == 0 || data.len() >= p.total_number_of_items as usize {
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
            if let Ok(mut m) = this.lib.playlist_images.lock() {
                m.clear();
                for (uuid, _, _, img) in &data {
                    m.insert(uuid.clone(), img.clone());
                }
            }
            let folders = this.fetch_folders().await;
            let offset = folders.len();
            let ids: Vec<(usize, String, Option<String>)> = data
                .iter()
                .enumerate()
                .map(|(i, (u, _, _, img))| (i + offset, u.clone(), img.clone()))
                .collect();
            this.ui(move |w| {
                let folder_entries = folders.into_iter().map(|(id, name, n)| PlaylistEntry {
                    uuid: format!("folder:{id}").into(),
                    title: name.into(),
                    subtitle: format!("{n} items").into(),
                    folder: true,
                    cover: Image::default(),
                });
                let entries: Vec<PlaylistEntry> = folder_entries
                    .chain(
                        data.into_iter()
                            .map(|(uuid, title, subtitle, _)| PlaylistEntry {
                                uuid: uuid.into(),
                                title: title.into(),
                                subtitle: subtitle.into(),
                                folder: false,
                                cover: Image::default(),
                            }),
                    )
                    .collect();
                w.set_playlists(ModelRc::new(VecModel::from(entries)));
                w.set_playlists_loading(false);
            });
            this.load_playlist_covers(ids).await;
        });
    }

    /// Thumbnails for the sidebar, a few at a time; entries appear first, art fills in.
    async fn load_playlist_covers(&self, ids: Vec<(usize, String, Option<String>)>) {
        let Some(state) = self.state() else { return };
        stream::iter(ids)
            .for_each_concurrent(BULK_CONCURRENCY, |(i, uuid, image)| {
                let state = Arc::clone(&state);
                async move {
                    let Some(img) = image else { return };
                    let Some(px) = self.images.get(&state, &tidal_image_url(&img, 160)).await
                    else {
                        return;
                    };
                    self.ui(move |w| set_playlist_cover(&w, i, &uuid, Image::from_rgba8(px)));
                }
            })
            .await;
    }

    // ---------- opening a list ----------

    /// Opens a track list. The header fills in as soon as its details arrive.
    pub fn open(&self, kind: SourceKind, selected: String) {
        let generation = self.lib.generation.fetch_add(1, Ordering::SeqCst) + 1;
        if let Ok(mut t) = self.lib.tracks.lock() {
            t.clear();
        }
        if let Ok(mut s) = self.lib.static_tracks.lock() {
            s.clear();
        }
        self.lib.total.store(0, Ordering::Relaxed);
        if let Ok(mut a) = self.lib.active.lock() {
            *a = Some(Active {
                kind: kind.clone(),
                title: String::new(),
            });
        }

        let this = self.clone();
        self.ui(move |w| {
            let for_fetch = this.clone();
            let model = LazyTracks::new(Box::new(move |page| {
                for_fetch.request_page(generation, page)
            }));
            w.set_tracks(ModelRc::from(Rc::new(model)));
            w.set_view("tracks".into());
            w.set_has_page(true);
            w.set_page_title("".into());
            w.set_page_subtitle("".into());
            w.set_page_loading(true);
            w.set_page_cover(Image::default());
            w.set_page_loved(false);
            w.set_selected_source(selected.into());
        });

        let this = self.clone();
        self.rt.spawn(async move {
            let header = this.header_for(&kind).await;
            if this.lib.generation.load(Ordering::SeqCst) != generation {
                return;
            }
            if let Ok(mut a) = this.lib.active.lock() {
                if let Some(a) = a.as_mut() {
                    a.title = header.title.clone();
                }
            }
            let (label, title, loved) = (header.label, header.title.clone(), header.loved);
            let subtitle = header.subtitle.clone();
            this.ui(move |w| {
                w.set_page_kind(label.into());
                w.set_page_title(title.into());
                w.set_page_subtitle(subtitle.into());
                w.set_page_loved(loved);
            });
            this.refresh_favorite_ui();
            if let (Some(image), Some(state)) = (header.cover, this.state()) {
                let this2 = this.clone();
                this.rt.spawn(async move {
                    if let Some(px) = this2
                        .images
                        .get(&state, &tidal_image_url(&image, 320))
                        .await
                    {
                        if this2.lib.generation.load(Ordering::SeqCst) == generation {
                            this2.ui(move |w| w.set_page_cover(Image::from_rgba8(px)));
                        }
                    }
                });
            }

            // Page 0 tells us the total; fill it before publishing the total so the
            // list does not ask for it a second time.
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

    async fn header_for(&self, kind: &SourceKind) -> Header {
        let none = |label, title: &str| Header {
            label,
            title: title.to_string(),
            subtitle: String::new(),
            cover: None,
            loved: false,
        };
        let Some(state) = self.state() else {
            return none("", "");
        };
        match kind {
            SourceKind::Loved => Header {
                loved: true,
                ..none("LOVED", "Loved Tracks")
            },
            SourceKind::Playlist(uuid) => {
                match slate_core::commands::metadata::get_playlist_details(&state, uuid.clone())
                    .await
                {
                    Ok(v) => {
                        let count = v["numberOfTracks"].as_u64().unwrap_or(0);
                        let by = v["creator"]["name"].as_str().unwrap_or("");
                        Header {
                            label: "PLAYLIST",
                            title: v["title"].as_str().unwrap_or("Playlist").to_string(),
                            subtitle: if by.is_empty() {
                                format!("{count} tracks")
                            } else {
                                format!("{by} · {count} tracks")
                            },
                            cover: v["squareImage"]
                                .as_str()
                                .or_else(|| v["image"].as_str())
                                .map(str::to_string),
                            loved: false,
                        }
                    }
                    Err(_) => none("PLAYLIST", "Playlist"),
                }
            }
            SourceKind::Album(id) => match pages::get_album_detail(&state, *id).await {
                Ok(a) => {
                    let artist = a
                        .artist
                        .as_ref()
                        .map(|x| x.name.clone())
                        .or_else(|| {
                            a.artists.as_ref().map(|v| {
                                v.iter()
                                    .map(|x| x.name.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            })
                        })
                        .unwrap_or_default();
                    let year = a
                        .release_date
                        .as_deref()
                        .and_then(|d| d.get(..4))
                        .unwrap_or("");
                    let count = a.number_of_tracks.unwrap_or(0);
                    let parts: Vec<String> = [artist, year.to_string(), format!("{count} tracks")]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect();
                    Header {
                        label: "ALBUM",
                        title: a.title,
                        subtitle: parts.join(" · "),
                        cover: a.cover,
                        loved: false,
                    }
                }
                Err(_) => none("ALBUM", "Album"),
            },
            SourceKind::Mix(id) => match pages::get_mix_items(&state, id.clone()).await {
                Ok(m) => {
                    let count = m.tracks.len();
                    if let Ok(mut s) = self.lib.static_tracks.lock() {
                        *s = m.tracks;
                    }
                    Header {
                        label: "MIX",
                        title: m.title.unwrap_or_else(|| "Mix".into()),
                        subtitle: format!("{} · {count} tracks", m.subtitle.unwrap_or_default()),
                        cover: m.image,
                        loved: false,
                    }
                }
                Err(_) => none("MIX", "Mix"),
            },
        }
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
    fn remember(&self, page: usize, tracks: &[TidalTrack]) -> Vec<RowData> {
        let mut rows = Vec::with_capacity(tracks.len());
        let mut covers: HashMap<String, Vec<usize>> = HashMap::new();
        if let Ok(mut cache) = self.lib.tracks.lock() {
            for (i, t) in tracks.iter().enumerate() {
                if let Some(track) = to_track(t) {
                    let index = page * PAGE_SIZE + i;
                    let mut row = row_of(&track);
                    row.favorite = self.is_favorite_track(track.id);
                    rows.push(row);
                    if let Some(c) = track.cover_id() {
                        covers.entry(c.to_string()).or_default().push(index);
                    }
                    cache.insert(index, track);
                }
            }
        }
        if !covers.is_empty() {
            self.load_row_covers(covers);
        }
        rows
    }

    /// Thumbnails for rows: one fetch per distinct album cover, applied to every row using it.
    fn load_row_covers(&self, covers: HashMap<String, Vec<usize>>) {
        let generation = self.lib.generation.load(Ordering::SeqCst);
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            stream::iter(covers)
                .for_each_concurrent(BULK_CONCURRENCY, |(id, indices)| {
                    let (state, this) = (Arc::clone(&state), this.clone());
                    async move {
                        let Some(px) = this.images.get(&state, &tidal_image_url(&id, 160)).await
                        else {
                            return;
                        };
                        if this.lib.generation.load(Ordering::SeqCst) != generation {
                            return;
                        }
                        this.ui(move |w| {
                            let img = Image::from_rgba8(px);
                            with_tracks(&w, |m| m.set_cover(&indices, &img));
                        });
                    }
                })
                .await;
        });
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
            SourceKind::Album(id) => pages::get_album_tracks(&state, *id, offset, limit).await,
            SourceKind::Mix(_) => {
                // Served from the single response stashed when the header loaded.
                let all = self
                    .lib
                    .static_tracks
                    .lock()
                    .map(|s| s.clone())
                    .unwrap_or_default();
                let total = all.len();
                let items = all
                    .into_iter()
                    .skip(offset as usize)
                    .take(limit as usize)
                    .collect();
                return Ok((items, total));
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
        let source = Self::source_for(&active, all.clone());
        Some((all, source))
    }

    pub fn play_row(&self, index: usize) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(ctl) = this.controller() else { return };
            let Some(track) = this.cached_track(index) else {
                return;
            };
            let total = this.lib.total.load(Ordering::Relaxed);
            let complete = this.lib.tracks.lock().is_ok_and(|c| c.len() >= total);
            if complete {
                let Some((all, source)) = this.ensure_all().await else {
                    return;
                };
                let album = source.kind == "album";
                let outcome = ctl.play_from_source(track, all, Some(source), album).await;
                this.apply(outcome).await;
                return;
            }
            // Big list: start right away from what is already loaded, then fill in
            // the rest of the queue once every page has arrived.
            let Some((partial, source)) = this.snapshot_source() else {
                return;
            };
            let album = source.kind == "album";
            let outcome = ctl
                .play_from_source(track, partial, Some(source), album)
                .await;
            this.apply(outcome).await;
            if let Some((all, source)) = this.ensure_all().await {
                ctl.extend_source(all, &source).await;
                this.refresh_queue_info().await;
                this.save().await;
            }
        });
    }

    /// What is loaded so far, as a playable list and its source (no network).
    fn snapshot_source(&self) -> Option<(Vec<Track>, Source)> {
        let active = self.lib.active.lock().ok()?.clone()?;
        let all: Vec<Track> = self.lib.tracks.lock().ok()?.values().cloned().collect();
        Some((all.clone(), Self::source_for(&active, all)))
    }

    fn source_for(active: &Active, all: Vec<Track>) -> Source {
        let (kind, id) = match &active.kind {
            SourceKind::Loved => ("favorites", serde_json::Value::from("loved")),
            SourceKind::Playlist(u) => ("playlist", serde_json::Value::from(u.as_str())),
            SourceKind::Album(a) => ("album", serde_json::Value::from(a.to_string())),
            SourceKind::Mix(m) => ("mix", serde_json::Value::from(m.as_str())),
        };
        Source {
            kind: kind.into(),
            id,
            name: active.title.clone(),
            image: None,
            subtitle: None,
            mix_type: None,
            tracks: all,
        }
    }

    pub fn play_all(&self, shuffle: bool) {
        // album pages play in album order with album ReplayGain

        let this = self.clone();
        self.rt.spawn(async move {
            let Some(ctl) = this.controller() else { return };
            let Some((all, source)) = this.ensure_all().await else {
                return;
            };
            let album = source.kind == "album" && !shuffle;
            let outcome = ctl.play_all(all, Some(source), album, shuffle).await;
            this.apply(outcome).await;
        });
    }
}
