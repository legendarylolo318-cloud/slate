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
use slint::{Image, Model, ModelRc, VecModel};

use crate::backend::Backend;
use crate::bootstrap::{AppWindow, CardItem, CardSection};
use crate::images::tidal_image_url;
use crate::library::SourceKind;

const GRID_PAGE: u32 = 100;
const MAX_GRID_PAGES: u32 = 30;

#[derive(Clone, Debug, PartialEq)]
pub enum Route {
    Home,
    Albums,
    Artists,
    Mixes,
    Loved,
    Playlist(String),
    Album(u64),
    Artist(u64),
    Mix(String),
    Search(String),
    Folder(String),
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
            "settings" => Self::Settings,
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
            Self::Settings => "settings".into(),
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

/// Best-effort card from an untyped home-feed item. Unknown shapes are skipped.
pub fn card_from_value(v: &Value) -> Option<CardData> {
    let s = |k: &str| v.get(k).and_then(Value::as_str);
    let img = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
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
            w.set_can_back(back);
            w.set_can_forward(fwd);
            w.set_selected_source(sel.into());
        });
        match route {
            Route::Home => self.show_home(),
            Route::Albums => self.show_library_albums(),
            Route::Artists => self.show_library_artists(),
            Route::Mixes => self.show_library_mixes(),
            Route::Search(q) => self.show_search(q),
            Route::Folder(id) => self.show_folder(id),
            Route::Artist(id) => self.show_artist(id),
            Route::Loved => self.open(SourceKind::Loved, "loved".into()),
            Route::Playlist(u) => self.open(SourceKind::Playlist(u.clone()), u),
            Route::Album(id) => self.open(SourceKind::Album(id), String::new()),
            Route::Mix(id) => self.open(SourceKind::Mix(id), String::new()),
            Route::Settings => {
                self.load_settings_ui();
                self.ui(|w| w.set_view("settings".into()));
            }
        }
    }

    pub fn open_playlist(&self, uuid: &str) {
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
                        format!("{n} playlists"),
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
        let plan: Vec<(usize, usize, String)> = sections
            .iter()
            .enumerate()
            .flat_map(|(s, sec)| {
                sec.cards
                    .iter()
                    .enumerate()
                    .filter_map(move |(c, card)| card.image.clone().map(|i| (s, c, i)))
            })
            .collect();
        let hero_image = hero.as_ref().and_then(|h| h.image.clone());
        let hero_label = hero.map(|h| h.label);
        self.ui(move |w| {
            let model: Vec<CardSection> = sections
                .into_iter()
                .map(|s| CardSection {
                    title: s.title.into(),
                    wrap: s.wrap,
                    cards: ModelRc::new(VecModel::from(
                        s.cards
                            .into_iter()
                            .map(|c| CardItem {
                                kind: c.kind.into(),
                                id: c.id.into(),
                                title: c.title.into(),
                                subtitle: c.subtitle.into(),
                                cover: Image::default(),
                            })
                            .collect::<Vec<_>>(),
                    )),
                })
                .collect();
            w.set_sections(ModelRc::new(VecModel::from(model)));
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
                if let Some(px) = this.images.get(&state, &tidal_image_url(&img, 320)).await {
                    if this.lib.cards_generation() == generation {
                        this.ui(move |w| w.set_hero_image(Image::from_rgba8(px)));
                    }
                }
            }
            stream::iter(plan)
                .for_each_concurrent(6, |(s, c, img)| {
                    let (state, this) = (Arc::clone(&state), this.clone());
                    async move {
                        let Some(px) = this.images.get(&state, &tidal_image_url(&img, 320)).await
                        else {
                            return;
                        };
                        if this.lib.cards_generation() != generation {
                            return;
                        }
                        this.ui(move |w| set_card_cover(&w, s, c, Image::from_rgba8(px)));
                    }
                })
                .await;
        });
    }

    fn begin_cards(&self, title: &str) -> u64 {
        let generation = self.lib.bump_cards();
        let title = title.to_string();
        self.ui(move |w| {
            w.set_cards_title(title.into());
            w.set_cards_subtitle("".into());
            w.set_cards_loading(true);
            w.set_has_hero(false);
            w.set_sections(ModelRc::new(VecModel::<CardSection>::default()));
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
        mut fetch: F,
        map: impl Fn(&T) -> CardData,
    ) -> Result<Vec<CardData>, String>
    where
        F: FnMut(u32) -> Fut,
        Fut: std::future::Future<
            Output = Result<slate_core::tidal_api::PaginatedResponse<T>, slate_core::SlateError>,
        >,
    {
        let mut cards = Vec::new();
        for page in 0..MAX_GRID_PAGES {
            let p = fetch(page * GRID_PAGE).await.map_err(|e| e.log_safe())?;
            let got = p.items.len();
            cards.extend(p.items.iter().map(&map));
            if got == 0 || cards.len() >= p.total_number_of_items as usize {
                break;
            }
        }
        Ok(cards)
    }

    fn show_library_albums(&self) {
        let generation = self.begin_cards("Albums");
        let this = self.clone();
        self.rt.spawn(async move {
            let (Some(state), Some(uid)) = (this.state(), this.user_id().await) else {
                return;
            };
            let res = this
                .collect_grid(
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
                Ok(cards) => {
                    let n = cards.len();
                    this.show_cards(
                        "Albums".into(),
                        format!("{n} albums"),
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

    fn show_library_artists(&self) {
        let generation = self.begin_cards("Artists");
        let this = self.clone();
        self.rt.spawn(async move {
            let (Some(state), Some(uid)) = (this.state(), this.user_id().await) else {
                return;
            };
            let res = this
                .collect_grid(
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
                Ok(cards) => {
                    let n = cards.len();
                    this.show_cards(
                        "Artists".into(),
                        format!("{n} artists"),
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

    fn show_library_mixes(&self) {
        let generation = self.begin_cards("Mixes");
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            let res = this
                .collect_grid(
                    |off| {
                        lib_cmds::get_favorite_mixes(
                            &state,
                            this.app.clone(),
                            off,
                            GRID_PAGE,
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
                Ok(cards) => {
                    let n = cards.len();
                    this.show_cards(
                        "Mixes".into(),
                        format!("{n} mixes"),
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
                    let mut sections = Vec::new();
                    let tracks: Vec<CardData> = r
                        .tracks
                        .iter()
                        .filter_map(|t| serde_json::to_value(t).ok())
                        .filter_map(|v| serde_json::from_value::<Track>(v).ok())
                        .map(|t| track_card(&t))
                        .collect();
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
                            .map(|a| {
                                artist_card(
                                    a.id,
                                    &a.name,
                                    &a.picture,
                                    &a.selected_album_cover_fallback,
                                )
                            })
                            .collect(),
                    );
                    push("Albums", r.albums.iter().map(album_card).collect());
                    push("Playlists", r.playlists.iter().map(playlist_card).collect());
                    if this.lib.cards_generation() == generation {
                        this.show_cards(format!("Search: {query}"), String::new(), sections, None);
                    }
                }
                Err(e) => this.fail_cards(format!("Search failed: {}", e.log_safe())),
            }
        });
    }

    fn show_artist(&self, id: u64) {
        let generation = self.begin_cards("Artist");
        self.lib.set_artist(Some(id));
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            let detail = pages::get_artist_detail(&state, id).await;
            let top = pages::get_artist_top_tracks(&state, this.app.clone(), id, 10).await;
            let albums = pages::get_artist_albums(&state, id, 50).await;
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
                .filter_map(|t| serde_json::to_value(t).ok())
                .filter_map(|v| serde_json::from_value(v).ok())
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
async fn fetch_all(
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
        };
        let got = items.len();
        out.extend(
            items
                .iter()
                .filter_map(|t| serde_json::to_value(t).ok())
                .filter_map(|v| serde_json::from_value::<Track>(v).ok()),
        );
        if got == 0 || out.len() >= total || matches!(kind, SourceKind::Mix(_)) {
            break;
        }
    }
    Ok(out)
}

fn set_card_cover(w: &AppWindow, s: usize, c: usize, cover: Image) {
    let sections = w.get_sections();
    let Some(section) = sections.row_data(s) else {
        return;
    };
    let cards = section.cards;
    let Some(list) = cards.as_any().downcast_ref::<VecModel<CardItem>>() else {
        return;
    };
    if let Some(mut item) = list.row_data(c) {
        item.cover = cover;
        list.set_row_data(c, item);
    }
}

#[cfg(test)]
mod tests {
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
