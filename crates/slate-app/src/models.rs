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

/// A row's text, `Send` so it can be built off the UI thread.
#[derive(Clone, Debug, Default)]
pub struct RowData {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: String,
    pub explicit: bool,
}

pub struct LazyTracks {
    covers: RefCell<HashMap<usize, Image>>,
    total: Cell<usize>,
    rows: RefCell<HashMap<usize, TrackRow>>,
    requested: RefCell<HashSet<usize>>,
    fetch: Box<dyn Fn(usize)>,
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
            notify: ModelNotify::default(),
        }
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
        for &i in indices {
            covers.insert(i, cover.clone());
        }
        drop(covers);
        for &i in indices {
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
        if let Some(r) = self.rows.borrow().get(&row) {
            let mut r = r.clone();
            if let Some(c) = self.covers.borrow().get(&row) {
                r.cover = c.clone();
            }
            return Some(r);
        }
        let page = row / PAGE_SIZE;
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
