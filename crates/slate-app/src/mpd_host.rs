//! The MPD server's view of Slate (`slate_core::mpd`): it reads the controller's queue and
//! carries commands out on the same actions MPRIS, the media keys and the MCP tools use
//! (`Control`, `queue_play`, `queue_remove`, …). There is no second controller.
//!
//! The MPD playlist is the playing track (position 0) followed by "next up" and the queue.
//! `clear` also stops playback and drops the playing track, as in MPD.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use slate_core::commands::{library as lib_cmds, metadata, playback, search, utility};
use slate_core::mpd::{proto, Action, BoxFut, Host, HostError, PlayState, Song, Status};
use slate_core::queue::{Repeat, Track, TrackRef};

use crate::backend::Backend;
use crate::control::Control;
use crate::library::SourceKind;

/// Most tracks a search returns to a client.
const SEARCH_LIMIT: u32 = 30;
const PLAYLIST_PAGE: u32 = 50;
const MAX_PLAYLIST_PAGES: u32 = 20;
/// Cover size MPD clients get.
const COVER_PX: u32 = 640;

pub struct AppMpd {
    backend: Backend,
}

impl AppMpd {
    pub fn new(backend: Backend) -> Arc<Self> {
        Arc::new(Self { backend })
    }
}

pub(crate) fn song_of(t: &Track) -> Song {
    let version = t.version();
    Song {
        id: t.id,
        title: if version.is_empty() {
            t.title().to_string()
        } else {
            format!("{} ({version})", t.title())
        },
        artist: t.artist_name().to_string(),
        album: t.album_title().to_string(),
        album_artist: String::new(),
        duration: f64::from(t.duration_secs()),
        track: t.track_number,
        date: String::new(),
    }
}

/// Order-sensitive hash of the queue's track ids (and their count).
fn queue_hash(ids: impl Iterator<Item = u64>) -> u64 {
    ids.fold(0xcbf2_9ce4_8422_2325u64, |h, id| {
        (h ^ id).wrapping_mul(0x0100_0000_01b3)
    })
}

impl AppMpd {
    /// The playing track and what follows, as shared handles: every `status` asks for this,
    /// so copying the tracks would copy the whole queue on each poll.
    async fn queue_tracks(&self) -> (Option<TrackRef>, Vec<TrackRef>) {
        match self.backend.controller() {
            Some(ctl) => {
                ctl.with_queue(|q| (q.current.clone(), q.upcoming().cloned().collect()))
                    .await
            }
            None => (None, Vec::new()),
        }
    }

    /// `(uuid, title)` of the user's playlists.
    async fn playlist_list(&self) -> Result<Vec<(String, String)>, HostError> {
        let (Some(state), Some(uid)) = (self.backend.state(), self.backend.user_id().await) else {
            return Err(HostError::system("not signed in"));
        };
        let mut out = Vec::new();
        for page in 0..MAX_PLAYLIST_PAGES {
            let p = lib_cmds::get_all_playlists(
                &state,
                self.backend.app.clone(),
                uid,
                page * PLAYLIST_PAGE,
                PLAYLIST_PAGE,
                "DATE".into(),
                "DESC".into(),
            )
            .await
            .map_err(|e| HostError::system(e.log_safe()))?;
            let got = p.items.len();
            out.extend(p.items.into_iter().map(|pl| (pl.uuid, pl.title)));
            if got == 0 || out.len() >= p.total_number_of_items as usize {
                break;
            }
        }
        Ok(out)
    }

    async fn playlist_tracks(&self, name: &str) -> Result<Vec<Track>, HostError> {
        let list = self.playlist_list().await?;
        let want = name.to_lowercase();
        let (uuid, _) = list
            .into_iter()
            .find(|(_, t)| t.to_lowercase() == want)
            .ok_or_else(|| HostError::no_exist("No such playlist"))?;
        let state = self
            .backend
            .state()
            .ok_or_else(|| HostError::system("not ready"))?;
        crate::pages::fetch_all(&state, &self.backend, &SourceKind::Playlist(uuid))
            .await
            .map_err(HostError::system)
    }

    /// Stops audio and waits for it, so a command that follows (`play` after `clear`) cannot
    /// be undone by a stop that was still queued.
    async fn stop_now(&self) {
        if let Some(state) = self.backend.state() {
            let _ = playback::stop_track(&state).await;
        }
        self.backend.shared.loaded.store(false, Ordering::Relaxed);
        self.backend.shared_set_playing(false);
    }

    /// Plays the queue entry `index` and waits until it has started.
    async fn play_index(&self, index: usize) {
        if let Some(ctl) = self.backend.controller() {
            let outcome = ctl.play_from_queue(index).await;
            self.backend.apply(outcome).await;
        }
    }

    async fn act_now(&self, action: Action) -> Result<(), HostError> {
        let b = &self.backend;
        let Some(ctl) = b.controller() else {
            return Err(HostError::system("not ready"));
        };
        match action {
            Action::Resume => {
                if ctl.with_queue(|q| q.current.is_some()).await {
                    b.handle_control(Control::Play);
                } else {
                    self.play_index(0).await;
                }
            }
            Action::Pause(true) => b.handle_control(Control::Pause),
            Action::Pause(false) => b.handle_control(Control::Play),
            Action::Stop => self.stop_now().await,
            Action::Next => b.next(),
            Action::Previous => b.previous(),
            Action::PlayPos(pos) => {
                let (has_current, len) = ctl
                    .with_queue(|q| (q.current.is_some(), q.upcoming().count()))
                    .await;
                let off = usize::from(has_current);
                if pos >= len + off {
                    return Err(HostError::no_exist("Bad song index"));
                }
                if has_current && pos == 0 {
                    let outcome = ctl.resume_current().await;
                    b.apply(outcome).await;
                } else {
                    self.play_index(pos - off).await;
                }
            }
            Action::Seek(secs) => b.seek_to_secs(secs as f32),
            Action::SetVolume(v) => b.set_volume(f32::from(v) / 100.0),
            Action::SetRandom(on) => b.handle_control(Control::SetShuffle(on)),
            Action::SetRepeat(on) => b.handle_control(Control::SetLoop(u8::from(on))),
            Action::Clear => {
                ctl.clear_all().await;
                self.stop_now().await;
                b.refresh_queue_info().await;
                b.save().await;
            }
            Action::Delete(pos) => {
                let (has_current, len) = ctl
                    .with_queue(|q| (q.current.is_some(), q.upcoming().count()))
                    .await;
                let off = usize::from(has_current);
                if pos >= len + off {
                    return Err(HostError::no_exist("Bad song index"));
                }
                if has_current && pos == 0 {
                    // Removing the playing song moves on to the next one.
                    b.next();
                } else {
                    ctl.remove_from_queue(pos - off).await;
                    b.refresh_queue_info().await;
                    b.save().await;
                }
            }
            Action::Move { from, to } => {
                let (has_current, len) = ctl
                    .with_queue(|q| (q.current.is_some(), q.upcoming().count()))
                    .await;
                let off = usize::from(has_current);
                if from < off || to < off {
                    return Err(HostError::arg("The playing song cannot be moved"));
                }
                if from >= len + off || to >= len + off {
                    return Err(HostError::no_exist("Bad song index"));
                }
                if ctl.move_in_queue(from - off, to - off).await {
                    b.refresh_queue_info().await;
                    b.save().await;
                }
            }
            Action::Add { uri, pos } => {
                let id =
                    proto::track_id_of(&uri).ok_or_else(|| HostError::no_exist("No such song"))?;
                let state = b.state().ok_or_else(|| HostError::system("not ready"))?;
                let v = metadata::get_track(&state, id)
                    .await
                    .map_err(|e| HostError::no_exist(e.log_safe()))?;
                let track = serde_json::from_value::<Track>(v)
                    .map_err(|_| HostError::no_exist("No such song"))?;
                match pos {
                    Some(p) => {
                        let has_current = ctl.with_queue(|q| q.current.is_some()).await;
                        ctl.insert_in_queue(p.saturating_sub(usize::from(has_current)), track)
                            .await;
                    }
                    None => ctl.add_to_queue(track, None).await,
                }
                b.refresh_queue_info().await;
                b.save().await;
            }
            Action::Load(name) => {
                let tracks = self.playlist_tracks(&name).await?;
                ctl.append_upcoming(tracks).await;
                b.refresh_queue_info().await;
                b.save().await;
            }
            Action::Shuffle => {
                ctl.shuffle_upcoming().await;
                b.refresh_queue_info().await;
                b.save().await;
            }
        }
        Ok(())
    }
}

impl Host for AppMpd {
    fn status(&self) -> BoxFut<'_, Status> {
        Box::pin(async move {
            let b = &self.backend;
            let (current, upcoming) = self.queue_tracks().await;
            let playing = b.shared.playing.load(Ordering::Relaxed);
            let loaded = b.shared.loaded.load(Ordering::Relaxed);
            let state = match (&current, playing, loaded) {
                (None, _, _) | (_, _, false) => PlayState::Stop,
                (Some(_), true, true) => PlayState::Play,
                (Some(_), false, true) => PlayState::Pause,
            };
            let off = usize::from(current.is_some());
            let (repeat, random) = match b.controller() {
                Some(ctl) => {
                    ctl.with_queue(|q| (q.repeat != Repeat::Off, q.shuffle))
                        .await
                }
                None => (false, false),
            };
            let volume = f32::from_bits(b.shared.volume.load(Ordering::Relaxed));
            Status {
                state,
                volume: (volume.clamp(0.0, 1.0) * 100.0).round() as u8,
                repeat,
                random,
                elapsed: f64::from(
                    f32::from_bits(b.shared.position.load(Ordering::Relaxed)).max(0.0),
                ),
                duration: f64::from(b.shared.duration().max(0.0)),
                queue_len: upcoming.len() + off,
                queue_hash: queue_hash(current.iter().chain(&upcoming).map(|t| t.id)),
                pos: current.as_ref().map(|_| 0),
                next: upcoming.first().map(|t| (off, t.id)),
                current: current.as_deref().map(song_of),
            }
        })
    }

    fn queue(&self) -> BoxFut<'_, Vec<Song>> {
        Box::pin(async move {
            let (current, upcoming) = self.queue_tracks().await;
            current
                .iter()
                .chain(&upcoming)
                .map(|t| song_of(t))
                .collect()
        })
    }

    fn act(&self, action: Action) -> BoxFut<'_, Result<(), HostError>> {
        Box::pin(self.act_now(action))
    }

    fn search(&self, text: String) -> BoxFut<'_, Result<Vec<Song>, HostError>> {
        Box::pin(async move {
            let state = self
                .backend
                .state()
                .ok_or_else(|| HostError::system("not ready"))?;
            let found = search::search_tidal(&state, text, SEARCH_LIMIT)
                .await
                .map_err(|e| HostError::system(e.log_safe()))?;
            Ok(found
                .tracks
                .iter()
                .filter_map(crate::library::to_track)
                .map(|t| song_of(&t))
                .collect())
        })
    }

    fn playlists(&self) -> BoxFut<'_, Result<Vec<String>, HostError>> {
        Box::pin(async move {
            Ok(self
                .playlist_list()
                .await?
                .into_iter()
                .map(|(_, title)| title)
                .collect())
        })
    }

    fn playlist_songs(&self, name: String) -> BoxFut<'_, Result<Vec<Song>, HostError>> {
        Box::pin(async move {
            Ok(self
                .playlist_tracks(&name)
                .await?
                .iter()
                .map(song_of)
                .collect())
        })
    }

    fn cover(&self, song_id: Option<u64>) -> BoxFut<'_, Option<Vec<u8>>> {
        Box::pin(async move {
            let state = self.backend.state()?;
            let (current, upcoming) = self.queue_tracks().await;
            let track = match song_id {
                None => current,
                Some(id) => current.into_iter().chain(upcoming).find(|t| t.id == id),
            }?;
            let url = crate::images::tidal_image_url(track.cover_id()?, COVER_PX);
            utility::get_image_bytes(&state, url).await.ok()
        })
    }
}

impl Backend {
    /// The player changed in a way MPD clients in `idle` care about. One atomic check when
    /// the MPD server is off; when on, bursts of changes are folded into one status read.
    pub(crate) fn mpd_changed(&self) {
        let Some(state) = self.state() else { return };
        if !state.mpd_active.load(Ordering::Relaxed) {
            return;
        }
        if self.shared.mpd_pending.swap(true, Ordering::AcqRel) {
            return;
        }
        let this = self.clone();
        self.rt.spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(60)).await;
            this.shared.mpd_pending.store(false, Ordering::Release);
            if let Some(host) = state.mpd_host.get() {
                let status = host.status().await;
                slate_core::commands::mpd::mpd_feed(&state, &status).await;
            }
        });
    }
}
