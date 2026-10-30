//! What the player tells the MCP server. Every function starts with one atomic check, so with
//! the server off the player pays nothing: no lock, no allocation, no formatting.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::activity::Undo;
use super::runtime::{self, Runtime, URI_NOW_PLAYING, URI_QUEUE};
use super::state_mirror::QueueTrackSnapshot;
use super::stats::PlayRec;
use crate::AppState;

/// The server's runtime, only when it is running.
fn active(state: &AppState) -> Option<Arc<Runtime>> {
    if !state.mcp_active.load(Ordering::Relaxed) {
        return None;
    }
    runtime::current(state)
}

/// A different track started. `last_position` is where the previous one stopped.
pub fn track_changed(
    state: &AppState,
    id: u64,
    title: &str,
    artist: &str,
    duration_secs: u32,
    last_position: u32,
) {
    let Some(rt) = active(state) else { return };
    rt.record_play(PlayRec {
        t: crate::now_secs(),
        id,
        title: title.to_string(),
        artist: artist.to_string(),
    });
    let events = rt
        .with_watch(|w| w.track_changed(id, duration_secs, last_position))
        .unwrap_or_default();
    for e in &events {
        rt.publish_event(e);
    }
    rt.notify_changed(URI_NOW_PLAYING);
}

/// Play state or position jumped.
pub fn now_playing_changed(state: &AppState) {
    if let Some(rt) = active(state) {
        rt.notify_changed(URI_NOW_PLAYING);
    }
}

pub fn seeked(state: &AppState, to_secs: u32) {
    let Some(rt) = active(state) else { return };
    if let Some(Some(e)) = rt.with_watch(|w| w.seeked(to_secs)) {
        rt.publish_event(&e);
    }
}

/// The queue changed; `len` is how many tracks are upcoming.
pub fn queue_changed(state: &AppState, len: usize, repeat_all: bool) {
    let Some(rt) = active(state) else { return };
    if let Some(Some(e)) = rt.with_watch(|w| w.queue_len(len, repeat_all)) {
        rt.publish_event(&e);
    }
    rt.notify_changed(URI_QUEUE);
}

/// Whether anything is subscribed to `queue_low` (used by the agent-DJ wait).
pub fn dj_waiting(state: &AppState) -> bool {
    active(state).is_some_and(|rt| rt.dj.load(Ordering::Relaxed) && rt.any_subscribers())
}

/// Snapshot rows for the mirror.
pub fn snapshot(id: u64, title: &str, artist: &str, duration: u32) -> QueueTrackSnapshot {
    QueueTrackSnapshot {
        id,
        title: title.to_string(),
        artist: artist.to_string(),
        duration_seconds: duration,
    }
}

/// Re-exported for tests of the dormancy contract.
pub fn is_active(state: &AppState) -> bool {
    state.mcp_active.load(Ordering::Relaxed)
}

/// Undo operations the app carries out (queue changes happen in the app).
pub fn undo_event(op: &Undo) -> Option<(&'static str, serde_json::Value)> {
    use super::events::{EV_PLAY_TRACKS, EV_REMOVE_FROM_QUEUE};
    match op {
        Undo::RemoveFromQueue { track_ids } => Some((
            EV_REMOVE_FROM_QUEUE,
            serde_json::json!({ "trackIds": track_ids }),
        )),
        Undo::RestoreQueue { track_ids, .. } => Some((
            EV_PLAY_TRACKS,
            serde_json::json!({ "trackIds": track_ids, "action": "queue" }),
        )),
        _ => None,
    }
}
