//! Lazily filled list model: the UI sees `total` rows from the start, but row
//! data is fetched a page at a time as the `ListView` asks for it. Lives on the
//! UI thread; results come back through `upgrade_in_event_loop` and are applied
//! by downcasting the window's model (see [`with_tracks`]).

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use slint::{Image, Model, ModelNotify, ModelTracker};

use crate::bootstrap::{AppWindow, TrackRow};

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

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
