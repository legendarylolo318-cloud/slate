//! Glue between the Slint window and `slate-core`. Contains no business logic:
//! every method forwards to a core command on the tokio runtime and publishes
//! the result with `upgrade_in_event_loop`.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use slate_core::commands::{auth, playback, utility};
use slate_core::controller::{Controller, Outcome, PlayFailure};
use slate_core::queue::{Repeat, Track};
use slate_core::{AppHandle, AppState, EventSink, Events};
use slint::{ComponentHandle, Image, Weak};

use crate::bootstrap::{AppWindow, Screen};
use crate::control::{Control, ControlFn, PositionFn};
use crate::images;

/// Playback facts the ticker needs without touching the UI thread.
#[derive(Default)]
pub(crate) struct Shared {
    playing: AtomicBool,
    has_track: AtomicBool,
    /// Audio pipeline holds the current track (false for a restored, not yet started one).
    loaded: AtomicBool,
    shuffle: AtomicBool,
    /// Latest position in seconds, as `f32` bits.
    position: AtomicU32,
    controller: OnceLock<Arc<Controller>>,
    /// Set once the backend exists; invoked when a track ends by itself.
    on_finished: OnceLock<Box<dyn Fn() + Send + Sync>>,
    #[cfg(target_os = "linux")]
    tray: tokio::sync::Mutex<Option<crate::platform::tray::TrayHandle>>,
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
                // Queue advance decides whether playback continues.
                if let Some(cb) = self.shared.on_finished.get() {
                    cb();
                }
            }
            _ => {}
        }
    }
}

#[derive(Clone)]
pub struct Backend {
    pub(crate) rt: tokio::runtime::Handle,
    pub(crate) app: AppHandle,
    pub(crate) ui: Weak<AppWindow>,
    pub(crate) shared: Arc<Shared>,
    pub(crate) lib: Arc<crate::library::LibState>,
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
            lib: Arc::new(crate::library::LibState::default()),
        };
        let finished = backend.clone();
        let _ = backend
            .shared
            .on_finished
            .set(Box::new(move || finished.advance(false)));
        backend.init();
        backend.spawn_ticker();
        backend.install_platform(&backend.rt);
        backend
    }

    pub(crate) fn ui<F: FnOnce(AppWindow) + Send + 'static>(&self, f: F) {
        if self.ui.upgrade_in_event_loop(f).is_err() {
            tracing::debug!("UI already closed; dropping update");
        }
    }

    pub(crate) fn state(&self) -> Option<Arc<AppState>> {
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
                        w.set_login_status("Could not start the Slate core. See the log.".into());
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

            let controller = Arc::new(Controller::new(Arc::clone(&state)));
            let (shuffle, repeat, autoplay) = controller.load_modes().await;
            controller.set_modes(shuffle, repeat, autoplay).await;
            match controller.restore().await {
                Ok(Some(track)) => this.show_track(&track, false),
                Ok(None) => {}
                Err(e) => tracing::warn!("queue restore failed: {}", e.log_safe()),
            }
            let _ = this.shared.controller.set(Arc::clone(&controller));
            this.shared.shuffle.store(shuffle, Ordering::Relaxed);
            this.ui(move |w| {
                w.set_shuffle(shuffle);
                w.set_repeat(i32::from(repeat.as_u8()));
            });
            this.refresh_queue_info().await;

            match auth::load_saved_auth(&state).await {
                Ok(Some(_)) => {
                    this.ui(|w| w.set_screen(Screen::Main));
                    this.load_library();
                }
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
                this.shared.position.store(pos.to_bits(), Ordering::Relaxed);
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
                    self.load_library();
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
            this.shared.loaded.store(false, Ordering::Relaxed);
            if let Some(ctl) = this.controller() {
                ctl.clear_queue().await;
            }
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

    pub(crate) fn controller(&self) -> Option<Arc<Controller>> {
        self.shared.controller.get().cloned()
    }

    /// Publishes a controller outcome: UI, OS media surfaces, persistence.
    pub(crate) async fn apply(&self, outcome: Outcome) {
        match outcome {
            Outcome::Playing(track) => {
                self.shared.loaded.store(true, Ordering::Relaxed);
                self.show_track(&track, true);
            }
            Outcome::Restarted => {
                self.ui(|w| {
                    w.set_progress(0.0);
                    w.set_position_text("0:00".into());
                });
            }
            Outcome::Stopped | Outcome::GaveUp | Outcome::Nothing => {
                if matches!(outcome, Outcome::GaveUp) {
                    self.ui(|w| w.set_error("Several tracks in a row could not be played.".into()));
                }
                if !matches!(outcome, Outcome::Nothing) {
                    self.set_playing(false);
                }
            }
            Outcome::Failed(f) => {
                let msg = match f {
                    PlayFailure::Unplayable => "Track unavailable.".to_string(),
                    PlayFailure::Blocked(r) => r,
                    PlayFailure::Network => "Network error. Check your connection.".to_string(),
                    PlayFailure::RateLimited => {
                        "TIDAL is rate-limiting requests; try again shortly.".to_string()
                    }
                    PlayFailure::Transient(m) => format!("Playback failed: {m}"),
                };
                self.set_playing(false);
                self.ui(move |w| w.set_error(msg.into()));
            }
        }
        self.refresh_queue_info().await;
        self.save().await;
    }

    fn set_playing(&self, playing: bool) {
        self.shared.playing.store(playing, Ordering::Relaxed);
        self.ui(move |w| w.set_playing(playing));
        if let Some(state) = self.state() {
            let pos = f32::from_bits(self.shared.position.load(Ordering::Relaxed));
            let _ = playback::update_mpris_playback_status(&state, playing, Some(f64::from(pos)));
        }
    }

    /// Fills the player bar and OS metadata for `track`.
    fn show_track(&self, track: &Track, playing: bool) {
        let id = track.id.to_string();
        let title = track.title().to_string();
        let artist = track.artist_name().to_string();
        let duration = track.duration_secs();
        let cover = track.cover_id().map(str::to_string);
        self.shared
            .duration
            .store(duration.to_bits(), Ordering::Relaxed);
        self.shared.position.store(0, Ordering::Relaxed);
        self.shared.has_track.store(true, Ordering::Relaxed);
        self.shared.playing.store(playing, Ordering::Relaxed);

        if let Some(state) = self.state() {
            let album = track
                .extra
                .get("album")
                .and_then(|a| a.get("title"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let meta = playback::MprisMetadata {
                track_id: track.id,
                title: title.clone(),
                artist: artist.clone(),
                album,
                art_url: cover
                    .as_deref()
                    .map(|c| images::tidal_image_url(c, 640))
                    .unwrap_or_default(),
                duration_secs: f64::from(duration),
                url: format!("https://tidal.com/track/{}", track.id),
                quality_text: String::new(),
                album_artist: None,
                track_number: None,
                disc_number: None,
                content_created: None,
                user_rating: None,
            };
            let _ = playback::update_mpris_metadata(&state, meta);
            let _ = playback::update_mpris_playback_status(&state, playing, Some(0.0));
        }
        #[cfg(target_os = "linux")]
        {
            let shared = Arc::clone(&self.shared);
            let tip = if artist.is_empty() {
                title.clone()
            } else {
                format!("{title} — {artist}")
            };
            self.rt.spawn(async move {
                if let Some(t) = shared.tray.lock().await.as_ref() {
                    t.set_tooltip(tip).await;
                }
            });
        }

        let dur_text = images::format_time(duration);
        self.ui(move |w| {
            w.set_playing_id(id.into());
            w.set_track_title(title.into());
            w.set_track_artist(artist.into());
            w.set_duration_text(dur_text.into());
            w.set_position_text("0:00".into());
            w.set_progress(0.0);
            w.set_has_track(true);
            w.set_playing(playing);
            w.set_track_cover(Image::default());
        });
        if let (Some(uuid), Some(state)) = (cover, self.state()) {
            let this = self.clone();
            self.rt.spawn(async move {
                this.load_cover(state, images::tidal_image_url(&uuid, 320))
                    .await;
            });
        }
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

    async fn refresh_queue_info(&self) {
        let Some(ctl) = self.controller() else { return };
        let (count, next) = ctl
            .with_queue(|q| {
                (
                    q.manual.len() + q.queue.len(),
                    q.upcoming()
                        .next()
                        .map(|t| format!("{} — {}", t.title(), t.artist_name())),
                )
            })
            .await;
        self.ui(move |w| {
            w.set_queue_count(i32::try_from(count).unwrap_or(i32::MAX));
            w.set_up_next(next.unwrap_or_default().into());
        });
    }

    /// Queue + modes to disk, off the UI thread.
    async fn save(&self) {
        let Some(ctl) = self.controller() else { return };
        if let Err(e) = ctl.save().await {
            tracing::warn!("could not save queue: {}", e.log_safe());
        }
        if let Err(e) = ctl.save_modes().await {
            tracing::warn!("could not save playback modes: {}", e.log_safe());
        }
    }

    pub fn next(&self) {
        self.advance(true);
    }

    fn advance(&self, explicit: bool) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(ctl) = this.controller() else { return };
            let outcome = ctl.next(explicit).await;
            this.apply(outcome).await;
        });
    }

    pub fn previous(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(ctl) = this.controller() else { return };
            let outcome = ctl.previous().await;
            this.apply(outcome).await;
        });
    }

    pub fn toggle_play(&self) {
        if !self.shared.has_track.load(Ordering::Relaxed) {
            return;
        }
        let this = self.clone();
        self.rt.spawn(async move {
            let (Some(state), Some(ctl)) = (this.state(), this.controller()) else {
                return;
            };
            // A restored track has no pipeline yet: start it instead of resuming.
            if !this.shared.loaded.load(Ordering::Relaxed) {
                let outcome = ctl.resume_current().await;
                this.apply(outcome).await;
                return;
            }
            let was_playing = this.shared.playing.load(Ordering::Relaxed);
            let result = if was_playing {
                playback::pause_track(&state).await
            } else {
                playback::resume_track(&state).await
            };
            match result {
                Ok(()) => this.set_playing(!was_playing),
                Err(e) => {
                    let msg = e.log_safe();
                    this.ui(move |w| w.set_error(msg.into()));
                }
            }
        });
    }

    pub fn toggle_shuffle(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(ctl) = this.controller() else { return };
            let on = ctl.toggle_shuffle().await;
            this.shared.shuffle.store(on, Ordering::Relaxed);
            this.ui(move |w| w.set_shuffle(on));
            if let Some(state) = this.state() {
                let _ = playback::update_mpris_shuffle(&state, on);
            }
            this.refresh_queue_info().await;
            this.save().await;
        });
    }

    pub fn cycle_repeat(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(ctl) = this.controller() else { return };
            let mode = ctl.cycle_repeat().await;
            this.publish_repeat(mode);
            this.save().await;
        });
    }

    fn publish_repeat(&self, mode: Repeat) {
        self.ui(move |w| w.set_repeat(i32::from(mode.as_u8())));
        if let Some(state) = self.state() {
            let _ = playback::update_mpris_loop_status(&state, mode.as_u8());
        }
    }

    pub fn seek(&self, fraction: f32) {
        let dur = self.shared.duration();
        if dur > 0.0 {
            self.seek_to_secs(fraction.clamp(0.0, 1.0) * dur);
        }
    }

    fn seek_to_secs(&self, secs: f32) {
        let secs = secs.max(0.0);
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            if !this.shared.loaded.load(Ordering::Relaxed) {
                return;
            }
            if let Err(e) = playback::seek_track(&state, secs).await {
                tracing::warn!("seek: {}", e.log_safe());
                return;
            }
            this.shared
                .position
                .store(secs.to_bits(), Ordering::Relaxed);
            let dur = this.shared.duration();
            let text = images::format_time(secs);
            this.ui(move |w| {
                w.set_progress(if dur > 0.0 {
                    (secs / dur).clamp(0.0, 1.0)
                } else {
                    0.0
                });
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

    // ---- external control (media keys, MPRIS, tray) ----

    pub fn control_fn(&self) -> ControlFn {
        let this = self.clone();
        Arc::new(move |c| this.handle_control(c))
    }

    pub fn position_fn(&self) -> PositionFn {
        let shared = Arc::clone(&self.shared);
        Arc::new(move || {
            shared
                .has_track
                .load(Ordering::Relaxed)
                .then(|| f32::from_bits(shared.position.load(Ordering::Relaxed)))
        })
    }

    fn handle_control(&self, c: Control) {
        let playing = self.shared.playing.load(Ordering::Relaxed);
        let pos = f32::from_bits(self.shared.position.load(Ordering::Relaxed));
        match c {
            Control::Play if !playing => self.toggle_play(),
            Control::Pause if playing => self.toggle_play(),
            Control::PlayPause => self.toggle_play(),
            Control::Play | Control::Pause => {}
            Control::Stop => {
                let this = self.clone();
                self.rt.spawn(async move {
                    if let Some(state) = this.state() {
                        let _ = playback::stop_track(&state).await;
                    }
                    this.shared.loaded.store(false, Ordering::Relaxed);
                    this.set_playing(false);
                });
            }
            Control::Next => self.next(),
            Control::Previous => self.previous(),
            Control::SeekRelative(off) => self.seek_to_secs(pos + off as f32),
            Control::SetPosition(secs) => self.seek_to_secs(secs as f32),
            Control::SetVolume(v) => self.set_volume(v as f32),
            Control::SetShuffle(want) => {
                if self.ui_shuffle() != want {
                    self.toggle_shuffle();
                }
            }
            Control::SetLoop(mode) => {
                let this = self.clone();
                self.rt.spawn(async move {
                    let Some(ctl) = this.controller() else { return };
                    let target = Repeat::from_u8(mode);
                    while ctl.cycle_repeat().await != target {}
                    this.publish_repeat(target);
                    this.save().await;
                });
            }
            Control::Raise => self.ui(|w| {
                let _ = w.show();
            }),
            Control::Quit => {
                let _ = slint::quit_event_loop();
            }
        }
    }

    fn ui_shuffle(&self) -> bool {
        self.shared.shuffle.load(Ordering::Relaxed)
    }

    /// Wires the OS integrations once the core is up.
    pub fn install_platform(&self, rt: &tokio::runtime::Handle) {
        let this = self.clone();
        let rt = rt.clone();
        self.rt.spawn(async move {
            // Wait for AppState; the integrations need the media hub.
            let state = loop {
                if let Some(s) = this.state() {
                    break s;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            };
            let _ = &rt;
            #[cfg(target_os = "linux")]
            {
                let sink = crate::platform::mpris::start(this.control_fn(), this.position_fn());
                state.media.set_sink(sink);
                if let Some(handle) = crate::platform::tray::start(this.control_fn()).await {
                    *this.shared.tray.lock().await = Some(handle);
                }
            }
            #[cfg(not(target_os = "linux"))]
            let _ = state;
        });
    }

    /// Close button: hide to the tray when enabled and present, else quit.
    pub fn close_requested(&self) -> slint::CloseRequestResponse {
        let to_tray = self
            .state()
            .is_some_and(|s| s.minimize_to_tray.load(Ordering::Relaxed))
            && self.tray_present();
        if !to_tray {
            let _ = slint::quit_event_loop();
        }
        slint::CloseRequestResponse::HideWindow
    }

    fn tray_present(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.shared
                .tray
                .try_lock()
                .map(|g| g.is_some())
                .unwrap_or(false)
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }

    /// Stops audio and flushes the queue; called once on shutdown.
    pub fn shutdown(&self) {
        let this = self.clone();
        self.rt.block_on(async move {
            this.save().await;
            if let Some(state) = this.state() {
                let _ = playback::stop_track(&state).await;
            }
        });
    }
}
