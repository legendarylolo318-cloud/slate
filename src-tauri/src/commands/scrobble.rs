use serde::Deserialize;
use tauri::State;

use crate::scrobble::listenbrainz::ListenBrainzProvider;
use crate::scrobble::{ProviderStatus, ScrobbleTrack};
use crate::{AppState, ListenBrainzCredentials, SoneError};

// ---------------------------------------------------------------------------
// Payload types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackStartedPayload {
    pub artist: String,
    pub title: String,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub duration_secs: u32,
    pub track_number: Option<u32>,
    pub chosen_by_user: bool,
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[tauri::command(rename_all = "camelCase")]
pub async fn notify_track_started(
    state: State<'_, AppState>,
    payload: TrackStartedPayload,
) -> Result<(), SoneError> {
    let track = ScrobbleTrack {
        artist: payload.artist,
        track: payload.title,
        album: payload.album,
        album_artist: payload.album_artist,
        duration_secs: payload.duration_secs,
        track_number: payload.track_number,
        timestamp: crate::now_secs() as i64,
        chosen_by_user: payload.chosen_by_user,
    };
    state.scrobble_manager.on_track_started(track).await;
    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn notify_track_paused(state: State<'_, AppState>) -> Result<(), SoneError> {
    state.scrobble_manager.on_pause().await;
    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn notify_track_resumed(state: State<'_, AppState>) -> Result<(), SoneError> {
    state.scrobble_manager.on_resume().await;
    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn notify_track_seeked(state: State<'_, AppState>) -> Result<(), SoneError> {
    state.scrobble_manager.on_seek().await;
    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn notify_track_stopped(state: State<'_, AppState>) -> Result<(), SoneError> {
    state.scrobble_manager.on_track_finished().await;
    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn get_scrobble_status(
    state: State<'_, AppState>,
) -> Result<Vec<ProviderStatus>, SoneError> {
    Ok(state.scrobble_manager.provider_statuses().await)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn get_scrobble_queue_size(state: State<'_, AppState>) -> Result<usize, SoneError> {
    Ok(state.scrobble_manager.queue_size().await)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn connect_listenbrainz(
    state: State<'_, AppState>,
    token: String,
) -> Result<String, SoneError> {
    let username = ListenBrainzProvider::validate_token(&token).await?;

    // Save credentials to settings
    if let Some(mut settings) = state.load_settings() {
        settings.scrobble.listenbrainz = Some(ListenBrainzCredentials {
            token,
            username: username.clone(),
        });
        state.save_settings(&settings)?;
    }

    Ok(username)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn disconnect_provider(
    state: State<'_, AppState>,
    provider: String,
) -> Result<(), SoneError> {
    // Clear credentials from settings
    if let Some(mut settings) = state.load_settings() {
        match provider.as_str() {
            "lastfm" => settings.scrobble.lastfm = None,
            "listenbrainz" => settings.scrobble.listenbrainz = None,
            "librefm" => settings.scrobble.librefm = None,
            _ => {
                return Err(SoneError::Scrobble(format!(
                    "unknown provider: {provider}"
                )));
            }
        }
        state.save_settings(&settings)?;
    }

    state.scrobble_manager.remove_provider(&provider).await;
    Ok(())
}
