//! Navigation and card-based pages (home, search, library tabs, artist).
//!
//! Track-list pages (playlist, album, mix, loved) live in `library.rs`; this
//! module routes to them and renders everything made of cards.

use std::sync::Arc;

use futures_util::stream::{self, StreamExt};
use serde_json::Value;
use slate_core::commands::{library as lib_cmds, metadata, pages, search};
use slate_core::queue::{Source, Track};
use slate_core::tidal_api::{TidalAlbumDetail, TidalArtist, TidalArtistDetail, TidalPlaylist};
use slint::{Color, Image, ModelRc};

use crate::backend::Backend;
use crate::bootstrap::CardItem;
use crate::images::tidal_image_url;
use crate::library::SourceKind;
use crate::models::{with_card_rows, CardRows, CardSectionData, CoverRequest};

const GRID_PAGE: u32 = 100;
/// Logical size of a card cover (`Dim.card-w` - 16px in pages.slint).
const CARD_COVER: u32 = 152;
/// v2 endpoints reject `limit` above 50 (HTTP 400), so mixes page in 50s.
const MIXES_PAGE: u32 = slate_core::tidal_api::MAX_V2_FAVORITES_LIMIT;
const MAX_GRID_PAGES: u32 = 30;
/// Pages of one library list fetched at once; the core's request cap bounds it
/// further.
const GRID_CONCURRENCY: usize = 3;

#[derive(Clone, Debug, PartialEq)]
pub enum Route {
    Home,
    Albums,
    Artists,
    Mixes,
    Loved,
    LovedVideos,
    Playlist(String),
    Album(u64),
    Artist(u64),
    Mix(String),
    Smart(String),
    Search(String),
    /// A TIDAL editorial page (`pages/explore`, a genre, a mood…) and its title.
    Page(String, String),
    Folder(String),
    Profile,
    Settings,
}

impl Route {
    /// Sidebar-level routes by key.
    pub fn from_key(key: &str) -> Option<Self> {
        Some(match key {
            "home" => Self::Home,
            "albums" => Self::Albums,
            "artists" => Self::Artists,
            "mixes" => Self::Mixes,
            "loved" => Self::Loved,
            "loved-videos" => Self::LovedVideos,
            "explore" => Self::Page("pages/explore".into(), "Explore".into()),
            "settings" => Self::Settings,
            "profile" => Self::Profile,
            _ => return None,
        })
    }

    /// What the sidebar highlights while this route is shown.
    pub fn highlight(&self) -> String {
        match self {
            Self::Home => "home".into(),
            Self::Albums => "albums".into(),
            Self::Artists => "artists".into(),
            Self::Mixes => "mixes".into(),
            Self::Loved => "loved".into(),
            Self::Playlist(u) => u.clone(),
            Self::Smart(id) => format!("smart:{id}"),
            Self::Settings => "settings".into(),
            Self::Profile => "profile".into(),
            _ => String::new(),
        }
    }
}

#[derive(Default)]
pub struct Nav {
    stack: Vec<Route>,
    pos: usize,
}

impl Nav {
    pub fn push(&mut self, r: Route) {
        if self.stack.get(self.pos) == Some(&r) {
            return;
        }
        if !self.stack.is_empty() {
            self.stack.truncate(self.pos + 1);
        }
        self.stack.push(r);
        self.pos = self.stack.len() - 1;
    }

    pub fn back(&mut self) -> Option<Route> {
        (self.pos > 0).then(|| {
            self.pos -= 1;
            self.stack[self.pos].clone()
        })
    }

    pub fn forward(&mut self) -> Option<Route> {
        (self.pos + 1 < self.stack.len()).then(|| {
            self.pos += 1;
            self.stack[self.pos].clone()
        })
    }

    pub fn can_back(&self) -> bool {
        self.pos > 0
    }

    pub fn can_forward(&self) -> bool {
        self.pos + 1 < self.stack.len()
    }
}

/// A card as plain data (`Send`); the cover image is attached later.
#[derive(Clone, Debug, PartialEq)]
pub struct CardData {
    pub kind: &'static str,
    pub id: String,
    pub title: String,
    pub subtitle: String,
    /// TIDAL image id or full URL.
    pub image: Option<String>,
}

pub struct SectionData {
    pub title: String,
    pub wrap: bool,
    pub cards: Vec<CardData>,
}

pub struct Hero {
    pub label: String,
    pub image: Option<String>,
}

fn names(artist: &Option<TidalArtist>, artists: &Option<Vec<TidalArtist>>) -> String {
    artist
        .as_ref()
        .map(|a| a.name.clone())
        .or_else(|| {
            artists.as_ref().map(|v| {
                v.iter()
                    .map(|a| a.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
        })
        .unwrap_or_default()
}

fn video_card(v: &slate_core::tidal_api::TidalVideo) -> CardData {
    CardData {
        kind: "video",
        id: v.id.to_string(),
        title: v.title.clone(),
        subtitle: v
            .artist
            .as_ref()
            .map(|a| a.name.clone())
            .unwrap_or_default(),
        image: v.image_id.clone(),
    }
}

pub fn album_card(a: &TidalAlbumDetail) -> CardData {
    let year = a
        .release_date
        .as_deref()
        .and_then(|d| d.get(..4))
        .unwrap_or("");
    let by = names(&a.artist, &a.artists);
    CardData {
        kind: "album",
        id: a.id.to_string(),
        title: a.title.clone(),
        subtitle: [by.as_str(), year]
            .iter()
            .filter(|s| !s.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" · "),
        image: a.cover.clone(),
    }
}

pub fn artist_card(
    id: u64,
    name: &str,
    picture: &Option<String>,
    fallback: &Option<String>,
) -> CardData {
    CardData {
        kind: "artist",
        id: id.to_string(),
        title: name.to_string(),
        subtitle: "Artist".into(),
        image: picture.clone().or_else(|| fallback.clone()),
    }
}

pub fn playlist_card(p: &TidalPlaylist) -> CardData {
    CardData {
        kind: "playlist",
        id: p.uuid.clone(),
        title: p.title.clone(),
        subtitle: p
            .number_of_tracks
            .map(|n| format!("{n} tracks"))
            .unwrap_or_default(),
        image: p.image.clone(),
    }
}

fn track_card(t: &Track) -> CardData {
    CardData {
        kind: "track",
        id: t.id.to_string(),
        title: t.title().to_string(),
        subtitle: t.artist_name().to_string(),
        image: t.cover_id().map(str::to_string),
    }
}

/// The last search, kept so a quality chip can re-filter without asking TIDAL again.
static SEARCH: std::sync::Mutex<Option<(String, slate_core::tidal_api::TidalSearchResults)>> =
    std::sync::Mutex::new(None);

/// Search results as card sections; the Tracks section honours the quality chip.
fn search_sections(
    r: &slate_core::tidal_api::TidalSearchResults,
    quality: i32,
) -> Vec<SectionData> {
    let filter = slate_core::list_filter::QualityFilter::from_index(quality);
    let tracks: Vec<CardData> = r
        .tracks
        .iter()
        .filter_map(Track::from_serialized)
        .filter(|t| {
            let b = crate::quality::track_badges(t);
            filter.accepts(b.tier, b.atmos)
        })
        .map(|t| track_card(&t))
        .collect();
    let mut sections = Vec::new();
    let mut push = |title: &str, cards: Vec<CardData>| {
        if !cards.is_empty() {
            sections.push(SectionData {
                title: title.into(),
                wrap: false,
                cards,
            });
        }
    };
    push("Tracks", tracks);
    push(
        "Artists",
        r.artists
            .iter()
            .map(|a| artist_card(a.id, &a.name, &a.picture, &a.selected_album_cover_fallback))
            .collect(),
    );
    push("Videos", r.videos.iter().map(video_card).collect());
    push("Albums", r.albums.iter().map(album_card).collect());
    push("Playlists", r.playlists.iter().map(playlist_card).collect());
    sections
}

/// Best-effort card from an untyped home-feed item. Unknown shapes are skipped.
/// Sections of an editorial page as card rows. A section with more behind it
/// ends in a "See all" card that opens its own page.
fn editorial_sections(sections: &[slate_core::tidal_api::HomePageSection]) -> Vec<SectionData> {
    sections
        .iter()
        .filter_map(|s| {
            let mut cards: Vec<CardData> = s
                .items
                .as_array()?
                .iter()
                .filter_map(card_from_value)
                .collect();
            if cards.is_empty() {
                return None;
            }
            if let Some(path) = s.api_path.as_deref().filter(|_| s.has_more) {
                let title = if s.title.is_empty() {
                    "More"
                } else {
                    s.title.as_str()
                };
                cards.push(page_card(path, "See all", title));
            }
            // Genre and mood links read better as a wrapped grid.
            let links = s.section_type.starts_with("PAGE_LINKS");
            Some(SectionData {
                title: s.title.clone(),
                wrap: links,
                cards,
            })
        })
        .collect()
}

/// A stable muted colour per title, for tiles that have no artwork.
fn tile_tint(title: &str) -> Color {
    let hue = title
        .bytes()
        .fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)))
        % 360;
    Color::from_hsva(hue as f32, 0.5, 0.45, 1.0)
}

/// A TIDAL image UUID (`8-4-4-4-12` hex) or a URL, as opposed to a slug such as
/// "hiphop" or "k-pop".
fn is_image_ref(s: &str) -> bool {
    if s.starts_with("http") {
        return true;
    }
    let groups: Vec<&str> = s.split('-').collect();
    groups.iter().map(|g| g.len()).eq([8, 4, 4, 4, 12])
        && groups
            .iter()
            .all(|g| g.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// "1 album" / "2 albums".
fn count_label(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// A link to another page. The card id carries the path and the page title.
fn page_card(path: &str, label: &str, page_title: &str) -> CardData {
    CardData {
        kind: "page",
        id: format!("{path}|{page_title}"),
        title: label.to_string(),
        subtitle: String::new(),
        image: None,
    }
}

pub fn card_from_value(v: &Value) -> Option<CardData> {
    let s = |k: &str| v.get(k).and_then(Value::as_str);
    let img = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    // Explore's genre / mood links: a title and a page path, no id.
    if let (Some(path), Some(title), None) = (s("apiPath"), s("title"), v.get("id")) {
        let mut card = page_card(path, title, title);
        // TIDAL sends a bare slug ("hiphop") here, which the CDN refuses; only a
        // real image id or URL is worth requesting, the rest get a name tile.
        card.image = img("imageId").filter(|i| is_image_ref(i));
        return Some(card);
    }
    if let Some(uuid) = s("uuid") {
        return Some(CardData {
            kind: "playlist",
            id: uuid.to_string(),
            title: s("title")?.to_string(),
            subtitle: v
                .get("numberOfTracks")
                .and_then(Value::as_u64)
                .map(|n| format!("{n} tracks"))
                .unwrap_or_default(),
            image: img("squareImage").or_else(|| img("image")),
        });
    }
    if let (Some(id), Some(_)) = (s("id"), v.get("mixType").or_else(|| v.get("images"))) {
        let image = v
            .get("images")
            .and_then(|i| i.get("MEDIUM").or_else(|| i.get("SMALL")))
            .and_then(|i| i.get("url"))
            .and_then(Value::as_str)
            .map(str::to_string);
        return Some(CardData {
            kind: "mix",
            id: id.to_string(),
            title: s("title").unwrap_or("Mix").to_string(),
            subtitle: s("subTitle")
                .or_else(|| s("subtitle"))
                .unwrap_or("")
                .to_string(),
            image,
        });
    }
    let id = v.get("id").and_then(Value::as_u64)?;
    if v.get("duration").is_some() && v.get("album").is_some() {
        let t: Track = serde_json::from_value(v.clone()).ok()?;
        return Some(track_card(&t));
    }
    if v.get("numberOfTracks").is_some() || v.get("releaseDate").is_some() {
        let by = v
            .get("artists")
            .and_then(|a| a.get(0))
            .or_else(|| v.get("artist"))
            .and_then(|a| a.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("");
        return Some(CardData {
            kind: "album",
            id: id.to_string(),
            title: s("title")?.to_string(),
            subtitle: by.to_string(),
            image: img("cover"),
        });
    }
    if v.get("picture").is_some() || (v.get("name").is_some() && v.get("title").is_none()) {
        return Some(CardData {
            kind: "artist",
            id: id.to_string(),
            title: s("name")?.to_string(),
            subtitle: "Artist".into(),
            image: img("picture"),
        });
    }
    // Music video: a duration and a still, but no album.
    if v.get("duration").is_some() && v.get("imageId").is_some() {
        let by = v
            .get("artists")
            .and_then(|a| a.get(0))
            .or_else(|| v.get("artist"))
            .and_then(|a| a.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("");
        return Some(CardData {
            kind: "video",
            id: id.to_string(),
            title: s("title")?.to_string(),
            subtitle: by.to_string(),
            image: img("imageId"),
        });
    }
    None
}

impl Backend {
    // ---------- navigation ----------

    pub fn go(&self, key: &str) {
        if let Some(r) = Route::from_key(key) {
            self.navigate(r);
        }
    }

    pub fn navigate(&self, route: Route) {
        if let Ok(mut n) = self.nav.lock() {
            n.push(route.clone());
        }
        self.render(route);
    }

    pub fn back(&self) {
        let r = self.nav.lock().ok().and_then(|mut n| n.back());
        if let Some(r) = r {
            self.render(r);
        }
    }

    pub fn forward(&self) {
        let r = self.nav.lock().ok().and_then(|mut n| n.forward());
        if let Some(r) = r {
            self.render(r);
        }
    }

    /// Re-renders `route` without adding a history entry.
    pub fn navigate_refresh(&self, route: Route) {
        self.render(route);
    }

    fn render(&self, route: Route) {
        let (back, fwd) = self
            .nav
            .lock()
            .map(|n| (n.can_back(), n.can_forward()))
            .unwrap_or((false, false));
        let sel = route.highlight();
        self.ui(move |w| {
            // An error banner belongs to the page that failed; leaving it up
            // after navigating makes a healthy page look broken.
            w.set_error("".into());
            w.set_can_back(back);
            w.set_can_forward(fwd);
            w.set_selected_source(sel.into());
        });
        match route {
            Route::Home => self.show_home(),
            Route::Albums => self.show_library_albums(),
            Route::Artists => self.show_library_artists(),
            Route::Mixes => self.show_library_mixes(),
            Route::Page(path, title) => self.show_editorial_page(path, title),
            Route::Search(q) => self.show_search(q),
            Route::Folder(id) => self.show_folder(id),
            Route::Artist(id) => self.show_artist(id),
            Route::Loved => self.open(SourceKind::Loved, "loved".into()),
            Route::LovedVideos => self.show_loved_videos(),
            Route::Playlist(u) => self.open(SourceKind::Playlist(u.clone()), u),
            Route::Album(id) => self.open(SourceKind::Album(id), String::new()),
            Route::Mix(id) => self.open(SourceKind::Mix(id), String::new()),
            Route::Smart(id) => self.open(SourceKind::Smart(id.clone()), format!("smart:{id}")),
            Route::Profile => {
                self.load_profile();
                self.ui(|w| w.set_view("profile".into()));
            }
            Route::Settings => {
                self.load_settings_ui();
                self.ui(|w| w.set_view("settings".into()));
            }
        }
    }

    pub fn open_playlist(&self, uuid: &str) {
        if let Some(id) = uuid.strip_prefix("smart:") {
            self.navigate(Route::Smart(id.to_string()));
            return;
        }
        match uuid.strip_prefix("folder:") {
            Some(folder) => self.navigate(Route::Folder(folder.to_string())),
            None => self.navigate(Route::Playlist(uuid.to_string())),
        }
    }

    fn show_folder(&self, id: String) {
        let generation = self.begin_cards("Folder");
        let this = self.clone();
        self.rt.spawn(async move {
            let res = this.fetch_folder_playlists(&id).await;
            if this.lib.cards_generation() != generation {
                return;
            }
            match res {
                Ok(cards) => {
                    let n = cards.len();
                    this.show_cards(
                        "Folder".into(),
                        count_label(n, "playlist", "playlists"),
                        vec![SectionData {
                            title: String::new(),
                            wrap: true,
                            cards,
                        }],
                        None,
                    );
                }
                Err(e) => this.fail_cards(format!("Could not open folder: {e}")),
            }
        });
    }

    pub fn search(&self, query: &str) {
        let q = query.trim();
        if !q.is_empty() {
            self.navigate(Route::Search(q.to_string()));
        }
    }

    /// A card was clicked.
    pub fn open_card(&self, kind: &str, id: &str) {
        match kind {
            "page" => {
                let (path, title) = id.split_once('|').unwrap_or((id, "Explore"));
                self.navigate(Route::Page(path.to_string(), title.to_string()));
            }
            "album" => {
                if let Ok(n) = id.parse() {
                    self.navigate(Route::Album(n));
                }
            }
            "artist" => {
                if let Ok(n) = id.parse() {
                    self.navigate(Route::Artist(n));
                }
            }
            "playlist" => self.navigate(Route::Playlist(id.to_string())),
            "mix" => self.navigate(Route::Mix(id.to_string())),
            "track" => self.play_track_id(id),
            "video" => {
                if let Ok(n) = id.parse() {
                    self.play_video(n);
                }
            }
            _ => {}
        }
    }

    /// A card's hover play button was clicked: play the whole thing.
    pub fn play_card(&self, kind: &str, id: &str) {
        let (kind, id) = (kind.to_string(), id.to_string());
        let this = self.clone();
        self.rt.spawn(async move {
            let (Some(state), Some(ctl)) = (this.state(), this.controller()) else {
                return;
            };
            let src = match kind.as_str() {
                "album" => id.parse().ok().map(SourceKind::Album),
                "playlist" => Some(SourceKind::Playlist(id.clone())),
                "mix" => Some(SourceKind::Mix(id.clone())),
                "track" => {
                    this.play_track_id(&id);
                    return;
                }
                _ => None,
            };
            let Some(src) = src else { return };
            let all = match fetch_all(&state, &this, &src).await {
                Ok(a) => a,
                Err(e) => {
                    this.ui(move |w| w.set_error(e.into()));
                    return;
                }
            };
            let source = Source {
                kind: kind.clone(),
                id: Value::from(id),
                name: String::new(),
                image: None,
                subtitle: None,
                mix_type: None,
                tracks: all.clone(),
            };
            let album = kind == "album";
            let outcome = ctl.play_all(all, Some(source), album, false).await;
            this.apply(outcome).await;
        });
    }

    /// Plays one track by id (a track card or a "go to" target).
    pub fn play_track_id(&self, id: &str) {
        let Ok(id) = id.parse::<u64>() else { return };
        let this = self.clone();
        self.rt.spawn(async move {
            let (Some(state), Some(ctl)) = (this.state(), this.controller()) else {
                return;
            };
            match metadata::get_track(&state, id).await {
                Ok(v) => {
                    if let Ok(track) = serde_json::from_value::<Track>(v) {
                        let outcome = ctl.play_single(track).await;
                        this.apply(outcome).await;
                    }
                }
                Err(e) => {
                    let msg = format!("Could not load track: {}", e.log_safe());
                    this.ui(move |w| w.set_error(msg.into()));
                }
            }
        });
    }

    // ---------- card pages ----------

    /// Publishes a card page and starts loading its covers.
    fn show_cards(
        &self,
        title: String,
        subtitle: String,
        sections: Vec<SectionData>,
        hero: Option<Hero>,
    ) {
        let generation = self.lib.bump_cards();
        let hero_image = hero.as_ref().and_then(|h| h.image.clone());
        let hero_label = hero.map(|h| h.label);
        let this = self.clone();
        self.ui(move |w| {
            let sections: Vec<CardSectionData> = sections
                .into_iter()
                .map(|s| CardSectionData {
                    title: s.title.into(),
                    wrap: s.wrap,
                    cards: s
                        .cards
                        .into_iter()
                        .map(|c| {
                            let item = CardItem {
                                kind: c.kind.into(),
                                id: c.id.into(),
                                tint: tile_tint(&c.title),
                                title: c.title.into(),
                                subtitle: c.subtitle.into(),
                                cover: Image::default(),
                            };
                            (item, c.image)
                        })
                        .collect(),
                })
                .collect();
            // Covers are fetched as their rows come on screen.
            let cols = usize::try_from(w.get_card_cols()).unwrap_or(1);
            let fetch = this.clone();
            let rows = CardRows::new(
                sections,
                cols,
                Box::new(move |epoch, req| fetch.load_card_covers(generation, epoch, req)),
            );
            w.set_card_rows(ModelRc::new(rows));
            w.set_cards_title(title.into());
            w.set_cards_subtitle(subtitle.into());
            w.set_cards_loading(false);
            w.set_has_hero(hero_label.is_some());
            w.set_hero_label(hero_label.unwrap_or_default().into());
            w.set_hero_image(Image::default());
            w.set_view("cards".into());
        });

        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            if let Some(img) = hero_image {
                if let Some(px) = this
                    .images
                    .get_fit(&state, &tidal_image_url(&img, 320), 160)
                    .await
                {
                    let lib = Arc::clone(&this.lib);
                    this.ui(move |w| {
                        if lib.cards_generation() == generation {
                            w.set_hero_image(Image::from_rgba8(px));
                        }
                    });
                }
            }
        });
    }

    /// Fetches covers for card rows that came on screen.
    fn load_card_covers(&self, generation: u64, epoch: u64, req: CoverRequest) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            stream::iter(req)
                .for_each_concurrent(6, |(s, c, img)| {
                    let (state, this) = (Arc::clone(&state), this.clone());
                    async move {
                        // Stop fetching once the page is gone.
                        if this.lib.cards_generation() != generation {
                            return;
                        }
                        let Some(px) = this
                            .images
                            .get_fit(&state, &tidal_image_url(&img, 320), CARD_COVER)
                            .await
                        else {
                            return;
                        };
                        // Checked again on the UI thread: a newer page may have been
                        // published between here and there.
                        let lib = Arc::clone(&this.lib);
                        this.ui(move |w| {
                            if lib.cards_generation() == generation {
                                with_card_rows(&w, |m| {
                                    m.set_cover(epoch, s, c, Image::from_rgba8(px))
                                });
                            }
                        });
                    }
                })
                .await;
        });
    }

    /// The window moved to a monitor with another scale factor: thumbnails are
    /// decoded again at the new size (from the disk cache, not the network).
    pub fn scale_factor_changed(&self, scale: f32) {
        self.images.set_scale(scale);
        let Some(w) = self.ui.upgrade() else { return };
        with_card_rows(&w, |m| m.refresh_covers());
        crate::models::with_tracks(&w, |m| m.refresh_covers());
        self.reload_playlist_covers(&w);
    }

    /// The card page's column count changed (resize, scale): regroup its rows.
    pub fn card_cols_changed(&self, cols: i32) {
        if let Some(w) = self.ui.upgrade() {
            with_card_rows(&w, |m| m.set_cols(usize::try_from(cols).unwrap_or(1)));
        }
    }

    fn begin_cards(&self, title: &str) -> u64 {
        let generation = self.lib.bump_cards();
        self.show_sort(None, &crate::uiprefs::UiPrefs::default());
        self.show_loved_tabs(None);
        let title = title.to_string();
        self.ui(move |w| {
            w.set_chips_visible(false);
            w.set_cards_title(title.into());
            w.set_cards_subtitle("".into());
            w.set_cards_loading(true);
            w.set_has_hero(false);
            w.set_card_rows(ModelRc::new(CardRows::new(
                Vec::new(),
                1,
                Box::new(|_, _| {}),
            )));
            w.set_view("cards".into());
        });
        generation
    }

    fn fail_cards(&self, msg: String) {
        self.ui(move |w| {
            w.set_cards_loading(false);
            w.set_error(msg.into());
        });
    }

    fn show_home(&self) {
        let generation = self.begin_cards("Home");
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            match pages::get_home_page(&state, this.app.clone(), None).await {
                Ok(page) => {
                    let v = serde_json::to_value(&page).unwrap_or(Value::Null);
                    let sections: Vec<SectionData> = v["home"]["sections"]
                        .as_array()
                        .map(|a| a.as_slice())
                        .unwrap_or_default()
                        .iter()
                        .filter_map(|s| {
                            let cards: Vec<CardData> = s["items"]
                                .as_array()?
                                .iter()
                                .filter_map(card_from_value)
                                .collect();
                            (!cards.is_empty()).then(|| SectionData {
                                title: s["title"].as_str().unwrap_or("").to_string(),
                                wrap: false,
                                cards,
                            })
                        })
                        .collect();
                    if this.lib.cards_generation() == generation {
                        this.show_cards("Home".into(), String::new(), sections, None);
                    }
                }
                Err(e) => this.fail_cards(format!("Could not load Home: {}", e.log_safe())),
            }
        });
    }

    /// Pages through a paginated favourites endpoint, mapping items to cards.
    async fn collect_grid<T, F, Fut>(
        &self,
        generation: u64,
        page_size: u32,
        fetch: F,
        map: impl Fn(&T) -> CardData,
    ) -> Result<Vec<CardData>, String>
    where
        F: FnMut(u32) -> Fut,
        Fut: std::future::Future<
            Output = Result<slate_core::tidal_api::PaginatedResponse<T>, slate_core::SlateError>,
        >,
    {
        collect_pages(
            page_size,
            GRID_CONCURRENCY,
            || self.lib.cards_generation() == generation,
            fetch,
            map,
        )
        .await
    }

    /// Loved → Videos: favourite music videos as a grid; a card plays it.
    /// An editorial page: its sections as card rows. Genre/mood links and
    /// "See all" open further pages.
    pub(crate) fn show_editorial_page(&self, path: String, title: String) {
        let generation = self.begin_cards(&title);
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            match pages::get_page_section(&state, this.app.clone(), path).await {
                Ok(page) => {
                    let sections = editorial_sections(&page.sections);
                    if this.lib.cards_generation() == generation {
                        this.show_cards(title, String::new(), sections, None);
                    }
                }
                Err(e) => this.fail_cards(format!("Could not load {title}: {}", e.log_safe())),
            }
        });
    }

    pub(crate) fn show_loved_videos(&self) {
        let generation = self.begin_cards("Loved Videos");
        self.show_loved_tabs(Some(1));
        let this = self.clone();
        self.rt.spawn(async move {
            let (Some(state), Some(uid)) = (this.state(), this.user_id().await) else {
                return;
            };
            let mut cards = Vec::new();
            let mut failed = None;
            for page in 0.. {
                if this.lib.cards_generation() != generation {
                    return;
                }
                match lib_cmds::get_favorite_videos(&state, uid, page * GRID_PAGE, GRID_PAGE).await
                {
                    Ok(videos) => {
                        let got = videos.len();
                        cards.extend(videos.iter().map(video_card));
                        if got < GRID_PAGE as usize || page >= 50 {
                            break;
                        }
                    }
                    Err(e) => {
                        failed = Some(e.log_safe());
                        break;
                    }
                }
            }
            if this.lib.cards_generation() != generation {
                return;
            }
            match failed {
                Some(e) if cards.is_empty() => {
                    this.fail_cards(format!("Could not load loved videos: {e}"))
                }
                _ => {
                    let n = cards.len();
                    this.show_cards(
                        "Loved Videos".into(),
                        count_label(n, "video", "videos"),
                        vec![SectionData {
                            title: String::new(),
                            wrap: true,
                            cards,
                        }],
                        None,
                    );
                    this.show_loved_tabs(Some(1));
                }
            }
        });
    }

    pub(crate) fn show_library_albums(&self) {
        let generation = self.begin_cards("Albums");
        let prefs = crate::uiprefs::UiPrefs::load();
        self.show_sort(Some(crate::sort::List::Albums), &prefs);
        let (sort_by, sort_desc) = crate::sort::List::Albums.current(&prefs);
        let this = self.clone();
        self.rt.spawn(async move {
            let (Some(state), Some(uid)) = (this.state(), this.user_id().await) else {
                return;
            };
            let res = this
                .collect_grid(
                    generation,
                    GRID_PAGE,
                    |off| {
                        lib_cmds::get_favorite_albums(
                            &state,
                            this.app.clone(),
                            uid,
                            off,
                            GRID_PAGE,
                            "DATE".into(),
                            "DESC".into(),
                        )
                    },
                    album_card,
                )
                .await;
            if this.lib.cards_generation() != generation {
                return;
            }
            match res {
                Ok(mut cards) => {
                    crate::sort::sort_cards(&mut cards, sort_by, sort_desc);
                    let n = cards.len();
                    this.show_cards(
                        "Albums".into(),
                        count_label(n, "album", "albums"),
                        vec![SectionData {
                            title: String::new(),
                            wrap: true,
                            cards,
                        }],
                        None,
                    );
                }
                Err(e) => this.fail_cards(format!("Could not load albums: {e}")),
            }
        });
    }

    pub(crate) fn show_library_artists(&self) {
        let generation = self.begin_cards("Artists");
        let prefs = crate::uiprefs::UiPrefs::load();
        self.show_sort(Some(crate::sort::List::Artists), &prefs);
        let (sort_by, sort_desc) = crate::sort::List::Artists.current(&prefs);
        let this = self.clone();
        self.rt.spawn(async move {
            let (Some(state), Some(uid)) = (this.state(), this.user_id().await) else {
                return;
            };
            let res = this
                .collect_grid(
                    generation,
                    GRID_PAGE,
                    |off| {
                        lib_cmds::get_favorite_artists(
                            &state,
                            this.app.clone(),
                            uid,
                            off,
                            GRID_PAGE,
                            "DATE".into(),
                            "DESC".into(),
                        )
                    },
                    |a: &TidalArtistDetail| {
                        artist_card(a.id, &a.name, &a.picture, &a.selected_album_cover_fallback)
                    },
                )
                .await;
            if this.lib.cards_generation() != generation {
                return;
            }
            match res {
                Ok(mut cards) => {
                    crate::sort::sort_cards(&mut cards, sort_by, sort_desc);
                    let n = cards.len();
                    this.show_cards(
                        "Artists".into(),
                        count_label(n, "artist", "artists"),
                        vec![SectionData {
                            title: String::new(),
                            wrap: true,
                            cards,
                        }],
                        None,
                    );
                }
                Err(e) => this.fail_cards(format!("Could not load artists: {e}")),
            }
        });
    }

    pub(crate) fn show_library_mixes(&self) {
        let generation = self.begin_cards("Mixes");
        let prefs = crate::uiprefs::UiPrefs::load();
        self.show_sort(Some(crate::sort::List::Mixes), &prefs);
        let (sort_by, sort_desc) = crate::sort::List::Mixes.current(&prefs);
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            let res = this
                .collect_grid(
                    generation,
                    MIXES_PAGE,
                    |off| {
                        lib_cmds::get_favorite_mixes(
                            &state,
                            this.app.clone(),
                            off,
                            MIXES_PAGE,
                            "DATE".into(),
                            "DESC".into(),
                        )
                    },
                    |m: &slate_core::tidal_api::TidalFavoriteMix| CardData {
                        kind: "mix",
                        id: m.id.clone(),
                        title: m.title.clone().unwrap_or_else(|| "Mix".into()),
                        subtitle: m.sub_title.clone().unwrap_or_default(),
                        image: m
                            .images
                            .as_ref()
                            .and_then(|i| i.medium.as_ref().or(i.small.as_ref()))
                            .map(|u| u.url.clone()),
                    },
                )
                .await;
            if this.lib.cards_generation() != generation {
                return;
            }
            match res {
                Ok(mut cards) => {
                    crate::sort::sort_cards(&mut cards, sort_by, sort_desc);
                    let n = cards.len();
                    this.show_cards(
                        "Mixes".into(),
                        count_label(n, "mix", "mixes"),
                        vec![SectionData {
                            title: String::new(),
                            wrap: true,
                            cards,
                        }],
                        None,
                    );
                }
                Err(e) => this.fail_cards(format!("Could not load mixes: {e}")),
            }
        });
    }

    fn show_search(&self, query: String) {
        let generation = self.begin_cards(&format!("Search: {query}"));
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            match search::search_tidal(&state, query.clone(), 30).await {
                Ok(r) => {
                    let quality = this.saved_search_quality();
                    if let Ok(mut s) = SEARCH.lock() {
                        *s = Some((query.clone(), r.clone()));
                    }
                    let sections = search_sections(&r, quality);
                    if this.lib.cards_generation() == generation {
                        this.show_cards(format!("Search: {query}"), String::new(), sections, None);
                        this.ui(move |w| {
                            w.set_chips_visible(true);
                            w.set_chip_index(quality);
                        });
                    }
                }
                Err(e) => this.fail_cards(format!("Search failed: {}", e.log_safe())),
            }
        });
    }

    fn saved_search_quality(&self) -> i32 {
        crate::uiprefs::UiPrefs::load()
            .quality_filters
            .get("search")
            .copied()
            .unwrap_or(0)
    }

    /// A quality chip on the search page: re-shows the stored results with the tracks filtered.
    pub fn search_chip(&self, quality: i32) {
        let mut prefs = crate::uiprefs::UiPrefs::load();
        if quality == 0 {
            prefs.quality_filters.remove("search");
        } else {
            prefs.quality_filters.insert("search".into(), quality);
        }
        let _ = prefs.save();
        let stored = SEARCH.lock().ok().and_then(|s| s.clone());
        let Some((query, r)) = stored else { return };
        self.show_cards(
            format!("Search: {query}"),
            String::new(),
            search_sections(&r, quality),
            None,
        );
        self.ui(move |w| {
            w.set_chips_visible(true);
            w.set_chip_index(quality);
        });
    }

    fn show_artist(&self, id: u64) {
        let generation = self.begin_cards("Artist");
        self.lib.set_artist(Some(id));
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            let (detail, top, albums) = tokio::join!(
                pages::get_artist_detail(&state, id),
                pages::get_artist_top_tracks(&state, this.app.clone(), id, 10),
                pages::get_artist_albums(&state, id, 50),
            );
            if this.lib.cards_generation() != generation {
                return;
            }
            let (name, picture) = match &detail {
                Ok(d) => (
                    d.name.clone(),
                    d.picture
                        .clone()
                        .or_else(|| d.selected_album_cover_fallback.clone()),
                ),
                Err(e) => {
                    this.fail_cards(format!("Could not load artist: {}", e.log_safe()));
                    return;
                }
            };
            let tracks: Vec<Track> = top
                .unwrap_or_default()
                .iter()
                .filter_map(Track::from_serialized)
                .collect();
            let mut sections = Vec::new();
            if !tracks.is_empty() {
                sections.push(SectionData {
                    title: "Popular".into(),
                    wrap: false,
                    cards: tracks.iter().map(track_card).collect(),
                });
            }
            if let Ok(a) = albums {
                let cards: Vec<CardData> = a.iter().map(album_card).collect();
                if !cards.is_empty() {
                    sections.push(SectionData {
                        title: "Discography".into(),
                        wrap: true,
                        cards,
                    });
                }
            }
            if let Ok(mut h) = this.lib.hero_tracks.lock() {
                *h = tracks;
            }
            this.show_cards(
                name,
                String::new(),
                sections,
                Some(Hero {
                    label: "ARTIST".into(),
                    image: picture,
                }),
            );
        });
    }

    pub fn hero_play(&self, shuffle: bool) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(ctl) = this.controller() else { return };
            let all = this
                .lib
                .hero_tracks
                .lock()
                .map(|h| h.clone())
                .unwrap_or_default();
            let source = Source {
                kind: "artist".into(),
                id: Value::Null,
                name: "Top tracks".into(),
                image: None,
                subtitle: None,
                mix_type: None,
                tracks: all.clone(),
            };
            let outcome = ctl.play_all(all, Some(source), false, shuffle).await;
            this.apply(outcome).await;
        });
    }
}

/// Every track of a source (used by hover-play on cards).
pub(crate) async fn fetch_all(
    state: &Arc<slate_core::AppState>,
    this: &Backend,
    kind: &SourceKind,
) -> Result<Vec<Track>, String> {
    let mut out = Vec::new();
    for page in 0..200usize {
        let off = u32::try_from(page * 100).unwrap_or(u32::MAX);
        let (items, total) = match kind {
            SourceKind::Album(id) => {
                let p = pages::get_album_tracks(state, *id, off, 100)
                    .await
                    .map_err(|e| e.log_safe())?;
                (p.items, p.total_number_of_items as usize)
            }
            SourceKind::Playlist(u) => {
                let p = lib_cmds::get_playlist_tracks_page(
                    state,
                    this.app.clone(),
                    u.clone(),
                    off,
                    100,
                    None,
                    None,
                )
                .await
                .map_err(|e| e.log_safe())?;
                (p.items, p.total_number_of_items as usize)
            }
            SourceKind::Mix(m) => {
                let r = pages::get_mix_items(state, m.clone())
                    .await
                    .map_err(|e| e.log_safe())?;
                let n = r.tracks.len();
                (r.tracks, n)
            }
            SourceKind::Loved => return Err("unsupported".into()),
            SourceKind::Smart(id) => {
                let (_, tracks) = this.evaluate_smart(id).await?;
                let n = tracks.len();
                (tracks, n)
            }
        };
        let got = items.len();
        out.extend(items.iter().filter_map(Track::from_serialized));
        if got == 0
            || out.len() >= total
            || matches!(kind, SourceKind::Mix(_) | SourceKind::Smart(_))
        {
            break;
        }
    }
    Ok(out)
}

/// Fetches a paginated list: the first page for the total, then the remaining
/// pages a few at a time (in order). Stops early, without fetching, once
/// `alive` says the page being built is no longer the one on screen. Never
/// asks for more pages than the total needs.
async fn collect_pages<T, F, Fut>(
    page_size: u32,
    concurrency: usize,
    alive: impl Fn() -> bool,
    mut fetch: F,
    map: impl Fn(&T) -> CardData,
) -> Result<Vec<CardData>, String>
where
    F: FnMut(u32) -> Fut,
    Fut: std::future::Future<
        Output = Result<slate_core::tidal_api::PaginatedResponse<T>, slate_core::SlateError>,
    >,
{
    use futures_util::StreamExt;

    if !alive() {
        return Ok(Vec::new());
    }
    let first = fetch(0).await.map_err(|e| e.log_safe())?;
    let mut cards: Vec<CardData> = first.items.iter().map(&map).collect();
    let total = first.total_number_of_items as usize;
    if first.items.is_empty() || cards.len() >= total {
        return Ok(cards);
    }
    let pages = total
        .div_ceil(page_size.max(1) as usize)
        .min(MAX_GRID_PAGES as usize) as u32;
    let alive = &alive;
    let mut rest = futures_util::stream::iter(1..pages)
        .map(|page| {
            let request = alive().then(|| fetch(page * page_size));
            async move {
                match request {
                    Some(request) => Some(request.await),
                    None => None,
                }
            }
        })
        .buffered(concurrency.max(1));
    while let Some(result) = rest.next().await {
        let Some(result) = result else { break };
        let page = result.map_err(|e| e.log_safe())?;
        if page.items.is_empty() {
            break;
        }
        cards.extend(page.items.iter().map(&map));
    }
    Ok(cards)
}

#[cfg(test)]
mod tests {
    fn fake_page(
        offset: u32,
        size: u32,
        total: u32,
    ) -> slate_core::tidal_api::PaginatedResponse<u32> {
        let items = (offset..total.min(offset + size)).collect();
        slate_core::tidal_api::PaginatedResponse {
            items,
            total_number_of_items: total,
            offset,
            limit: size,
        }
    }

    fn id_card(n: &u32) -> CardData {
        CardData {
            kind: "album",
            id: n.to_string(),
            title: String::new(),
            subtitle: String::new(),
            image: None,
        }
    }

    #[tokio::test]
    async fn collect_pages_keeps_order_and_asks_for_each_page_once() {
        let asked = std::sync::Mutex::new(Vec::new());
        let cards = collect_pages(
            10,
            3,
            || true,
            |off| {
                asked.lock().unwrap().push(off);
                // Later pages answer first.
                let delay = 50u64.saturating_sub(u64::from(off));
                async move {
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    Ok(fake_page(off, 10, 35))
                }
            },
            id_card,
        )
        .await
        .unwrap();
        let ids: Vec<String> = cards.into_iter().map(|c| c.id).collect();
        assert_eq!(ids, (0..35).map(|n| n.to_string()).collect::<Vec<_>>());
        let mut asked = asked.into_inner().unwrap();
        asked.sort();
        assert_eq!(asked, vec![0, 10, 20, 30]);
    }

    #[tokio::test]
    async fn collect_pages_with_one_page_asks_once_and_a_dead_page_asks_never() {
        let asked = std::sync::atomic::AtomicU32::new(0);
        let fetch = |off| {
            asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async move { Ok(fake_page(off, 10, 7)) }
        };
        let cards = collect_pages(10, 3, || true, fetch, id_card).await.unwrap();
        assert_eq!(cards.len(), 7);
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1);

        let cards = collect_pages(10, 3, || false, fetch, id_card)
            .await
            .unwrap();
        assert!(cards.is_empty());
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn collect_pages_stops_fetching_when_the_page_goes_away() {
        let asked = std::sync::atomic::AtomicU32::new(0);
        let gone = std::sync::atomic::AtomicBool::new(false);
        let _ = collect_pages(
            10,
            1,
            || !gone.load(std::sync::atomic::Ordering::SeqCst),
            |off| {
                asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                gone.store(true, std::sync::atomic::Ordering::SeqCst);
                async move { Ok(fake_page(off, 10, 100)) }
            },
            id_card,
        )
        .await;
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn collect_pages_reports_a_failed_page() {
        let err = collect_pages(
            10,
            3,
            || true,
            |off| async move {
                if off == 10 {
                    Err(slate_core::SlateError::Network("down".into()))
                } else {
                    Ok(fake_page(off, 10, 30))
                }
            },
            id_card,
        )
        .await
        .unwrap_err();
        assert!(err.contains("down") || !err.is_empty());
    }

    #[test]
    fn slug_image_ids_are_not_requested() {
        for slug in ["pop", "k-pop", "r-b-soul"] {
            let c = card_from_value(
                &json!({ "title": "Pop", "apiPath": "pages/genre_pop", "imageId": slug }),
            )
            .unwrap();
            assert_eq!(c.image, None, "{slug}");
        }
        let uuid = "0a1b2c3d-4e5f-6789-abcd-ef0123456789";
        let c = card_from_value(
            &json!({ "title": "Pop", "apiPath": "pages/genre_pop", "imageId": uuid }),
        )
        .unwrap();
        assert!(c.image.is_some());
    }

    #[test]
    fn tile_tint_is_stable_per_title() {
        assert_eq!(tile_tint("Pop"), tile_tint("Pop"));
        assert_ne!(tile_tint("Pop"), tile_tint("Latin"));
    }

    #[test]
    fn count_labels_are_singular_for_one() {
        assert_eq!(count_label(0, "album", "albums"), "0 albums");
        assert_eq!(count_label(1, "album", "albums"), "1 album");
        assert_eq!(count_label(2, "mix", "mixes"), "2 mixes");
    }

    use super::*;
    use serde_json::json;

    #[test]
    fn history_back_forward_and_truncation() {
        let mut n = Nav::default();
        n.push(Route::Home);
        n.push(Route::Albums);
        n.push(Route::Artists);
        assert!(n.can_back() && !n.can_forward());
        assert_eq!(n.back(), Some(Route::Albums));
        assert_eq!(n.forward(), Some(Route::Artists));
        n.back();
        n.back();
        n.push(Route::Mixes); // drops the forward branch
        assert!(!n.can_forward());
        assert_eq!(n.back(), Some(Route::Home));
        assert_eq!(n.back(), None);
    }

    #[test]
    fn duplicate_push_is_ignored() {
        let mut n = Nav::default();
        n.push(Route::Home);
        n.push(Route::Home);
        assert!(!n.can_back());
    }

    #[test]
    fn route_keys_round_trip() {
        assert_eq!(Route::from_key("albums"), Some(Route::Albums));
        assert_eq!(Route::from_key("nope"), None);
        assert_eq!(Route::Playlist("u".into()).highlight(), "u");
    }

    #[test]
    fn classifies_home_items() {
        let pl = card_from_value(
            &json!({"uuid":"u1","title":"P","numberOfTracks":3,"squareImage":"a-b"}),
        );
        assert_eq!(
            pl.map(|c| (c.kind, c.subtitle)),
            Some(("playlist", "3 tracks".into()))
        );
        let mix = card_from_value(
            &json!({"id":"m1","mixType":"X","title":"Mix","images":{"MEDIUM":{"url":"https://x/y.jpg"}}}),
        );
        assert_eq!(mix.and_then(|c| c.image), Some("https://x/y.jpg".into()));
        let album = card_from_value(
            &json!({"id":5,"title":"A","numberOfTracks":9,"artists":[{"name":"Z"}],"cover":"c-d"}),
        );
        assert_eq!(
            album.map(|c| (c.kind, c.subtitle)),
            Some(("album", "Z".into()))
        );
        let artist = card_from_value(&json!({"id":7,"name":"N","picture":"p-q"}));
        assert_eq!(artist.map(|c| c.kind), Some("artist"));
        assert!(card_from_value(&json!({"foo":1})).is_none());
    }
}

#[cfg(test)]
mod editorial_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn page_links_become_page_cards_carrying_path_and_title() {
        let c = card_from_value(
            &json!({ "title": "Pop", "apiPath": "pages/genre_pop", "imageId": "0a1b2c3d-4e5f-6789-abcd-ef0123456789" }),
        )
        .expect("a page link");
        assert_eq!(
            (c.kind, c.id.as_str(), c.title.as_str()),
            ("page", "pages/genre_pop|Pop", "Pop")
        );
        assert_eq!(
            c.image.as_deref(),
            Some("0a1b2c3d-4e5f-6789-abcd-ef0123456789")
        );
        // Anything with an id is still content, not a link.
        let album =
            card_from_value(&json!({ "id": 7, "title": "A", "numberOfTracks": 3, "apiPath": "x" }));
        assert_eq!(album.map(|c| c.kind), Some("album"));
        let video = card_from_value(&json!({ "id": 9, "title": "V", "duration": 200, "imageId": "i-d", "artists": [{ "name": "X" }] }))
            .expect("a video");
        assert_eq!((video.kind, video.subtitle.as_str()), ("video", "X"));
    }

    #[test]
    fn sections_with_more_end_in_a_see_all_card() {
        let section = |has_more: bool, kind: &str| slate_core::tidal_api::HomePageSection {
            title: "New Albums".into(),
            section_type: kind.into(),
            items: json!([{ "id": 1, "title": "A", "numberOfTracks": 1 }]),
            has_more,
            api_path: Some("pages/new_albums".into()),
            header: None,
        };
        let out = editorial_sections(&[
            section(true, "ALBUM_LIST"),
            section(false, "PAGE_LINKS_CLOUD"),
        ]);
        assert_eq!(out.len(), 2);
        let last = out[0].cards.last().expect("cards");
        assert_eq!(
            (last.title.as_str(), last.id.as_str()),
            ("See all", "pages/new_albums|New Albums")
        );
        assert_eq!(out[1].cards.len(), 1);
        assert!(out[1].wrap && !out[0].wrap);
    }
}
