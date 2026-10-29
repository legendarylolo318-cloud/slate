//! Windows: `SetThreadExecutionState` on a thread of our own. The flags hold
//! only while the thread that set them is alive, so setting and clearing must
//! both happen on one long-lived thread, not on whichever tokio worker runs
//! `inhibit`.

use std::sync::mpsc;

use windows_sys::Win32::System::Power::{
    SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
};

pub struct WinInhibitor {
    tx: Option<mpsc::Sender<bool>>,
}

impl WinInhibitor {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel::<bool>();
        let spawned = std::thread::Builder::new()
            .name("slate-power".into())
            .spawn(move || {
                for keep_awake in rx {
                    // Same as Linux: keep both the system and the screen awake.
                    let flags = if keep_awake {
                        ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED
                    } else {
                        ES_CONTINUOUS
                    };
                    // SAFETY: plain Win32 call taking flag bits; no pointers.
                    if unsafe { SetThreadExecutionState(flags) } == 0 {
                        log::warn!("SetThreadExecutionState({flags:#x}) failed");
                    } else {
                        log::info!(
                            "system sleep {}",
                            if keep_awake { "inhibited" } else { "allowed" }
                        );
                    }
                }
            });
        if let Err(e) = &spawned {
            log::warn!("idle inhibit unavailable: {e}");
        }
        Self {
            tx: spawned.ok().map(|_| tx),
        }
    }

    pub fn set(&self, keep_awake: bool) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(keep_awake);
        }
    }
}
