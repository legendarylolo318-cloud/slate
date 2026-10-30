use serde::Serialize;

use crate::error::SlateError;
use crate::mcp::{NowPlayingSnapshot, QueueTrackSnapshot};
use crate::AppState;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpConnectionInfo {
    pub enabled: bool,
    pub url: Option<String>,
    pub port: Option<u16>,
}

pub async fn mcp_get_connection_info(state: &AppState) -> Result<McpConnectionInfo, SlateError> {
    let handle = state.mcp_handle.lock().await;
    Ok(match handle.as_ref() {
        Some(h) => McpConnectionInfo {
            enabled: true,
            url: Some(h.url()),
            port: Some(h.port),
        },
        None => McpConnectionInfo {
            enabled: false,
            url: None,
            port: None,
        },
    })
}

pub async fn mcp_publish_state(
    state: &AppState,
    now_playing: Option<NowPlayingSnapshot>,
    queue: Option<Vec<QueueTrackSnapshot>>,
) -> Result<(), SlateError> {
    let mut s = state.mcp_state.write().await;
    if let Some(np) = now_playing {
        s.now_playing = Some(np);
    }
    if let Some(q) = queue {
        s.queue = q;
    }
    Ok(())
}

pub async fn mcp_set_enabled(
    state: &AppState,
    app_handle: crate::AppHandle,
    enabled: bool,
) -> Result<McpConnectionInfo, SlateError> {
    let mut settings = state.load_settings().unwrap_or_default();

    settings.mcp_enabled = enabled;

    if enabled && settings.mcp_token.is_empty() {
        settings.mcp_token = uuid::Uuid::new_v4().simple().to_string();
    }

    {
        // Hold the guard across stop→bind→store so this cannot race the
        // startup spawn (or a concurrent regenerate) into a double bind.
        let mut guard = state.mcp_handle.lock().await;
        if let Some(handle) = guard.take() {
            handle.shutdown().await;
        }
        if enabled {
            let h = crate::mcp::start_server(
                app_handle.clone(),
                settings.mcp_port,
                settings.mcp_token.clone(),
            )
            .await?;
            *guard = Some(h);
        }
    }
    // Persist only after the server matches — a failed enable must not
    // stick across launches.
    state.save_settings(&settings)?;

    mcp_get_connection_info(state).await
}

pub async fn mcp_regenerate_token(
    state: &AppState,
    app_handle: crate::AppHandle,
) -> Result<McpConnectionInfo, SlateError> {
    let old_settings = state.load_settings().unwrap_or_default();
    let mut settings = old_settings.clone();
    settings.mcp_token = uuid::Uuid::new_v4().simple().to_string();

    {
        let mut guard = state.mcp_handle.lock().await;
        if let Some(handle) = guard.take() {
            handle.shutdown().await;
        }
        if settings.mcp_enabled {
            match crate::mcp::start_server(
                app_handle.clone(),
                settings.mcp_port,
                settings.mcp_token.clone(),
            )
            .await
            {
                Ok(h) => *guard = Some(h),
                Err(e) => {
                    // Best-effort rollback with the old token so a working
                    // server isn't left dead.
                    if let Ok(h) = crate::mcp::start_server(
                        app_handle.clone(),
                        old_settings.mcp_port,
                        old_settings.mcp_token.clone(),
                    )
                    .await
                    {
                        *guard = Some(h);
                    }
                    return Err(e);
                }
            }
        }
    }
    state.save_settings(&settings)?;

    mcp_get_connection_info(state).await
}

// ---- agent options, approvals, activity and undo ----

use crate::mcp::activity::{Entry, Log, Undo};
use crate::mcp::policy::{self, Policy};
use crate::mcp::runtime;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpOptions {
    pub read_only: bool,
    pub ask_destructive: bool,
    pub agent_dj: bool,
    pub bearer: String,
}

pub fn mcp_get_options(state: &AppState) -> McpOptions {
    let s = state.load_settings().unwrap_or_default();
    McpOptions {
        read_only: s.mcp_read_only,
        ask_destructive: s.mcp_ask_destructive,
        agent_dj: s.mcp_agent_dj,
        bearer: s.mcp_bearer,
    }
}

fn push_policy(state: &AppState, s: &crate::Settings) {
    if let Some(rt) = runtime::current(state) {
        rt.set_policy(Policy {
            read_only: s.mcp_read_only,
            ask_destructive: s.mcp_ask_destructive,
        });
        rt.dj
            .store(s.mcp_agent_dj, std::sync::atomic::Ordering::Relaxed);
        rt.set_bearer(s.mcp_bearer.clone());
    }
}

/// `which`: "read-only" | "ask-destructive" | "agent-dj".
pub fn mcp_set_option(state: &AppState, which: &str, on: bool) -> Result<(), SlateError> {
    let mut s = state.load_settings().unwrap_or_default();
    match which {
        "read-only" => s.mcp_read_only = on,
        "ask-destructive" => s.mcp_ask_destructive = on,
        "agent-dj" => s.mcp_agent_dj = on,
        _ => return Ok(()),
    }
    state.save_settings(&s)?;
    push_policy(state, &s);
    Ok(())
}

/// A fresh bearer token (replaces any existing one); returns it.
pub fn mcp_generate_bearer(state: &AppState) -> Result<String, SlateError> {
    let mut s = state.load_settings().unwrap_or_default();
    s.mcp_bearer = policy::generate_token();
    state.save_settings(&s)?;
    push_policy(state, &s);
    Ok(s.mcp_bearer)
}

pub fn mcp_clear_bearer(state: &AppState) -> Result<(), SlateError> {
    let mut s = state.load_settings().unwrap_or_default();
    s.mcp_bearer.clear();
    state.save_settings(&s)?;
    push_policy(state, &s);
    Ok(())
}

/// The user's answer to "<client> wants to …". False when the request already ended.
pub fn mcp_answer_approval(state: &AppState, id: u64, allow: bool) -> bool {
    runtime::current(state).is_some_and(|rt| rt.answer(id, allow))
}

/// Newest first.
pub fn mcp_activity(state: &AppState, limit: usize) -> Vec<Entry> {
    match runtime::current(state) {
        Some(rt) => rt
            .log
            .lock()
            .map(|l| l.recent(limit, 0))
            .unwrap_or_default(),
        None => crate::config_dir_for_env()
            .map(|d| Log::load(&d.join("agent_activity.json")).recent(limit, 0))
            .unwrap_or_default(),
    }
}

/// Puts back what a logged agent action changed. Returns a sentence for the user.
pub async fn mcp_undo(state: &AppState, app: crate::AppHandle, id: u64) -> Result<String, String> {
    let rt =
        runtime::current(state).ok_or("The MCP server is off, so there is nothing to undo.")?;
    let undo = rt
        .log
        .lock()
        .map_err(|_| "The log is unavailable.".to_string())?
        .undo_for(id)?;
    let msg = run_undo(state, &app, &rt, &undo).await?;
    if let Ok(mut log) = rt.log.lock() {
        log.mark_undone(id);
    }
    rt.persist();
    Ok(msg)
}

async fn run_undo(
    state: &AppState,
    app: &crate::AppHandle,
    rt: &runtime::Runtime,
    undo: &Undo,
) -> Result<String, String> {
    use crate::commands::library as lib;
    let fail = |e: SlateError| e.log_safe();
    match undo {
        Undo::RemoveFromQueue { .. } | Undo::RestoreQueue { .. } => {
            let (ev, payload) = crate::mcp::hooks::undo_event(undo).ok_or("Cannot undo this.")?;
            app.emit(ev, payload).map_err(fail)?;
            Ok("Queue change undone.".into())
        }
        Undo::MoveQueueItem { track_id, to } => {
            let from = state
                .mcp_state
                .read()
                .await
                .queue
                .iter()
                .position(|t| t.id == *track_id)
                .ok_or("That track is no longer in the queue.")?;
            app.emit(
                crate::mcp::events::EV_MOVE_QUEUE_ITEM,
                serde_json::json!({ "from": from, "to": to }),
            )
            .map_err(fail)?;
            Ok("Queue order restored.".into())
        }
        Undo::RemoveFromPlaylist {
            playlist,
            track_ids,
        } => {
            let tracks = lib::get_playlist_tracks(state, app.clone(), playlist.clone())
                .await
                .map_err(fail)?;
            let mut idx: Vec<u32> = tracks
                .iter()
                .enumerate()
                .filter(|(_, t)| track_ids.contains(&t.id))
                .map(|(i, _)| i as u32)
                .collect();
            idx.sort_unstable_by(|a, b| b.cmp(a));
            for i in &idx {
                lib::remove_track_from_playlist(state, playlist.clone(), *i)
                    .await
                    .map_err(fail)?;
            }
            Ok(format!("Removed {} tracks again.", idx.len()))
        }
        Undo::AddToPlaylist {
            playlist,
            track_ids,
        } => {
            let n = lib::add_tracks_paced(state, playlist, track_ids)
                .await
                .map_err(fail)?;
            Ok(format!(
                "Added {n} tracks back (at the end of the playlist)."
            ))
        }
        Undo::ReorderPlaylist { playlist, order } => {
            let cur: Vec<u64> = lib::get_playlist_tracks(state, app.clone(), playlist.clone())
                .await
                .map_err(fail)?
                .iter()
                .map(|t| t.id)
                .collect();
            let moves = crate::playlist_tools::moves_to_order(&cur, order);
            for (from, to) in &moves {
                lib::move_playlist_item(state, playlist.clone(), *from, *to)
                    .await
                    .map_err(fail)?;
            }
            Ok(format!("Playlist order restored ({} moves).", moves.len()))
        }
        Undo::DeleteCreatedPlaylist { playlist } => {
            let name = crate::commands::metadata::get_playlist_details(state, playlist.clone())
                .await
                .ok()
                .and_then(|d| d["title"].as_str().map(str::to_string))
                .unwrap_or_else(|| playlist.clone());
            let what = format!("delete the playlist “{name}” (undoing its creation)");
            if !rt.ask_user(app, "Slate", &what).await {
                return Err("Not deleted: the deletion was not allowed.".into());
            }
            let uid = state
                .tidal_client
                .lock()
                .await
                .tokens
                .as_ref()
                .and_then(|t| t.user_id)
                .ok_or("Not signed in.")?;
            lib::delete_playlist(state, uid, playlist.clone())
                .await
                .map_err(fail)?;
            Ok(format!("Deleted “{name}”."))
        }
        Undo::SetFavorite { kind, id, on } => {
            let uid = state
                .tidal_client
                .lock()
                .await
                .tokens
                .as_ref()
                .and_then(|t| t.user_id)
                .ok_or("Not signed in.")?;
            let r = match (kind.as_str(), *on) {
                ("track", true) => lib::add_favorite_track(state, uid, *id).await,
                ("track", false) => lib::remove_favorite_track(state, uid, *id).await,
                ("album", true) => lib::add_favorite_album(state, uid, *id).await,
                ("album", false) => lib::remove_favorite_album(state, uid, *id).await,
                ("artist", true) => lib::add_favorite_artist(state, uid, *id).await,
                ("artist", false) => lib::remove_favorite_artist(state, uid, *id).await,
                _ => return Err("Cannot undo this.".into()),
            };
            r.map_err(fail)?;
            Ok("Favourite change undone.".into())
        }
    }
}
