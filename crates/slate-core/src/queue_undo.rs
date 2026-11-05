//! Undo and redo for edits the user makes to the upcoming queue.
//!
//! A state is the "next up" list, the context queue, the pre-shuffle order and the shuffle flag,
//! as lists of shared track handles (`Arc<Track>`): a snapshot costs one pointer per track, and
//! tracks that were in the previous snapshot are shared instead of copied again. What plays now
//! and the history are never part of a state, so undo cannot interrupt the current track.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::queue::{Queue, Track};

/// States kept for undo (and, separately, for redo).
pub const DEPTH: usize = 20;
/// Edits closer together than this share one undo step (adding ten selected tracks one by one
/// is one edit to the user).
pub const BURST: Duration = Duration::from_millis(600);

pub type Tracks = Arc<[Arc<Track>]>;

#[derive(Debug, Clone)]
pub struct QueueState {
    manual: Tracks,
    queue: Tracks,
    original: Option<Tracks>,
    shuffle: bool,
}

type Pool = HashMap<(u64, Option<String>), Arc<Track>>;

fn share(list: &[Track], pool: &Pool) -> Tracks {
    list.iter()
        .map(|t| {
            pool.get(&(t.id, t.qid.clone()))
                .cloned()
                .unwrap_or_else(|| Arc::new(t.clone()))
        })
        .collect()
}

fn ids(t: &Tracks) -> impl Iterator<Item = (u64, Option<&str>)> {
    t.iter().map(|t| (t.id, t.qid.as_deref()))
}

impl QueueState {
    /// The editable part of `q`, reusing the handles of `prev` where the track is the same.
    pub fn capture(q: &Queue, prev: Option<&QueueState>) -> Self {
        let mut pool = Pool::new();
        for list in prev.into_iter().flat_map(|p| {
            [Some(&p.manual), Some(&p.queue), p.original.as_ref()]
                .into_iter()
                .flatten()
        }) {
            for t in list.iter() {
                pool.entry((t.id, t.qid.clone())).or_insert_with(|| t.clone());
            }
        }
        Self {
            manual: share(&q.manual, &pool),
            queue: share(&q.queue, &pool),
            original: q.original.as_deref().map(|o| share(o, &pool)),
            shuffle: q.shuffle,
        }
    }

    /// Puts this state back into `q`; the playing track and the history stay as they are.
    pub fn apply(&self, q: &mut Queue) {
        let own = |l: &Tracks| -> Vec<Track> { l.iter().map(|t| (**t).clone()).collect() };
        q.manual = own(&self.manual);
        q.queue = own(&self.queue);
        q.original = self.original.as_ref().map(own);
        q.shuffle = self.shuffle;
        // Anything built from the old list (gapless arming, a DJ block) is stale now.
        q.epoch += 1;
    }

    /// Same tracks in the same places and the same shuffle flag.
    pub fn same_as(&self, other: &Self) -> bool {
        self.shuffle == other.shuffle
            && ids(&self.manual).eq(ids(&other.manual))
            && ids(&self.queue).eq(ids(&other.queue))
            && self.original.as_ref().map(|o| ids(o).collect::<Vec<_>>())
                == other.original.as_ref().map(|o| ids(o).collect::<Vec<_>>())
    }
}

#[derive(Debug, Default)]
pub struct UndoStack {
    undo: VecDeque<QueueState>,
    redo: Vec<QueueState>,
    last_edit: Option<Instant>,
}

impl UndoStack {
    pub fn new() -> Self {
        Self::default()
    }

    /// The newest undo state, for sharing track handles with the next capture.
    pub fn newest(&self) -> Option<&QueueState> {
        self.undo.back()
    }

    /// A user edit is about to happen to a queue that currently looks like `before`.
    /// Redo history is dropped; a burst of edits and an edit that changes nothing add no step.
    pub fn record(&mut self, before: QueueState, now: Instant) {
        let burst = self.last_edit.is_some_and(|t| now.duration_since(t) < BURST);
        self.last_edit = Some(now);
        self.redo.clear();
        if burst || self.undo.back().is_some_and(|b| b.same_as(&before)) {
            return;
        }
        if self.undo.len() == DEPTH {
            self.undo.pop_front();
        }
        self.undo.push_back(before);
    }

    /// Steps back: returns the state to apply and remembers `current` for redo.
    pub fn undo(&mut self, current: QueueState) -> Option<QueueState> {
        let s = self.undo.pop_back()?;
        if self.redo.len() == DEPTH {
            self.redo.remove(0);
        }
        self.redo.push(current);
        self.last_edit = None;
        Some(s)
    }

    /// Steps forward again after an undo.
    pub fn redo(&mut self, current: QueueState) -> Option<QueueState> {
        let s = self.redo.pop()?;
        if self.undo.len() == DEPTH {
            self.undo.pop_front();
        }
        self.undo.push_back(current);
        self.last_edit = None;
        Some(s)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q_of(manual: &[u64], queue: &[u64]) -> Queue {
        let mut q = Queue::new();
        q.manual = manual.iter().map(|i| Track::new(*i)).collect();
        q.queue = queue.iter().map(|i| Track::new(*i)).collect();
        q
    }

    fn ids_of(q: &Queue) -> (Vec<u64>, Vec<u64>) {
        (q.manual.iter().map(|t| t.id).collect(), q.queue.iter().map(|t| t.id).collect())
    }

    fn step(t0: Instant, n: u64) -> Instant {
        t0 + BURST * (n as u32 + 1)
    }

    #[test]
    fn undo_and_redo_walk_the_edits() {
        let t0 = Instant::now();
        let mut q = q_of(&[], &[1, 2, 3]);
        let mut u = UndoStack::new();
        u.record(QueueState::capture(&q, u.newest()), step(t0, 0));
        q.queue.remove(0);
        u.record(QueueState::capture(&q, u.newest()), step(t0, 1));
        q.queue.remove(0);
        assert_eq!(ids_of(&q).1, vec![3]);

        let s = u.undo(QueueState::capture(&q, None)).unwrap();
        s.apply(&mut q);
        assert_eq!(ids_of(&q).1, vec![2, 3]);
        let s = u.undo(QueueState::capture(&q, None)).unwrap();
        s.apply(&mut q);
        assert_eq!(ids_of(&q).1, vec![1, 2, 3]);
        assert!(u.undo(QueueState::capture(&q, None)).is_none());

        let s = u.redo(QueueState::capture(&q, None)).unwrap();
        s.apply(&mut q);
        assert_eq!(ids_of(&q).1, vec![2, 3]);
        assert!(u.can_redo());
    }

    #[test]
    fn a_new_edit_clears_redo() {
        let t0 = Instant::now();
        let mut q = q_of(&[], &[1, 2]);
        let mut u = UndoStack::new();
        u.record(QueueState::capture(&q, None), step(t0, 0));
        q.queue.pop();
        let s = u.undo(QueueState::capture(&q, None)).unwrap();
        s.apply(&mut q);
        assert!(u.can_redo());
        u.record(QueueState::capture(&q, u.newest()), step(t0, 2));
        assert!(!u.can_redo());
    }

    #[test]
    fn keeps_twenty_states() {
        let t0 = Instant::now();
        let mut q = q_of(&[], &[]);
        let mut u = UndoStack::new();
        for i in 0..30u64 {
            u.record(QueueState::capture(&q, u.newest()), step(t0, i));
            q.queue.push(Track::new(i));
        }
        let mut n = 0;
        while let Some(s) = u.undo(QueueState::capture(&q, None)) {
            s.apply(&mut q);
            n += 1;
        }
        assert_eq!(n, DEPTH);
        // The oldest ten states fell off: the queue stops at what it held 20 edits ago.
        assert_eq!(q.queue.len(), 10);
    }

    #[test]
    fn a_burst_of_edits_is_one_step_and_no_change_adds_none() {
        let t0 = Instant::now();
        let mut q = q_of(&[], &[1]);
        let mut u = UndoStack::new();
        let first = step(t0, 0);
        u.record(QueueState::capture(&q, u.newest()), first);
        q.queue.push(Track::new(2));
        u.record(QueueState::capture(&q, u.newest()), first + Duration::from_millis(50));
        q.queue.push(Track::new(3));
        let s = u.undo(QueueState::capture(&q, None)).unwrap();
        s.apply(&mut q);
        assert_eq!(ids_of(&q).1, vec![1]);
        assert!(!u.can_undo());

        let mut u = UndoStack::new();
        u.record(QueueState::capture(&q, None), step(t0, 3));
        u.record(QueueState::capture(&q, u.newest()), step(t0, 5));
        assert_eq!(u.undo.len(), 1);
    }

    #[test]
    fn undo_restores_shuffle_and_never_touches_what_plays() {
        let t0 = Instant::now();
        let mut q = q_of(&[9], &[1, 2, 3]);
        q.current = Some(Track::new(100));
        q.history = vec![Track::new(50)];
        let mut u = UndoStack::new();
        u.record(QueueState::capture(&q, None), step(t0, 0));
        q.shuffle = true;
        q.original = Some(vec![Track::new(1), Track::new(2), Track::new(3)]);
        q.queue = vec![Track::new(3), Track::new(1), Track::new(2)];
        let epoch = q.epoch;
        u.undo(QueueState::capture(&q, None)).unwrap().apply(&mut q);
        assert!(!q.shuffle && q.original.is_none());
        assert_eq!(ids_of(&q), (vec![9], vec![1, 2, 3]));
        assert_eq!(q.current.as_ref().map(|t| t.id), Some(100));
        assert_eq!(q.history.len(), 1);
        assert!(q.epoch > epoch);
    }

    #[test]
    fn captures_share_track_handles_with_the_previous_state() {
        let q = q_of(&[], &[1, 2]);
        let a = QueueState::capture(&q, None);
        let b = QueueState::capture(&q, Some(&a));
        assert!(Arc::ptr_eq(&a.queue[0], &b.queue[0]));
        assert!(a.same_as(&b));
    }
}
