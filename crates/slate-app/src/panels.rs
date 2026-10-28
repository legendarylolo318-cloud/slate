//! Queue and lyrics side panels, plus per-row actions.

use std::sync::atomic::Ordering;

use slate_core::commands::metadata;
use slate_core::lyrics::{self, Line};
use slate_core::queue::Track;
use slint::{Model, ModelRc, VecModel};

use crate::backend::Backend;
use crate::bootstrap::{LyricLine, QueueRow};
use crate::pages::Route;

const MAX_QUEUE_ROWS: usize = 300;

impl Backend {
    // ---------- queue panel ----------

    pub async fn publish_queue(&self) {
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
        let action = action.to_string();
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(ctl) = this.controller() else { return };
            match action.as_str() {
                "favorite" => this.toggle_track_favorite(track.id),
                "remove-from-playlist" => this.remove_from_playlist(index),
                "play-next" => {
                    ctl.play_next_in_queue(track, None).await;
                    this.after_queue_edit("Will play next").await;
                }
                "queue" => {
                    ctl.add_to_queue(track, None).await;
                    this.after_queue_edit("Added to queue").await;
                }
                "album" => {
                    if let Some(id) = track
                        .extra
                        .get("album")
                        .and_then(|a| a.get("id"))
                        .and_then(|v| v.as_u64())
                    {
                        this.navigate(Route::Album(id));
                    }
                }
                "artist" => {
                    let id = track
                        .extra
                        .get("artist")
                        .and_then(|a| a.get("id"))
                        .and_then(|v| v.as_u64())
                        .or_else(|| {
                            track
                                .extra
                                .get("artists")
                                .and_then(|a| a.get(0))
                                .and_then(|a| a.get("id"))
                                .and_then(|v| v.as_u64())
                        });
                    if let Some(id) = id {
                        this.navigate(Route::Artist(id));
                    }
                }
                "copy-link" => this.copy_link(&track).await,
                _ => {}
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
