//! Playlist edits that send many requests (remove duplicates, merge, replace a version): one
//! at a time, paced, and stopped by the Cancel button on the notice bar or by signing out.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::backend::Backend;

/// Pause between two edits of one job, on top of the client's in-flight cap.
pub const STEP_PAUSE: Duration = Duration::from_millis(250);

static RUNNING: AtomicBool = AtomicBool::new(false);
static CANCEL: AtomicBool = AtomicBool::new(false);

/// The running job. Dropping it ends the job and hides the Cancel button.
pub struct Job {
    backend: Backend,
}

impl Job {
    /// Waits before edit `n` (none before the first) and says whether to go on.
    pub async fn step(&self, n: usize) -> bool {
        if n > 0 && !cancelled() {
            tokio::time::sleep(STEP_PAUSE).await;
        }
        !cancelled() && self.backend.state().is_some()
    }

    pub fn cancelled(&self) -> bool {
        cancelled()
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::SeqCst);
        self.backend.ui(|w| w.set_bulk_running(false));
    }
}

fn cancelled() -> bool {
    CANCEL.load(Ordering::SeqCst)
}

impl Backend {
    /// Starts a job, or says why not when another one is still running.
    pub(crate) fn begin_bulk(&self) -> Option<Job> {
        if RUNNING.swap(true, Ordering::SeqCst) {
            self.ui(|w| w.set_error("Another playlist edit is still running.".into()));
            return None;
        }
        CANCEL.store(false, Ordering::SeqCst);
        self.ui(|w| w.set_bulk_running(true));
        Some(Job {
            backend: self.clone(),
        })
    }

    /// The Cancel button, and signing out: the running job stops before its next edit.
    pub fn cancel_bulk(&self) {
        if RUNNING.load(Ordering::SeqCst) {
            CANCEL.store(true, Ordering::SeqCst);
        }
    }
}
