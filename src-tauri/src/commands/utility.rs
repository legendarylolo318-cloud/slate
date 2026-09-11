use std::sync::atomic::Ordering;
use tauri::{Manager, State};

use super::playback::compute_norm_gain;
use crate::audio::AudioDevice;
use crate::cache::{CacheResult, CacheTier};
use crate::AppState;
use crate::SignalPath;
use crate::SoneError;

/// Open the SONE log directory (`~/.config/sone/logs`) in the system file
/// manager, creating it if it does not exist yet.
#[tauri::command]
pub fn open_log_folder() -> Result<(), String> {
    let dir = dirs::config_dir()
        .map(|d| d.join("sone").join("logs"))
        .ok_or_else(|| "could not resolve config directory".to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::process::Command::new("xdg-open")
        .arg(&dir)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("failed to launch file manager: {e}"))
}

#[tauri::command]
pub fn get_signal_path(state: State<'_, AppState>) -> SignalPath {
    state.signal_path.snapshot()
}

#[tauri::command]
pub fn refresh_signal_path(state: State<'_, AppState>) -> SignalPath {
    state.pipeline_probe.refresh();
    state.signal_path.snapshot()
}

#[tauri::command]
pub async fn update_tray_tooltip(app: tauri::AppHandle, text: String) -> Result<String, SoneError> {
    #[cfg(target_os = "linux")]
    if let Some(tray_handle) = app.try_state::<crate::tray::TrayHandle>() {
        tray_handle.inner().update_tooltip(text).await;
        return Ok("updated".into());
    }
    Ok("tray not available".into())
}

#[tauri::command]
pub async fn get_image_bytes(
    state: State<'_, AppState>,
    url: String,
) -> Result<tauri::ipc::Response, SoneError> {
    log::debug!("[get_image_bytes]: url={}", url);

    match state.disk_cache.get(&url, CacheTier::Image).await {
        CacheResult::Fresh(bytes) | CacheResult::Stale(bytes) => {
            log::debug!("[get_image_bytes]: cache hit ({} bytes)", bytes.len());
            Ok(tauri::ipc::Response::new(bytes))
        }
        CacheResult::Miss => {
            let http_client = state
                .proxied_http
                .client()
                .map_err(|e| SoneError::ProxyBlocked { reason: e.cause })?;
            let res = http_client.get(&url).send().await?;
            let bytes = res.bytes().await?.to_vec();

            state
                .disk_cache
                .put(&url, &bytes, CacheTier::Image, &["image"])
                .await
                .ok();
            log::debug!(
                "[get_image_bytes]: fetched and cached {} bytes",
                bytes.len()
            );

            Ok(tauri::ipc::Response::new(bytes))
        }
    }
}

#[tauri::command]
pub async fn get_cache_stats(
    state: State<'_, AppState>,
) -> Result<crate::cache::CacheStats, SoneError> {
    Ok(state.disk_cache.stats().await)
}

#[tauri::command]
pub async fn clear_disk_cache(state: State<'_, AppState>) -> Result<(), SoneError> {
    log::info!("[clear_disk_cache]: user-initiated cache clear");
    state.disk_cache.clear().await;
    Ok(())
}

#[tauri::command]
pub fn get_decorations(state: State<'_, AppState>) -> bool {
    state.decorations.load(Ordering::Relaxed)
}

#[tauri::command]
pub fn set_decorations(
    window: tauri::Window,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<(), SoneError> {
    state.decorations.store(enabled, Ordering::Relaxed);
    window.set_decorations(enabled).map_err(SoneError::from)?;
    let mut settings = state.load_settings().unwrap_or_default();
    settings.decorations = enabled;
    state.save_settings(&settings)?;
    Ok(())
}

#[tauri::command]
pub fn get_minimize_to_tray(state: State<'_, AppState>) -> bool {
    state.minimize_to_tray.load(Ordering::Relaxed)
}

#[tauri::command]
pub fn set_minimize_to_tray(state: State<'_, AppState>, enabled: bool) -> Result<(), SoneError> {
    state.minimize_to_tray.store(enabled, Ordering::Relaxed);
    let mut settings = state.load_settings().unwrap_or_default();
    settings.minimize_to_tray = enabled;
    state.save_settings(&settings)?;
    Ok(())
}

#[tauri::command]
pub fn get_volume_normalization(state: State<'_, AppState>) -> bool {
    state.volume_normalization.load(Ordering::Relaxed)
}

#[tauri::command]
pub fn set_volume_normalization(
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<(), SoneError> {
    state.volume_normalization.store(enabled, Ordering::Relaxed);

    // Immediately apply/reset normalization on the current track
    let norm_gain = if enabled {
        let rg = f64::from_bits(state.last_replay_gain.load(Ordering::Relaxed));
        let peak = f64::from_bits(state.last_peak_amplitude.load(Ordering::Relaxed));
        let rg_opt = if rg.is_finite() { Some(rg) } else { None };
        let peak_opt = if peak.is_finite() { Some(peak) } else { None };
        compute_norm_gain(rg_opt, peak_opt)
    } else {
        1.0
    };
    state
        .audio_player
        .set_normalization_gain(norm_gain)
        .map_err(SoneError::Audio)?;
    state.signal_path.set_normalization_enabled(enabled);
    let mut settings = state.load_settings().unwrap_or_default();
    settings.volume_normalization = enabled;
    state.save_settings(&settings)?;
    Ok(())
}

#[tauri::command]
pub fn get_exclusive_mode(state: State<'_, AppState>) -> bool {
    state.exclusive_mode.load(Ordering::Relaxed)
}

#[tauri::command]
pub fn set_exclusive_mode(state: State<'_, AppState>, enabled: bool) -> Result<(), SoneError> {
    state.exclusive_mode.store(enabled, Ordering::Relaxed);

    if !enabled {
        state.bit_perfect.store(false, Ordering::Relaxed);
        state
            .audio_player
            .set_bit_perfect(false)
            .map_err(SoneError::Audio)?;
    }

    let device = state.exclusive_device.lock().unwrap().clone();
    state
        .audio_player
        .set_exclusive_mode(enabled, device)
        .map_err(SoneError::Audio)?;

    let mut settings = state.load_settings().unwrap_or_default();
    settings.exclusive_mode = enabled;
    if !enabled {
        settings.bit_perfect = false;
    }
    state.save_settings(&settings)?;
    Ok(())
}

#[tauri::command]
pub fn get_bit_perfect(state: State<'_, AppState>) -> bool {
    state.bit_perfect.load(Ordering::Relaxed)
}

#[tauri::command]
pub fn set_bit_perfect(state: State<'_, AppState>, enabled: bool) -> Result<(), SoneError> {
    state.bit_perfect.store(enabled, Ordering::Relaxed);

    if enabled && !state.exclusive_mode.load(Ordering::Relaxed) {
        state.exclusive_mode.store(true, Ordering::Relaxed);
        let device = state.exclusive_device.lock().unwrap().clone();
        state
            .audio_player
            .set_exclusive_mode(true, device)
            .map_err(SoneError::Audio)?;
    }

    state
        .audio_player
        .set_bit_perfect(enabled)
        .map_err(SoneError::Audio)?;

    let mut settings = state.load_settings().unwrap_or_default();
    settings.bit_perfect = enabled;
    if enabled {
        settings.exclusive_mode = true;
    }
    state.save_settings(&settings)?;
    Ok(())
}

#[tauri::command]
pub fn get_gapless(state: State<'_, AppState>) -> bool {
    state.gapless.load(Ordering::Relaxed)
}

#[tauri::command]
pub fn get_gapless_supported() -> bool {
    crate::audio::gapless_supported()
}

#[tauri::command]
pub fn set_gapless(state: State<'_, AppState>, enabled: bool) -> Result<(), SoneError> {
    state.gapless.store(enabled, Ordering::Relaxed);
    state
        .audio_player
        .set_gapless(enabled)
        .map_err(SoneError::Audio)?;
    let mut settings = state.load_settings().unwrap_or_default();
    settings.gapless = enabled;
    state.save_settings(&settings)?;
    Ok(())
}

#[tauri::command]
pub fn get_max_quality(state: State<'_, AppState>) -> String {
    state.max_quality.lock().unwrap().clone()
}

#[tauri::command]
pub fn set_max_quality(state: State<'_, AppState>, quality: String) -> Result<(), SoneError> {
    if !matches!(quality.as_str(), "HI_RES_LOSSLESS" | "LOSSLESS" | "HIGH") {
        return Err(SoneError::Parse(format!("invalid max_quality: {quality}")));
    }
    *state.max_quality.lock().unwrap() = quality.clone();
    let mut settings = state.load_settings().unwrap_or_default();
    settings.max_quality = quality;
    state.save_settings(&settings)?;
    Ok(())
}

#[tauri::command]
pub fn get_exclusive_device(state: State<'_, AppState>) -> Option<String> {
    state.exclusive_device.lock().unwrap().clone()
}

#[tauri::command]
pub fn set_exclusive_device(state: State<'_, AppState>, device: String) -> Result<(), SoneError> {
    *state.exclusive_device.lock().unwrap() = Some(device.clone());

    let enabled = state.exclusive_mode.load(Ordering::Relaxed);
    state
        .audio_player
        .set_exclusive_mode(enabled, Some(device.clone()))
        .map_err(SoneError::Audio)?;
    let mut settings = state.load_settings().unwrap_or_default();
    settings.exclusive_device = Some(device);
    state.save_settings(&settings)?;
    Ok(())
}

#[tauri::command]
pub fn list_audio_devices(state: State<'_, AppState>) -> Result<Vec<AudioDevice>, SoneError> {
    // Return cached devices if available (avoids slow GStreamer DeviceMonitor probe)
    let cached = state.cached_audio_devices.lock().unwrap().clone();
    if let Some(devices) = cached {
        return Ok(devices);
    }

    // First call: probe directly (not via audio thread) and cache
    let devices = crate::audio::list_alsa_devices().map_err(SoneError::Audio)?;
    *state.cached_audio_devices.lock().unwrap() = Some(devices.clone());
    Ok(devices)
}

#[tauri::command]
pub fn get_discord_rpc(state: State<'_, AppState>) -> bool {
    state
        .load_settings()
        .map(|s| s.discord_rpc)
        .unwrap_or(false)
}

#[tauri::command]
pub fn set_discord_rpc(state: State<'_, AppState>, enabled: bool) -> Result<(), SoneError> {
    if enabled {
        state
            .discord
            .send(crate::discord::DiscordCommand::Connect);
    } else {
        state
            .discord
            .send(crate::discord::DiscordCommand::Disconnect);
    }
    let mut settings = state.load_settings().unwrap_or_default();
    settings.discord_rpc = enabled;
    state.save_settings(&settings)?;
    Ok(())
}

#[tauri::command]
pub fn get_report_plays(state: State<'_, AppState>) -> bool {
    state.load_settings().map(|s| s.report_plays).unwrap_or(true)
}

#[tauri::command]
pub async fn set_report_plays(state: State<'_, AppState>, enabled: bool) -> Result<(), SoneError> {
    // Persist first: if the write fails the caller sees an error and the
    // in-memory state still matches disk. Reversing this order can silently
    // discard a user's opt-out.
    let mut settings = state.load_settings().unwrap_or_default();
    settings.report_plays = enabled;
    state.save_settings(&settings)?;

    state.tidal_reporter.set_enabled(enabled);
    if enabled {
        // Flush any offline backlog now that reporting is on.
        state.tidal_reporter.drain_queue().await;
    } else {
        // Drop the in-flight session: every lifecycle hook is gated on
        // `enabled`, so an orphaned session would keep accruing wall-clock time
        // and get reported on the next enable.
        state.tidal_reporter.clear_session().await;
    }
    Ok(())
}

#[tauri::command]
pub fn get_discord_status_text(state: State<'_, AppState>) -> String {
    state
        .load_settings()
        .map(|s| s.discord_status_text)
        .unwrap_or_default()
}

#[tauri::command]
pub fn set_discord_status_text(state: State<'_, AppState>, text: String) -> Result<(), SoneError> {
    state
        .discord
        .send(crate::discord::DiscordCommand::SetStatusText { text: text.clone() });

    let mut settings = state.load_settings().unwrap_or_default();
    settings.discord_status_text = text;
    state.save_settings(&settings)?;
    Ok(())
}

#[tauri::command]
pub fn get_proxy_settings(state: State<'_, AppState>) -> crate::ProxySettings {
    state
        .load_settings()
        .map(|s| s.proxy)
        .unwrap_or_default()
}

#[tauri::command]
pub async fn set_proxy_settings(
    state: State<'_, AppState>,
    settings: crate::ProxySettings,
) -> Result<(), SoneError> {
    let caps = crate::proxy::HostCaps::assume_all_present();

    // Swapping the one shared cell is the whole update: every reqwest consumer
    // reads through it, so there is nothing left to push out to them. `apply`
    // is the single place that decides what unplannable settings mean, so this
    // cannot drift back to a direct connection independently of startup.
    // It is blocking (`build_client` may resolve the proxy host), hence the
    // detour off the runtime worker.
    let http = state.proxied_http.clone();
    let candidate = settings.clone();
    tokio::task::spawn_blocking(move || http.apply(&candidate, &caps))
        .await
        .map_err(|e| SoneError::Io(format!("proxy update task failed: {e}")))?;

    // Apply proxy changes to future GStreamer HTTP sources without disrupting
    // the currently playing pipeline.
    state.audio_player.set_proxy_settings(settings.clone());

    // Save to disk
    let mut app_settings = state.load_settings().unwrap_or_default();
    app_settings.proxy = settings;
    state.save_settings(&app_settings)?;
    Ok(())
}

#[tauri::command]
pub async fn inhibit_idle(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
) -> Result<(), SoneError> {
    state.idle_inhibitor.lock().await.inhibit(&window).await;
    Ok(())
}

#[tauri::command]
pub async fn uninhibit_idle(state: State<'_, AppState>) -> Result<(), SoneError> {
    state.idle_inhibitor.lock().await.uninhibit().await;
    Ok(())
}

/// The client the "Test connection" button must use, or the reason there is
/// none. Split out from the command so the refusals can be tested without
/// touching the network.
///
/// Three things this must never do, because the banner reads any `Ok` as a
/// green success: test the *saved* proxy instead of the candidate one, build a
/// client by any path other than the one the app itself uses, or hand back a
/// direct connection. `ProxyPlan::Direct` is a refusal here — a direct
/// connection always "succeeds", and reporting that as a working proxy is the
/// exact lie this function used to tell.
fn proxy_test_client(
    settings: &crate::ProxySettings,
    caps: &crate::proxy::HostCaps,
) -> Result<reqwest::Client, String> {
    let plan = crate::proxy::plan(settings, caps).map_err(|e| e.to_string())?;
    if plan == crate::proxy::ProxyPlan::Direct {
        return Err("No proxy configured — enable one before testing".to_string());
    }
    crate::proxy_http::build_client(&plan, caps).map_err(|e| e.cause)
}

/// Probe the settings the user is editing — NOT the live cell, which still
/// holds the saved proxy.
#[tauri::command]
pub async fn test_proxy_connection(settings: crate::ProxySettings) -> Result<String, String> {
    let caps = crate::proxy::HostCaps::assume_all_present();
    // `build_client` resolves the proxy host for SOCKS5; keep that off the
    // runtime worker.
    let client = tokio::task::spawn_blocking(move || proxy_test_client(&settings, &caps))
        .await
        .map_err(|e| format!("proxy test task failed: {e}"))??;

    match client
        .get("https://api.tidal.com/v1/ping")
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() || status.as_u16() == 404 || status.as_u16() == 401 {
                Ok("Connection successful".to_string())
            } else {
                Ok(format!("Tidal responded with status {status}"))
            }
        }
        Err(e) => Err(format!("Connection failed: {e}")),
    }
}

fn logging_toggle_path() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|d| d.join("sone").join("logging.toggle"))
}

#[tauri::command]
pub fn get_enable_logging() -> bool {
    let Some(path) = logging_toggle_path() else {
        return true;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return true;
    };
    match text.trim() {
        "false" => false,
        _ => true,
    }
}

#[tauri::command]
pub fn set_enable_logging(enabled: bool) -> Result<(), SoneError> {
    let Some(path) = logging_toggle_path() else {
        return Err(SoneError::Io("Could not resolve config dir".into()));
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| SoneError::Io(format!("Failed to create config dir: {e}")))?;
    }
    let body = if enabled { "true" } else { "false" };
    std::fs::write(&path, body)
        .map_err(|e| SoneError::Io(format!("Failed to write logging toggle: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ProxySettings, ProxyType};

    fn caps() -> crate::proxy::HostCaps {
        crate::proxy::HostCaps::assume_all_present()
    }

    fn enabled(proxy_type: ProxyType, host: &str, port: u16) -> ProxySettings {
        ProxySettings {
            enabled: true,
            proxy_type,
            host: host.to_string(),
            port,
            username: None,
            password: None,
        }
    }

    /// The banner paints any `Ok` green, so "cannot report success" means the
    /// command must not return `Ok` at all for settings that would go direct.
    #[tokio::test]
    async fn a_blocked_plan_can_never_report_a_successful_connection() {
        // SOCKS5 resolves the proxy host at build time, so this blocks before a
        // single byte leaves the process — no network is touched by this test.
        let blocked = enabled(ProxyType::Socks5, "no-such-host.invalid", 3128);
        let err = test_proxy_connection(blocked)
            .await
            .expect_err("an unresolvable proxy must not report success");
        assert!(
            err.contains("no-such-host.invalid"),
            "the block must name its cause, got: {err}"
        );
    }

    #[tokio::test]
    async fn settings_that_do_not_form_a_plan_are_refused_not_tested_directly() {
        // Each of these used to reach `build_http_client`, which silently
        // returned a proxy-LESS client — so the request went out direct and the
        // banner said "Connection successful".
        //
        // Asserting the specific refusal, not merely the absence of the word
        // "successful": this function can also return `Ok("… responded with
        // status …")`, which contains neither, so a weaker assertion would pass
        // on a request that actually went out.
        for (bad, expected) in [
            (
                enabled(ProxyType::Http, "127.0.0.1", 0),
                "proxy port must not be 0",
            ),
            (
                enabled(ProxyType::Http, "proxy.local:3128", 3128),
                "enter the host without a port; use the port field",
            ),
            (
                enabled(ProxyType::Http, "ho st", 3128),
                "invalid proxy host: ho st",
            ),
            (
                enabled(ProxyType::Http, "[::1]", 3128),
                "enter an IPv6 address without brackets",
            ),
            (
                enabled(ProxyType::Http, "", 3128),
                "invalid proxy host: ",
            ),
            (
                enabled(ProxyType::Http, "пример.рф", 3128),
                "proxy host must be ASCII",
            ),
        ] {
            let err = test_proxy_connection(bad.clone())
                .await
                .expect_err("unplannable settings must never reach the network");
            assert_eq!(
                err, expected,
                "{bad:?} must be refused with its own reason"
            );
            assert!(
                proxy_test_client(&bad, &caps()).is_err(),
                "unplannable settings {bad:?} must yield no client"
            );
        }
    }

    #[tokio::test]
    async fn a_disabled_proxy_is_refused_rather_than_tested_as_a_direct_connection() {
        // `plan()` maps a disabled proxy to `Direct`, whose client works fine.
        // Sending through it would always succeed and paint the banner green
        // for a connection that is not proxied at all.
        let mut off = enabled(ProxyType::Http, "127.0.0.1", 3128);
        off.enabled = false;
        assert!(matches!(
            crate::proxy::plan(&off, &caps()),
            Ok(crate::proxy::ProxyPlan::Direct)
        ));
        let err = test_proxy_connection(off).await.expect_err(
            "a direct connection must never be reported as a working proxy",
        );
        assert!(err.contains("No proxy configured"), "got: {err}");
    }

    #[test]
    fn a_usable_proxy_still_yields_a_client_to_test_with() {
        // The guard above must not have closed off the case the button exists
        // for. Built only — never sent, so no network.
        let good = enabled(ProxyType::Http, "127.0.0.1", 3128);
        let c = proxy_test_client(&good, &caps()).expect("a valid proxy must be testable");
        assert!(
            format!("{c:?}").contains("All(http://127.0.0.1:3128)"),
            "the test must go through the proxy it is testing: {c:?}"
        );
    }
}
