//! Frontend-agnostic event delivery.
//!
//! The core used to emit named events straight into a Tauri webview. It now
//! publishes them to an [`EventSink`] supplied by whichever frontend hosts it,
//! keeping the original `emit("name", payload)` call shape.

use serde::Serialize;
use serde_json::Value;
use std::sync::{Arc, OnceLock};

/// Receives every event the core emits. Implementations must not block.
pub trait EventSink: Send + Sync + 'static {
    fn emit(&self, event: &str, payload: Value);
}

/// Cheaply cloneable handle to an [`EventSink`].
#[derive(Clone)]
pub struct Events(Arc<dyn EventSink>);

impl Events {
    pub fn new(sink: Arc<dyn EventSink>) -> Self {
        Self(sink)
    }

    /// A sink that drops everything. Used by tests and headless tools.
    pub fn null() -> Self {
        struct Null;
        impl EventSink for Null {
            fn emit(&self, _event: &str, _payload: Value) {}
        }
        Self(Arc::new(Null))
    }

    /// Serialises `payload` and forwards it. Serialisation failures are logged
    /// and dropped: events are best-effort notifications.
    pub fn emit<S: Serialize>(&self, event: &str, payload: S) -> Result<(), crate::SlateError> {
        match serde_json::to_value(payload) {
            Ok(v) => self.0.emit(event, v),
            Err(e) => log::warn!("[events] could not serialise payload for {event}: {e}"),
        }
        Ok(())
    }
}

/// Marker for types reachable through [`AppHandle::state`]. Only [`crate::AppState`]
/// is managed, mirroring the single managed state of the original Tauri shell.
pub trait Managed: Sized {
    fn resolve(handle: &AppHandle) -> Arc<Self>;
}

impl Managed for crate::AppState {
    fn resolve(handle: &AppHandle) -> Arc<Self> {
        match handle.shared.get() {
            Some(state) => Arc::clone(state),
            None => {
                // Every constructor installs the state before any task that can
                // reach here is spawned, so this is a programming error and
                // continuing would mean operating on a half-built app.
                log::error!("[events] AppHandle::state called before AppState was installed");
                std::process::abort()
            }
        }
    }
}

/// Frontend-agnostic stand-in for `crate::AppHandle`: emits events and hands out
/// the shared [`crate::AppState`]. Cloning is cheap.
#[derive(Clone)]
pub struct AppHandle {
    events: Events,
    shared: Arc<OnceLock<Arc<crate::AppState>>>,
}

impl AppHandle {
    pub fn new(events: Events) -> Self {
        Self {
            events,
            shared: Arc::new(OnceLock::new()),
        }
    }

    /// Installs the shared state. Returns `false` if one was already installed.
    pub fn install(&self, state: Arc<crate::AppState>) -> bool {
        self.shared.set(state).is_ok()
    }

    pub fn state<T: Managed>(&self) -> Arc<T> {
        T::resolve(self)
    }

    /// The shared state, or `None` while the app is still starting up.
    pub fn try_state(&self) -> Option<Arc<crate::AppState>> {
        self.shared.get().cloned()
    }

    pub fn emit<S: Serialize>(&self, event: &str, payload: S) -> Result<(), crate::SlateError> {
        self.events.emit(event, payload)
    }

    pub fn events(&self) -> &Events {
        &self.events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Rec(Mutex<Vec<(String, Value)>>);
    impl EventSink for Rec {
        fn emit(&self, event: &str, payload: Value) {
            if let Ok(mut g) = self.0.lock() {
                g.push((event.to_string(), payload));
            }
        }
    }

    #[test]
    fn forwards_serialised_payload() {
        let rec = Arc::new(Rec(Mutex::new(Vec::new())));
        let ev = Events::new(rec.clone());
        ev.emit("audio-error", "boom").ok();
        let got = rec.0.lock().map(|g| g.clone()).unwrap_or_default();
        assert_eq!(got, vec![("audio-error".to_string(), Value::from("boom"))]);
    }
}
