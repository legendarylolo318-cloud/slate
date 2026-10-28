//! Glue between the Slint window and `sone-core`. Contains no business logic:
//! every method forwards to a core command on the tokio runtime and publishes
//! the result with `upgrade_in_event_loop`.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use slint::{ComponentHandle, Image, Weak};
use sone_core::commands::{auth, metadata, playback, utility};
use sone_core::{AppHandle, AppState, EventSink, Events};

use crate::bootstrap::{AppWindow, Screen};
use crate::images;

/// Playback facts the ticker needs without touching the UI thread.
#[derive(Default)]
struct Shared {
    playing: AtomicBool,
    has_track: AtomicBool,
    /// Track length in seconds, stored as `f32` bits.
    duration: AtomicU32,
    login_url: Mutex<String>,
}

impl Shared {
    fn duration(&self) -> f32 {
        f32::from_bits(self.duration.load(Ordering::Relaxed))
    }
}

/// Receives core events on arbitrary threads and republishes them to the UI.
struct UiSink {
    ui: Weak<AppWindow>,
    shared: Arc<Shared>,
}

impl EventSink for UiSink {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        match event {
            "audio-error" => {
                let kind = payload
                    .get("kind")
                    .and_then(|k| k.as_str())
                    .unwrap_or("unknown");
                let msg = format!("Audio error: {kind}");
                self.shared.playing.store(false, Ordering::Relaxed);
                let _ = self.ui.upgrade_in_event_loop(move |w| {
                    w.set_error(msg.into());
                    w.set_playing(false);
                });
            }
            "track-finished" => {
                self.shared.playing.store(false, Ordering::Relaxed);
                let _ = self.ui.upgrade_in_event_loop(|w| {
                    w.set_playing(false);
                    w.set_progress(0.0);
                    w.set_position_text("0:00".into());
                });
            }
            _ => {}
        }
    }
}

#[derive(Clone)]
pub struct Backend {
    rt: tokio::runtime::Handle,
    app: AppHandle,
    ui: Weak<AppWindow>,
    shared: Arc<Shared>,
}

impl Backend {
    /// Creates the backend and starts core initialisation in the background.
    pub fn start(rt: tokio::runtime::Handle, window: &AppWindow) -> Self {
        let shared = Arc::new(Shared::default());
        let sink = UiSink {
            ui: window.as_weak(),
            shared: Arc::clone(&shared),
        };
        let app = AppHandle::new(Events::new(Arc::new(sink)));
        let backend = Self {
            rt,
            app,
            ui: window.as_weak(),
            shared,
        };
        backend.init();
        backend.spawn_ticker();
        backend
    }

    fn ui<F: FnOnce(AppWindow) + Send + 'static>(&self, f: F) {
        if self.ui.upgrade_in_event_loop(f).is_err() {
            tracing::debug!("UI already closed; dropping update");
        }
    }

    fn state(&self) -> Option<Arc<AppState>> {
        self.app.try_state()
    }

    fn init(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            let app = this.app.clone();
            // AppState::new does blocking disk and keyring work.
            let built = tokio::task::spawn_blocking(move || AppState::new(app)).await;
            let state = match built {
                Ok(s) => Arc::new(s),
                Err(e) => {
                    tracing::error!("core initialisation failed: {e}");
                    this.ui(|w| {
                        w.set_login_status("Could not start the SONE core. See the log.".into());
                        w.set_screen(Screen::Login);
                    });
                    return;
                }
            };
            if !this.app.install(Arc::clone(&state)) {
                tracing::error!("AppState installed twice");
                return;
            }

            let volume = state.load_settings().map(|s| s.volume).unwrap_or(1.0);
            this.ui(move |w| w.set_volume(volume));

            match auth::load_saved_auth(&state).await {
                Ok(Some(_)) => this.ui(|w| w.set_screen(Screen::Main)),
                Ok(None) => this.ui(|w| w.set_screen(Screen::Login)),
                Err(e) => {
                    tracing::warn!("saved session unusable: {}", e.log_safe());
                    let msg = format!("Session expired: {}", e.log_safe());
                    this.ui(move |w| {
                        w.set_login_status(msg.into());
                        w.set_screen(Screen::Login);
                    });
                }
            }
        });
    }

    /// Polls the audio position twice a second while something is playing.
    fn spawn_ticker(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(500));
            loop {
                tick.tick().await;
                if !this.shared.playing.load(Ordering::Relaxed) {
                    continue;
                }
                let Some(state) = this.state() else { continue };
                let Ok(pos) = playback::get_playback_position(&state).await else {
                    continue;
                };
                let dur = this.shared.duration();
                let progress = if dur > 0.0 {
                    (pos / dur).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let text = images::format_time(pos);
                this.ui(move |w| {
                    w.set_progress(progress);
                    w.set_position_text(text.into());
                });
            }
        });
    }

    // ---- login ----

    pub fn start_login(&self) {
        let this = self.clone();
        self.ui(|w| {
            w.set_login_busy(true);
            w.set_login_status("".into());
        });
        self.rt.spawn(async move {
            if let Err(msg) = this.run_login().await {
                this.ui(move |w| {
                    w.set_login_status(msg.into());
                    w.set_login_busy(false);
                    w.set_login_has_code(false);
                });
            }
        });
    }

    async fn run_login(&self) -> Result<(), String> {
        let state = self
            .state()
            .ok_or("The core is still starting, try again.")?;
        let (id, secret) = auth::get_default_credentials().map_err(|e| e.log_safe())?;
        if id.is_empty() {
            return Err(
                "This build has no embedded TIDAL credentials, so device-code sign-in \
                        is unavailable."
                    .into(),
            );
        }
        let resp = auth::start_device_auth(&state, id.clone(), secret.clone())
            .await
            .map_err(|e| e.log_safe())?;

        let url = resp
            .verification_uri_complete
            .clone()
            .unwrap_or_else(|| resp.verification_uri.clone());
        let url = if url.starts_with("http") {
            url
        } else {
            format!("https://{url}")
        };
        if let Ok(mut g) = self.shared.login_url.lock() {
            *g = url.clone();
        }
        let qr_buf = images::qr(&url);
        let code = resp.user_code.clone();
        let shown_url = resp.verification_uri.clone();
        self.ui(move |w| {
            if let Some(buf) = qr_buf {
                w.set_login_qr(Image::from_rgba8(buf));
            }
            w.set_login_code(code.into());
            w.set_login_url(shown_url.into());
            w.set_login_has_code(true);
            w.set_login_status("Waiting for you to approve…".into());
        });

        let deadline = tokio::time::Instant::now() + Duration::from_secs(resp.expires_in);
        let every = Duration::from_secs(resp.interval.max(2));
        while tokio::time::Instant::now() < deadline {
            tokio::time::sleep(every).await;
            match auth::poll_device_auth(
                &state,
                self.app.clone(),
                resp.device_code.clone(),
                id.clone(),
                secret.clone(),
            )
            .await
            {
                Ok(Some(_)) => {
                    self.ui(|w| {
                        w.set_login_busy(false);
                        w.set_login_has_code(false);
                        w.set_screen(Screen::Main);
                    });
                    return Ok(());
                }
                Ok(None) => {}
                Err(e) => return Err(e.log_safe()),
            }
        }
        Err("The code expired. Try again.".into())
    }

    pub fn open_login_url(&self) {
        let url = self
            .shared
            .login_url
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default();
        if url.is_empty() {
            return;
        }
        self.rt.spawn_blocking(move || {
            if let Err(e) = open::that(&url) {
                tracing::warn!("could not open browser: {e}");
            }
        });
    }

    pub fn logout(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            if let Err(e) = auth::logout(&state).await {
                tracing::warn!("logout: {}", e.log_safe());
            }
            this.shared.playing.store(false, Ordering::Relaxed);
            this.shared.has_track.store(false, Ordering::Relaxed);
            this.ui(|w| {
                w.set_has_track(false);
                w.set_playing(false);
                w.set_track_title("".into());
                w.set_track_artist("".into());
                w.set_track_cover(Image::default());
                w.set_progress(0.0);
                w.set_screen(Screen::Login);
            });
        });
    }

    // ---- playback ----

    pub fn play_track(&self, text: &str) {
        let Ok(id) = text.trim().parse::<u64>() else {
            self.ui(|w| w.set_error("That is not a track ID.".into()));
            return;
        };
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            this.ui(|w| w.set_error("".into()));

            let meta = match metadata::get_track(&state, id).await {
                Ok(m) => m,
                Err(e) => {
                    let msg = format!("Could not load track: {}", e.log_safe());
                    this.ui(move |w| w.set_error(msg.into()));
                    return;
                }
            };
            if let Err(e) = playback::play_tidal_track(&state, id, false).await {
                let msg = format!("Could not play track: {}", e.log_safe());
                this.ui(move |w| w.set_error(msg.into()));
                return;
            }

            let title = meta["title"]
                .as_str()
                .unwrap_or("Unknown title")
                .to_string();
            let artist = meta["artist"]["name"].as_str().unwrap_or("").to_string();
            let duration = meta["duration"].as_f64().unwrap_or(0.0) as f32;
            let cover = meta["album"]["cover"].as_str().map(str::to_string);

            this.shared
                .duration
                .store(duration.to_bits(), Ordering::Relaxed);
            this.shared.has_track.store(true, Ordering::Relaxed);
            this.shared.playing.store(true, Ordering::Relaxed);
            let dur_text = images::format_time(duration);
            this.ui(move |w| {
                w.set_track_title(title.into());
                w.set_track_artist(artist.into());
                w.set_duration_text(dur_text.into());
                w.set_position_text("0:00".into());
                w.set_progress(0.0);
                w.set_has_track(true);
                w.set_playing(true);
                w.set_track_cover(Image::default());
            });

            if let Some(uuid) = cover {
                this.load_cover(state, images::tidal_image_url(&uuid, 320))
                    .await;
            }
        });
    }

    async fn load_cover(&self, state: Arc<AppState>, url: String) {
        let bytes = match utility::get_image_bytes(&state, url).await {
            Ok(b) => b,
            Err(e) => {
                tracing::debug!("cover fetch failed: {}", e.log_safe());
                return;
            }
        };
        // Decode off the async workers and off the UI thread.
        let buf = tokio::task::spawn_blocking(move || images::decode(&bytes))
            .await
            .ok()
            .flatten();
        if let Some(buf) = buf {
            self.ui(move |w| w.set_track_cover(Image::from_rgba8(buf)));
        }
    }

    pub fn toggle_play(&self) {
        if !self.shared.has_track.load(Ordering::Relaxed) {
            return;
        }
        let was_playing = self.shared.playing.load(Ordering::Relaxed);
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            let result = if was_playing {
                playback::pause_track(&state).await
            } else {
                playback::resume_track(&state).await
            };
            match result {
                Ok(()) => {
                    this.shared.playing.store(!was_playing, Ordering::Relaxed);
                    this.ui(move |w| w.set_playing(!was_playing));
                }
                Err(e) => {
                    let msg = e.log_safe();
                    this.ui(move |w| w.set_error(msg.into()));
                }
            }
        });
    }

    pub fn seek(&self, fraction: f32) {
        let dur = self.shared.duration();
        if dur <= 0.0 {
            return;
        }
        let secs = fraction.clamp(0.0, 1.0) * dur;
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            if let Err(e) = playback::seek_track(&state, secs).await {
                tracing::warn!("seek: {}", e.log_safe());
                return;
            }
            let text = images::format_time(secs);
            this.ui(move |w| {
                w.set_progress(fraction);
                w.set_position_text(text.into());
            });
        });
    }

    pub fn set_volume(&self, level: f32) {
        let level = level.clamp(0.0, 1.0);
        self.ui(move |w| w.set_volume(level));
        let Some(state) = self.state() else { return };
        // `set_volume` persists settings, which is disk I/O.
        self.rt.spawn_blocking(move || {
            if let Err(e) = playback::set_volume(&state, level) {
                tracing::warn!("set_volume: {}", e.log_safe());
            }
        });
    }
}
