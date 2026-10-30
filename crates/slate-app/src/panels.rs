//! Queue and lyrics side panels, plus per-row actions.

use std::sync::atomic::Ordering;

use slate_core::commands::metadata;
use slate_core::lyrics::{self, Line};
use slate_core::queue::Track;
use slint::{Model, ModelRc, VecModel};

use crate::backend::Backend;
use crate::bootstrap::{Choice, LyricLine, QueueRow};
use crate::pages::Route;

const MAX_QUEUE_ROWS: usize = 300;

impl Backend {
    // ---------- queue panel ----------

    pub async fn publish_queue(&self) {
        self.publish_queue_to_mcp().await;
        let Some(ctl) = self.controller() else { return };
        let rows: Vec<(String, String, bool)> = ctl
            .with_queue(|q| {
                q.manual
                    .iter()
                    .map(|t| (t, true))
                    .chain(q.queue.iter().map(|t| (t, false)))
                    .take(MAX_QUEUE_ROWS)
                    .map(|(t, manual)| (t.title().to_string(), t.artist_name().to_string(), manual))
                    .collect()
            })
            .await;
        self.ui(move |w| {
            let model: Vec<QueueRow> = rows
                .into_iter()
                .map(|(title, artist, manual)| QueueRow {
                    title: title.into(),
                    artist: artist.into(),
                    manual,
                })
                .collect();
            w.set_queue_rows(ModelRc::new(VecModel::from(model)));
        });
    }

    pub fn queue_play(&self, index: usize) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(ctl) = this.controller() else { return };
            let outcome = ctl.play_from_queue(index).await;
            this.apply(outcome).await;
        });
    }

    pub fn queue_remove(&self, index: usize) {
        let this = self.clone();
        self.rt.spawn(async move {
            if let Some(ctl) = this.controller() {
                ctl.remove_from_queue(index).await;
            }
            this.refresh_queue_info().await;
            this.save().await;
        });
    }

    /// Moves an upcoming track (drag-and-drop in the queue, or an agent).
    pub fn queue_move(&self, from: usize, to: usize) {
        let this = self.clone();
        self.rt.spawn(async move {
            if let Some(ctl) = this.controller() {
                if ctl.move_in_queue(from, to).await {
                    this.refresh_queue_info().await;
                    this.save().await;
                }
            }
        });
    }

    pub fn queue_clear(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            if let Some(ctl) = this.controller() {
                ctl.clear_queue().await;
            }
            this.refresh_queue_info().await;
            this.save().await;
        });
    }

    // ---------- lyrics ----------

    /// Loads lyrics for the track that just started.
    pub fn load_lyrics(&self, track_id: u64) {
        if let Ok(mut l) = self.lyrics.lock() {
            l.clear();
        }
        self.lyric_index.store(-1, Ordering::Relaxed);
        self.ui(|w| {
            w.set_lyric_lines(ModelRc::new(VecModel::<LyricLine>::default()));
            w.set_lyric_current(-1);
            w.set_lyrics_status("Loading lyrics…".into());
        });
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            let result = metadata::get_track_lyrics(&state, track_id).await;
            if this.current_track_id() != Some(track_id) {
                return;
            }
            let (lines, synced, status): (Vec<Line>, bool, String) = match result {
                Ok(l) => match (
                    l.subtitles.as_deref().map(lyrics::parse_lrc),
                    l.lyrics.as_deref(),
                ) {
                    (Some(synced), _) if !synced.is_empty() => (synced, true, String::new()),
                    (_, Some(plain)) if !plain.trim().is_empty() => (
                        lyrics::plain_lines(plain)
                            .into_iter()
                            .map(|t| Line { time: 0.0, text: t })
                            .collect(),
                        false,
                        "Not time-synced for this track.".into(),
                    ),
                    _ => (Vec::new(), false, "No lyrics for this track.".into()),
                },
                Err(_) => (Vec::new(), false, "No lyrics for this track.".into()),
            };
            let rows: Vec<String> = lines.iter().map(|l| l.text.clone()).collect();
            if let Ok(mut store) = this.lyrics.lock() {
                *store = if synced { lines } else { Vec::new() };
            }
            this.ui(move |w| {
                let model: Vec<LyricLine> = rows
                    .into_iter()
                    .map(|t| LyricLine {
                        text: t.into(),
                        active: false,
                    })
                    .collect();
                w.set_lyric_lines(ModelRc::new(VecModel::from(model)));
                w.set_lyrics_synced(synced);
                w.set_lyrics_status(status.into());
            });
        });
    }

    /// Called every tick with the playback position: highlights the current line.
    pub fn update_lyric_position(&self, pos: f32) {
        let Some(idx) = self
            .lyrics
            .lock()
            .ok()
            .map(|l| lyrics::current_index(&l, pos).map_or(-1, |i| i as i32))
        else {
            return;
        };
        let prev = self.lyric_index.swap(idx, Ordering::Relaxed);
        if prev == idx {
            return;
        }
        // Only while an MCP server runs: the line for `get_current_lyric_line`.
        if let Some(state) = self
            .state()
            .filter(|s| s.mcp_active.load(Ordering::Relaxed))
        {
            let line = usize::try_from(idx).ok().and_then(|i| {
                self.lyrics
                    .lock()
                    .ok()
                    .and_then(|l| l.get(i).map(|x| x.text.clone()))
            });
            self.rt.spawn(async move {
                state.mcp_state.write().await.lyric_line = line;
            });
        }
        self.ui(move |w| {
            let lines = w.get_lyric_lines();
            let Some(list) = lines.as_any().downcast_ref::<VecModel<LyricLine>>() else {
                return;
            };
            for (i, on) in [(prev, false), (idx, true)] {
                if let Ok(i) = usize::try_from(i) {
                    if let Some(mut row) = list.row_data(i) {
                        row.active = on;
                        list.set_row_data(i, row);
                    }
                }
            }
            w.set_lyric_current(idx);
        });
    }

    pub fn lyric_seek(&self, index: usize) {
        let t = self
            .lyrics
            .lock()
            .ok()
            .and_then(|l| l.get(index).map(|x| x.time));
        if let Some(t) = t {
            self.seek_to_secs(t);
        }
    }

    // ---------- row actions ----------

    pub fn row_action(&self, index: usize, action: &str) {
        let Some(track) = self.cached_track(index) else {
            return;
        };
        self.track_action(track, Some(index), action);
    }

    /// A context-menu action on `track`. `index` is its row in the open list, when it has one.
    pub(crate) fn track_action(&self, track: Track, index: Option<usize>, action: &str) {
        let action = action.to_string();
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(ctl) = this.controller() else { return };
            match action.as_str() {
                "favorite" => this.toggle_track_favorite(track.id),
                "remove-from-playlist" => {
                    if let Some(i) = index {
                        this.remove_from_playlist(this.real_index(i));
                    }
                }
                "play-next" => {
                    ctl.play_next_in_queue(track, None).await;
                    this.after_queue_edit("Will play next").await;
                }
                "queue" => {
                    ctl.add_to_queue(track, None).await;
                    this.after_queue_edit("Added to queue").await;
                }
                "album" => this.go_album(&track),
                "artist" => this.go_artist(&track),
                "copy-link" => this.copy_link(&track).await,
                "credits" => this.show_credits(&track),
                _ => {}
            }
        });
    }

    pub(crate) fn go_album(&self, track: &Track) {
        match album_id(track) {
            Some(id) => self.navigate(Route::Album(id)),
            None => self.notify("This track has no album page"),
        }
    }

    /// One artist: open the page. Several: ask which.
    pub(crate) fn go_artist(&self, track: &Track) {
        let mut artists = artists_of(track);
        match artists.len() {
            0 => self.notify("This track has no artist page"),
            1 => self.navigate(Route::Artist(artists.remove(0).0)),
            _ => self.ui(move |w| {
                let model: Vec<Choice> = artists
                    .into_iter()
                    .map(|(id, name)| Choice {
                        title: name.into(),
                        id: id.to_string().into(),
                    })
                    .collect();
                w.set_dialog_choices(ModelRc::new(VecModel::from(model)));
                w.set_dialog_kind("artists".into());
                w.set_dialog_open(true);
            }),
        }
    }

    fn go_to(&self, track: &Track, kind: &str) {
        match kind {
            "album" => self.go_album(track),
            "artist" => self.go_artist(track),
            _ => {}
        }
    }

    /// Title / artist click on the player bar, full-screen player or mini-player.
    pub fn go_current(&self, kind: &str) {
        let (this, kind) = (self.clone(), kind.to_string());
        self.rt.spawn(async move {
            let Some(ctl) = this.controller() else { return };
            if let Some(t) = ctl.with_queue(|q| q.current.clone()).await {
                this.go_to(&t, &kind);
            }
        });
    }

    /// "Go to" on a queue row.
    pub fn queue_go(&self, index: usize, kind: &str) {
        let (this, kind) = (self.clone(), kind.to_string());
        self.rt.spawn(async move {
            let Some(ctl) = this.controller() else { return };
            if let Some(t) = ctl.with_queue(|q| q.upcoming().nth(index).cloned()).await {
                this.go_to(&t, &kind);
            }
        });
    }

    async fn after_queue_edit(&self, notice: &str) {
        self.refresh_queue_info().await;
        self.save().await;
        self.notify(notice);
    }

    async fn copy_link(&self, track: &Track) {
        let url = format!("https://tidal.com/track/{}", track.id);
        let result = tokio::task::spawn_blocking(move || {
            arboard::Clipboard::new().and_then(|mut c| c.set_text(url))
        })
        .await;
        match result {
            Ok(Ok(())) => self.notify("Link copied"),
            _ => self.ui(|w| w.set_error("Could not access the clipboard.".into())),
        }
    }

    /// Brief confirmation banner.
    pub fn notify(&self, text: &str) {
        let text = text.to_string();
        self.ui(move |w| w.set_notice(text.into()));
        let this = self.clone();
        self.rt.spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            this.ui(|w| w.set_notice("".into()));
        });
    }
}

/// TIDAL album id of a queue track.
pub(crate) fn album_id(t: &Track) -> Option<u64> {
    t.extra.get("album")?.get("id")?.as_u64()
}

/// Every credited artist with a page, primary first, without repeats.
pub(crate) fn artists_of(t: &Track) -> Vec<(u64, String)> {
    let mut out: Vec<(u64, String)> = Vec::new();
    let one = |v: &serde_json::Value| {
        Some((v.get("id")?.as_u64()?, v.get("name")?.as_str()?.to_string()))
    };
    let primary = t.extra.get("artist").and_then(one);
    let all = t
        .extra
        .get("artists")
        .and_then(|a| a.as_array())
        .into_iter()
        .flatten()
        .filter_map(one);
    for a in primary.into_iter().chain(all) {
        if !out.iter().any(|(id, _)| *id == a.0) {
            out.push(a);
        }
    }
    out
}

#[cfg(test)]
mod goto_tests {
    use super::*;
    use serde_json::json;

    fn track(v: serde_json::Value) -> Track {
        let mut t = Track::new(1);
        if let serde_json::Value::Object(m) = v {
            t.extra = m;
        }
        t
    }

    #[test]
    fn artists_are_unique_and_primary_first() {
        let t = track(json!({
            "artist": {"id": 2, "name": "B"},
            "artists": [{"id": 1, "name": "A"}, {"id": 2, "name": "B"}],
        }));
        assert_eq!(artists_of(&t), vec![(2, "B".into()), (1, "A".into())]);
    }

    #[test]
    fn missing_ids_give_nothing() {
        let t = track(json!({"artist": {"name": "X"}, "album": {"title": "Y"}}));
        assert!(artists_of(&t).is_empty());
        assert_eq!(album_id(&t), None);
        assert_eq!(album_id(&track(json!({"album": {"id": 9}}))), Some(9));
    }
}
