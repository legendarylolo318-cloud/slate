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
    /// A smart playlist (its id); the tracks are computed when it opens.
    Smart(String),
}

/// What the page header shows.
pub struct Header {
    pub label: &'static str,
    pub title: String,
    pub subtitle: String,
    pub cover: Option<String>,
    pub loved: bool,
    /// Quality badges (albums only).
    pub badges: crate::quality::Badges,
}

#[derive(Clone)]
struct Active {
    kind: SourceKind,
    title: String,
    /// Sidebar selection, so a re-sort can re-open the same list.
    selected: String,
    /// `order` / `orderDirection` for the paged fetches, fixed when opened.
    order: Option<(String, String)>,
}

impl LibState {
    /// What the track page shows: its source and title.
    pub(crate) fn active_for_export(&self) -> Option<(SourceKind, String)> {
        let active = self.active.lock().ok()?;
        let a = active.as_ref()?;
        Some((a.kind.clone(), a.title.clone()))
    }

    pub(crate) fn generation_now(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    pub(crate) fn total_now(&self) -> usize {
        self.total.load(Ordering::Relaxed)
    }
}

#[derive(Default)]
pub struct LibState {
    generation: AtomicU64,
    active: Mutex<Option<Active>>,
    pub(crate) tracks: Mutex<BTreeMap<usize, Track>>,
    pub(crate) total: AtomicUsize,
    user_id: Mutex<Option<u64>>,
    /// (uuid, title) of the sidebar's playlists, for smart-playlist rules that name one.
    pub(crate) playlist_titles: Mutex<Vec<(String, String)>>,
    /// Playlist uuid → TIDAL image id, for the page header.
    playlist_images: Mutex<HashMap<String, Option<String>>>,
    /// Mix items arrive in one response; paging serves slices of it.
    pub(crate) static_tracks: Mutex<Vec<TidalTrack>>,
    /// Artist page "Play top tracks" list.
    pub hero_tracks: Mutex<Vec<Track>>,
    /// While a filter is active: source index of each row shown (see `filter.rs`).
    pub(crate) view: Mutex<Option<Vec<usize>>>,
    cards_gen: AtomicU64,
    artist: Mutex<Option<u64>>,
    /// The sortable list on screen, if any.
    sort_list: Mutex<Option<crate::sort::List>>,
}

pub(crate) fn to_track(t: &TidalTrack) -> Option<Track> {
    Track::from_serialized(t)
}

pub(crate) fn row_of(t: &Track) -> RowData {
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
        quality: crate::quality::track_badges(t).tier,
        atmos: crate::quality::track_badges(t).atmos,
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
    /// Row `index` as the window shows it (after any filter) → its place in the list.
    /// The open playlist is shown in its own order, so dragging rows can reorder it.
    pub(crate) fn reorderable(&self) -> bool {
        match self.active_order() {
            None => true,
            Some((order, dir)) => order == "INDEX" && dir.eq_ignore_ascii_case("ASC"),
        }
    }

    pub(crate) fn real_index(&self, index: usize) -> usize {
        match self.lib.view.lock() {
            Ok(v) => v
                .as_ref()
                .and_then(|v| v.get(index).copied())
                .unwrap_or(index),
            Err(_) => index,
        }
    }

    pub(crate) fn cached_track(&self, index: usize) -> Option<Track> {
        let index = self.real_index(index);
        self.lib.tracks.lock().ok()?.get(&index).cloned()
    }

    /// The tracks a filtered view shows, in order. `None` when no filter is active.
    pub(crate) fn filtered_tracks(&self) -> Option<Vec<Track>> {
        let view = self.lib.view.lock().ok()?;
        let view = view.as_ref()?;
        let cache = self.lib.tracks.lock().ok()?;
        Some(view.iter().filter_map(|i| cache.get(i).cloned()).collect())
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
                                .map(|n| {
                                    item_count(n.into(), pl.number_of_videos.unwrap_or(0).into())
                                })
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
            if let Ok(mut t) = this.lib.playlist_titles.lock() {
                *t = data
                    .iter()
                    .map(|(u, title, _, _)| (u.clone(), title.clone()))
                    .collect();
            }
            this.load_smart_sidebar();
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

    /// Fetches the sidebar thumbnails again (after a scale-factor change).
    pub(crate) fn reload_playlist_covers(&self, w: &AppWindow) {
        let Ok(images) = self.lib.playlist_images.lock() else {
            return;
        };
        let ids: Vec<(usize, String, Option<String>)> = w
            .get_playlists()
            .iter()
            .enumerate()
            .filter(|(_, e)| !e.folder)
            .map(|(i, e)| {
                let uuid = e.uuid.to_string();
                let image = images.get(&uuid).cloned().flatten();
                (i, uuid, image)
            })
            .collect();
        drop(images);
        let this = self.clone();
        self.rt
            .spawn(async move { this.load_playlist_covers(ids).await });
    }

    /// Thumbnails for the sidebar, a few at a time; entries appear first, art fills in.
    async fn load_playlist_covers(&self, ids: Vec<(usize, String, Option<String>)>) {
        let Some(state) = self.state() else { return };
        stream::iter(ids)
            .for_each_concurrent(BULK_CONCURRENCY, |(i, uuid, image)| {
                let state = Arc::clone(&state);
                async move {
                    let Some(img) = image else { return };
                    let Some(px) = self
                        .images
                        .get_fit(&state, &tidal_image_url(&img, 160), 40)
                        .await
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
        if let Ok(mut v) = self.lib.view.lock() {
            *v = None;
        }
        self.suggest_reset();
        self.ui(|w| w.set_can_reorder(false));
        let list = match kind {
            SourceKind::Loved => Some(crate::sort::List::Loved),
            SourceKind::Playlist(_) => Some(crate::sort::List::Playlist),
            SourceKind::Album(_) | SourceKind::Mix(_) | SourceKind::Smart(_) => None,
        };
        let prefs = crate::uiprefs::UiPrefs::load();
        if let Ok(mut a) = self.lib.active.lock() {
            *a = Some(Active {
                kind: kind.clone(),
                title: String::new(),
                selected: selected.clone(),
                order: list.and_then(|l| l.api_params(&prefs)),
            });
        }
        self.show_sort(list, &prefs);
        self.show_loved_tabs(matches!(kind, SourceKind::Loved).then_some(0));

        let saved_quality = self.saved_quality_filter(&kind);
        let this = self.clone();
        self.ui(move |w| {
            let for_fetch = this.clone();
            let for_covers = this.clone();
            let model = LazyTracks::new(Box::new(move |page| {
                for_fetch.request_page(generation, page)
            }))
            .with_cover_refetch(Box::new(move |page| {
                for_covers.refetch_covers(generation, page)
            }));
            w.set_tracks(ModelRc::from(Rc::new(model)));
            w.set_view("tracks".into());
            w.set_has_page(true);
            w.set_page_title("".into());
            w.set_page_subtitle("".into());
            w.set_page_quality(0);
            w.set_page_atmos(false);
            w.set_page_loading(true);
            w.set_page_cover(Image::default());
            w.set_page_loved(false);
            w.set_selected_source(selected.into());
            w.set_filter_open(false);
            w.set_filter_text("".into());
            w.set_filter_status("".into());
            w.set_filter_quality(saved_quality);
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
            let badges = header.badges;
            this.ui(move |w| {
                w.set_page_quality(badges.tier);
                w.set_page_atmos(badges.atmos);
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
                        .get_fit(&state, &tidal_image_url(&image, 320), 160)
                        .await
                    {
                        let lib = Arc::clone(&this2.lib);
                        this2.ui(move |w| {
                            if lib.generation.load(Ordering::SeqCst) == generation {
                                w.set_page_cover(Image::from_rgba8(px));
                            }
                        });
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
                    if saved_quality != 0 {
                        this.page_filter_changed("", saved_quality);
                    }
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
            badges: Default::default(),
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
                        let count = item_count(
                            v["numberOfTracks"].as_u64().unwrap_or(0),
                            v["numberOfVideos"].as_u64().unwrap_or(0),
                        );
                        let count = match v["duration"].as_u64().filter(|d| *d > 0) {
                            Some(d) => format!("{count} · {}", human_duration(d)),
                            None => count,
                        };
                        let by = v["creator"]["name"].as_str().unwrap_or("");
                        let owned = match self.user_id().await {
                            Some(me) => v["creator"]["id"].as_u64() == Some(me),
                            None => false,
                        };
                        self.suggest_page_opened(uuid, owned);
                        let can = owned && self.reorderable();
                        self.ui(move |w| w.set_can_reorder(can));
                        Header {
                            label: "PLAYLIST",
                            title: v["title"].as_str().unwrap_or("Playlist").to_string(),
                            subtitle: if by.is_empty() {
                                count
                            } else {
                                format!("{by} · {count}")
                            },
                            cover: v["squareImage"]
                                .as_str()
                                .or_else(|| v["image"].as_str())
                                .map(str::to_string),
                            loved: false,
                            badges: Default::default(),
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
                    let total = a
                        .duration
                        .filter(|d| *d > 0)
                        .map(|d| human_duration(u64::from(d)));
                    let parts: Vec<String> = [artist, year.to_string(), format!("{count} tracks")]
                        .into_iter()
                        .chain(total)
                        .filter(|s| !s.is_empty())
                        .collect();
                    Header {
                        label: "ALBUM",
                        title: a.title,
                        subtitle: parts.join(" · "),
                        cover: a.cover,
                        loved: false,
                        badges: crate::quality::album_badges(
                            a.audio_quality.as_deref(),
                            a.media_metadata.as_ref(),
                            a.audio_modes.as_deref(),
                        ),
                    }
                }
                Err(_) => none("ALBUM", "Album"),
            },
            SourceKind::Smart(id) => match self.evaluate_smart(id).await {
                Ok((list, tracks)) => {
                    let count = tracks.len();
                    if let Ok(mut s) = self.lib.static_tracks.lock() {
                        *s = tracks;
                    }
                    Header {
                        label: "SMART PLAYLIST",
                        title: list.name,
                        subtitle: format!("{count} tracks"),
                        cover: None,
                        loved: false,
                        badges: Default::default(),
                    }
                }
                Err(msg) => {
                    self.ui(move |w| w.set_error(msg.into()));
                    none("SMART PLAYLIST", "Smart playlist")
                }
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
                        badges: Default::default(),
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

    /// The Tracks / Videos tabs on the Loved pages: `Some(tab)` shows them.
    pub(crate) fn show_loved_tabs(&self, tab: Option<i32>) {
        self.ui(move |w| {
            w.set_loved_tabs(tab.is_some());
            w.set_loved_tab(tab.unwrap_or(0));
        });
    }

    /// The tracks of `page` if every row of it is already loaded.
    pub(crate) fn loaded_page(&self, page: usize, wanted: usize) -> Option<Vec<Track>> {
        let cache = self.lib.tracks.lock().ok()?;
        (0..wanted)
            .map(|i| cache.get(&(page * PAGE_SIZE + i)).cloned())
            .collect()
    }

    /// One page of the open list, for exporting: nothing is added to the
    /// on-screen cache.
    pub(crate) async fn fetch_page_for_export(
        &self,
        kind: &SourceKind,
        page: usize,
    ) -> Result<(Vec<TidalTrack>, usize), String> {
        self.fetch_page(kind, page).await
    }

    fn active_order(&self) -> Option<(String, String)> {
        self.lib.active.lock().ok()?.as_ref()?.order.clone()
    }

    /// Shows or hides the sort controls for `list`.
    pub(crate) fn show_sort(
        &self,
        list: Option<crate::sort::List>,
        prefs: &crate::uiprefs::UiPrefs,
    ) {
        if let Ok(mut l) = self.lib.sort_list.lock() {
            *l = list;
        }
        let shown = list.map(|l| (l.label(prefs), l.current(prefs).1));
        self.ui(move |w| match shown {
            Some((label, desc)) => {
                w.set_sort_label(label.into());
                w.set_sort_desc(desc);
                w.set_sort_visible(true);
            }
            None => w.set_sort_visible(false),
        });
    }

    /// The sort button: `flip` reverses the direction, otherwise the next
    /// order is chosen. Saved, then the list reloads in the new order.
    pub fn change_sort(&self, flip: bool) {
        let Some(list) = self.lib.sort_list.lock().ok().and_then(|l| *l) else {
            return;
        };
        let reopen = self
            .lib
            .active
            .lock()
            .ok()
            .and_then(|a| a.as_ref().map(|a| (a.kind.clone(), a.selected.clone())));
        let this = self.clone();
        self.rt.spawn_blocking(move || {
            let mut prefs = crate::uiprefs::UiPrefs::load();
            let pref = if flip {
                list.flipped(&prefs)
            } else {
                list.cycled(&prefs)
            };
            prefs.sorts.insert(list.key().into(), pref);
            if let Err(e) = prefs.save() {
                tracing::warn!("could not save the sort order: {e}");
            }
            use crate::sort::List;
            match (list, reopen) {
                (List::Loved | List::Playlist, Some((kind, selected))) => this.open(kind, selected),
                (List::Albums, _) => this.show_library_albums(),
                (List::Artists, _) => this.show_library_artists(),
                (List::Mixes, _) => this.show_library_mixes(),
                _ => {}
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
    pub(crate) fn remember(&self, page: usize, tracks: &[TidalTrack]) -> Vec<RowData> {
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

    /// Thumbnails again for a page the model evicted (usually an image-cache hit).
    fn refetch_covers(&self, generation: u64, page: usize) {
        if self.lib.generation.load(Ordering::SeqCst) != generation {
            return;
        }
        let mut covers: HashMap<String, Vec<usize>> = HashMap::new();
        if let Ok(cache) = self.lib.tracks.lock() {
            for index in page * PAGE_SIZE..(page + 1) * PAGE_SIZE {
                if let Some(c) = cache.get(&index).and_then(|t| t.cover_id()) {
                    covers.entry(c.to_string()).or_default().push(index);
                }
            }
        }
        if !covers.is_empty() {
            self.load_row_covers(covers);
        }
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
                        if this.lib.generation.load(Ordering::SeqCst) != generation {
                            return;
                        }
                        let Some(px) = this
                            .images
                            .get_fit(&state, &tidal_image_url(&id, 160), 34)
                            .await
                        else {
                            return;
                        };
                        let lib = Arc::clone(&this.lib);
                        this.ui(move |w| {
                            if lib.generation.load(Ordering::SeqCst) != generation {
                                return;
                            }
                            let img = Image::from_rgba8(px);
                            with_tracks(&w, |m| m.set_cover(&indices, &img));
                        });
                    }
                })
                .await;
        });
    }

    pub(crate) async fn fetch_page(
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
                let (order, dir) = self
                    .active_order()
                    .unwrap_or_else(|| ("DATE".into(), "DESC".into()));
                lib_cmds::get_favorite_tracks(
                    &state,
                    self.app.clone(),
                    uid,
                    offset,
                    limit,
                    order,
                    dir,
                )
                .await
            }
            SourceKind::Playlist(uuid) => {
                let (order, dir) = self.active_order().unzip();
                lib_cmds::get_playlist_tracks_page(
                    &state,
                    self.app.clone(),
                    uuid.clone(),
                    offset,
                    limit,
                    order,
                    dir,
                )
                .await
            }
            SourceKind::Album(id) => pages::get_album_tracks(&state, *id, offset, limit).await,
            SourceKind::Mix(_) | SourceKind::Smart(_) => {
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
    pub(crate) async fn ensure_all(&self) -> Option<(Vec<Track>, Source)> {
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
            if let (Some(list), Some(active)) = (
                this.filtered_tracks(),
                this.lib.active.lock().ok().and_then(|a| a.clone()),
            ) {
                // Playing from a filtered view plays what the view shows.
                let source = Self::source_for(&active, list.clone());
                let outcome = ctl.play_from_source(track, list, Some(source), false).await;
                this.apply(outcome).await;
                return;
            }
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
            SourceKind::Smart(s) => ("smart", serde_json::Value::from(s.as_str())),
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
            let Some((mut all, mut source)) = this.ensure_all().await else {
                return;
            };
            if let Some(list) = this.filtered_tracks() {
                source.tracks = list.clone();
                all = list;
            }
            let album = source.kind == "album" && !shuffle && this.filtered_tracks().is_none();
            let outcome = ctl.play_all(all, Some(source), album, shuffle).await;
            this.apply(outcome).await;
        });
    }
}

/// "2 h 15 min", "45 min", "3 min".
pub(crate) fn human_duration(secs: u64) -> String {
    let mins = (secs + 30) / 60;
    match (mins / 60, mins % 60) {
        (0, m) => format!("{} min", m.max(1)),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// "12 tracks", or "3 videos · 12 tracks" for a mixed playlist.
fn item_count(tracks: u64, videos: u64) -> String {
    let n = |count: u64, one: &str, many: &str| {
        format!("{count} {}", if count == 1 { one } else { many })
    };
    if videos == 0 {
        n(tracks, "track", "tracks")
    } else {
        format!(
            "{} · {}",
            n(videos, "video", "videos"),
            n(tracks, "track", "tracks")
        )
    }
}

#[cfg(test)]
mod item_count_tests {
    #[test]
    fn durations_read_naturally() {
        assert_eq!(super::human_duration(20), "1 min");
        assert_eq!(super::human_duration(45 * 60), "45 min");
        assert_eq!(super::human_duration(3600), "1 h");
        assert_eq!(super::human_duration(2 * 3600 + 15 * 60 + 20), "2 h 15 min");
    }

    #[test]
    fn counts_read_like_the_original() {
        assert_eq!(super::item_count(12, 0), "12 tracks");
        assert_eq!(super::item_count(1, 0), "1 track");
        assert_eq!(super::item_count(12, 3), "3 videos · 12 tracks");
        assert_eq!(super::item_count(0, 1), "1 video · 0 tracks");
    }
}
