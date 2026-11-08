//! Volume and equalizer per output device (Settings → General, off by default).
//!
//! With the setting off nothing here runs: no file is read, no `pactl` is started, no timer
//! exists. With it on, the device is looked at when something already happens — a track
//! starts, or the user switches exclusive mode or picks an exclusive device — never on a
//! timer, so a device plugged in mid-track is noticed at the next track.
//!
//! The store lives in the encrypted state folder (`devices.json`), is written off the UI
//! thread after a short quiet period and at quit, and is forgotten at sign-out (#124/#139).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use slate_core::commands::utility;
use slate_core::device_memory::{self as mem, DeviceMemory};
use slate_core::AppState;

use crate::backend::Backend;
use crate::debounce::Debounce;

const DEBOUNCE: Duration = Duration::from_millis(700);

static ENABLED: AtomicBool = AtomicBool::new(false);
/// `None` until first used (and again after sign-out or turning the setting off).
static STORE: Mutex<Option<DeviceMemory>> = Mutex::new(None);
static DIRTY: AtomicBool = AtomicBool::new(false);
/// Bumped at sign-out and when the setting is turned off.
static EPOCH: AtomicU64 = AtomicU64::new(0);
/// From sign-out until the next sign-in nothing is read, recorded or written.
static SIGNED_OUT: AtomicBool = AtomicBool::new(false);
static SAVE: Debounce = Debounce::new();
/// The device the player is on, once known. Changes are made under [`APPLY`].
static CURRENT: Mutex<Option<String>> = Mutex::new(None);
/// Serialises "which device is it now → what to restore → restore it", so two quick device
/// changes apply in the order they were seen and the last one wins (#150).
static APPLY: Mutex<()> = Mutex::new(());

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Release);
    if !on {
        EPOCH.fetch_add(1, Ordering::AcqRel);
        SAVE.cancel();
        *lock(&STORE) = None;
        *lock(&CURRENT) = None;
        DIRTY.store(false, Ordering::Release);
    }
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Acquire)
}

fn lock<T>(m: &'static Mutex<T>) -> std::sync::MutexGuard<'static, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Sign-out: nothing of the old account stays in memory and nothing waiting is written (the
/// core deletes the file).
pub fn forget() {
    SIGNED_OUT.store(true, Ordering::Release);
    EPOCH.fetch_add(1, Ordering::AcqRel);
    SAVE.cancel();
    *lock(&STORE) = None;
    *lock(&CURRENT) = None;
    DIRTY.store(false, Ordering::Release);
}

pub fn allow() {
    SIGNED_OUT.store(false, Ordering::Release);
}

fn load(state: &AppState) -> DeviceMemory {
    state
        .read_state_file(mem::FILE)
        .map(|t| DeviceMemory::parse(&t))
        .unwrap_or_default()
}

/// Runs `f` on the store (read from disk on first use); None when off or signed out.
fn with<R>(state: &AppState, f: impl FnOnce(&mut DeviceMemory) -> R) -> Option<R> {
    let mut g = lock(&STORE);
    if !enabled() || SIGNED_OUT.load(Ordering::Acquire) {
        return None;
    }
    Some(f(g.get_or_insert_with(|| load(state))))
}

fn write_now(state: &AppState) {
    let g = lock(&STORE);
    let Some(store) = g.as_ref() else { return };
    if SIGNED_OUT.load(Ordering::Acquire) || !enabled() {
        return;
    }
    if !DIRTY.swap(false, Ordering::AcqRel) {
        return;
    }
    if let Err(e) = state.write_state_file(mem::FILE, &store.to_json()) {
        tracing::warn!("device memory not saved: {}", e.log_safe());
    }
}

/// Quit: writes a change still waiting for the debounce.
pub fn flush(state: &AppState) {
    if DIRTY.load(Ordering::Acquire) {
        write_now(state);
    }
}

/// The key of the device the player is on, or None when it cannot be told.
fn current_key(state: &AppState) -> Option<String> {
    if utility::get_exclusive_mode(state) || utility::get_bit_perfect(state) {
        let dev = utility::get_exclusive_device(state)?;
        return mem::key(if cfg!(windows) { "wasapi" } else { "alsa" }, &dev);
    }
    #[cfg(target_os = "linux")]
    {
        mem::key("pw", &slate_core::pipeline_probe::default_sink_name()?)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

impl Backend {
    /// Something that can change the output device happened: find out which device it is
    /// and, if it is a different one, restore what it remembers. A no-op when off.
    pub(crate) fn device_check(&self) {
        if !enabled() {
            return;
        }
        let Some(state) = self.state() else { return };
        let epoch = EPOCH.load(Ordering::Acquire);
        let this = self.clone();
        self.rt.spawn_blocking(move || {
            let _serial = lock_apply();
            if EPOCH.load(Ordering::Acquire) != epoch {
                return;
            }
            let Some(key) = current_key(&state) else {
                return;
            };
            let previous = lock(&CURRENT).replace(key.clone());
            if previous.as_deref() == Some(key.as_str()) {
                return;
            }
            let first = previous.is_none();
            let entry = with(&state, |m| {
                let e = m.get(&key).cloned();
                if m.used(&key) {
                    DIRTY.store(true, Ordering::Release);
                }
                e
            })
            .flatten();
            let Some(entry) = entry else { return };
            // Start at half volume decides the volume at launch; the first device seen is
            // that launch.
            let half = crate::uiprefs::UiPrefs::load().start_half_volume;
            let mut restored = Vec::new();
            if let Some(v) = mem::volume_to_restore(&entry, first, half) {
                this.set_volume(v);
                restored.push(format!("volume {:.0} %", v * 100.0));
            }
            if let Some(eq) = entry.eq {
                if utility::set_eq(&state, eq).is_ok() {
                    restored.push("equalizer".to_string());
                }
            }
            if DIRTY.load(Ordering::Acquire) {
                this.device_save_later(&state);
            }
            if !restored.is_empty() && !first {
                this.notify(&format!(
                    "Output device changed: restored {}",
                    restored.join(" and ")
                ));
            }
        });
    }

    /// The volume changed: remember it for the current device, if known.
    pub(crate) fn device_note_volume(&self, level: f32) {
        if !enabled() {
            return;
        }
        let Some(state) = self.state() else { return };
        let Some(key) = lock(&CURRENT).clone() else {
            return;
        };
        if with(&state, |m| m.set_volume(&key, level)) == Some(true) {
            DIRTY.store(true, Ordering::Release);
            self.device_save_later(&state);
        }
    }

    /// The equalizer changed: remember it for the current device, if known.
    pub(crate) fn device_note_eq(&self, eq: &slate_core::eq::Settings) {
        if !enabled() {
            return;
        }
        let Some(state) = self.state() else { return };
        let Some(key) = lock(&CURRENT).clone() else {
            return;
        };
        if with(&state, |m| m.set_eq(&key, eq)) == Some(true) {
            DIRTY.store(true, Ordering::Release);
            self.device_save_later(&state);
        }
    }

    fn device_save_later(&self, state: &std::sync::Arc<AppState>) {
        let state = std::sync::Arc::clone(state);
        SAVE.run_later(&self.rt, DEBOUNCE, async move {
            let _ = tokio::task::spawn_blocking(move || write_now(&state)).await;
        });
    }
}

fn lock_apply() -> std::sync::MutexGuard<'static, ()> {
    APPLY.lock().unwrap_or_else(|e| e.into_inner())
}
