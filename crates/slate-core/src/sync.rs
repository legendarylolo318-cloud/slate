//! Poison-tolerant locking.
//!
//! A poisoned `std::sync::Mutex` only means another thread panicked while holding
//! it. For the plain-data state guarded here, continuing with the last written
//! value beats cascading the panic through the audio and UI threads.

use std::sync::{Mutex, MutexGuard};

pub trait LockExt<T> {
    fn lock_safe(&self) -> MutexGuard<'_, T>;
}

impl<T> LockExt<T> for Mutex<T> {
    fn lock_safe(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn recovers_from_a_poisoned_lock() {
        let m = Arc::new(Mutex::new(5));
        let m2 = Arc::clone(&m);
        let _ = std::thread::spawn(move || {
            let _g = m2.lock_safe();
            panic!("poison it");
        })
        .join();
        assert!(m.lock().is_err(), "the lock really is poisoned");
        assert_eq!(*m.lock_safe(), 5);
    }
}
