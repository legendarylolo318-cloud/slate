//! The MPD server's settings page: switching it on and off, LAN access and the password.
//! Nothing here runs, and no socket exists, until the server is switched on.

use std::net::IpAddr;
use std::sync::atomic::Ordering;

use crate::error::SlateError;
use crate::mpd::{self, Config};
use crate::{AppState, Settings};

#[derive(Clone, Default, PartialEq, Eq)]
pub struct MpdInfo {
    pub enabled: bool,
    pub running: bool,
    pub lan: bool,
    pub password: String,
    pub port: u16,
    /// Where clients connect (this computer, or the LAN address), `host:port`.
    pub address: String,
}

impl std::fmt::Debug for MpdInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MpdInfo")
            .field("enabled", &self.enabled)
            .field("running", &self.running)
            .field("lan", &self.lan)
            .field("password", &crate::mpd::Secret(self.password.clone()))
            .field("port", &self.port)
            .field("address", &self.address)
            .finish()
    }
}

/// A fresh password: 20 hex digits of a random UUID (74 random bits; two digits of a v4 UUID
/// are fixed).
pub fn generate_password() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..20].to_string()
}

/// This computer's address on the local network, if it has one. A UDP "connection" to a
/// documentation address picks the outgoing interface without sending anything.
pub fn lan_ip() -> Option<IpAddr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("192.0.2.1:9").ok()?;
    let ip = sock.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then_some(ip)
}

/// The settings the server runs with, or why it must not start.
pub fn config_of(s: &Settings) -> Result<Config, String> {
    let cfg = Config {
        port: s.mpd_port,
        lan: s.mpd_lan,
        password: s.mpd_password.0.clone(),
    };
    cfg.check()?;
    Ok(cfg)
}

pub async fn mpd_info(state: &AppState) -> MpdInfo {
    let s = state.load_settings().unwrap_or_default();
    let running = state.mpd_handle.lock().await.is_some();
    let host = if s.mpd_lan {
        lan_ip().map_or_else(
            || "this computer's address".to_string(),
            |ip| ip.to_string(),
        )
    } else {
        "127.0.0.1".to_string()
    };
    MpdInfo {
        enabled: s.mpd_enabled,
        running,
        lan: s.mpd_lan,
        password: s.mpd_password.0,
        port: s.mpd_port,
        address: format!("{host}:{}", s.mpd_port),
    }
}

/// (Re)starts the server to match `s`, or stops it. Holds the handle lock throughout, so
/// two callers cannot both bind the port.
async fn apply(state: &AppState, s: &Settings) -> Result<(), SlateError> {
    let mut guard = state.mpd_handle.lock().await;
    if let Some(h) = guard.take() {
        // Waits for the old socket to close: the new one may want the same port.
        h.shutdown().await;
    }
    state.mpd_active.store(false, Ordering::Relaxed);
    if !s.mpd_enabled {
        return Ok(());
    }
    let cfg = config_of(s).map_err(SlateError::Io)?;
    let host = state
        .mpd_host
        .get()
        .cloned()
        .ok_or_else(|| SlateError::Io("the player is not ready yet".into()))?;
    let handle = mpd::server::start(host, cfg)
        .await
        .map_err(|e| SlateError::Io(format!("MPD server could not listen: {e}")))?;
    *guard = Some(handle);
    state.mpd_active.store(true, Ordering::Relaxed);
    Ok(())
}

/// At launch: starts the server if it was left on. Does nothing at all when it is off.
pub async fn mpd_start_if_enabled(state: &AppState) {
    let s = state.load_settings().unwrap_or_default();
    if !s.mpd_enabled {
        return;
    }
    if let Err(e) = apply(state, &s).await {
        log::warn!("[mpd] not started: {}", e.log_safe());
    }
}

pub async fn mpd_set_enabled(state: &AppState, enabled: bool) -> Result<(), SlateError> {
    let mut s = state.load_settings().unwrap_or_default();
    s.mpd_enabled = enabled;
    // The server must match before it is remembered as on.
    apply(state, &s).await?;
    state.save_settings(&s)?;
    Ok(())
}

/// LAN access on binds every interface and makes a password mandatory: one is generated if
/// there is none. Off goes back to this computer only (the password is kept).
pub async fn mpd_set_lan(state: &AppState, lan: bool) -> Result<(), SlateError> {
    let mut s = state.load_settings().unwrap_or_default();
    s.mpd_lan = lan;
    if lan && s.mpd_password.is_empty() {
        s.mpd_password = crate::mpd::Secret(generate_password());
    }
    if s.mpd_enabled {
        apply(state, &s).await?;
    }
    state.save_settings(&s)?;
    Ok(())
}

/// Replaces the password (clients must be given the new one); returns it.
pub async fn mpd_new_password(state: &AppState) -> Result<String, SlateError> {
    let mut s = state.load_settings().unwrap_or_default();
    s.mpd_password = crate::mpd::Secret(generate_password());
    if s.mpd_enabled {
        apply(state, &s).await?;
    }
    state.save_settings(&s)?;
    Ok(s.mpd_password.0)
}

/// Gives the running server (if any) a fresh status; a no-op when it is off.
pub async fn mpd_feed(state: &AppState, status: &mpd::Status) {
    if let Some(h) = state.mpd_handle.lock().await.as_ref() {
        h.feed(status);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_are_long_and_different() {
        let (a, b) = (generate_password(), generate_password());
        assert_eq!(a.len(), 20);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn the_settings_page_info_never_prints_the_password() {
        let info = MpdInfo {
            password: "0123456789abcdef0123".into(),
            ..MpdInfo::default()
        };
        let shown = format!("{info:?}");
        assert!(!shown.contains("0123456789"), "{shown}");
        assert!(shown.contains("<set>"), "{shown}");
    }

    #[test]
    fn lan_needs_a_password_but_this_computer_does_not() {
        let mut s = Settings::default();
        assert!(config_of(&s).is_ok());
        assert_eq!(config_of(&s).map(|c| c.port), Ok(6600));
        s.mpd_lan = true;
        assert!(config_of(&s).is_err());
        s.mpd_password = "x".into();
        assert!(config_of(&s).is_ok());
    }

    #[test]
    fn settings_default_to_off_and_old_files_still_load() {
        let s: Settings = serde_json::from_str("{}").unwrap_or_default();
        assert!(!s.mpd_enabled && !s.mpd_lan && s.mpd_password.is_empty());
        assert_eq!(s.mpd_port, 6600);
        let d = Settings::default();
        assert!(!d.mpd_enabled && d.mpd_port == 6600 && !d.hide_explicit && !d.album_shuffle);
    }
}
