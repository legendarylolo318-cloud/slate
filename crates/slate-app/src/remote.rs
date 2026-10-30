//! The MCP server and the OBS overlay, seen from the app.
//!
//! MCP playback tools reach the app as `mcp:*` events and are carried out
//! here with the same actions as the player bar, MPRIS and media keys. What is
//! playing, and the queue, are published back: to the MCP state the
//! `get_now_playing` / `get_queue` tools read, and to the overlay's page.
//! Publishing is local (a lock and a broadcast); nothing here makes a TIDAL
//! request except `play_tracks`, which looks up the tracks it is given.

use std::sync::atomic::Ordering;

use serde_json::Value;
use slate_core::commands::{metadata, overlay as overlay_cmds};
use slate_core::mcp::events as ev;
use slate_core::mcp::{NowPlayingSnapshot, QueueTrackSnapshot};
use slate_core::overlay::OverlayTrackState;
use slate_core::queue::{Repeat, Source, Track};

use crate::backend::Backend;
use crate::control::Control;
use crate::library::SourceKind;

/// Most tracks one `play_tracks` call looks up (one request each).
const MAX_PLAY_TRACKS: usize = 100;

/// The playing track as the remote views need it.
#[derive(Clone, Debug, Default)]
pub(crate) struct NowInfo {
    pub id: u64,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub cover_url: Option<String>,
    pub quality: String,
}

impl NowInfo {
    pub fn of(track: &Track) -> Self {
        Self {
            id: track.id,
            title: track.title().to_string(),
            artist: track.artist_name().to_string(),
            album: track
                .extra
                .get("album")
                .and_then(|a| a.get("title"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            cover_url: track
                .cover_id()
                .map(|c| crate::images::tidal_image_url(c, 320)),
            quality: String::new(),
        }
    }
}

fn repeat_of(mode: &str) -> Option<Repeat> {
    match mode {
        "off" => Some(Repeat::Off),
        "all" => Some(Repeat::All),
        "one" => Some(Repeat::One),
        _ => None,
    }
}

impl Backend {
    /// An `mcp:*` event from the MCP server.
    pub(crate) fn mcp_event(&self, event: &str, payload: Value) {
        let playing = self.shared.playing.load(Ordering::Relaxed);
        match event {
            ev::EV_PAUSE if playing => self.toggle_play(),
            ev::EV_RESUME if !playing => self.toggle_play(),
            ev::EV_PAUSE | ev::EV_RESUME => {}
            ev::EV_SKIP_NEXT => self.next(),
            ev::EV_SKIP_PREVIOUS => self.previous(),
            ev::EV_SEEK => {
                if let Some(secs) = payload.get("positionSeconds").and_then(Value::as_f64) {
                    self.seek_to_secs(secs as f32);
                }
            }
            ev::EV_SET_VOLUME => {
                if let Some(level) = payload.get("level").and_then(Value::as_f64) {
                    self.set_volume(level as f32);
                }
            }
            ev::EV_TOGGLE_SHUFFLE => self.toggle_shuffle(),
            ev::EV_SET_REPEAT => {
                if let Some(mode) = payload
                    .get("mode")
                    .and_then(Value::as_str)
                    .and_then(repeat_of)
                {
                    self.handle_control(Control::SetLoop(mode.as_u8()));
                }
            }
            ev::EV_CLEAR_QUEUE => self.queue_clear(),
            ev::EV_MOVE_QUEUE_ITEM => {
                let n = |k: &str| payload.get(k).and_then(Value::as_u64).map(|v| v as usize);
                if let (Some(from), Some(to)) = (n("from"), n("to")) {
                    self.queue_move(from, to);
                }
            }
            slate_core::mcp::runtime::EV_APPROVAL_REQUEST => self.approval_requested(&payload),
            slate_core::mcp::runtime::EV_APPROVAL_CLOSED => self.approval_closed(&payload),
            ev::EV_REMOVE_FROM_QUEUE => {
                let ids = payload
                    .get("trackIds")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_u64).collect::<Vec<_>>())
                    .unwrap_or_default();
                if let Some(id) = payload.get("trackId").and_then(Value::as_u64) {
                    self.remove_track_id_from_queue(id);
                }
                for id in ids {
                    self.remove_track_id_from_queue(id);
                }
            }
            ev::EV_PLAY_TRACKS => {
                let ids: Vec<u64> = payload
                    .get("trackIds")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_u64).collect())
                    .unwrap_or_default();
                let action = payload
                    .get("action")
                    .and_then(Value::as_str)
                    .unwrap_or("play_now")
                    .to_string();
                self.mcp_play_tracks(ids, action);
            }
            ev::EV_PLAY_SOURCE | ev::EV_SHUFFLE_SOURCE => {
                let kind = payload
                    .get("sourceType")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let id = payload
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                self.mcp_play_source(kind, id, event == ev::EV_SHUFFLE_SOURCE);
            }
            ev::EV_PLAYLIST_CREATED
            | ev::EV_PLAYLIST_UPDATED
            | ev::EV_PLAYLIST_DELETED
            | ev::EV_PLAYLIST_TRACKS_CHANGED => self.load_library(),
            ev::EV_FAVORITE_CHANGED => self.load_favorites(),
            _ => tracing::debug!("unhandled MCP event {event}"),
        }
    }

    fn remove_track_id_from_queue(&self, id: u64) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(ctl) = this.controller() else { return };
            let index = ctl
                .with_queue(|q| {
                    q.manual
                        .iter()
                        .chain(q.queue.iter())
                        .position(|t| t.id == id)
                })
                .await;
            if let Some(index) = index {
                this.queue_remove(index);
            }
        });
    }

    fn mcp_play_tracks(&self, ids: Vec<u64>, action: String) {
        let this = self.clone();
        self.rt.spawn(async move {
            let (Some(state), Some(ctl)) = (this.state(), this.controller()) else {
                return;
            };
            let mut tracks = Vec::new();
            for id in ids.into_iter().take(MAX_PLAY_TRACKS) {
                match metadata::get_track(&state, id).await {
                    Ok(v) => {
                        if let Ok(t) = serde_json::from_value::<Track>(v) {
                            tracks.push(t);
                        }
                    }
                    // A rate limit will not clear for the rest of the list.
                    Err(e) if e.is_rate_limited() => break,
                    Err(e) => tracing::debug!("MCP play_tracks: {}", e.log_safe()),
                }
            }
            if tracks.is_empty() {
                return;
            }
            match action.as_str() {
                "queue" => {
                    for t in tracks {
                        ctl.add_to_queue(t, None).await;
                    }
                    // An agent DJ was holding the autoplay radio back: continue from these.
                    this.dj_refilled();
                }
                "play_next" => {
                    // Inserted one by one at the front: reverse keeps their order.
                    for t in tracks.into_iter().rev() {
                        ctl.play_next_in_queue(t, None).await;
                    }
                }
                _ => {
                    let outcome = ctl.play_all(tracks, None, false, false).await;
                    this.apply(outcome).await;
                    return;
                }
            }
            this.refresh_queue_info().await;
            this.save().await;
        });
    }

    fn mcp_play_source(&self, kind: String, id: String, shuffle: bool) {
        let this = self.clone();
        self.rt.spawn(async move {
            let (Some(state), Some(ctl)) = (this.state(), this.controller()) else {
                return;
            };
            let all = match kind.as_str() {
                "album" => match id.parse() {
                    Ok(n) => crate::pages::fetch_all(&state, &this, &SourceKind::Album(n)).await,
                    Err(_) => return,
                },
                "playlist" => {
                    crate::pages::fetch_all(&state, &this, &SourceKind::Playlist(id.clone())).await
                }
                "mix" => crate::pages::fetch_all(&state, &this, &SourceKind::Mix(id.clone())).await,
                "artist" => {
                    let Ok(n) = id.parse::<u64>() else { return };
                    slate_core::commands::pages::get_artist_top_tracks(
                        &state,
                        this.app.clone(),
                        n,
                        50,
                    )
                    .await
                    .map(|tracks| {
                        tracks
                            .iter()
                            .filter_map(crate::library::to_track)
                            .collect::<Vec<_>>()
                    })
                    .map_err(|e| e.log_safe())
                }
                _ => return,
            };
            let all = match all {
                Ok(a) => a,
                Err(e) => {
                    this.ui(move |w| w.set_error(e.into()));
                    return;
                }
            };
            let source = Source {
                kind: kind.clone(),
                id: Value::from(id),
                name: String::new(),
                image: None,
                subtitle: None,
                mix_type: None,
                tracks: all.clone(),
            };
            let album = kind == "album" && !shuffle;
            let outcome = ctl.play_all(all, Some(source), album, shuffle).await;
            if shuffle {
                let on = ctl.with_queue(|q| q.shuffle).await;
                this.shared.shuffle.store(on, Ordering::Relaxed);
                this.ui(move |w| w.set_shuffle(on));
            }
            this.apply(outcome).await;
        });
    }

    /// Records the playing track for the remote views and publishes it.
    pub(crate) fn set_now(&self, now: Option<NowInfo>) {
        if let Ok(mut slot) = self.shared.now.lock() {
            *slot = now;
        }
        self.publish_now(true);
    }

    /// The stream quality became known after the track started.
    pub(crate) fn set_now_quality(&self, quality: String) {
        if let Ok(mut slot) = self.shared.now.lock() {
            if let Some(now) = slot.as_mut() {
                now.quality = quality;
            }
        }
        self.publish_now(true);
    }

    /// Publishes the current state to MCP and, when `overlay`, to the overlay
    /// page too. The playback tick passes `false`: the page animates progress
    /// itself and only needs changes of track, play state and position jumps.
    pub(crate) fn publish_now(&self, overlay: bool) {
        let Some(state) = self.state() else { return };
        let now = self.shared.now.lock().ok().and_then(|n| n.clone());
        let playing = self.shared.playing.load(Ordering::Relaxed);
        let position = f32::from_bits(self.shared.position.load(Ordering::Relaxed)).max(0.0);
        let duration = self.shared.duration().max(0.0);
        self.rt.spawn(async move {
            let snapshot = now.as_ref().map(|n| NowPlayingSnapshot {
                track_id: Some(n.id),
                title: n.title.clone(),
                artist: n.artist.clone(),
                album: (!n.album.is_empty()).then(|| n.album.clone()),
                duration_seconds: duration.round() as u32,
                position_seconds: position.round() as u32,
                is_playing: playing,
            });
            if state.mcp_active.load(Ordering::Relaxed) {
                state.mcp_state.write().await.now_playing = snapshot;
                slate_core::mcp::hooks::now_playing_changed(&state);
            }
            if overlay {
                let track = now.map(|n| OverlayTrackState {
                    title: n.title,
                    artist: n.artist,
                    album: (!n.album.is_empty()).then_some(n.album),
                    cover_url: n.cover_url,
                    is_playing: playing,
                    position_seconds: f64::from(position),
                    duration_seconds: f64::from(duration),
                    quality: n.quality,
                });
                let _ = overlay_cmds::overlay_publish_state(&state, track).await;
            }
        });
    }

    /// The queue (manual "next up" first, then the rest) for `get_queue`.
    pub(crate) async fn publish_queue_to_mcp(&self) {
        let (Some(state), Some(ctl)) = (self.state(), self.controller()) else {
            return;
        };
        // Nothing is collected, cloned or locked unless an MCP server is running.
        if !state.mcp_active.load(Ordering::Relaxed) {
            return;
        }
        let snap = |t: &Track| {
            slate_core::mcp::hooks::snapshot(
                t.id,
                t.title(),
                t.artist_name(),
                t.duration_secs() as u32,
            )
        };
        let (queue, history, repeat_all) = ctl
            .with_queue(|q| {
                (
                    q.manual
                        .iter()
                        .chain(q.queue.iter())
                        .map(snap)
                        .collect::<Vec<QueueTrackSnapshot>>(),
                    q.history
                        .iter()
                        .rev()
                        .take(50)
                        .map(snap)
                        .collect::<Vec<_>>(),
                    q.repeat == Repeat::All,
                )
            })
            .await;
        let len = queue.len();
        {
            let mut s = state.mcp_state.write().await;
            s.queue = queue;
            s.history = history;
        }
        slate_core::mcp::hooks::queue_changed(&state, len, repeat_all);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeat_modes_map_to_the_player() {
        assert_eq!(repeat_of("off"), Some(Repeat::Off));
        assert_eq!(repeat_of("all"), Some(Repeat::All));
        assert_eq!(repeat_of("one"), Some(Repeat::One));
        assert_eq!(repeat_of("loop"), None);
    }

    #[test]
    fn now_info_reads_the_track() {
        let mut t = Track::new(5);
        if let Value::Object(m) = serde_json::json!({
            "title": "Song",
            "artist": {"name": "Band"},
            "album": {"title": "Record", "cover": "ab-cd"},
        }) {
            t.extra = m;
        }
        let n = NowInfo::of(&t);
        assert_eq!(
            (n.id, n.title.as_str(), n.artist.as_str()),
            (5, "Song", "Band")
        );
        assert_eq!(n.album, "Record");
    }
}
