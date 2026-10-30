//! Work the original app did once the window existed: apply saved audio modes,
//! start the optional servers, re-register scrobblers and flush retry queues.

use std::sync::atomic::Ordering;

use crate::AppHandle;

/// Call once after `AppState` is installed. Safe to call from any task.
pub async fn run_startup(app: &AppHandle) {
    let state = app.state::<crate::AppState>();

    crate::mcp::ensure_mcp_started(app).await;
    crate::overlay::ensure_overlay_started(app).await;

    // Saved audio modes reach the engine only once the pipeline exists.
    {
        let excl = state.exclusive_mode.load(Ordering::Relaxed);
        let bp = state.bit_perfect.load(Ordering::Relaxed);
        let dev = state
            .exclusive_device
            .lock()
            .map(|d| d.clone())
            .unwrap_or_default();
        if excl || bp {
            let _ = state.audio_player.set_exclusive_mode(excl, dev);
        }
        if bp {
            let _ = state.audio_player.set_bit_perfect(true);
        }
        let _ = state
            .audio_player
            .set_gapless(state.gapless.load(Ordering::Relaxed));
        if let Some(s) = state.load_settings() {
            state
                .audio_player
                .set_crossfade(s.crossfade_secs, s.crossfade_skip_same_album);
        }
    }

    #[cfg(target_os = "linux")]
    {
        let state = state.clone();
        std::thread::spawn(move || {
            if let Ok(devices) = crate::audio::list_alsa_devices() {
                if let Ok(mut cache) = state.cached_audio_devices.lock() {
                    *cache = Some(devices);
                }
            }
        });
    }

    if let Some(settings) = state.load_settings() {
        let http = state.proxied_http.clone();
        if let Some(creds) = &settings.scrobble.lastfm {
            if crate::embedded_lastfm::has_stream_keys() {
                let provider = crate::scrobble::lastfm::AudioscrobblerProvider::new(
                    "lastfm",
                    "https://ws.audioscrobbler.com/2.0/",
                    "https://www.last.fm/api/auth/",
                    crate::embedded_lastfm::stream_key_a(),
                    crate::embedded_lastfm::stream_key_b(),
                    http.clone(),
                );
                provider
                    .set_session(creds.session_key.clone(), creds.username.clone())
                    .await;
                state
                    .scrobble_manager
                    .add_provider(Box::new(provider))
                    .await;
                log::info!("Last.fm scrobbling enabled for {}", creds.username);
            }
        }
        if let Some(creds) = &settings.scrobble.librefm {
            if crate::embedded_librefm::has_stream_keys() {
                let provider = crate::scrobble::lastfm::AudioscrobblerProvider::new(
                    "librefm",
                    crate::scrobble::librefm::LIBREFM_API_URL,
                    "https://libre.fm/api/auth/",
                    crate::embedded_librefm::stream_key_a(),
                    crate::embedded_librefm::stream_key_b(),
                    http.clone(),
                );
                provider
                    .set_session(creds.session_key.clone(), creds.username.clone())
                    .await;
                state
                    .scrobble_manager
                    .add_provider(Box::new(provider))
                    .await;
                log::info!("Libre.fm scrobbling enabled for {}", creds.username);
            }
        }
        if let Some(creds) = &settings.scrobble.listenbrainz {
            let provider = crate::scrobble::listenbrainz::ListenBrainzProvider::new(http.clone());
            provider
                .set_token(creds.token.clone(), creds.username.clone())
                .await;
            state
                .scrobble_manager
                .add_provider(Box::new(provider))
                .await;
            log::info!("ListenBrainz scrobbling enabled for {}", creds.username);
        }
    }

    state.scrobble_manager.drain_queue().await;
    state.tidal_reporter.drain_queue().await;
}

/// A track ended on its own: let scrobbling and play reporting finalise it.
pub async fn on_track_finished(state: &crate::AppState) {
    state.scrobble_manager.try_scrobble_finished().await;
    state.tidal_reporter.try_finish().await;
}
