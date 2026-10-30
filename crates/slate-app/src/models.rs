//! Lazily filled list model: the UI sees `total` rows from the start, but row
//! data is fetched a page at a time as the `ListView` asks for it. Lives on the
//! UI thread; results come back through `upgrade_in_event_loop` and are applied
//! by downcasting the window's model (see [`with_tracks`]).

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use slint::{Image, Model, ModelNotify, ModelRc, ModelTracker, SharedString, VecModel};

use crate::bootstrap::{AppWindow, CardItem, CardRow, TrackRow};

/// Rows per fetch. Matches what TIDAL accepts for playlist/favourite pages.
pub const PAGE_SIZE: usize = 100;

/// Pages whose thumbnails the model keeps. Each thumbnail is ~100 KB decoded,
/// and the model's reference keeps it alive past the image cache's budget, so
/// covers far from the viewport are dropped and fetched again on return.
const MAX_COVER_PAGES: usize = 6;

/// A row's text, `Send` so it can be built off the UI thread.
#[derive(Clone, Debug, Default)]
pub struct RowData {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: String,
    pub explicit: bool,
    pub favorite: bool,
    /// 0 none, 1 Lossless, 2 Hi-Res.
    pub quality: i32,
    pub atmos: bool,
}

pub struct LazyTracks {
    covers: RefCell<HashMap<usize, Image>>,
    total: Cell<usize>,
    rows: RefCell<HashMap<usize, TrackRow>>,
    requested: RefCell<HashSet<usize>>,
    fetch: Box<dyn Fn(usize)>,
    /// Re-requests a page's thumbnails after they were evicted.
    refetch_covers: Box<dyn Fn(usize)>,
    /// Pages holding at least one cover.
    cover_pages: RefCell<HashSet<usize>>,
    /// Loaded pages whose covers were dropped and not yet asked for again.
    evicted: RefCell<HashSet<usize>>,
    /// Page of the row the view asked for most recently: where the viewport is.
    last_page: Cell<usize>,
    notify: ModelNotify,
}

impl LazyTracks {
    pub fn new(fetch: Box<dyn Fn(usize)>) -> Self {
        Self {
            covers: RefCell::new(HashMap::new()),
            total: Cell::new(0),
            rows: RefCell::new(HashMap::new()),
            requested: RefCell::new(HashSet::new()),
            fetch,
            refetch_covers: Box::new(|_| {}),
            cover_pages: RefCell::new(HashSet::new()),
            evicted: RefCell::new(HashSet::new()),
            last_page: Cell::new(0),
            notify: ModelNotify::default(),
        }
    }

    /// Sets how evicted thumbnails are fetched again (by page).
    pub fn with_cover_refetch(mut self, f: Box<dyn Fn(usize)>) -> Self {
        self.refetch_covers = f;
        self
    }

    pub fn set_total(&self, total: usize) {
        self.total.set(total);
        self.notify.reset();
    }

    /// Stores a fetched page starting at `offset` and redraws those rows.
    pub fn fill(&self, offset: usize, rows: Vec<RowData>) {
        let mut map = self.rows.borrow_mut();
        let count = rows.len();
        for (i, r) in rows.into_iter().enumerate() {
            map.insert(
                offset + i,
                TrackRow {
                    id: r.id.into(),
                    title: r.title.into(),
                    artist: r.artist.into(),
                    album: r.album.into(),
                    duration: r.duration.into(),
                    explicit: r.explicit,
                    loaded: true,
                    favorite: r.favorite,
                    quality: r.quality,
                    atmos: r.atmos,
                    cover: Image::default(),
                },
            );
        }
        drop(map);
        for i in 0..count {
            self.notify.row_changed(offset + i);
        }
    }

    /// Shows `cover` on every listed row.
    pub fn set_cover(&self, indices: &[usize], cover: &Image) {
        let mut covers = self.covers.borrow_mut();
        let mut pages = self.cover_pages.borrow_mut();
        for &i in indices {
            covers.insert(i, cover.clone());
            pages.insert(i / PAGE_SIZE);
            self.evicted.borrow_mut().remove(&(i / PAGE_SIZE));
        }
        let mut dropped = Vec::new();
        if pages.len() > MAX_COVER_PAGES {
            let here = self.last_page.get();
            let mut by_distance: Vec<usize> = pages.iter().copied().collect();
            by_distance.sort_by_key(|p| std::cmp::Reverse(p.abs_diff(here)));
            for page in by_distance.into_iter().take(pages.len() - MAX_COVER_PAGES) {
                pages.remove(&page);
                self.evicted.borrow_mut().insert(page);
                let rows = page * PAGE_SIZE..(page + 1) * PAGE_SIZE;
                covers.retain(|i, _| !rows.contains(i));
                dropped.extend(rows);
            }
        }
        drop((covers, pages));
        for &i in indices {
            self.notify.row_changed(i);
        }
        for i in dropped.into_iter().filter(|&i| i < self.total.get()) {
            self.notify.row_changed(i);
        }
    }

    /// Updates the heart on every loaded row of `track_id`.
    pub fn set_favorite(&self, track_id: &str, on: bool) {
        let mut changed = Vec::new();
        for (i, row) in self.rows.borrow_mut().iter_mut() {
            if row.id == track_id && row.favorite != on {
                row.favorite = on;
                changed.push(*i);
            }
        }
        for i in changed {
            self.notify.row_changed(i);
        }
    }

    /// Drops every cover so rows fetch theirs again as they come into view
    /// (after a scale-factor change, so they are decoded at the new size).
    pub fn refresh_covers(&self) {
        self.covers.borrow_mut().clear();
        let pages: Vec<usize> = self.cover_pages.borrow_mut().drain().collect();
        self.evicted.borrow_mut().extend(pages);
        self.notify.reset();
    }

    /// Lets a failed fetch be retried the next time the rows scroll into view.
    pub fn forget_request(&self, page: usize) {
        self.requested.borrow_mut().remove(&page);
    }

    fn placeholder() -> TrackRow {
        TrackRow {
            id: "".into(),
            title: "".into(),
            artist: "".into(),
            album: "".into(),
            duration: "".into(),
            explicit: false,
            loaded: false,
            favorite: false,
            quality: 0,
            atmos: false,
            cover: Image::default(),
        }
    }
}

impl Model for LazyTracks {
    type Data = TrackRow;

    fn row_count(&self) -> usize {
        self.total.get()
    }

    fn row_data(&self, row: usize) -> Option<TrackRow> {
        if row >= self.total.get() {
            return None;
        }
        let page = row / PAGE_SIZE;
        self.last_page.set(page);
        if let Some(r) = self.rows.borrow().get(&row) {
            if self.evicted.borrow_mut().remove(&page) {
                (self.refetch_covers)(page);
            }
            let mut r = r.clone();
            if let Some(c) = self.covers.borrow().get(&row) {
                r.cover = c.clone();
            }
            return Some(r);
        }
        if self.requested.borrow_mut().insert(page) {
            (self.fetch)(page);
        }
        Some(Self::placeholder())
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Runs `f` on the window's [`LazyTracks`], if that is the model currently shown.
pub fn with_tracks<R>(w: &AppWindow, f: impl FnOnce(&LazyTracks) -> R) -> Option<R> {
    let model = slint::Model::as_any(&w.get_tracks())
        .downcast_ref::<LazyTracks>()
        .map(f);
    model
}

/// Grid covers a [`CardRows`] model keeps (~100 MB of 320 px covers, less once
/// decoded at display size). Rows far from the viewport lose theirs first and
/// fetch them again when they scroll back in.
const MAX_GRID_COVERS: usize = 240;

/// A card page section as [`CardRows`] keeps it: each card with its image id,
/// so the cover can be fetched once its row is on screen.
pub struct CardSectionData {
    pub title: SharedString,
    /// true: wrapping grid; false: one horizontally scrolling strip.
    pub wrap: bool,
    pub cards: Vec<(CardItem, Option<String>)>,
}

/// Covers to fetch: (section, card, image id).
pub type CoverRequest = Vec<(usize, usize, String)>;

#[derive(Clone, Copy, PartialEq)]
enum RowRef {
    Header,
    Title(usize),
    Strip(usize),
    Grid { section: usize, start: usize },
}

impl RowRef {
    fn section(self) -> Option<usize> {
        match self {
            Self::Header => None,
            Self::Title(s) | Self::Strip(s) | Self::Grid { section: s, .. } => Some(s),
        }
    }
}

/// A card page as a flat list of rows (header, section titles, strips, grid
/// rows of `cols` cards) for a `ListView`, so only the rows on screen exist.
/// A column change regroups the rows without refetching anything.
pub struct CardRows {
    sections: Vec<CardSectionData>,
    cols: Cell<usize>,
    layout: RefCell<Vec<RowRef>>,
    /// First card row (strip or grid) of each section, if it has cards.
    starts: RefCell<Vec<Option<usize>>>,
    covers: RefCell<HashMap<(usize, usize), Image>>,
    requested: RefCell<HashSet<(usize, usize)>>,
    /// Row the view asked for most recently: where the viewport is.
    last_row: Cell<usize>,
    /// Bumped by [`CardRows::refresh_covers`]; covers fetched before it are dropped.
    epoch: Cell<u64>,
    fetch_covers: Box<dyn Fn(u64, CoverRequest)>,
    notify: ModelNotify,
}

impl CardRows {
    pub fn new(
        sections: Vec<CardSectionData>,
        cols: usize,
        fetch_covers: Box<dyn Fn(u64, CoverRequest)>,
    ) -> Self {
        let cols = cols.max(1);
        let (layout, starts) = Self::build_layout(&sections, cols);
        Self {
            sections,
            cols: Cell::new(cols),
            layout: RefCell::new(layout),
            starts: RefCell::new(starts),
            covers: RefCell::new(HashMap::new()),
            requested: RefCell::new(HashSet::new()),
            last_row: Cell::new(0),
            epoch: Cell::new(0),
            fetch_covers,
            notify: ModelNotify::default(),
        }
    }

    fn build_layout(
        sections: &[CardSectionData],
        cols: usize,
    ) -> (Vec<RowRef>, Vec<Option<usize>>) {
        let mut rows = vec![RowRef::Header];
        let mut starts = Vec::with_capacity(sections.len());
        for (s, sec) in sections.iter().enumerate() {
            if !sec.title.is_empty() {
                rows.push(RowRef::Title(s));
            }
            if sec.cards.is_empty() {
                starts.push(None);
                continue;
            }
            starts.push(Some(rows.len()));
            if sec.wrap {
                rows.extend(
                    (0..sec.cards.len())
                        .step_by(cols)
                        .map(|start| RowRef::Grid { section: s, start }),
                );
            } else {
                rows.push(RowRef::Strip(s));
            }
        }
        (rows, starts)
    }

    /// Regroups the grid rows for a new column count.
    pub fn set_cols(&self, cols: usize) {
        let cols = cols.max(1);
        if cols == self.cols.get() {
            return;
        }
        self.cols.set(cols);
        let (layout, starts) = Self::build_layout(&self.sections, cols);
        *self.layout.borrow_mut() = layout;
        *self.starts.borrow_mut() = starts;
        self.notify.reset();
    }

    fn row_of(&self, section: usize, card: usize) -> Option<usize> {
        let start = (*self.starts.borrow().get(section)?)?;
        let wrap = self.sections.get(section)?.wrap;
        Some(if wrap {
            start + card / self.cols.get()
        } else {
            start
        })
    }

    /// Drops every cover so visible rows fetch theirs again (after a
    /// scale-factor change, so they are decoded at the new size).
    pub fn refresh_covers(&self) {
        self.epoch.set(self.epoch.get() + 1);
        self.covers.borrow_mut().clear();
        self.requested.borrow_mut().clear();
        self.notify.reset();
    }

    /// Shows a fetched cover, dropping grid covers far from the viewport once
    /// more than [`MAX_GRID_COVERS`] are held. `epoch` is the one the request
    /// was made in; a cover from before a refresh is ignored.
    pub fn set_cover(&self, epoch: u64, section: usize, card: usize, cover: Image) {
        if epoch != self.epoch.get() {
            return;
        }
        self.covers.borrow_mut().insert((section, card), cover);
        let mut changed: Vec<usize> = self.row_of(section, card).into_iter().collect();
        let grid = |s: usize| self.sections.get(s).is_some_and(|sec| sec.wrap);
        let held: Vec<(usize, usize)> = self
            .covers
            .borrow()
            .keys()
            .copied()
            .filter(|&(s, _)| grid(s))
            .collect();
        if held.len() > MAX_GRID_COVERS {
            let here = self.last_row.get();
            let mut by_distance: Vec<(usize, (usize, usize))> = held
                .into_iter()
                .filter_map(|k| self.row_of(k.0, k.1).map(|r| (r.abs_diff(here), k)))
                .collect();
            by_distance.sort_unstable_by_key(|&(d, _)| std::cmp::Reverse(d));
            let excess = by_distance.len().saturating_sub(MAX_GRID_COVERS);
            let mut covers = self.covers.borrow_mut();
            let mut requested = self.requested.borrow_mut();
            for (_, key) in by_distance.into_iter().take(excess) {
                covers.remove(&key);
                requested.remove(&key);
                changed.extend(self.row_of(key.0, key.1));
            }
        }
        changed.sort_unstable();
        changed.dedup();
        for r in changed {
            self.notify.row_changed(r);
        }
    }
}

impl Model for CardRows {
    type Data = CardRow;

    fn row_count(&self) -> usize {
        self.layout.borrow().len()
    }

    fn row_data(&self, row: usize) -> Option<CardRow> {
        let (this, next) = {
            let layout = self.layout.borrow();
            (*layout.get(row)?, layout.get(row + 1).copied())
        };
        self.last_row.set(row);
        let last = this == RowRef::Header || next.and_then(RowRef::section) != this.section();
        let (kind, title, cards) = match this {
            RowRef::Header => (0, SharedString::default(), None),
            RowRef::Title(s) => (1, self.sections.get(s)?.title.clone(), None),
            RowRef::Strip(s) => (2, SharedString::default(), Some((s, 0, usize::MAX))),
            RowRef::Grid { section, start } => (
                3,
                SharedString::default(),
                Some((section, start, start + self.cols.get())),
            ),
        };
        let mut items = Vec::new();
        let mut missing = CoverRequest::new();
        if let Some((s, start, end)) = cards {
            let sec = self.sections.get(s)?;
            let covers = self.covers.borrow();
            let mut requested = self.requested.borrow_mut();
            for (c, (item, image)) in sec
                .cards
                .iter()
                .enumerate()
                .take(end.min(sec.cards.len()))
                .skip(start)
            {
                let mut item = item.clone();
                match covers.get(&(s, c)) {
                    Some(cover) => item.cover = cover.clone(),
                    None => {
                        if let Some(img) = image {
                            if requested.insert((s, c)) {
                                missing.push((s, c, img.clone()));
                            }
                        }
                    }
                }
                items.push(item);
            }
        }
        if !missing.is_empty() {
            (self.fetch_covers)(self.epoch.get(), missing);
        }
        Some(CardRow {
            kind,
            title,
            cards: ModelRc::new(VecModel::from(items)),
            last,
        })
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Runs `f` on the window's [`CardRows`], if that is the model currently shown.
pub fn with_card_rows<R>(w: &AppWindow, f: impl FnOnce(&CardRows) -> R) -> Option<R> {
    let model = slint::Model::as_any(&w.get_card_rows())
        .downcast_ref::<CardRows>()
        .map(f);
    model
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    fn card(id: &str) -> (CardItem, Option<String>) {
        (
            CardItem {
                id: id.into(),
                ..CardItem::default()
            },
            Some(format!("img-{id}")),
        )
    }

    fn grid(n: usize) -> Vec<CardSectionData> {
        vec![CardSectionData {
            title: "".into(),
            wrap: true,
            cards: (0..n).map(|i| card(&i.to_string())).collect(),
        }]
    }

    #[test]
    fn card_rows_regroup_on_column_change_without_refetching() {
        let asked = Rc::new(RefCell::new(Vec::new()));
        let log = Rc::clone(&asked);
        let m = CardRows::new(
            grid(10),
            4,
            Box::new(move |_, r| log.borrow_mut().extend(r)),
        );
        // Header + ceil(10 / 4) grid rows.
        assert_eq!(m.row_count(), 4);
        let row = m.row_data(1).expect("row");
        assert_eq!((row.kind, row.cards.row_count()), (3, 4));
        assert_eq!(asked.borrow().len(), 4);
        m.set_cover(0, 0, 0, Image::default());
        m.set_cols(3);
        assert_eq!(m.row_count(), 5);
        // Same cards again: already fetched or in flight, nothing new asked.
        m.row_data(1);
        assert_eq!(asked.borrow().len(), 4);
        assert!(m
            .row_data(4)
            .is_some_and(|r| r.cards.row_count() == 1 && r.last));
    }

    #[test]
    fn card_rows_drop_far_grid_covers_and_ask_again_on_return() {
        let asked = Rc::new(RefCell::new(Vec::new()));
        let log = Rc::clone(&asked);
        let n = MAX_GRID_COVERS * 3;
        let m = CardRows::new(grid(n), 6, Box::new(move |_, r| log.borrow_mut().extend(r)));
        for row in 1..m.row_count() {
            m.row_data(row);
            for c in (row - 1) * 6..(row * 6).min(n) {
                m.set_cover(0, 0, c, Image::default());
            }
            assert!(m.covers.borrow().len() <= MAX_GRID_COVERS);
        }
        // The top was evicted; scrolling back asks for its covers again.
        let before = asked.borrow().len();
        m.row_data(1);
        assert_eq!(asked.borrow().len(), before + 6);
    }

    #[test]
    fn card_rows_refresh_asks_again_and_ignores_older_covers() {
        let asked = Rc::new(RefCell::new(Vec::new()));
        let log = Rc::clone(&asked);
        let m = CardRows::new(
            grid(4),
            4,
            Box::new(move |e, r| log.borrow_mut().push((e, r.len()))),
        );
        m.row_data(1);
        m.refresh_covers();
        m.set_cover(0, 0, 0, Image::default());
        assert!(m.covers.borrow().is_empty());
        m.row_data(1);
        assert_eq!(*asked.borrow(), vec![(0, 4), (1, 4)]);
    }

    #[test]
    fn strips_and_titles_mark_section_ends() {
        let sections = vec![
            CardSectionData {
                title: "A".into(),
                wrap: false,
                cards: vec![card("1"), card("2")],
            },
            CardSectionData {
                title: "B".into(),
                wrap: true,
                cards: vec![card("3")],
            },
        ];
        let m = CardRows::new(sections, 4, Box::new(|_, _| {}));
        let kinds: Vec<(i32, bool)> = (0..m.row_count())
            .filter_map(|r| m.row_data(r).map(|d| (d.kind, d.last)))
            .collect();
        assert_eq!(
            kinds,
            vec![(0, true), (1, false), (2, true), (1, false), (3, true)]
        );
    }

    fn row(t: &str) -> RowData {
        RowData {
            id: t.into(),
            title: t.into(),
            ..RowData::default()
        }
    }

    #[test]
    fn requests_each_page_once_and_serves_filled_rows() {
        let asked = Rc::new(RefCell::new(Vec::new()));
        let log = Rc::clone(&asked);
        let m = LazyTracks::new(Box::new(move |p| log.borrow_mut().push(p)));
        m.set_total(250);
        assert_eq!(m.row_count(), 250);
        assert!(!m.row_data(5).map(|r| r.loaded).unwrap_or(true));
        m.row_data(7);
        m.row_data(150);
        assert_eq!(*asked.borrow(), vec![0, 1]);
        m.fill(0, vec![row("a"), row("b")]);
        assert_eq!(m.row_data(1).map(|r| r.title.to_string()), Some("b".into()));
        assert!(m.row_data(250).is_none());
    }

    #[test]
    fn covers_attach_to_rows() {
        let m = LazyTracks::new(Box::new(|_| {}));
        m.set_total(3);
        m.fill(0, vec![row("a"), row("b")]);
        m.set_cover(&[1], &Image::default());
        assert!(m.row_data(1).is_some());
        assert_eq!(m.covers.borrow().len(), 1);
    }

    #[test]
    fn covers_far_from_the_viewport_are_dropped_and_refetched_on_return() {
        let refetched = Rc::new(RefCell::new(Vec::new()));
        let log = Rc::clone(&refetched);
        let m = LazyTracks::new(Box::new(|_| {}))
            .with_cover_refetch(Box::new(move |p| log.borrow_mut().push(p)));
        let pages = 20;
        m.set_total(pages * PAGE_SIZE);
        // Scroll from the top to the bottom, covers arriving for each page.
        for page in 0..pages {
            let first = page * PAGE_SIZE;
            m.fill(first, (0..PAGE_SIZE).map(|_| row("t")).collect());
            m.row_data(first);
            let rows: Vec<usize> = (first..first + PAGE_SIZE).collect();
            m.set_cover(&rows, &Image::default());
            assert!(m.cover_pages.borrow().len() <= MAX_COVER_PAGES);
            assert!(m.covers.borrow().len() <= MAX_COVER_PAGES * PAGE_SIZE);
        }
        // The pages kept are the ones nearest the viewport.
        assert!(m.cover_pages.borrow().contains(&(pages - 1)));
        assert!(!m.cover_pages.borrow().contains(&0));
        assert!(refetched.borrow().is_empty());
        // Back at the top: page 0's covers are asked for once.
        m.row_data(3);
        m.row_data(4);
        assert_eq!(*refetched.borrow(), vec![0]);
        // Rows themselves stay loaded.
        assert!(m.row_data(3).is_some_and(|r| r.loaded));
    }

    #[test]
    fn failed_pages_can_be_requested_again() {
        let asked = Rc::new(RefCell::new(Vec::new()));
        let log = Rc::clone(&asked);
        let m = LazyTracks::new(Box::new(move |p| log.borrow_mut().push(p)));
        m.set_total(10);
        m.row_data(0);
        m.forget_request(0);
        m.row_data(0);
        assert_eq!(*asked.borrow(), vec![0, 0]);
    }
}
