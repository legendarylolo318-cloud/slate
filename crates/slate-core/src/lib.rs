// Inherited from the Tauri-era backend: these style lints would force invasive
// signature churn in code that is otherwise tested and unchanged by the port.
#![allow(
    clippy::double_must_use,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::large_enum_variant,
    clippy::doc_lazy_continuation,
    clippy::doc_overindented_list_items
)]

pub mod audio;
pub mod events;
pub mod media;
pub mod queue;
pub use events::{AppHandle, EventSink, Events};
pub mod cache;
pub mod commands;
pub mod controller;
pub mod crypto;
pub mod discord;
pub mod discord_ipc;
pub mod embedded_config;
pub mod embedded_lastfm;
pub mod embedded_librefm;
pub mod error;
pub mod http_util;
pub mod idle_inhibit;
pub mod logging;
pub mod mcp;
pub mod overlay;
pub mod pipeline_probe;
pub mod proxy;
pub mod proxy_http;
pub mod rate_gate;
pub mod scrobble;
pub mod signal_path;
pub mod theme_config;
#[cfg(target_os = "linux")]
pub mod tidal_api;
pub mod tidal_report;

pub use error::SlateError;
pub use signal_path::{SignalPath, SignalPathTracker};

use audio::{AudioDevice, AudioPlayer};
use cache::DiskCache;
use crypto::Crypto;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tidal_api::{AuthTokens, TidalClient};
use tokio::sync::Mutex;

mod defaults {
    pub fn yes() -> bool {
        true
    }
    pub fn volume() -> f32 {
        1.0
    }
    pub fn mcp_enabled() -> bool {
        false
    }
    pub fn mcp_port() -> u16 {
        5577
    }
    pub fn overlay_enabled() -> bool {
        false
    }
    pub fn overlay_port() -> u16 {
        5578
    }
    pub fn overlay_host() -> String {
        "127.0.0.1".to_string()
    }
    pub fn max_quality() -> String {
        "HI_RES_LOSSLESS".to_string()
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct LastfmCredentials {
    pub session_key: String,
    pub username: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct ListenBrainzCredentials {
    pub token: String,
    pub username: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct ScrobbleSettings {
    pub lastfm: Option<LastfmCredentials>,
    pub librefm: Option<LastfmCredentials>,
    pub listenbrainz: Option<ListenBrainzCredentials>,
}

/// Tracks which embedded credential pair the saved tokens belong to,
/// so refresh-token requests use the matching client_id/secret.
/// Only relevant when the user has not provided custom credentials.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum AuthMethod {
    #[default]
    LoginCode,
    Pkce,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum ProxyType {
    #[default]
    Http,
    Socks5,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct ProxySettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub proxy_type: ProxyType,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Settings {
    pub auth_tokens: Option<AuthTokens>,
    #[serde(default = "defaults::volume")]
    pub volume: f32,
    pub last_track_id: Option<u64>,
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    /// Which embedded credential pair to use for refresh when `client_id`
    /// is empty. Defaults to LoginCode for backward compatibility with
    /// existing installs.
    #[serde(default)]
    pub auth_method: AuthMethod,
    #[serde(default)]
    pub minimize_to_tray: bool,
    #[serde(default)]
    pub decorations: bool,
    /// One-shot flag: was the user migrated from native chrome to the
    /// custom React titlebar? `false` (or missing) on existing installs
    /// triggers a silent flip of `decorations` to `false` at startup.
    #[serde(default)]
    pub titlebar_migration_v1: bool,
    #[serde(default)]
    pub volume_normalization: bool,
    #[serde(default)]
    pub exclusive_mode: bool,
    #[serde(default)]
    pub exclusive_device: Option<String>,
    #[serde(default)]
    pub bit_perfect: bool,
    #[serde(default = "defaults::yes")]
    pub gapless: bool,
    #[serde(default = "defaults::max_quality")]
    pub max_quality: String,
    #[serde(default)]
    pub scrobble: ScrobbleSettings,
    #[serde(default)]
    pub proxy: ProxySettings,
    #[serde(default = "defaults::yes")]
    pub discord_rpc: bool,
    #[serde(default)]
    pub discord_status_text: String,
    /// How many times we've shown the "legacy sign-in" notice to users still
    /// on the device-code (LoginCode) auth method. Caps at 5; never resets.
    #[serde(default)]
    pub legacy_auth_notice_count: u8,
    #[serde(default = "defaults::mcp_enabled")]
    pub mcp_enabled: bool,
    #[serde(default = "defaults::mcp_port")]
    pub mcp_port: u16,
    /// Persistent UUID token for the MCP URL path. Empty string means
    /// "not yet generated" — bootstrap will populate and save on first run.
    #[serde(default)]
    pub mcp_token: String,
    #[serde(default = "defaults::overlay_enabled")]
    pub overlay_enabled: bool,
    #[serde(default = "defaults::overlay_port")]
    pub overlay_port: u16,
    #[serde(default = "defaults::overlay_host")]
    pub overlay_host: String,
    /// Report plays to TIDAL (Recently Played). On by default; disable in
    /// Settings → Scrobbling.
    #[serde(default = "defaults::yes")]
    pub report_plays: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auth_tokens: None,
            volume: 1.0,
            last_track_id: None,
            client_id: String::new(),
            client_secret: String::new(),
            auth_method: AuthMethod::default(),
            minimize_to_tray: false,
            decorations: false,
            titlebar_migration_v1: true,
            volume_normalization: false,
            exclusive_mode: false,
            exclusive_device: None,
            bit_perfect: false,
            gapless: true,
            max_quality: "HI_RES_LOSSLESS".to_string(),
            scrobble: Default::default(),
            proxy: Default::default(),
            discord_rpc: true,
            discord_status_text: String::new(),
            legacy_auth_notice_count: 0,
            mcp_enabled: false,
            mcp_port: 5577,
            mcp_token: String::new(),
            overlay_enabled: false,
            overlay_port: 5578,
            overlay_host: "127.0.0.1".to_string(),
            report_plays: true,
        }
    }
}

pub struct AppState {
    pub audio_player: Arc<AudioPlayer>,
    pub pipeline_probe: Arc<crate::pipeline_probe::PipelineProbe>,
    pub tidal_client: Mutex<TidalClient>,
    /// The one reqwest client every consumer shares. Blocked plans hold an
    /// `Err`, so no caller can fall back to an unproxied client.
    pub proxied_http: crate::proxy_http::ProxiedHttp,
    /// The probed GStreamer capabilities, re-read whenever the proxy settings
    /// are saved. Not a plain value: the audio thread re-probes on every
    /// `SetProxySettings`, and a copy frozen at startup lets the advisory
    /// refusal in `commands::playback` outlive the facts the boundary decides
    /// on — refusing a tier the audio thread would have built. This keeps the
    /// two answering from one probe; it does not promise that a plugin
    /// installed mid-session becomes visible, which is why those refusals say
    /// to restart. Read it through `host_caps()`.
    pub host_caps: std::sync::Mutex<crate::proxy::HostCaps>,
    pub settings_path: PathBuf,
    pub cache_dir: PathBuf,
    pub disk_cache: DiskCache,
    pub crypto: Arc<Crypto>,
    pub minimize_to_tray: AtomicBool,
    pub decorations: AtomicBool,
    pub volume_normalization: AtomicBool,
    pub exclusive_mode: AtomicBool,
    pub bit_perfect: AtomicBool,
    pub gapless: AtomicBool,
    pub max_quality: std::sync::Mutex<String>,
    pub exclusive_device: std::sync::Mutex<Option<String>>,
    pub cached_audio_devices: std::sync::Mutex<Option<Vec<AudioDevice>>>,
    /// Current track's selected replay gain (dB) stored as f64 bits. NAN = no data.
    /// Album or track gain depending on playback context.
    pub last_replay_gain: AtomicU64,
    /// Current track's selected peak amplitude (linear) stored as f64 bits. NAN = no data.
    /// Album or track peak depending on playback context.
    pub last_peak_amplitude: AtomicU64,
    pub media: media::MediaHub,
    pub scrobble_manager: scrobble::ScrobbleManager,
    pub tidal_reporter: tidal_report::TidalReporter,
    pub discord: discord::DiscordHandle,
    pub idle_inhibitor: Mutex<idle_inhibit::IdleInhibitor>,
    pub mcp_state: crate::mcp::McpStateRef,
    pub mcp_handle: Mutex<Option<crate::mcp::McpHandle>>,
    pub overlay_state: crate::overlay::OverlayStateRef,
    pub overlay_handle: Mutex<Option<crate::overlay::OverlayHandle>>,
    pub signal_path: Arc<SignalPathTracker>,
}

/// The config directory, resolvable before `AppState` exists — `main.rs` needs
/// it while the process is still single-threaded, long before the keyring and
/// the decryption key are available.
///
/// `AppState::new` falls back to `./slate` when there is no user config dir at
/// all; this returns `None` there instead, so the startup scrub simply does
/// nothing rather than reading a sidecar from the working directory.
pub fn config_dir_for_env() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|d| d.join("slate"))
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Write refreshed tokens back into the stored settings. Called from every
/// refresh, including the automatic one after a 401 — without it the stored
/// access token stays stale and each launch burns a 401 before the first
/// request succeeds.
fn persist_auth_tokens(path: &Path, crypto: &Crypto, tokens: &AuthTokens) {
    let mut settings = fs::read(path)
        .ok()
        .and_then(|data| crypto.decrypt(&data).ok())
        .and_then(|plain| serde_json::from_slice::<Settings>(&plain).ok())
        .unwrap_or_default();
    settings.auth_tokens = Some(tokens.clone());

    let write = || -> Result<(), SlateError> {
        let json = serde_json::to_string_pretty(&settings)?;
        let encrypted = crypto.encrypt(json.as_bytes())?;
        fs::write(path, encrypted)?;
        Ok(())
    };
    if let Err(e) = write() {
        log::warn!("Failed to persist refreshed auth tokens: {e}");
    }
}

impl AppState {
    pub fn new(app_handle: AppHandle) -> Self {
        // Get config dir
        let mut config_dir = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        config_dir.push("slate");
        fs::create_dir_all(&config_dir).ok();

        let settings_path = config_dir.join("settings.json");
        let cache_dir = config_dir.join("cache");
        fs::create_dir_all(&cache_dir).ok();

        // Initialize encryption
        let crypto = match Crypto::new(&config_dir) {
            Ok(c) => Arc::new(c),
            Err(e) => {
                log::error!("Failed to initialize crypto: {e}. Data-at-rest encryption disabled.");
                panic!("Crypto initialization failed: {e}");
            }
        };

        let disk_cache = DiskCache::new(&cache_dir, crypto.clone());

        // Load preferences from saved settings (decrypt if needed)
        let mut saved = fs::read(&settings_path)
            .ok()
            .and_then(|data| crypto.decrypt(&data).ok())
            .and_then(|plain| String::from_utf8(plain).ok())
            .and_then(|s| serde_json::from_str::<Settings>(&s).ok());

        // One-shot custom-titlebar migration: existing installs had
        // `decorations: true` (native GTK chrome); silent-flip to false so
        // the custom React titlebar is shown by default. The toggle in
        // Settings remains as an escape hatch.
        if let Some(ref mut s) = saved {
            if !s.titlebar_migration_v1 {
                log::info!("[migration] custom-titlebar v1: flipping decorations to false");
                s.decorations = false;
                s.titlebar_migration_v1 = true;
                if let Ok(json) = serde_json::to_string_pretty(s) {
                    if let Ok(encrypted) = crypto.encrypt(json.as_bytes()) {
                        if let Err(e) = fs::write(&settings_path, encrypted) {
                            log::warn!("[migration] failed to persist titlebar_migration_v1: {e}");
                        }
                    }
                }
            }
        }

        // One-shot proxy migration. A pre-branch install can hold an enabled
        // proxy with no host or a zero port, which the old client builder read
        // as "no proxy". This module fails closed instead, so left alone it
        // would come up with every request blocked and no way in. Turning it
        // off restores exactly what that install already had.
        if let Some(ref mut s) = saved {
            if crate::proxy::migrate_incomplete_proxy(&mut s.proxy) {
                log::info!(
                    "[migration] proxy was enabled with no host or port; disabling it \
                     so the app is not blocked by settings a previous version accepted"
                );
                if let Ok(json) = serde_json::to_string_pretty(s) {
                    if let Ok(encrypted) = crypto.encrypt(json.as_bytes()) {
                        if let Err(e) = fs::write(&settings_path, encrypted) {
                            log::warn!("[migration] failed to persist the proxy migration: {e}");
                        }
                    }
                }
            }
        }

        // Eager migration: if settings exist but aren't encrypted, re-save encrypted
        if settings_path.exists() {
            if let Ok(raw) = fs::read(&settings_path) {
                if !crypto::is_encrypted(&raw) {
                    if let Some(ref settings) = saved {
                        if let Ok(json) = serde_json::to_string_pretty(settings) {
                            if let Ok(encrypted) = crypto.encrypt(json.as_bytes()) {
                                if let Err(e) = fs::write(&settings_path, encrypted) {
                                    log::warn!("Failed to migrate settings to encrypted: {e}");
                                } else {
                                    log::info!("Migrated settings.json to encrypted format");
                                }
                            }
                        }
                    }
                }
            }
        }

        let minimize_to_tray = saved.as_ref().map(|s| s.minimize_to_tray).unwrap_or(false);
        let decorations = saved.as_ref().map(|s| s.decorations).unwrap_or(false);
        let volume_normalization = saved
            .as_ref()
            .map(|s| s.volume_normalization)
            .unwrap_or(false);
        let exclusive_mode = saved.as_ref().map(|s| s.exclusive_mode).unwrap_or(false);
        let bit_perfect = saved.as_ref().map(|s| s.bit_perfect).unwrap_or(false);
        let gapless = saved.as_ref().map(|s| s.gapless).unwrap_or(true);
        let exclusive_device = saved.as_ref().and_then(|s| s.exclusive_device.clone());
        let max_quality = saved
            .as_ref()
            .map(|s| s.max_quality.clone())
            .unwrap_or_else(defaults::max_quality);

        // `saved` is `None` when the settings cannot be decrypted at all, and
        // the default proxy is disabled — so a failed decrypt overwrites an
        // existing `on` sidecar with `off`. That direction is the safe one: the
        // next launch declines to scrub and every consumer reads the system's
        // own configuration, which is what a user with no usable settings
        // should get. It self-heals as soon as the settings are readable again.
        let proxy_settings = saved.as_ref().map(|s| s.proxy.clone()).unwrap_or_default();
        // Reconcile the launch sidecar with the encrypted truth on every start.
        // Saving the proxy settings writes it too, but that only covers people
        // who touch the proxy screen again: without this, anyone who configured
        // a proxy before the sidecar existed — or whose sidecar was deleted
        // with the rest of a stale config — would never get the startup scrub,
        // because `main.rs` would keep reading "no sidecar" as "not proxying".
        // Takes effect on the *next* launch; this one already has threads.
        crate::proxy::write_sidecar(&config_dir, &proxy_settings);
        let host_caps = crate::audio::probe_host_caps();
        log::info!(
            "[proxy] GStreamer {}.{}.{}, dashdemux={}, curlhttpsrc={}",
            host_caps.gst_version.0,
            host_caps.gst_version.1,
            host_caps.gst_version.2,
            host_caps.has_dashdemux,
            host_caps.has_curlhttpsrc
        );
        // Settings that do not form a plan block egress rather than falling back
        // to Direct: "we could not read your proxy" must not become "so we went
        // around it". The user fixes it in settings, which needs no network.
        let proxied_http =
            crate::proxy_http::ProxiedHttp::from_settings(&proxy_settings, &host_caps);

        let scrobble_manager = scrobble::ScrobbleManager::new(
            app_handle.clone(),
            crypto.clone(),
            &config_dir,
            proxied_http.clone(),
        );

        let report_plays = saved.as_ref().map(|s| s.report_plays).unwrap_or(true);
        let tidal_reporter = tidal_report::TidalReporter::new(
            app_handle.clone(),
            crypto.clone(),
            &config_dir,
            proxied_http.clone(),
            report_plays,
        );

        let discord_rpc_enabled = saved.as_ref().map(|s| s.discord_rpc).unwrap_or(true);
        let discord_status_text = saved
            .as_ref()
            .map(|s| s.discord_status_text.clone())
            .unwrap_or_default();
        let discord_handle = discord::DiscordHandle::new();
        discord_handle.send(discord::DiscordCommand::SetStatusText {
            text: discord_status_text,
        });
        if discord_rpc_enabled {
            discord_handle.send(discord::DiscordCommand::Connect);
        }

        let signal_path = Arc::new(SignalPathTracker::new(app_handle.clone()));
        signal_path.set_audio_modes(exclusive_mode, bit_perfect);
        signal_path.set_normalization_enabled(volume_normalization);

        let audio_player = Arc::new(AudioPlayer::new(
            app_handle.clone(),
            Arc::clone(&signal_path),
            proxy_settings.clone(),
        ));
        let pipeline_probe = Arc::new(crate::pipeline_probe::PipelineProbe::new(
            Arc::clone(&signal_path),
            Arc::clone(&audio_player),
        ));

        let mut tidal_client = TidalClient::new(proxied_http.clone());
        tidal_client.set_token_persist({
            let settings_path = settings_path.clone();
            let crypto = Arc::clone(&crypto);
            Arc::new(move |tokens: &AuthTokens| {
                persist_auth_tokens(&settings_path, &crypto, tokens);
            })
        });

        Self {
            audio_player,
            pipeline_probe,
            tidal_client: Mutex::new(tidal_client),
            proxied_http,
            host_caps: std::sync::Mutex::new(host_caps),
            settings_path,
            cache_dir,
            disk_cache,
            crypto,
            minimize_to_tray: AtomicBool::new(minimize_to_tray),
            decorations: AtomicBool::new(decorations),
            volume_normalization: AtomicBool::new(volume_normalization),
            exclusive_mode: AtomicBool::new(exclusive_mode),
            bit_perfect: AtomicBool::new(bit_perfect),
            gapless: AtomicBool::new(gapless),
            max_quality: std::sync::Mutex::new(max_quality),
            exclusive_device: std::sync::Mutex::new(exclusive_device),
            cached_audio_devices: std::sync::Mutex::new(None),
            last_replay_gain: AtomicU64::new(f64::NAN.to_bits()),
            last_peak_amplitude: AtomicU64::new(f64::NAN.to_bits()),
            media: media::MediaHub::default(),
            scrobble_manager,
            tidal_reporter,
            discord: discord_handle,
            idle_inhibitor: Mutex::new(idle_inhibit::IdleInhibitor::new()),
            mcp_state: crate::mcp::new_state(),
            mcp_handle: Mutex::new(None),
            overlay_state: {
                let (s, _rx, _theme_rx) = crate::overlay::new_state();
                s
            },
            overlay_handle: Mutex::new(None),
            signal_path,
        }
    }

    /// The capabilities as last probed. `HostCaps` is `Copy`, so nothing holds
    /// the lock past the read.
    pub fn host_caps(&self) -> crate::proxy::HostCaps {
        *self
            .host_caps
            .lock()
            .unwrap_or_else(|poislated| poislated.into_inner())
    }

    /// Re-read the registry and store the result. Called where the proxy
    /// settings are saved, which is the moment the audio thread re-probes too —
    /// so the advisory layer and the boundary answer from the same facts.
    pub fn refresh_host_caps(&self) {
        let probed = crate::audio::probe_host_caps();
        let mut caps = self
            .host_caps
            .lock()
            .unwrap_or_else(|poislated| poislated.into_inner());
        if *caps != probed {
            log::info!(
                "[proxy] host capabilities changed: GStreamer {}.{}.{}, \
                 dashdemux={}, curlhttpsrc={}",
                probed.gst_version.0,
                probed.gst_version.1,
                probed.gst_version.2,
                probed.has_dashdemux,
                probed.has_curlhttpsrc
            );
        }
        *caps = probed;
    }

    pub fn load_settings(&self) -> Option<Settings> {
        let data = fs::read(&self.settings_path).ok()?;
        let plain = self.crypto.decrypt(&data).ok()?;
        let text = String::from_utf8(plain).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<(), SlateError> {
        let json = serde_json::to_string_pretty(settings)?;
        let encrypted = self.crypto.encrypt(json.as_bytes())?;
        fs::write(&self.settings_path, encrypted)?;
        Ok(())
    }

    // ---- Persistent state (not cache — survives restarts) ----

    pub fn read_state_file(&self, name: &str) -> Option<String> {
        let path = self.cache_dir.join(name);
        let data = match fs::read(&path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                log::warn!("Failed to read state file {name}: {e}");
                return None;
            }
        };
        let plain = match self.crypto.decrypt(&data) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("Failed to decrypt state file {name}: {e}");
                return None;
            }
        };
        match String::from_utf8(plain) {
            Ok(s) => Some(s),
            Err(e) => {
                log::warn!("State file {name} contains invalid UTF-8: {e}");
                None
            }
        }
    }

    pub fn write_state_file(&self, name: &str, content: &str) -> Result<(), SlateError> {
        let path = self.cache_dir.join(name);
        let encrypted = self.crypto.encrypt(content.as_bytes())?;
        fs::write(&path, encrypted)?;
        Ok(())
    }
}
