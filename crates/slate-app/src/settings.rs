//! Settings page glue: reads current values from the core into the
//! `SettingsState` global and applies changes. No policy lives here.

use std::sync::Arc;

use slate_core::commands::{mcp, overlay, scrobble, utility};
use slate_core::{AppState, ProxySettings, ProxyType};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::backend::Backend;
use crate::bootstrap::{AppWindow, SettingsState, Theme};
use crate::uiprefs::UiPrefs;

const QUALITIES: [&str; 3] = ["HI_RES_LOSSLESS", "LOSSLESS", "HIGH"];

/// A consistent reading of every setting, built off the UI thread.
#[derive(Default)]
struct Snapshot {
    minimize_to_tray: bool,
    logging: bool,
    normalization: bool,
    gapless: bool,
    gapless_supported: bool,
    quality_index: i32,
    exclusive: bool,
    bit_perfect: bool,
    devices: Vec<String>,
    device_index: i32,
    report_plays: bool,
    providers: Vec<(String, bool, String)>,
    discord_rpc: bool,
    discord_text: String,
    proxy: ProxySettings,
    mcp: (bool, String),
    overlay: (bool, String),
}

async fn snapshot(state: &Arc<AppState>) -> Snapshot {
    let mut s = Snapshot {
        minimize_to_tray: utility::get_minimize_to_tray(state),
        logging: utility::get_enable_logging(),
        normalization: utility::get_volume_normalization(state),
        gapless: utility::get_gapless(state),
        gapless_supported: utility::get_gapless_supported(),
        exclusive: utility::get_exclusive_mode(state),
        bit_perfect: utility::get_bit_perfect(state),
        report_plays: utility::get_report_plays(state),
        discord_rpc: utility::get_discord_rpc(state),
        discord_text: utility::get_discord_status_text(state),
        proxy: utility::get_proxy_settings(state),
        ..Snapshot::default()
    };
    let quality = utility::get_max_quality(state);
    s.quality_index = QUALITIES
        .iter()
        .position(|q| *q == quality)
        .map_or(0, |i| i as i32);
    #[cfg(target_os = "linux")]
    if let Ok(devs) = utility::list_audio_devices(state) {
        let current = utility::get_exclusive_device(state);
        s.device_index = devs
            .iter()
            .position(|d| Some(&d.id) == current.as_ref())
            .map_or(0, |i| i as i32);
        s.devices = devs.into_iter().map(|d| d.name).collect();
    }
    if let Ok(list) = scrobble::get_scrobble_status(state).await {
        s.providers = list
            .into_iter()
            .map(|p| (p.name, p.connected, p.username.unwrap_or_default()))
            .collect();
    }
    if let Ok(i) = mcp::mcp_get_connection_info(state).await {
        s.mcp = (i.enabled, i.url.unwrap_or_default());
    }
    if let Ok(i) = overlay::overlay_get_connection_info(state).await {
        s.overlay = (i.enabled, i.url.unwrap_or_default());
    }
    s
}

fn apply(w: &AppWindow, s: Snapshot, prefs: &UiPrefs) {
    let g = w.global::<SettingsState>();
    g.set_minimize_to_tray(s.minimize_to_tray);
    g.set_logging(s.logging);
    g.set_ui_scale_index(prefs.scale_index as i32);
    g.set_normalization(s.normalization);
    g.set_gapless(s.gapless);
    g.set_gapless_supported(s.gapless_supported);
    g.set_quality_index(s.quality_index);
    g.set_exclusive(s.exclusive);
    g.set_bit_perfect(s.bit_perfect);
    g.set_show_alsa_options(cfg!(target_os = "linux"));
    let names: Vec<SharedString> = s.devices.into_iter().map(Into::into).collect();
    g.set_devices(ModelRc::new(VecModel::from(names)));
    g.set_device_index(s.device_index);
    g.set_report_plays(s.report_plays);
    for (name, connected, user) in s.providers {
        match name.as_str() {
            "listenbrainz" => {
                g.set_lb_connected(connected);
                g.set_lb_user(user.into());
            }
            "lastfm" => {
                g.set_lastfm_connected(connected);
                g.set_lastfm_user(user.into());
            }
            "librefm" => {
                g.set_librefm_connected(connected);
                g.set_librefm_user(user.into());
            }
            _ => {}
        }
    }
    g.set_discord_rpc(s.discord_rpc);
    g.set_discord_text(s.discord_text.into());
    g.set_proxy_enabled(s.proxy.enabled);
    g.set_proxy_type_index(i32::from(s.proxy.proxy_type == ProxyType::Socks5));
    g.set_proxy_host(s.proxy.host.into());
    g.set_proxy_port(
        if s.proxy.port == 0 {
            String::new()
        } else {
            s.proxy.port.to_string()
        }
        .into(),
    );
    g.set_proxy_user(s.proxy.username.unwrap_or_default().into());
    g.set_proxy_pass(s.proxy.password.unwrap_or_default().into());
    g.set_mcp_enabled(s.mcp.0);
    g.set_mcp_url(s.mcp.1.into());
    g.set_overlay_enabled(s.overlay.0);
    g.set_overlay_url(s.overlay.1.into());
    g.set_version(env!("CARGO_PKG_VERSION").into());
}

impl Backend {
    fn settings_status(&self, text: impl Into<String>) {
        let text = text.into();
        self.ui(move |w| w.global::<SettingsState>().set_status(text.into()));
    }

    /// Fills the settings page from the core.
    pub fn load_settings_ui(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            let snap = snapshot(&state).await;
            let prefs = UiPrefs::load();
            this.ui(move |w| apply(&w, snap, &prefs));
        });
    }

    pub fn setting_bool(&self, key: &str, v: bool) {
        let key = key.to_string();
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            let result: Result<(), String> = match key.as_str() {
                "dark" => {
                    let mut p = UiPrefs::load();
                    p.dark = v;
                    p.save().map_err(|e| e.to_string())
                }
                "minimize-to-tray" => {
                    utility::set_minimize_to_tray(&state, v).map_err(|e| e.log_safe())
                }
                "logging" => utility::set_enable_logging(v).map_err(|e| e.log_safe()),
                "normalization" => {
                    utility::set_volume_normalization(&state, v).map_err(|e| e.log_safe())
                }
                "gapless" => utility::set_gapless(&state, v).map_err(|e| e.log_safe()),
                "exclusive" => utility::set_exclusive_mode(&state, v).map_err(|e| e.log_safe()),
                "bit-perfect" => utility::set_bit_perfect(&state, v).map_err(|e| e.log_safe()),
                "report-plays" => utility::set_report_plays(&state, v)
                    .await
                    .map_err(|e| e.log_safe()),
                "discord-rpc" => utility::set_discord_rpc(&state, v).map_err(|e| e.log_safe()),
                "mcp" => mcp::mcp_set_enabled(&state, this.app.clone(), v)
                    .await
                    .map(|_| ())
                    .map_err(|e| e.log_safe()),
                "overlay" => overlay::overlay_set_enabled(&state, v)
                    .await
                    .map(|_| ())
                    .map_err(|e| e.log_safe()),
                _ => Ok(()),
            };
            match result {
                // Re-read so the page shows what the core actually accepted
                // (bit-perfect, for one, can force other settings).
                Ok(()) => {
                    this.settings_status("");
                    this.load_settings_ui();
                }
                Err(e) => {
                    this.settings_status(format!("Could not change {key}: {e}"));
                    this.load_settings_ui();
                }
            }
        });
    }

    pub fn setting_int(&self, key: &str, i: i32) {
        let key = key.to_string();
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            let usize_i = usize::try_from(i).unwrap_or(0);
            let result: Result<(), String> = match key.as_str() {
                "ui-scale" => {
                    let mut p = UiPrefs::load();
                    p.scale_index = usize_i;
                    this.settings_status("Interface size applies the next time Slate starts.");
                    p.save().map_err(|e| e.to_string())
                }
                "quality" => utility::set_max_quality(
                    &state,
                    QUALITIES
                        .get(usize_i)
                        .copied()
                        .unwrap_or("HI_RES_LOSSLESS")
                        .to_string(),
                )
                .map_err(|e| e.log_safe()),
                "device" => match utility::list_audio_devices(&state) {
                    Ok(devs) => match devs.get(usize_i) {
                        Some(d) => utility::set_exclusive_device(&state, d.id.clone())
                            .map_err(|e| e.log_safe()),
                        None => Ok(()),
                    },
                    Err(e) => Err(e.log_safe()),
                },
                _ => Ok(()),
            };
            if let Err(e) = result {
                this.settings_status(format!("Could not change {key}: {e}"));
            }
        });
    }

    pub fn setting_str(&self, key: &str, v: &str) {
        let (key, v) = (key.to_string(), v.to_string());
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            if key == "discord-text" {
                if let Err(e) = utility::set_discord_status_text(&state, v) {
                    this.settings_status(format!("Could not save status text: {}", e.log_safe()));
                }
            }
        });
    }

    pub fn setting_action(&self, name: &str) {
        let name = name.to_string();
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            match name.as_str() {
                "open-logs" => {
                    if let Err(e) = utility::open_log_folder() {
                        this.settings_status(e);
                    }
                }
                "clear-cache" => {
                    state.disk_cache.clear().await;
                    this.settings_status("Cache cleared.");
                }
                "lb-connect" => {
                    let token = this
                        .read_ui(|w| w.global::<SettingsState>().get_lb_token().to_string())
                        .await;
                    match scrobble::connect_listenbrainz(&state, token).await {
                        Ok(user) => {
                            this.settings_status(format!("Connected to ListenBrainz as {user}."))
                        }
                        Err(e) => this.settings_status(format!("ListenBrainz: {}", e.log_safe())),
                    }
                    this.ui(|w| w.global::<SettingsState>().set_lb_token("".into()));
                    this.load_settings_ui();
                }
                "lb-disconnect" => this.disconnect("listenbrainz", &state).await,
                "lastfm-disconnect" => this.disconnect("lastfm", &state).await,
                "librefm-disconnect" => this.disconnect("librefm", &state).await,
                "lastfm-connect" => this.begin_audioscrobbler("lastfm", &state).await,
                "librefm-connect" => this.begin_audioscrobbler("librefm", &state).await,
                "lastfm-finish" => this.finish_audioscrobbler("lastfm", &state).await,
                "librefm-finish" => this.finish_audioscrobbler("librefm", &state).await,
                "mcp-regenerate" => match mcp::mcp_regenerate_token(&state, this.app.clone()).await
                {
                    Ok(_) => {
                        this.settings_status("New MCP URL generated.");
                        this.load_settings_ui();
                    }
                    Err(e) => this.settings_status(e.log_safe()),
                },
                "proxy-save" | "proxy-test" => {
                    let settings = this.read_ui(proxy_from_ui).await;
                    let Some(settings) = settings else {
                        this.settings_status("Enter a host and a port between 1 and 65535.");
                        return;
                    };
                    if name == "proxy-test" {
                        match utility::test_proxy_connection(settings, &state).await {
                            Ok(msg) => this.settings_status(msg),
                            Err(e) => this.settings_status(format!("Proxy test failed: {e}")),
                        }
                    } else {
                        match utility::set_proxy_settings(&state, settings).await {
                            Ok(()) => this.settings_status("Proxy settings saved."),
                            Err(e) => this
                                .settings_status(format!("Could not save proxy: {}", e.log_safe())),
                        }
                    }
                }
                _ => {}
            }
        });
    }

    async fn disconnect(&self, provider: &str, state: &Arc<AppState>) {
        match scrobble::disconnect_provider(state, provider.to_string()).await {
            Ok(()) => self.settings_status(format!("Disconnected {provider}.")),
            Err(e) => self.settings_status(e.log_safe()),
        }
        self.load_settings_ui();
    }

    async fn begin_audioscrobbler(&self, provider: &str, state: &Arc<AppState>) {
        let started = if provider == "lastfm" {
            scrobble::connect_lastfm(state).await
        } else {
            scrobble::connect_librefm(state).await
        };
        match started {
            Ok(a) => {
                if let Ok(mut p) = self.pending_scrobble.lock() {
                    *p = Some((provider.to_string(), a.token));
                }
                let url = a.url;
                self.rt.spawn_blocking(move || {
                    if let Err(e) = open::that(&url) {
                        tracing::warn!("could not open browser: {e}");
                    }
                });
                self.ui(|w| w.global::<SettingsState>().set_audioscrobbler_pending(true));
                self.settings_status("Approve Slate in your browser, then press Finish.");
            }
            Err(e) => self.settings_status(format!("{provider}: {}", e.log_safe())),
        }
    }

    async fn finish_audioscrobbler(&self, provider: &str, state: &Arc<AppState>) {
        let pending = self.pending_scrobble.lock().ok().and_then(|mut p| p.take());
        let Some((name, token)) = pending.filter(|(n, _)| n == provider) else {
            self.settings_status("Press Connect first.");
            return;
        };
        match scrobble::complete_audioscrobbler_auth(state, name, token).await {
            Ok(user) => self.settings_status(format!("Connected as {user}.")),
            Err(e) => self.settings_status(format!("{provider}: {}", e.log_safe())),
        }
        self.ui(|w| {
            w.global::<SettingsState>()
                .set_audioscrobbler_pending(false)
        });
        self.load_settings_ui();
    }

    /// Runs `f` on the UI thread and waits for its result.
    pub(crate) async fn read_ui<R: Send + Default + 'static>(
        &self,
        f: impl FnOnce(AppWindow) -> R + Send + 'static,
    ) -> R {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.ui(move |w| {
            let _ = tx.send(f(w));
        });
        rx.await.unwrap_or_default()
    }

    /// Applies the saved theme mode to the window.
    pub fn apply_theme(w: &AppWindow, prefs: &UiPrefs) {
        w.global::<Theme>().set_dark(prefs.dark);
    }
}

fn proxy_from_ui(w: AppWindow) -> Option<ProxySettings> {
    let g = w.global::<SettingsState>();
    let host = g.get_proxy_host().trim().to_string();
    let port: u16 = g.get_proxy_port().trim().parse().ok().filter(|p| *p > 0)?;
    if host.is_empty() {
        return None;
    }
    let opt = |s: SharedString| {
        let s = s.trim().to_string();
        (!s.is_empty()).then_some(s)
    };
    Some(ProxySettings {
        enabled: g.get_proxy_enabled(),
        proxy_type: if g.get_proxy_type_index() == 1 {
            ProxyType::Socks5
        } else {
            ProxyType::Http
        },
        host,
        port,
        username: opt(g.get_proxy_user()),
        password: opt(g.get_proxy_pass()),
    })
}
