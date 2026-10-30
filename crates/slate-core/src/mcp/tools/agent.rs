//! The tools added for AI agents (queue control, richer search and info, playlist tools,
//! context, `play_query`, `batch`), plus the helpers the guarded call path uses: describing
//! destructive calls, capturing what is needed to undo, and the activity log.

use std::borrow::Cow;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use rmcp::schemars::JsonSchema;
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{tool_router, ErrorData};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::commands::library as lib_cmds;
use crate::error::SlateError;
use crate::mcp::activity::Undo;
use crate::mcp::duration::{self, Item};
use crate::mcp::events::{PlayTracksPayload, EV_MOVE_QUEUE_ITEM, EV_PLAY_SOURCE, EV_PLAY_TRACKS};
use crate::mcp::query::{self, Hit, Kind};
use crate::mcp::sanitizer::SanitizedTrack;
use crate::mcp::server::SlateMcpServer;
use crate::playlist_tools;
use crate::tidal_api::{TidalClient, TidalTrack};
use crate::AppState;

use super::util::{require_user_id, NoArgs};

// ---------- small helpers ----------

fn ok(v: Value) -> Result<CallToolResult, ErrorData> {
    Ok(CallToolResult::success(vec![ContentBlock::text(
        v.to_string(),
    )]))
}

fn bad(msg: impl Into<String>) -> ErrorData {
    ErrorData::invalid_params(msg.into(), None)
}

/// A TIDAL error as a sentence an agent can act on.
pub(crate) fn friendly(e: &SlateError) -> String {
    if e.is_rate_limited() {
        return "TIDAL is rate-limiting Slate right now. Wait a minute and try again.".into();
    }
    match e {
        SlateError::Api { status: 404, .. } => "TIDAL has nothing with that id.".into(),
        SlateError::Api {
            status: 401 | 403, ..
        }
        | SlateError::NotAuthenticated => {
            "Slate is not signed in to TIDAL (or the session expired). Sign in in the app.".into()
        }
        _ => e.log_safe(),
    }
}

fn ierr(e: SlateError) -> ErrorData {
    ErrorData::internal_error(friendly(&e), None)
}

/// 1..=50 items, default 20.
fn page(limit: Option<u32>, offset: Option<u32>) -> (usize, usize) {
    (
        limit.unwrap_or(20).clamp(1, 50) as usize,
        offset.unwrap_or(0) as usize,
    )
}

fn track_json(t: &TidalTrack) -> Value {
    let s = SanitizedTrack::from_tidal(t);
    json!({
        "id": s.id, "title": s.title, "artist": s.artist, "album": s.album,
        "duration_seconds": s.duration_seconds,
        "explicit": t.explicit.unwrap_or(false),
    })
}

fn tracks_json(tracks: &[TidalTrack], limit: usize, offset: usize) -> Value {
    let total: u32 = tracks.iter().map(|t| t.duration).sum();
    json!({
        "track_count": tracks.len(),
        "total_duration_seconds": total,
        "offset": offset,
        "tracks": tracks.iter().skip(offset).take(limit).map(track_json).collect::<Vec<_>>(),
    })
}

fn did_you_mean(needle: &str, names: &[String]) -> String {
    let mut scored: Vec<(f64, &String)> = names
        .iter()
        .map(|n| (query::fuzzy(n, needle), n))
        .filter(|(s, _)| *s > 0.35)
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    match scored.first() {
        Some((_, n)) => format!("No playlist named “{needle}”; did you mean “{n}”?"),
        None => format!("No playlist named “{needle}”. Use get_user_playlists to see the names."),
    }
}

/// Playlist uuid from a uuid or a name (case-insensitive), with a helpful error.
pub(crate) async fn resolve_playlist(
    client: &mut TidalClient,
    uuid: Option<&str>,
    name: Option<&str>,
) -> Result<String, ErrorData> {
    if let Some(u) = uuid.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(u.to_string());
    }
    let needle = name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad("Give playlist_uuid or playlist_name."))?;
    let user_id = require_user_id(client)?;
    let owned = client
        .get_user_playlists(user_id, 0, 200)
        .await
        .map_err(ierr)?;
    let faved = client
        .get_favorite_playlists(user_id, 0, 200)
        .await
        .map_err(ierr)?;
    let all: Vec<_> = owned.items.into_iter().chain(faved.items).collect();
    let lower = needle.to_lowercase();
    if let Some(p) = all.iter().find(|p| p.title.to_lowercase() == lower) {
        return Ok(p.uuid.clone());
    }
    let names: Vec<String> = all.iter().map(|p| p.title.clone()).collect();
    Err(bad(did_you_mean(needle, &names)))
}

/// Seconds since local midnight (UTC where the platform cannot say).
pub fn local_secs_of_day() -> u32 {
    #[cfg(unix)]
    {
        let now = libc::time_t::try_from(crate::now_secs()).unwrap_or(0);
        // SAFETY: `tm` is plain data that localtime_r fully initialises; `now` is a valid time_t.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let r = unsafe { libc::localtime_r(&now, &mut tm) };
        if !r.is_null() {
            return u32::try_from(tm.tm_hour * 3600 + tm.tm_min * 60 + tm.tm_sec).unwrap_or(0);
        }
    }
    u32::try_from(crate::now_secs() % 86_400).unwrap_or(0)
}

// ---------- what a guarded call needs to know before it runs ----------

/// State captured before a mutating call so it can be undone.
pub(crate) enum Before {
    None,
    Queue(Vec<u64>),
    Order(Vec<u64>),
    Track(Option<u64>),
    Favorite(bool),
}

impl SlateMcpServer {
    fn arg_str(req: &CallToolRequestParams, key: &str) -> Option<String> {
        req.arguments
            .as_ref()?
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    fn arg_u64(req: &CallToolRequestParams, key: &str) -> Option<u64> {
        req.arguments.as_ref()?.get(key).and_then(Value::as_u64)
    }

    async fn playlist_of(
        &self,
        req: &CallToolRequestParams,
    ) -> Option<(String, String, Vec<TidalTrack>)> {
        let state = self.app_handle.state::<AppState>();
        let mut client = state.tidal_client.lock().await;
        let uuid = resolve_playlist(
            &mut client,
            Self::arg_str(req, "playlist_uuid").as_deref(),
            Self::arg_str(req, "playlist_name").as_deref(),
        )
        .await
        .ok()?;
        let title = client
            .get_playlist_details(&uuid)
            .await
            .ok()
            .and_then(|d| d["title"].as_str().map(str::to_string))
            .unwrap_or_else(|| uuid.clone());
        let tracks = client.get_playlist_tracks(&uuid).await.ok()?;
        Some((uuid, title, tracks))
    }

    /// For playlist tools: the helpful error ("did you mean …?") when the playlist cannot be found.
    pub(crate) async fn playlist_problem(&self, req: &CallToolRequestParams) -> Option<String> {
        let takes_playlist = matches!(
            req.name.as_ref(),
            "delete_playlist"
                | "remove_track_from_playlist"
                | "dedupe_playlist"
                | "reorder_playlist"
        );
        if !takes_playlist {
            return None;
        }
        let state = self.app_handle.try_state()?;
        let mut client = state.tidal_client.lock().await;
        resolve_playlist(
            &mut client,
            Self::arg_str(req, "playlist_uuid").as_deref(),
            Self::arg_str(req, "playlist_name").as_deref(),
        )
        .await
        .err()
        .map(|e| e.message.to_string())
    }

    /// A sentence for the user (and the agent) saying what a destructive call would do.
    pub(crate) async fn describe(&self, req: &CallToolRequestParams) -> String {
        let name = req.name.as_ref();
        let Some(state) = self.app_handle.try_state() else {
            return format!("run {name}");
        };
        match name {
            "clear_queue" => {
                let n = state.mcp_state.read().await.queue.len();
                format!("clear the play queue ({n} tracks)")
            }
            "delete_playlist" => match self.playlist_of(req).await {
                Some((_, title, tracks)) => {
                    format!(
                        "delete playlist “{title}” ({} tracks) for good",
                        tracks.len()
                    )
                }
                None => "delete a playlist".into(),
            },
            "remove_track_from_playlist" => match self.playlist_of(req).await {
                Some((_, title, tracks)) => {
                    let idx = Self::arg_u64(req, "index").unwrap_or(0) as usize;
                    match tracks.get(idx) {
                        Some(t) => format!("remove “{}” from playlist “{title}”", t.title),
                        None => format!("remove track #{idx} from playlist “{title}”"),
                    }
                }
                None => "remove a track from a playlist".into(),
            },
            "dedupe_playlist" => match self.playlist_of(req).await {
                Some((_, title, tracks)) => {
                    let n = playlist_tools::find_duplicates(&tracks).len();
                    format!("remove {n} duplicate tracks from playlist “{title}”")
                }
                None => "remove duplicate tracks from a playlist".into(),
            },
            "reorder_playlist" => match self.playlist_of(req).await {
                Some((_, title, _)) => format!(
                    "move track #{} to #{} in playlist “{title}”",
                    Self::arg_u64(req, "from_index").unwrap_or(0),
                    Self::arg_u64(req, "to_index").unwrap_or(0)
                ),
                None => "reorder a playlist".into(),
            },
            other => format!("run {other}"),
        }
    }

    /// What must be read before the call to be able to undo it afterwards.
    pub(crate) async fn capture_before(&self, req: &CallToolRequestParams) -> Before {
        let Some(state) = self.app_handle.try_state() else {
            return Before::None;
        };
        match req.name.as_ref() {
            "clear_queue" | "remove_from_queue" => Before::Queue(
                state
                    .mcp_state
                    .read()
                    .await
                    .queue
                    .iter()
                    .map(|t| t.id)
                    .collect(),
            ),
            "move_queue_item" => Before::Queue(
                state
                    .mcp_state
                    .read()
                    .await
                    .queue
                    .iter()
                    .map(|t| t.id)
                    .collect(),
            ),
            "reorder_playlist" => match self.playlist_of(req).await {
                Some((_, _, tracks)) => Before::Order(tracks.iter().map(|t| t.id).collect()),
                None => Before::None,
            },
            "remove_track_from_playlist" => match self.playlist_of(req).await {
                Some((_, _, tracks)) => {
                    let idx = Self::arg_u64(req, "index").unwrap_or(0) as usize;
                    Before::Track(tracks.get(idx).map(|t| t.id))
                }
                None => Before::None,
            },
            "dedupe_playlist" => match self.playlist_of(req).await {
                Some((_, _, tracks)) => {
                    let dups = playlist_tools::find_duplicates(&tracks);
                    Before::Order(dups.iter().map(|d| d.track_id).collect())
                }
                None => Before::None,
            },
            "favorite_track" | "favorite_album" | "favorite_artist" => {
                // The tools take action "add" | "remove"; undo does the opposite.
                let added = Self::arg_str(req, "action").as_deref() != Some("remove");
                Before::Favorite(added)
            }
            _ => Before::None,
        }
    }

    /// Writes the activity-log entry for a finished mutating call.
    pub(crate) fn record(
        &self,
        client: &str,
        tool: &str,
        args: Option<&Map<String, Value>>,
        before: Before,
        result: &Result<CallToolResult, ErrorData>,
    ) {
        let (result_text, success, payload) = match result {
            Ok(r) if r.is_error != Some(true) => {
                let text = r
                    .content
                    .iter()
                    .find_map(|c| c.as_text().map(|t| t.text.clone()))
                    .unwrap_or_default();
                let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                ("ok".to_string(), true, v)
            }
            Ok(_) => ("refused".to_string(), false, Value::Null),
            Err(e) => (format!("failed: {}", e.message), false, Value::Null),
        };
        let a = |k: &str| args.and_then(|m| m.get(k));
        let ids = |k: &str| -> Vec<u64> {
            a(k).and_then(Value::as_array)
                .map(|v| v.iter().filter_map(Value::as_u64).collect())
                .unwrap_or_default()
        };
        let playlist = a("playlist_uuid")
            .and_then(Value::as_str)
            .or_else(|| payload.get("playlist_uuid").and_then(Value::as_str))
            .or_else(|| payload.get("uuid").and_then(Value::as_str))
            .unwrap_or("")
            .to_string();
        let summary = match tool {
            "add_to_queue" | "play_next" => format!("{tool}: {} tracks", ids("track_ids").len()),
            "add_to_playlist" => format!("Added {} tracks to a playlist", ids("track_ids").len()),
            _ => format!(
                "{tool} {}",
                args.map(|m| Value::Object(m.clone()).to_string())
                    .unwrap_or_default()
            ),
        };
        let mut not_undoable = false;
        let undo = if !success {
            None
        } else {
            match (tool, before) {
                ("add_to_queue" | "play_next", _) => Some(Undo::RemoveFromQueue {
                    track_ids: ids("track_ids"),
                }),
                ("play_tracks", _)
                    if matches!(
                        a("action").and_then(Value::as_str),
                        Some("queue" | "play_next")
                    ) =>
                {
                    Some(Undo::RemoveFromQueue {
                        track_ids: ids("track_ids"),
                    })
                }
                ("remove_from_queue", Before::Queue(q)) => {
                    let id = a("track_id").and_then(Value::as_u64).unwrap_or(0);
                    let at = q.iter().position(|x| *x == id).unwrap_or(0);
                    Some(Undo::RestoreQueue {
                        track_ids: vec![id],
                        at,
                    })
                }
                ("clear_queue", Before::Queue(q)) => Some(Undo::RestoreQueue {
                    track_ids: q,
                    at: 0,
                }),
                ("move_queue_item", Before::Queue(q)) => {
                    let from = a("from").and_then(Value::as_u64).unwrap_or(0) as usize;
                    q.get(from).map(|id| Undo::MoveQueueItem {
                        track_id: *id,
                        to: from,
                    })
                }
                ("add_to_playlist", _) if !playlist.is_empty() => Some(Undo::RemoveFromPlaylist {
                    playlist: playlist.clone(),
                    track_ids: ids("track_ids"),
                }),
                ("remove_track_from_playlist", Before::Track(Some(id))) => {
                    Some(Undo::AddToPlaylist {
                        playlist: playlist.clone(),
                        track_ids: vec![id],
                    })
                }
                ("dedupe_playlist", Before::Order(removed))
                    if !removed.is_empty()
                        && a("dry_run").and_then(Value::as_bool) != Some(true) =>
                {
                    Some(Undo::AddToPlaylist {
                        playlist: playlist.clone(),
                        track_ids: removed,
                    })
                }
                ("reorder_playlist", Before::Order(order)) => Some(Undo::ReorderPlaylist {
                    playlist: playlist.clone(),
                    order,
                }),
                ("create_playlist" | "create_playlist_from_tracks", _) if !playlist.is_empty() => {
                    Some(Undo::DeleteCreatedPlaylist {
                        playlist: playlist.clone(),
                    })
                }
                ("favorite_track" | "favorite_album" | "favorite_artist", Before::Favorite(on)) => {
                    let kind = tool.trim_start_matches("favorite_").to_string();
                    let id = a("id").and_then(Value::as_u64).unwrap_or(0);
                    Some(Undo::SetFavorite { kind, id, on: !on })
                }
                ("delete_playlist", _) => {
                    not_undoable = true;
                    None
                }
                _ => None,
            }
        };
        if let Ok(mut log) = self.rt.log.lock() {
            log.push(
                crate::now_secs(),
                client,
                tool,
                &summary,
                &result_text,
                undo,
                not_undoable,
            );
            if log.len() % 20 == 0 {
                drop(log);
                self.rt.persist();
            }
        }
    }

    pub(crate) async fn now_playing_json(&self, state: &AppState) -> Value {
        let s = state.mcp_state.read().await;
        let Some(np) = &s.now_playing else {
            return json!({ "nowPlaying": null });
        };
        let queue_secs: Vec<u32> = s.queue.iter().map(|t| t.duration_seconds).collect();
        let _ = queue_secs;
        let pos = crate::commands::playback::get_playback_position(state)
            .await
            .map_or(np.position_seconds, |p| p.max(0.0) as u32);
        let left = np.duration_seconds.saturating_sub(pos);
        json!({ "nowPlaying": {
            "track_id": np.track_id, "title": np.title, "artist": np.artist, "album": np.album,
            "duration_seconds": np.duration_seconds, "position_seconds": pos,
            "is_playing": np.is_playing,
            "remaining_seconds": left,
            "ends_at": duration::ends_at(local_secs_of_day(), left),
        }})
    }

    pub(crate) async fn queue_json(&self, state: &AppState, limit: usize, offset: usize) -> Value {
        let s = state.mcp_state.read().await;
        let upcoming: Vec<u32> = s.queue.iter().map(|t| t.duration_seconds).collect();
        let (cur_dur, cur_pos) = s
            .now_playing
            .as_ref()
            .map_or((0, 0), |n| (n.duration_seconds, n.position_seconds));
        drop(s);
        let pos = if cur_dur > 0 {
            crate::commands::playback::get_playback_position(state)
                .await
                .map_or(cur_pos, |p| p.max(0.0) as u32)
        } else {
            0
        };
        let s = state.mcp_state.read().await;
        let total: u32 = upcoming.iter().sum();
        json!({
            "queue": s.queue.iter().skip(offset).take(limit).collect::<Vec<_>>(),
            "track_count": s.queue.len(),
            "offset": offset,
            "total_duration_seconds": total,
            "remaining_seconds": duration::remaining_secs(cur_dur, pos, &upcoming),
        })
    }

    /// Looks the ids up for their durations; unknown ids are dropped.
    async fn lookup_tracks(&self, ids: &[u64]) -> Vec<TidalTrack> {
        let state = self.app_handle.state::<AppState>();
        let mut out = Vec::new();
        for id in ids.iter().take(50) {
            let v = state.tidal_client.lock().await.get_track(*id).await;
            if let Some(t) = v
                .ok()
                .and_then(|v| serde_json::from_value::<TidalTrack>(v).ok())
            {
                out.push(t);
            }
        }
        out
    }

    fn emit_play(&self, ids: Vec<u64>, action: &str) -> Result<(), ErrorData> {
        self.app_handle
            .emit(
                EV_PLAY_TRACKS,
                PlayTracksPayload {
                    track_ids: ids,
                    action: action.into(),
                },
            )
            .map_err(|e| ErrorData::internal_error(format!("emit failed: {e}"), None))
    }

    /// Runs one tool by name through the full guarded path (used by `batch`).
    async fn run_inner(
        &self,
        name: &str,
        arguments: Option<Map<String, Value>>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut req = CallToolRequestParams::new(Cow::Owned(name.to_string()));
        req.arguments = arguments;
        self.guarded(req, ctx).await
    }
}

// ---------- arguments ----------

#[derive(Deserialize, JsonSchema)]
pub struct ListArgs {
    /// Max items (default 20, max 50).
    pub limit: Option<u32>,
    /// Items to skip (default 0).
    pub offset: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct TrackIdsArgs {
    /// Tidal track ids (at most 100).
    pub track_ids: Vec<u64>,
    /// Keep adding the following ids until the total is within ±3 minutes of this (optional).
    pub target_duration_minutes: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct MoveQueueArgs {
    /// 0-based position in the upcoming queue.
    pub from: u32,
    /// New 0-based position.
    pub to: u32,
}

#[derive(Deserialize, JsonSchema)]
pub struct SearchArgs {
    /// Search text, e.g. "daft punk discovery".
    pub query: String,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct IdArgs {
    /// Tidal id.
    pub id: u64,
    pub limit: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct SuggestionsArgs {
    pub playlist_uuid: Option<String>,
    pub playlist_name: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DedupeArgs {
    pub playlist_uuid: Option<String>,
    pub playlist_name: Option<String>,
    /// true: only report the duplicates, change nothing (default false).
    pub dry_run: Option<bool>,
    /// Must be true to actually remove (destructive).
    #[allow(dead_code)]
    pub confirm: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ReorderArgs {
    pub playlist_uuid: Option<String>,
    pub playlist_name: Option<String>,
    /// 0-based index of the track to move.
    pub from_index: u32,
    /// 0-based index it should end up at.
    pub to_index: u32,
    /// Must be true (destructive).
    #[allow(dead_code)]
    pub confirm: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct CompareArgs {
    pub a_uuid: Option<String>,
    pub a_name: Option<String>,
    pub b_uuid: Option<String>,
    pub b_name: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct MatchArgs {
    /// ISRC codes or "artist - title" strings, at most 100.
    pub items: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct CreateFromTracksArgs {
    pub name: String,
    /// Track ids, ISRCs or "artist - title" strings.
    pub items: Vec<String>,
    /// Stop adding once within ±3 minutes of this length (optional).
    pub target_duration_minutes: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct StatsArgs {
    /// "day", "week" (default), "month", "year" or "all".
    pub period: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct PlayQueryArgs {
    /// Natural request, e.g. "the new Kendrick album", "lofi playlist".
    pub query: String,
    /// Play even when the match is not confident (default false).
    pub play_anyway: Option<bool>,
    /// For a playlist/album request: stop once about this long (optional).
    pub target_duration_minutes: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct SmartArgs {
    /// Smart playlist id from get_smart_playlists.
    pub id: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct BatchCall {
    /// Tool name.
    pub tool: String,
    /// That tool's arguments.
    pub arguments: Option<Map<String, Value>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct BatchArgs {
    /// Up to 20 calls, run in order.
    pub calls: Vec<BatchCall>,
    /// Stop at the first error (default true).
    pub stop_on_error: Option<bool>,
}

// ---------- the tools ----------

#[tool_router(router = agent_tools, vis = "pub(crate)")]
impl SlateMcpServer {
    #[rmcp::tool(
        name = "add_to_queue",
        description = "Append tracks to the end of the Next-up queue. Params: track_ids (array), target_duration_minutes (optional, adds ids in order until within ±3 min). Example: {\"track_ids\":[123,456]}. Returns the count and total duration added."
    )]
    async fn add_to_queue(
        &self,
        Parameters(args): Parameters<TrackIdsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.queue_tracks(args, "queue").await
    }

    #[rmcp::tool(
        name = "play_next",
        description = "Insert tracks at the front of the Next-up queue so they play right after the current track. Params: track_ids. Example: {\"track_ids\":[123]}."
    )]
    async fn play_next(
        &self,
        Parameters(args): Parameters<TrackIdsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.queue_tracks(args, "play_next").await
    }

    #[rmcp::tool(
        name = "move_queue_item",
        description = "Move one upcoming track to another position in the queue. Params: from, to (0-based). Example: {\"from\":3,\"to\":0}."
    )]
    async fn move_queue_item(
        &self,
        Parameters(args): Parameters<MoveQueueArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.app_handle.state::<AppState>();
        let n = state.mcp_state.read().await.queue.len() as u32;
        if args.from >= n || args.to >= n {
            return Err(bad(format!(
                "The queue has {n} upcoming tracks; positions run from 0 to {}.",
                n.saturating_sub(1)
            )));
        }
        self.app_handle
            .emit(
                EV_MOVE_QUEUE_ITEM,
                json!({ "from": args.from, "to": args.to }),
            )
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        ok(json!({ "status": "moved", "from": args.from, "to": args.to }))
    }

    #[rmcp::tool(
        name = "get_history",
        description = "Recently played tracks, newest first. Params: limit (≤50, default 20), offset. Example: {\"limit\":10}."
    )]
    async fn get_history(
        &self,
        Parameters(a): Parameters<ListArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let (limit, offset) = page(a.limit, a.offset);
        let state = self.app_handle.state::<AppState>();
        let s = state.mcp_state.read().await;
        ok(json!({
            "track_count": s.history.len(),
            "offset": offset,
            "history": s.history.iter().skip(offset).take(limit).collect::<Vec<_>>(),
        }))
    }

    #[rmcp::tool(
        name = "search_albums",
        description = "Search TIDAL for albums. Params: query, limit, offset. Example: {\"query\":\"discovery daft punk\"}."
    )]
    async fn search_albums(
        &self,
        Parameters(a): Parameters<SearchArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let (limit, offset) = page(a.limit, a.offset);
        let r = self.search_all(&a.query, (limit + offset) as u32).await?;
        ok(
            json!({ "albums": r.albums.iter().skip(offset).take(limit).map(|x| json!({
            "id": x.id, "title": x.title,
            "artist": x.artist.as_ref().map(|a| a.name.clone()).unwrap_or_default(),
            "year": x.release_date.as_deref().and_then(|d| d.get(..4)),
            "track_count": x.number_of_tracks, "total_duration_seconds": x.duration,
        })).collect::<Vec<_>>() }),
        )
    }

    #[rmcp::tool(
        name = "search_artists",
        description = "Search TIDAL for artists. Params: query, limit, offset. Example: {\"query\":\"kendrick\"}."
    )]
    async fn search_artists(
        &self,
        Parameters(a): Parameters<SearchArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let (limit, offset) = page(a.limit, a.offset);
        let r = self.search_all(&a.query, (limit + offset) as u32).await?;
        ok(json!({ "artists": r.artists.iter().skip(offset).take(limit)
            .map(|x| json!({ "id": x.id, "name": x.name })).collect::<Vec<_>>() }))
    }

    #[rmcp::tool(
        name = "search_playlists",
        description = "Search TIDAL for playlists. Params: query, limit, offset. Example: {\"query\":\"lofi beats\"}."
    )]
    async fn search_playlists(
        &self,
        Parameters(a): Parameters<SearchArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let (limit, offset) = page(a.limit, a.offset);
        let r = self.search_all(&a.query, (limit + offset) as u32).await?;
        ok(
            json!({ "playlists": r.playlists.iter().skip(offset).take(limit).map(|p| json!({
            "uuid": p.uuid, "title": p.title, "track_count": p.number_of_tracks,
            "total_duration_seconds": p.duration,
        })).collect::<Vec<_>>() }),
        )
    }

    #[rmcp::tool(
        name = "get_mixes",
        description = "The user's TIDAL mixes (daily mixes, radios). Params: limit, offset. Example: {\"limit\":10}."
    )]
    async fn get_mixes(
        &self,
        Parameters(a): Parameters<ListArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let (limit, offset) = page(a.limit, a.offset);
        let state = self.app_handle.state::<AppState>();
        let mixes = state
            .tidal_client
            .lock()
            .await
            .get_favorite_mixes(offset as u32, limit as u32, "DATE", "DESC")
            .await
            .map_err(ierr)?;
        ok(
            json!({ "total": mixes.total_number_of_items, "offset": offset, "mixes": mixes.items.iter().map(|m| json!({
            "id": m.id, "title": m.title, "subtitle": m.sub_title, "type": m.mix_type,
        })).collect::<Vec<_>>() }),
        )
    }

    #[rmcp::tool(
        name = "get_explore",
        description = "TIDAL's Explore page: genres and moods, with what each section holds. Params: limit (sections), offset. Example: {}."
    )]
    async fn get_explore(
        &self,
        Parameters(a): Parameters<ListArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let (limit, offset) = page(a.limit, a.offset);
        let state = self.app_handle.state::<AppState>();
        let page_ = state
            .tidal_client
            .lock()
            .await
            .get_page("pages/explore")
            .await
            .map_err(ierr)?;
        let sections: Vec<Value> = page_
            .sections
            .iter()
            .skip(offset)
            .take(limit)
            .map(|s| {
                let items: Vec<Value> = s
                    .items
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .take(20)
                            .map(|i| {
                                let d = i.get("data").unwrap_or(i);
                                json!({
                                    "title": d.get("title").or_else(|| d.get("name")),
                                    "api_path": d.get("apiPath").or_else(|| d.get("path")),
                                    "id": d.get("id").or_else(|| d.get("uuid")),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                json!({ "title": s.title, "type": s.section_type, "items": items })
            })
            .collect();
        ok(json!({ "section_count": page_.sections.len(), "offset": offset, "sections": sections }))
    }

    #[rmcp::tool(
        name = "get_similar_artists",
        description = "Artists similar to an artist. Params: id (artist id), limit. Example: {\"id\":1566}."
    )]
    async fn get_similar_artists(
        &self,
        Parameters(a): Parameters<IdArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let (limit, _) = page(a.limit, None);
        let state = self.app_handle.state::<AppState>();
        let artists = state
            .tidal_client
            .lock()
            .await
            .get_similar_artists(a.id, limit as u32)
            .await
            .map_err(ierr)?;
        ok(
            json!({ "artists": artists.iter().map(|x| json!({ "id": x.id, "name": x.name })).collect::<Vec<_>>() }),
        )
    }

    #[rmcp::tool(
        name = "get_track_credits",
        description = "Who made a track (writers, producers, performers). Params: id (track id). Example: {\"id\":123}."
    )]
    async fn get_track_credits(
        &self,
        Parameters(a): Parameters<IdArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.app_handle.state::<AppState>();
        let credits = state
            .tidal_client
            .lock()
            .await
            .get_track_credits(a.id)
            .await
            .map_err(ierr)?;
        ok(json!({ "credits": credits.iter().map(|c| json!({
            "role": c.credit_type,
            "people": c.contributors.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
        })).collect::<Vec<_>>() }))
    }

    #[rmcp::tool(
        name = "get_album_info",
        description = "Details of an album: artist, year, track count, total duration, quality, copyright. Params: id (album id). Example: {\"id\":77646168}."
    )]
    async fn get_album_info(
        &self,
        Parameters(a): Parameters<IdArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.app_handle.state::<AppState>();
        let d = state
            .tidal_client
            .lock()
            .await
            .get_album_detail(a.id)
            .await
            .map_err(ierr)?;
        ok(json!({
            "id": d.id, "title": d.title, "version": d.version,
            "artist": d.artist.as_ref().map(|x| x.name.clone()),
            "release_date": d.release_date, "track_count": d.number_of_tracks,
            "total_duration_seconds": d.duration, "explicit": d.explicit,
            "audio_quality": d.audio_quality, "audio_modes": d.audio_modes,
            "copyright": d.copyright, "type": d.album_type,
        }))
    }

    #[rmcp::tool(
        name = "get_artist_info",
        description = "Details and bio of an artist. Params: id (artist id). Example: {\"id\":1566}."
    )]
    async fn get_artist_info(
        &self,
        Parameters(a): Parameters<IdArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.app_handle.state::<AppState>();
        let mut client = state.tidal_client.lock().await;
        let d = client.get_artist_detail(a.id).await.map_err(ierr)?;
        let bio = client.get_artist_bio(a.id).await.unwrap_or_default();
        let bio: String = bio.chars().take(1500).collect();
        ok(json!({ "id": d.id, "name": d.name, "popularity": d.popularity, "bio": bio }))
    }

    #[rmcp::tool(
        name = "get_other_versions",
        description = "Other versions of a track (explicit/clean, remasters, live, Dolby Atmos…). Params: id (track id), limit. Example: {\"id\":123}."
    )]
    async fn get_other_versions(
        &self,
        Parameters(a): Parameters<IdArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let (limit, _) = page(a.limit, None);
        let state = self.app_handle.state::<AppState>();
        let versions = crate::commands::versions::track_versions(&state, a.id)
            .await
            .map_err(ierr)?;
        ok(json!({ "versions": versions.into_iter().take(limit).collect::<Vec<_>>() }))
    }

    #[rmcp::tool(
        name = "get_playlist_suggestions",
        description = "Tracks TIDAL suggests adding to a playlist (never ones already in it). Params: playlist_uuid or playlist_name, limit. Example: {\"playlist_name\":\"Road trip\",\"limit\":10}."
    )]
    async fn get_playlist_suggestions(
        &self,
        Parameters(a): Parameters<SuggestionsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let (limit, _) = page(a.limit, None);
        let state = self.app_handle.state::<AppState>();
        let uuid = {
            let mut c = state.tidal_client.lock().await;
            resolve_playlist(
                &mut c,
                a.playlist_uuid.as_deref(),
                a.playlist_name.as_deref(),
            )
            .await?
        };
        let s = lib_cmds::playlist_suggestions(&state, self.app_handle.clone(), &uuid, 0, limit)
            .await
            .map_err(ierr)?;
        ok(tracks_json(&s.tracks, limit, 0))
    }

    #[rmcp::tool(
        name = "compare_playlists",
        description = "Compare two playlists: tracks only in A, only in B, in both (matched by ISRC, else track id). Params: a_uuid|a_name, b_uuid|b_name. Example: {\"a_name\":\"Gym\",\"b_name\":\"Run\"}."
    )]
    async fn compare_playlists(
        &self,
        Parameters(a): Parameters<CompareArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.app_handle.state::<AppState>();
        let mut c = state.tidal_client.lock().await;
        let au = resolve_playlist(&mut c, a.a_uuid.as_deref(), a.a_name.as_deref()).await?;
        let bu = resolve_playlist(&mut c, a.b_uuid.as_deref(), a.b_name.as_deref()).await?;
        let at = c.get_playlist_tracks(&au).await.map_err(ierr)?;
        let bt = c.get_playlist_tracks(&bu).await.map_err(ierr)?;
        drop(c);
        let cmp = playlist_tools::compare(&at, &bt);
        let list = |idx: &[usize], src: &[TidalTrack]| -> Value {
            let v: Vec<&TidalTrack> = idx.iter().filter_map(|i| src.get(*i)).collect();
            json!({ "count": v.len(), "total_duration_seconds": v.iter().map(|t| t.duration).sum::<u32>(),
                    "tracks": v.iter().take(50).map(|t| track_json(t)).collect::<Vec<_>>() })
        };
        ok(json!({
            "only_in_a": list(&cmp.only_a, &at),
            "only_in_b": list(&cmp.only_b, &bt),
            "in_both": list(&cmp.both, &at),
        }))
    }

    #[rmcp::tool(
        name = "match_tracks",
        description = "Turn ISRCs or \"artist - title\" strings into TIDAL track ids (one paced search each, at most 100). Params: items. Example: {\"items\":[\"Daft Punk - One More Time\"]}."
    )]
    async fn match_tracks(
        &self,
        Parameters(a): Parameters<MatchArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let matches = self.match_items(&a.items).await?;
        let found = matches.iter().filter(|m| m["id"].is_u64()).count();
        ok(json!({ "matched": found, "total": matches.len(), "matches": matches }))
    }

    #[rmcp::tool(
        name = "create_playlist_from_tracks",
        description = "Create a playlist in one call from track ids, ISRCs or \"artist - title\" strings. Params: name, items, target_duration_minutes (optional). Example: {\"name\":\"Focus\",\"items\":[\"Brian Eno - An Ending (Ascent)\"],\"target_duration_minutes\":45}."
    )]
    async fn create_playlist_from_tracks(
        &self,
        Parameters(a): Parameters<CreateFromTracksArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let name = a.name.trim().to_string();
        if name.is_empty() {
            return Err(bad("name must not be empty."));
        }
        let matches = self.match_items(&a.items).await?;
        let mut cands: Vec<Item> = matches
            .iter()
            .filter_map(|m| {
                Some(Item {
                    id: m["id"].as_u64()?,
                    secs: m["duration_seconds"].as_u64()? as u32,
                })
            })
            .collect();
        let unmatched: Vec<&Value> = matches
            .iter()
            .filter(|m| !m["id"].is_u64())
            .map(|m| &m["query"])
            .collect();
        if let Some(target) = a.target_duration_minutes {
            let (chosen, _) = duration::build_to_target(&cands, target);
            cands = chosen;
        }
        if cands.is_empty() {
            return Err(bad(
                "None of the items matched a TIDAL track, so no playlist was made.",
            ));
        }
        let ids: Vec<u64> = cands.iter().map(|c| c.id).collect();
        let total = duration::total_secs(&cands);
        let state = self.app_handle.state::<AppState>();
        let (pl, added) = lib_cmds::create_playlist_with_tracks(&state, name, "UNLISTED", &ids)
            .await
            .map_err(ierr)?;
        ok(json!({
            "playlist_uuid": pl.uuid, "name": pl.title, "track_count": added,
            "total_duration_seconds": total, "unmatched": unmatched,
        }))
    }

    #[rmcp::tool(
        name = "dedupe_playlist",
        description = "Remove duplicate tracks (same ISRC, else same track) from a playlist, keeping the first. Params: playlist_uuid|playlist_name, dry_run (true = report only), confirm (must be true to remove). Example: {\"playlist_name\":\"Gym\",\"dry_run\":true}."
    )]
    async fn dedupe_playlist(
        &self,
        Parameters(a): Parameters<DedupeArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.app_handle.state::<AppState>();
        let (uuid, tracks) = {
            let mut c = state.tidal_client.lock().await;
            let uuid = resolve_playlist(
                &mut c,
                a.playlist_uuid.as_deref(),
                a.playlist_name.as_deref(),
            )
            .await?;
            let tracks = c.get_playlist_tracks(&uuid).await.map_err(ierr)?;
            (uuid, tracks)
        };
        let dups = playlist_tools::find_duplicates(&tracks);
        let listing: Vec<Value> = dups
            .iter()
            .map(|d| {
                json!({ "index": d.index, "track_id": d.track_id, "same_as_index": d.kept_index,
                             "title": tracks.get(d.index as usize).map(|t| t.title.clone()) })
            })
            .collect();
        if a.dry_run.unwrap_or(false) || dups.is_empty() {
            return ok(json!({ "playlist_uuid": uuid, "dry_run": true, "duplicates": listing }));
        }
        let mut removed = 0;
        for index in playlist_tools::removal_order(&dups) {
            lib_cmds::remove_track_from_playlist(&state, uuid.clone(), index)
                .await
                .map_err(ierr)?;
            removed += 1;
        }
        ok(json!({ "playlist_uuid": uuid, "removed": removed, "duplicates": listing }))
    }

    #[rmcp::tool(
        name = "reorder_playlist",
        description = "Move one track within a playlist. Params: playlist_uuid|playlist_name, from_index, to_index (0-based), confirm (must be true). Example: {\"playlist_name\":\"Gym\",\"from_index\":10,\"to_index\":0,\"confirm\":true}."
    )]
    async fn reorder_playlist(
        &self,
        Parameters(a): Parameters<ReorderArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.app_handle.state::<AppState>();
        let (uuid, n) = {
            let mut c = state.tidal_client.lock().await;
            let uuid = resolve_playlist(
                &mut c,
                a.playlist_uuid.as_deref(),
                a.playlist_name.as_deref(),
            )
            .await?;
            let n = c.get_playlist_tracks(&uuid).await.map_err(ierr)?.len() as u32;
            (uuid, n)
        };
        if a.from_index >= n || a.to_index >= n {
            return Err(bad(format!(
                "The playlist has {n} tracks; indices run from 0 to {}.",
                n.saturating_sub(1)
            )));
        }
        lib_cmds::move_playlist_item(&state, uuid.clone(), a.from_index, a.to_index)
            .await
            .map_err(ierr)?;
        ok(json!({ "playlist_uuid": uuid, "moved_from": a.from_index, "moved_to": a.to_index }))
    }

    #[rmcp::tool(
        name = "get_current_lyric_line",
        description = "The lyric line being sung right now, if the track has synced lyrics. Example: {}."
    )]
    async fn get_current_lyric_line(
        &self,
        Parameters(_): Parameters<NoArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.app_handle.state::<AppState>();
        let s = state.mcp_state.read().await;
        ok(json!({ "line": s.lyric_line }))
    }

    #[rmcp::tool(
        name = "get_signal_path",
        description = "How the audio reaches the speakers: codec, bit depth, sample rate, exclusive / bit-perfect, crossfade, and whether the path is PRISTINE or MODIFIED. Example: {}."
    )]
    async fn get_signal_path(
        &self,
        Parameters(_): Parameters<NoArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.app_handle.state::<AppState>();
        let sp = crate::commands::utility::get_signal_path(&state);
        let v = crate::signal_path::verdict(&sp);
        let mut out = serde_json::to_value(&sp).unwrap_or(Value::Null);
        out["verdict"] = json!(if v.pristine { "PRISTINE" } else { "MODIFIED" });
        ok(json!({ "signal_path": out }))
    }

    #[rmcp::tool(
        name = "get_listening_stats",
        description = "Top artists and tracks from Slate's local play log (kept only while the MCP server runs). Params: period (day|week|month|year|all), limit. Example: {\"period\":\"week\"}."
    )]
    async fn get_listening_stats(
        &self,
        Parameters(a): Parameters<StatsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let period = a.period.as_deref().unwrap_or("week");
        let secs = crate::mcp::stats::period_secs(period)
            .ok_or_else(|| bad("period must be day, week, month, year or all."))?;
        let (limit, _) = page(a.limit, None);
        let s = self.rt.stats(secs, limit.min(25));
        ok(json!({ "period": period, "stats": s }))
    }

    #[rmcp::tool(
        name = "get_smart_playlists",
        description = "The user's smart playlists (rule-based, local). Example: {}."
    )]
    async fn get_smart_playlists(
        &self,
        Parameters(_): Parameters<NoArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let path = crate::config_dir_for_env().map(|d| d.join("smart_playlists.json"));
        let store = path
            .map(|p| crate::smart::Store::load(&p))
            .unwrap_or_default();
        ok(
            json!({ "smart_playlists": store.lists.iter().map(|l| json!({
            "id": l.id, "name": l.name, "rule_count": l.rules.len(), "match": if l.match_all { "all" } else { "any" },
        })).collect::<Vec<_>>() }),
        )
    }

    #[rmcp::tool(
        name = "play_smart_playlist",
        description = "Evaluate a smart playlist and play it. Params: id (from get_smart_playlists). Example: {\"id\":\"sp1\"}. Returns the track count and total duration."
    )]
    async fn play_smart_playlist(
        &self,
        Parameters(a): Parameters<SmartArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.app_handle.state::<AppState>();
        let Some(eval) = state.smart_bridge.get() else {
            return Err(bad(
                "Smart playlists are not available yet; try again in a moment.",
            ));
        };
        let (name, tracks) = eval(a.id.clone()).await.map_err(bad)?;
        if tracks.is_empty() {
            return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "“{name}” matches no tracks right now."
            ))]));
        }
        let ids: Vec<u64> = tracks.iter().map(|t| t.id).collect();
        self.emit_play(ids, "play_now")?;
        ok(
            json!({ "playing": name, "track_count": tracks.len(), "total_duration_seconds": tracks.iter().map(|t| t.duration).sum::<u32>() }),
        )
    }

    #[rmcp::tool(
        name = "get_party_status",
        description = "Whether a listening party is active: role, guest count and the current track. Read-only. Example: {}."
    )]
    async fn get_party_status(
        &self,
        Parameters(_): Parameters<NoArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.app_handle.state::<AppState>();
        let s = state.mcp_state.read().await;
        ok(s.party
            .clone()
            .unwrap_or_else(|| json!({ "active": false })))
    }

    #[rmcp::tool(
        name = "play_query",
        description = "Play what the user asked for in plain words (\"the new Kendrick album\", \"that Daft Punk song from Tron\", \"lofi playlist\"). Searches tracks, albums, artists and playlists, scores the matches and plays the best one when confidence ≥ 0.7 (or play_anyway=true); otherwise returns the alternatives without playing. Params: query, play_anyway, target_duration_minutes. Example: {\"query\":\"lofi playlist\"}."
    )]
    async fn play_query(
        &self,
        Parameters(a): Parameters<PlayQueryArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let intent = query::parse(&a.query);
        if intent.text.is_empty() {
            return Err(bad("Say what to play, e.g. \"the new Kendrick album\"."));
        }
        let r = self.search_all(&intent.text, 20).await?;
        let mut hits: Vec<Hit> = Vec::new();
        let year_of = |d: &Option<String>| {
            d.as_deref()
                .and_then(|d| d.get(..4))
                .and_then(|y| y.parse().ok())
        };
        for t in &r.tracks {
            hits.push(Hit {
                kind: Kind::Track,
                id: t.id.to_string(),
                title: t.title.clone(),
                artist: t
                    .artist
                    .as_ref()
                    .map(|x| x.name.clone())
                    .unwrap_or_default(),
                extra: t
                    .album
                    .as_ref()
                    .map(|x| x.title.clone())
                    .unwrap_or_default(),
                popularity: t.popularity.unwrap_or(0),
                year: t.album.as_ref().and_then(|x| year_of(&x.release_date)),
            });
        }
        for x in &r.albums {
            hits.push(Hit {
                kind: Kind::Album,
                id: x.id.to_string(),
                title: x.title.clone(),
                artist: x
                    .artist
                    .as_ref()
                    .map(|a| a.name.clone())
                    .unwrap_or_default(),
                extra: String::new(),
                popularity: x.popularity.unwrap_or(0),
                year: year_of(&x.release_date),
            });
        }
        for x in &r.artists {
            hits.push(Hit {
                kind: Kind::Artist,
                id: x.id.to_string(),
                title: x.name.clone(),
                artist: x.name.clone(),
                extra: String::new(),
                popularity: 50,
                year: None,
            });
        }
        for x in &r.playlists {
            hits.push(Hit {
                kind: Kind::Playlist,
                id: x.uuid.clone(),
                title: x.title.clone(),
                artist: x
                    .creator
                    .as_ref()
                    .and_then(|c| c.name.clone())
                    .unwrap_or_default(),
                extra: String::new(),
                popularity: 40,
                year: None,
            });
        }
        let this_year = 1970 + (crate::now_secs() / 31_556_952) as i32;
        let ranked = query::rank(&intent, hits, this_year);
        let confidence = query::confidence(&ranked);
        let desc = |r: &query::Ranked| {
            json!({ "type": r.hit.kind.label(), "id": r.hit.id, "title": r.hit.title,
            "artist": r.hit.artist, "score": (r.score * 100.0).round() / 100.0 })
        };
        let Some(best) = ranked.first() else {
            return Err(bad(format!("Nothing on TIDAL matched “{}”.", a.query)));
        };
        let alternatives: Vec<Value> = ranked.iter().skip(1).take(3).map(desc).collect();
        let play = query::should_play(confidence, a.play_anyway.unwrap_or(false));
        let mut out = json!({ "chosen": desc(best), "confidence": (confidence * 100.0).round() / 100.0,
                              "alternatives": alternatives, "played": false });
        if !play {
            out["note"] = json!("Not confident enough to play. Pick one of the alternatives, or call again with play_anyway=true.");
            return ok(out);
        }
        match best.hit.kind {
            Kind::Track => {
                let id: u64 = best.hit.id.parse().unwrap_or(0);
                self.emit_play(vec![id], "play_now")?;
            }
            Kind::Album | Kind::Playlist | Kind::Artist => {
                let st = match best.hit.kind {
                    Kind::Album => "album",
                    Kind::Playlist => "playlist",
                    _ => "artist",
                };
                self.app_handle
                    .emit(
                        EV_PLAY_SOURCE,
                        crate::mcp::events::PlaySourcePayload {
                            source_type: st.into(),
                            id: best.hit.id.clone(),
                        },
                    )
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
            }
        }
        out["played"] = json!(true);
        if let Some(target) = a.target_duration_minutes {
            out["note"] = json!(format!("Playing; aim of about {target} minutes noted — use create_playlist_from_tracks for an exact-length playlist."));
        }
        ok(out)
    }

    #[rmcp::tool(
        name = "batch",
        description = "Run up to 20 tool calls in order in one request. Each call is checked exactly as if made alone (read-only mode, confirm, approval). Params: calls [{tool, arguments}], stop_on_error (default true). Example: {\"calls\":[{\"tool\":\"search_tracks\",\"arguments\":{\"query\":\"halo\"}},{\"tool\":\"pause\"}]}."
    )]
    async fn batch(
        &self,
        Parameters(a): Parameters<BatchArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        if a.calls.is_empty() || a.calls.len() > 20 {
            return Err(bad("A batch holds 1 to 20 calls."));
        }
        let stop = a.stop_on_error.unwrap_or(true);
        let mut results = Vec::with_capacity(a.calls.len());
        let mut stopped = false;
        for (i, call) in a.calls.into_iter().enumerate() {
            if stopped {
                results.push(json!({ "index": i, "tool": call.tool, "skipped": true }));
                continue;
            }
            if call.tool == "batch" {
                results.push(
                    json!({ "index": i, "tool": "batch", "error": "batch cannot contain batch." }),
                );
                stopped = stop;
                continue;
            }
            let out = self
                .run_inner(&call.tool, call.arguments, ctx.clone())
                .await;
            match out {
                Ok(r) if r.is_error != Some(true) => {
                    let text = r
                        .content
                        .iter()
                        .find_map(|c| c.as_text().map(|t| t.text.clone()))
                        .unwrap_or_default();
                    let v = serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text));
                    results.push(json!({ "index": i, "tool": call.tool, "result": v }));
                }
                Ok(r) => {
                    let text = r
                        .content
                        .iter()
                        .find_map(|c| c.as_text().map(|t| t.text.clone()))
                        .unwrap_or_default();
                    results.push(json!({ "index": i, "tool": call.tool, "error": text }));
                    stopped = stop;
                }
                Err(e) => {
                    results.push(json!({ "index": i, "tool": call.tool, "error": e.message }));
                    stopped = stop;
                }
            }
        }
        ok(json!({ "results": results, "stopped_early": stopped }))
    }
}

// ---------- shared by several tools ----------

impl SlateMcpServer {
    async fn search_all(
        &self,
        q: &str,
        limit: u32,
    ) -> Result<crate::tidal_api::TidalSearchResults, ErrorData> {
        let state = self.app_handle.state::<AppState>();
        let mut c = state.tidal_client.lock().await;
        c.search(q, limit.clamp(1, 50)).await.map_err(ierr)
    }

    async fn queue_tracks(
        &self,
        a: TrackIdsArgs,
        action: &str,
    ) -> Result<CallToolResult, ErrorData> {
        if a.track_ids.is_empty() {
            return Err(bad("track_ids must not be empty."));
        }
        let mut ids: Vec<u64> = a.track_ids.into_iter().take(100).collect();
        let mut total = None;
        if a.target_duration_minutes.is_some() || ids.len() <= 50 {
            let found = self.lookup_tracks(&ids).await;
            let items: Vec<Item> = found
                .iter()
                .map(|t| Item {
                    id: t.id,
                    secs: t.duration,
                })
                .collect();
            if let Some(target) = a.target_duration_minutes {
                let (chosen, secs) = duration::build_to_target(&items, target);
                ids = chosen.iter().map(|i| i.id).collect();
                total = Some(secs);
            } else {
                total = Some(duration::total_secs(&items));
            }
        }
        if ids.is_empty() {
            return Err(bad("No track fit the requested length."));
        }
        let n = ids.len();
        self.emit_play(ids, action)?;
        ok(json!({ "status": action, "track_count": n, "total_duration_seconds": total }))
    }

    async fn match_items(&self, items: &[String]) -> Result<Vec<Value>, ErrorData> {
        if items.is_empty() || items.len() > 100 {
            return Err(bad("Give 1 to 100 items."));
        }
        let mut out = Vec::with_capacity(items.len());
        for (n, raw) in items.iter().enumerate() {
            if n > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            }
            let item = raw.trim();
            if let Ok(id) = item.parse::<u64>() {
                let found = self.lookup_tracks(&[id]).await;
                out.push(match found.first() {
                    Some(t) => json!({ "query": item, "id": t.id, "title": t.title, "duration_seconds": t.duration }),
                    None => json!({ "query": item, "id": null }),
                });
                continue;
            }
            // ISRC (12 alphanumerics) or free text: both go through search.
            let r = self.search_all(item, 5).await;
            let pick = match r {
                Ok(r) => {
                    let intent = query::parse(&item.replace(" - ", " "));
                    let hits: Vec<Hit> = r
                        .tracks
                        .iter()
                        .map(|t| Hit {
                            kind: Kind::Track,
                            id: t.id.to_string(),
                            title: t.title.clone(),
                            artist: t
                                .artist
                                .as_ref()
                                .map(|x| x.name.clone())
                                .unwrap_or_default(),
                            extra: String::new(),
                            popularity: t.popularity.unwrap_or(0),
                            year: None,
                        })
                        .collect();
                    let isrc_hit = r.tracks.iter().find(|t| {
                        t.isrc
                            .as_deref()
                            .is_some_and(|i| i.eq_ignore_ascii_case(item))
                    });
                    let best = isrc_hit.map(|t| t.id.to_string()).or_else(|| {
                        let ranked = query::rank(&intent, hits, 2025);
                        ranked
                            .first()
                            .filter(|r| r.score >= 0.5)
                            .map(|r| r.hit.id.clone())
                    });
                    best.and_then(|id| r.tracks.into_iter().find(|t| t.id.to_string() == id))
                }
                Err(e) if e.message.contains("rate-limiting") => return Err(e),
                Err(_) => None,
            };
            out.push(match pick {
                Some(t) => json!({ "query": item, "id": t.id, "title": t.title, "duration_seconds": t.duration }),
                None => json!({ "query": item, "id": null }),
            });
        }
        Ok(out)
    }
}
