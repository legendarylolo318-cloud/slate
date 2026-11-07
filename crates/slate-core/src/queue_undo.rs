//! Undo and redo for edits the user makes to the upcoming queue.
//!
//! A state is the "next up" list, the context queue, the pre-shuffle order, the shuffle flag and
//! the context the queue came from, as lists of shared track handles (`Arc<Track>`): a snapshot
//! costs one pointer per track, and tracks already held by another state are shared instead of
//! copied again. What plays now and the history are never part of a state, so undo cannot
//! interrupt the current track.
//!
//! The queue also changes between the user's edits: tracks are played off the front, autoplay
//! appends radio tracks, party requests are placed. Those changes are not undo steps, and undo
//! must not reverse them either (a played track must not come back, a guest's request must not
//! vanish). So the stack remembers the queue as the last edit left it; before anything else it
//! compares that with the queue now and carries the difference (entries gone, entries added and
//! where) into every stored state. A wholesale replacement of the context (playing an album,
//! autoplay starting a radio, a restored session) starts a new history instead.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::queue::{Queue, Source, Track};

/// States kept for undo (and, separately, for redo).
pub const DEPTH: usize = 20;
/// Edits closer together than this share one undo step (adding ten selected tracks one by one
/// is one edit to the user).
pub const BURST: Duration = Duration::from_millis(600);

pub type Tracks = Arc<[Arc<Track>]>;

/// What identifies one queue entry: every entry gets its own `qid` when it is queued.
type Key<'a> = (u64, Option<&'a str>);

fn key(t: &Track) -> Key<'_> {
    (t.id, t.qid.as_deref())
}

#[derive(Debug, Clone)]
pub struct QueueState {
    manual: Tracks,
    queue: Tracks,
    original: Option<Tracks>,
    shuffle: bool,
    context_source: Option<Source>,
    use_track_gain: bool,
    /// `Queue::epoch` when captured: a different epoch later means the context was replaced.
    epoch: u64,
}

fn share(list: &[Arc<Track>], pool: &HashMap<Key<'_>, &Arc<Track>>) -> Tracks {
    list.iter()
        .map(|t| {
            pool.get(&key(t))
                .map_or_else(|| Arc::clone(t), |a| Arc::clone(a))
        })
        .collect()
}

impl QueueState {
    fn lists(&self) -> impl Iterator<Item = &Tracks> {
        [
            Some(&self.manual),
            Some(&self.queue),
            self.original.as_ref(),
        ]
        .into_iter()
        .flatten()
    }

    /// The editable part of `q`, reusing the handles of `prev` where the entry is the same.
    pub fn capture(q: &Queue, prev: Option<&QueueState>) -> Self {
        let mut pool: HashMap<Key<'_>, &Arc<Track>> = HashMap::new();
        for t in prev
            .into_iter()
            .flat_map(QueueState::lists)
            .flat_map(|l| l.iter())
        {
            pool.entry(key(t)).or_insert(t);
        }
        Self {
            manual: share(&q.manual, &pool),
            queue: share(&q.queue, &pool),
            original: q.original.as_deref().map(|o| share(o, &pool)),
            shuffle: q.shuffle,
            context_source: q.context_source.clone(),
            use_track_gain: q.use_track_gain,
            epoch: q.epoch,
        }
    }

    /// Puts this state back into `q`; the playing track and the history stay as they are.
    pub fn apply(&self, q: &mut Queue) {
        let own = |l: &Tracks| -> Vec<Arc<Track>> { l.to_vec() };
        q.manual = own(&self.manual);
        q.queue = own(&self.queue);
        q.original = self.original.as_ref().map(own);
        q.shuffle = self.shuffle;
        q.context_source.clone_from(&self.context_source);
        q.use_track_gain = self.use_track_gain;
        // Anything built from the old list (gapless arming, a DJ block) is stale now.
        q.epoch += 1;
    }

    /// Same tracks in the same places and the same shuffle flag.
    pub fn same_as(&self, other: &Self) -> bool {
        fn eq(a: &Tracks, b: &Tracks) -> bool {
            a.len() == b.len() && a.iter().map(|t| key(t)).eq(b.iter().map(|t| key(t)))
        }
        self.shuffle == other.shuffle
            && eq(&self.manual, &other.manual)
            && eq(&self.queue, &other.queue)
            && match (&self.original, &other.original) {
                (Some(a), Some(b)) => eq(a, b),
                (None, None) => true,
                _ => false,
            }
    }

    /// Track handles held, counting shared ones once per list (for tests and the review).
    pub fn len(&self) -> usize {
        self.lists().map(|l| l.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// How one list changed outside the user's edits: entries that left it, and entries that
/// arrived, each with the entry it follows in the new list (`None` = at the top).
struct Drift {
    gone: HashSet<(u64, Option<String>)>,
    /// Arrivals in order, with the key of the nearest older entry before them.
    came: Vec<(Option<(u64, Option<String>)>, Arc<Track>)>,
}

fn owned(t: &Track) -> (u64, Option<String>) {
    (t.id, t.qid.clone())
}

impl Drift {
    fn between(was: &Tracks, now: &Tracks) -> Option<Self> {
        let old: HashSet<Key<'_>> = was.iter().map(|t| key(t)).collect();
        let new: HashSet<Key<'_>> = now.iter().map(|t| key(t)).collect();
        let gone: HashSet<_> = was
            .iter()
            .filter(|t| !new.contains(&key(t)))
            .map(|t| owned(t))
            .collect();
        let mut came = Vec::new();
        let mut anchor = None;
        for t in now.iter() {
            if old.contains(&key(t)) {
                anchor = Some(owned(t));
            } else {
                came.push((anchor.clone(), Arc::clone(t)));
            }
        }
        (!gone.is_empty() || !came.is_empty()).then_some(Self { gone, came })
    }

    /// `list` with the same entries gone and the arrivals placed after the entry they followed
    /// (or after the nearest earlier one this list still has; at the top when there is none).
    fn apply(&self, list: &Tracks) -> Tracks {
        let kept: Vec<&Arc<Track>> = list
            .iter()
            .filter(|t| !self.gone.contains(&(t.id, t.qid.clone())))
            .collect();
        if self.came.is_empty() {
            return kept.into_iter().cloned().collect();
        }
        let at: HashMap<Key<'_>, usize> =
            kept.iter().enumerate().map(|(i, t)| (key(t), i)).collect();
        // Arrivals grouped by the index (in `kept`) they go after; `None` = before everything.
        let mut after: BTreeMap<Option<usize>, Vec<&Arc<Track>>> = BTreeMap::new();
        let mut last = None;
        for (anchor, t) in &self.came {
            if let Some(i) = anchor
                .as_ref()
                .and_then(|(id, q)| at.get(&(*id, q.as_deref())))
            {
                last = Some(*i);
            }
            after.entry(last).or_default().push(t);
        }
        let mut out: Vec<Arc<Track>> = Vec::with_capacity(kept.len() + self.came.len());
        out.extend(after.remove(&None).into_iter().flatten().cloned());
        for (i, t) in kept.into_iter().enumerate() {
            out.push(Arc::clone(t));
            out.extend(after.remove(&Some(i)).into_iter().flatten().cloned());
        }
        out.into()
    }
}

#[derive(Debug, Default)]
pub struct UndoStack {
    undo: VecDeque<QueueState>,
    redo: Vec<QueueState>,
    last_edit: Option<Instant>,
    /// The queue as the last edit, undo or redo left it.
    settled: Option<QueueState>,
}

impl UndoStack {
    pub fn new() -> Self {
        Self::default()
    }

    /// Any state, to share track handles with the next capture.
    fn pool(&self) -> Option<&QueueState> {
        self.settled.as_ref().or(self.undo.back())
    }

    /// Carries what happened to the queue since the last edit into the stored states (see the
    /// module notes). `now` is the queue as it is.
    fn catch_up(&mut self, now: &QueueState) {
        let Some(was) = self.settled.take() else {
            return;
        };
        if was.epoch != now.epoch {
            // The context was replaced by something that is not an edit: a new history.
            self.undo.clear();
            self.redo.clear();
            self.last_edit = None;
            return;
        }
        if was.same_as(now) {
            return;
        }
        let manual = Drift::between(&was.manual, &now.manual);
        let queue = Drift::between(&was.queue, &now.queue);
        let original = match (&was.original, &now.original) {
            (Some(a), Some(b)) => Drift::between(a, b),
            _ => None,
        };
        for s in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            if let Some(d) = &manual {
                s.manual = d.apply(&s.manual);
            }
            if let Some(d) = &queue {
                s.queue = d.apply(&s.queue);
            }
            if let (Some(d), Some(o)) = (&original, &s.original) {
                s.original = Some(d.apply(o));
            }
        }
        // Something that changed nothing the user sees is no step any more.
        let mut kept: VecDeque<QueueState> = VecDeque::with_capacity(self.undo.len());
        for s in self.undo.drain(..) {
            if !kept.back().is_some_and(|b: &QueueState| b.same_as(&s)) {
                kept.push_back(s);
            }
        }
        if kept.back().is_some_and(|b| b.same_as(now)) {
            kept.pop_back();
        }
        self.undo = kept;
        self.redo.retain(|s| !s.same_as(now));
    }

    /// A user edit: `f` changes `q`. The queue before it becomes an undo step unless `f`
    /// changed nothing or the previous edit was less than [`BURST`] ago; any edit that
    /// changes something drops the redo history.
    pub fn edit<R>(&mut self, q: &mut Queue, now: Instant, f: impl FnOnce(&mut Queue) -> R) -> R {
        let before = self.begin(q, now);
        let r = f(q);
        self.settle(q, before, now);
        r
    }

    /// First half of [`edit`](Self::edit), for an edit that has to wait (an `await`) between
    /// looking at the queue and changing it.
    pub fn begin(&mut self, q: &Queue, now: Instant) -> Pending {
        let before = QueueState::capture(q, self.pool());
        self.catch_up(&before);
        let burst = self
            .last_edit
            .is_some_and(|t| now.duration_since(t) < BURST);
        Pending { before, burst }
    }

    /// Second half of [`edit`](Self::edit): `q` is the queue after the edit.
    pub fn settle(&mut self, q: &Queue, p: Pending, now: Instant) {
        let after = QueueState::capture(q, Some(&p.before));
        let changed = !after.same_as(&p.before);
        if changed {
            self.redo.clear();
            if !p.burst {
                if self.undo.len() == DEPTH {
                    self.undo.pop_front();
                }
                self.undo.push_back(p.before);
            }
            self.last_edit = Some(now);
        }
        self.settled = Some(after);
    }

    /// Steps back (`redo` false) or forward again. False when there is nothing to step to.
    pub fn step(&mut self, q: &mut Queue, redo: bool) -> bool {
        let now = QueueState::capture(q, self.pool());
        self.catch_up(&now);
        let target = if redo {
            self.redo.pop()
        } else {
            self.undo.pop_back()
        };
        let Some(target) = target else {
            self.settled = Some(now);
            return false;
        };
        if redo {
            if self.undo.len() == DEPTH {
                self.undo.pop_front();
            }
            self.undo.push_back(now);
        } else {
            if self.redo.len() == DEPTH {
                self.redo.remove(0);
            }
            self.redo.push(now);
        }
        target.apply(q);
        self.last_edit = None;
        self.settled = Some(QueueState {
            epoch: q.epoch,
            ..target
        });
        true
    }

    /// Forgets everything (sign-out: the next account must not undo into this one's queue).
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Distinct tracks held by all states (shared handles counted once).
    pub fn distinct_tracks(&self) -> usize {
        let mut seen: HashSet<*const Track> = HashSet::new();
        for s in self.undo.iter().chain(&self.redo).chain(&self.settled) {
            for l in s.lists() {
                for t in l.iter() {
                    seen.insert(Arc::as_ptr(t));
                }
            }
        }
        seen.len()
    }
}

/// The queue before an edit that is still under way (see [`UndoStack::begin`]).
#[derive(Debug)]
pub struct Pending {
    before: QueueState,
    burst: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(id: u64) -> Arc<Track> {
        Arc::new(Track::new(id).restamped())
    }

    fn q_of(manual: &[u64], queue: &[u64]) -> Queue {
        let mut q = Queue::new();
        q.manual = manual.iter().map(|i| t(*i)).collect();
        q.queue = queue.iter().map(|i| t(*i)).collect();
        q
    }

    fn ids_of(q: &Queue) -> (Vec<u64>, Vec<u64>) {
        (
            q.manual.iter().map(|t| t.id).collect(),
            q.queue.iter().map(|t| t.id).collect(),
        )
    }

    fn at(t0: Instant, n: u64) -> Instant {
        t0 + BURST * (n as u32 * 2 + 1)
    }

    /// The front of the queue starts playing (what `Queue::next` does to the lists).
    fn play_one(q: &mut Queue) {
        let next = if q.manual.is_empty() {
            q.queue.remove(0)
        } else {
            q.manual.remove(0)
        };
        if let Some(c) = q.current.replace(next) {
            q.history.push(c);
        }
    }

    #[test]
    fn undo_and_redo_walk_the_edits() {
        let t0 = Instant::now();
        let mut q = q_of(&[], &[1, 2, 3]);
        let mut u = UndoStack::new();
        u.edit(&mut q, at(t0, 0), |q| q.queue.remove(0));
        u.edit(&mut q, at(t0, 1), |q| q.queue.remove(0));
        assert_eq!(ids_of(&q).1, vec![3]);

        assert!(u.step(&mut q, false));
        assert_eq!(ids_of(&q).1, vec![2, 3]);
        assert!(u.step(&mut q, false));
        assert_eq!(ids_of(&q).1, vec![1, 2, 3]);
        assert!(!u.step(&mut q, false));

        assert!(u.step(&mut q, true));
        assert_eq!(ids_of(&q).1, vec![2, 3]);
        assert!(u.can_redo());
    }

    #[test]
    fn a_new_edit_clears_redo() {
        let t0 = Instant::now();
        let mut q = q_of(&[], &[1, 2]);
        let mut u = UndoStack::new();
        u.edit(&mut q, at(t0, 0), |q| q.queue.pop());
        assert!(u.step(&mut q, false));
        assert!(u.can_redo());
        u.edit(&mut q, at(t0, 2), |q| q.queue.pop());
        assert!(!u.can_redo());
    }

    #[test]
    fn keeps_twenty_states() {
        let t0 = Instant::now();
        let mut q = q_of(&[], &[]);
        let mut u = UndoStack::new();
        for i in 0..30u64 {
            u.edit(&mut q, at(t0, i), |q| q.queue.push(t(i)));
        }
        let mut n = 0;
        while u.step(&mut q, false) {
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
        let first = at(t0, 0);
        u.edit(&mut q, first, |q| q.queue.push(t(2)));
        u.edit(&mut q, first + Duration::from_millis(50), |q| {
            q.queue.push(t(3))
        });
        assert!(u.step(&mut q, false));
        assert_eq!(ids_of(&q).1, vec![1]);
        assert!(!u.can_undo());

        let mut u = UndoStack::new();
        u.edit(&mut q, at(t0, 3), |_| ());
        assert!(!u.can_undo());
    }

    #[test]
    fn undo_restores_shuffle_and_never_touches_what_plays() {
        let t0 = Instant::now();
        let mut q = q_of(&[9], &[1, 2, 3]);
        q.current = Some(t(100));
        q.history = vec![t(50)];
        let mut u = UndoStack::new();
        u.edit(&mut q, at(t0, 0), |q| {
            q.shuffle = true;
            q.original = Some(q.queue.clone());
            q.queue.reverse();
        });
        let epoch = q.epoch;
        assert!(u.step(&mut q, false));
        assert!(!q.shuffle && q.original.is_none());
        assert_eq!(ids_of(&q), (vec![9], vec![1, 2, 3]));
        assert_eq!(q.current.as_ref().map(|t| t.id), Some(100));
        assert_eq!(q.history.len(), 1);
        assert!(q.epoch > epoch);
    }

    #[test]
    fn played_tracks_do_not_come_back() {
        let t0 = Instant::now();
        let mut q = q_of(&[], &[1, 2, 3, 4]);
        let mut u = UndoStack::new();
        u.edit(&mut q, at(t0, 0), |q| q.queue.pop()); // removes 4
        play_one(&mut q); // 1 plays
        play_one(&mut q); // 2 plays
        assert!(u.step(&mut q, false));
        assert_eq!(q.current.as_ref().map(|t| t.id), Some(2));
        assert_eq!(ids_of(&q).1, vec![3, 4]);
        // Redo still removes only what the edit removed.
        assert!(u.step(&mut q, true));
        assert_eq!(ids_of(&q).1, vec![3]);
    }

    #[test]
    fn additions_between_edits_survive_an_undo() {
        let t0 = Instant::now();
        let mut q = q_of(&[], &[1, 2, 3]);
        let mut u = UndoStack::new();
        u.edit(&mut q, at(t0, 0), |q| {
            q.queue.remove(1);
        }); // [1, 3]
            // A party request placed after 1, autoplay tracks appended at the end.
        q.queue.insert(1, t(70));
        q.queue.push(t(80));
        q.queue.push(t(81));
        assert!(u.step(&mut q, false));
        assert_eq!(ids_of(&q).1, vec![1, 70, 2, 3, 80, 81]);
        assert!(u.step(&mut q, true));
        assert_eq!(ids_of(&q).1, vec![1, 70, 3, 80, 81]);
    }

    #[test]
    fn a_replaced_context_starts_a_new_history() {
        let t0 = Instant::now();
        let mut q = q_of(&[], &[1, 2]);
        let mut u = UndoStack::new();
        u.edit(&mut q, at(t0, 0), |q| q.queue.pop());
        q.set_tracks(vec![Track::new(10), Track::new(11)], None, true);
        assert!(!u.step(&mut q, false));
        assert_eq!(ids_of(&q).1, vec![10, 11]);
        assert!(!u.can_undo() && !u.can_redo());
    }

    #[test]
    fn an_edit_around_an_await_is_one_step() {
        let t0 = Instant::now();
        let mut q = q_of(&[], &[1, 2]);
        let mut u = UndoStack::new();
        let p = u.begin(&q, at(t0, 0));
        q.set_tracks(vec![Track::new(5), Track::new(6)], None, false);
        u.settle(&q, p, at(t0, 0));
        assert!(u.step(&mut q, false));
        assert_eq!(ids_of(&q).1, vec![1, 2]);
    }

    #[test]
    fn steps_share_handles_and_stay_bounded() {
        let t0 = Instant::now();
        let mut q = q_of(&[], &(0..1000).collect::<Vec<_>>());
        let mut u = UndoStack::new();
        for i in 0..(DEPTH as u64 * 3) {
            u.edit(&mut q, at(t0, i), |q| q.queue.swap(0, 1));
        }
        for _ in 0..DEPTH {
            u.step(&mut q, false);
        }
        for _ in 0..DEPTH {
            u.step(&mut q, true);
        }
        // Forty full states, but each of the 1,000 tracks is held once.
        assert_eq!(u.distinct_tracks(), 1000);
    }
}
