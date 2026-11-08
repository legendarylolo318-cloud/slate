//! Resume points for long tracks. The store lives in the encrypted state directory
//! (`resume.json`, via the core's state-file helpers), is read the first time a track of
//! 20 minutes or more starts or is left, written off the UI thread after a short quiet
//! period and at quit, and forgotten at sign-out. Shorter tracks never touch it.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use slate_core::resume::{self, ResumePoints};
use slate_core::AppState;

use crate::backend::Backend;
use crate::debounce::Debounce;
use crate::images;

/// A write waits this long for further changes.
const DEBOUNCE: Duration = Duration::from_millis(700);

/// The offer goes away once the track has played this far without it being taken.
const OFFER_LIFETIME_SECS: f32 = 20.0;

/// `None` until first used (and again after sign-out).
static STORE: Mutex<Option<ResumePoints>> = Mutex::new(None);
static DIRTY: AtomicBool = AtomicBool::new(false);
/// Bumped at sign-out: work that started before it must not touch the store afterwards.
static EPOCH: AtomicU64 = AtomicU64::new(0);
/// From sign-out until the next sign-in nothing is read, recorded or written: a track left
/// while the sign-out is under way must not put `resume.json` back after `auth::logout`
/// deleted it (the queue file's rule, #139).
static SIGNED_OUT: AtomicBool = AtomicBool::new(false);
static SAVE: Debounce = Debounce::new();

fn with<R>(state: &AppState, f: impl FnOnce(&mut ResumePoints) -> R) -> R {
    let mut g = STORE.lock().unwrap_or_else(|e| e.into_inner());
    if SIGNED_OUT.load(Ordering::Acquire) {
        return f(&mut ResumePoints::default());
    }
    let store = g.get_or_insert_with(|| {
        state
            .read_state_file(resume::FILE)
            .map(|t| ResumePoints::parse(&t))
            .unwrap_or_default()
    });
    f(store)
}

/// Writes the store now, on the calling thread (under the store's lock, so a sign-out
/// cannot interleave).
fn write_now(state: &AppState) {
    let g = STORE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(store) = g.as_ref() else { return };
    if SIGNED_OUT.load(Ordering::Acquire) {
        return;
    }
    if !DIRTY.swap(false, Ordering::AcqRel) {
        return;
    }
    if let Err(e) = state.write_state_file(resume::FILE, &store.to_json()) {
        tracing::warn!("resume points not saved: {}", e.log_safe());
    }
}

/// Quit: writes a change still waiting for the debounce.
pub fn flush(state: &AppState) {
    if DIRTY.load(Ordering::Acquire) {
        write_now(state);
    }
}

/// Sign-out: nothing of the old account stays in memory, and nothing waiting is written
/// (the core deletes the file).
pub fn forget() {
    SIGNED_OUT.store(true, Ordering::Release);
    EPOCH.fetch_add(1, Ordering::AcqRel);
    SAVE.cancel();
    let mut g = STORE.lock().unwrap_or_else(|e| e.into_inner());
    *g = None;
    DIRTY.store(false, Ordering::Release);
}

/// Signed in (again): resume points are kept from here on.
pub fn allow() {
    SIGNED_OUT.store(false, Ordering::Release);
}

/// Records where a track was left. Called on the thread that leaves it; cheap and
/// file-free for tracks under 20 minutes.
fn leave_now(state: &Arc<AppState>, epoch: u64, id: u64, duration: f32, position: f32) {
    if !resume::is_long(duration) {
        return;
    }
    let mut g = STORE.lock().unwrap_or_else(|e| e.into_inner());
    if EPOCH.load(Ordering::Acquire) != epoch || SIGNED_OUT.load(Ordering::Acquire) {
        return;
    }
    let store = g.get_or_insert_with(|| {
        state
            .read_state_file(resume::FILE)
            .map(|t| ResumePoints::parse(&t))
            .unwrap_or_default()
    });
    if store.leave(id, duration, position) {
        DIRTY.store(true, Ordering::Release);
    }
}

/// Quit: remembers where the current track is (the caller is already off the UI loop).
pub fn leave_at_quit(state: &Arc<AppState>, id: u64, duration: f32, position: f32) {
    leave_now(state, EPOCH.load(Ordering::Acquire), id, duration, position);
    flush(state);
}

impl Backend {
    /// The current track is being replaced or stopped: remember where it was, and, if the
    /// new track (`next`) has a point, offer it. Does nothing for short tracks.
    pub(crate) fn resume_track_changed(&self, left: Option<(u64, f32, f32)>, next: (u64, f32)) {
        let left = left.filter(|(_, dur, _)| resume::is_long(*dur));
        let next_long = resume::is_long(next.1);
        if left.is_none() && !next_long {
            self.resume_offer(None);
            return;
        }
        let Some(state) = self.state() else { return };
        let epoch = EPOCH.load(Ordering::Acquire);
        let this = self.clone();
        self.rt.spawn_blocking(move || {
            if let Some((id, dur, pos)) = left {
                leave_now(&state, epoch, id, dur, pos);
            }
            let offer = if next_long && EPOCH.load(Ordering::Acquire) == epoch {
                with(&state, |s| s.get(next.0, next.1))
            } else {
                None
            };
            if DIRTY.load(Ordering::Acquire) {
                this.resume_save_later(&state);
            }
            // Only if that track is still the one on screen.
            if this.shared.current_id.load(Ordering::Relaxed) == next.0 {
                this.resume_offer(offer);
            }
        });
    }

    /// Stop (MPRIS, MPD, MCP): the current track stays loaded but is left here.
    pub(crate) fn resume_leave_current(&self) {
        let id = self.shared.current_id.load(Ordering::Relaxed);
        let dur = self.shared.duration();
        if !resume::is_long(dur) {
            return;
        }
        let pos = f32::from_bits(self.shared.position.load(Ordering::Relaxed));
        self.resume_track_changed(Some((id, dur, pos)), (id, 0.0));
    }

    fn resume_save_later(&self, state: &Arc<AppState>) {
        let state = Arc::clone(state);
        SAVE.run_later(&self.rt, DEBOUNCE, async move {
            let _ = tokio::task::spawn_blocking(move || write_now(&state)).await;
        });
    }

    /// Shows (or hides, with `None`) the "Resume at mm:ss" button.
    fn resume_offer(&self, secs: Option<u32>) {
        let text = secs
            .map(|s| format!("Resume at {}", images::format_time(s as f32)))
            .unwrap_or_default();
        self.shared
            .resume_secs
            .store(secs.unwrap_or(0), Ordering::Relaxed);
        self.ui(move |w| w.set_resume_text(text.into()));
    }

    /// The button was clicked: seek there. Hides the offer.
    pub fn resume_accept(&self) {
        let secs = self.shared.resume_secs.swap(0, Ordering::Relaxed);
        self.ui(|w| w.set_resume_text("".into()));
        if secs > 0 {
            self.seek_to_secs(secs as f32);
        }
    }

    /// Playing on, or seeking elsewhere, past the offer's own point (or 20 s into the
    /// track) makes it moot. One atomic load when no offer is up.
    pub(crate) fn resume_dismiss_if_past(&self, position: f32) {
        let offered = self.shared.resume_secs.load(Ordering::Relaxed);
        if offered > 0 && (position >= offered as f32 || position > OFFER_LIFETIME_SECS) {
            self.resume_dismiss();
        }
    }

    /// Any other way of moving through the track makes the offer moot.
    pub(crate) fn resume_dismiss(&self) {
        if self.shared.resume_secs.swap(0, Ordering::Relaxed) > 0 {
            self.ui(|w| w.set_resume_text("".into()));
        }
    }
}
