//! "Run this once things have been quiet for a moment": every request restarts the wait and only
//! the last one runs. Used for writes that follow a burst of changes (the queue file).

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

#[derive(Default)]
pub struct Debounce {
    latest: AtomicU64,
}

impl Debounce {
    pub const fn new() -> Self {
        Self {
            latest: AtomicU64::new(0),
        }
    }

    /// Drops the request that is waiting, if any.
    pub fn cancel(&self) {
        self.latest.fetch_add(1, Ordering::AcqRel);
    }

    /// Runs `job` on `rt` after `delay`, unless another `run_later` came in meanwhile.
    pub fn run_later<F>(&'static self, rt: &tokio::runtime::Handle, delay: Duration, job: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let gen = self.latest.fetch_add(1, Ordering::AcqRel) + 1;
        rt.spawn(async move {
            tokio::time::sleep(delay).await;
            if self.latest.load(Ordering::Acquire) == gen {
                job.await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;

    static D: Debounce = Debounce::new();

    #[test]
    fn a_burst_runs_once_after_the_last_request() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let runs = Arc::new(AtomicUsize::new(0));
        for _ in 0..5 {
            let runs = Arc::clone(&runs);
            D.run_later(rt.handle(), Duration::from_millis(80), async move {
                runs.fetch_add(1, Ordering::SeqCst);
            });
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(runs.load(Ordering::SeqCst), 0, "still waiting");
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        let again = Arc::clone(&runs);
        D.run_later(rt.handle(), Duration::from_millis(10), async move {
            again.fetch_add(1, Ordering::SeqCst);
        });
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(runs.load(Ordering::SeqCst), 2, "the next burst runs too");
    }

    #[test]
    fn a_cancelled_request_never_runs() {
        static C: Debounce = Debounce::new();
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let runs = Arc::new(AtomicUsize::new(0));
        let r = Arc::clone(&runs);
        C.run_later(rt.handle(), Duration::from_millis(30), async move {
            r.fetch_add(1, Ordering::SeqCst);
        });
        C.cancel();
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(runs.load(Ordering::SeqCst), 0);
    }
}
