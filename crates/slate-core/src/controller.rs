//! Playback controller: drives [`Queue`] against the audio engine.
//!
//! Owns the orchestration the React layer used to do around `playTrack` /
//! `playNext` / `playPrevious`: device-busy retry, skipping unplayable tracks
//! with a failure cap, re-queueing on transient failures, autoplay radio and
//! queue persistence. Frontends call these methods and render the result.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use crate::commands::{pages, playback};
use crate::queue::{Next, Previous, Queue, Repeat, Skip, Snapshot, Source, Track};
use crate::{AppState, SlateError};

const DEVICE_RETRY_DELAY: Duration = Duration::from_millis(500);
const DEVICE_MAX_RETRIES: u32 = 10;
/// Consecutive unplayable tracks before auto-advance gives up.
pub const MAX_CONSECUTIVE_PLAY_FAILS: u32 = 3;

/// Content preferences applied when picking the next track.
#[derive(Debug, Clone, Copy)]
pub struct ContentPrefs {
    pub allow_explicit: bool,
    pub allow_ai: bool,
}

impl Default for ContentPrefs {
    fn default() -> Self {
        Self {
            allow_explicit: true,
            allow_ai: true,
        }
    }
}

impl ContentPrefs {
    pub fn blocks(&self, t: &Track) -> bool {
        let flag = |k: &str| t.extra.get(k).and_then(serde_json::Value::as_bool);
        (!self.allow_explicit && flag("explicit") == Some(true))
            || (!self.allow_ai && flag("ai") == Some(true))
    }
}

/// How a start attempt ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayFailure {
    /// This item can never play (removed, region-locked, …).
    Unplayable,
    /// Proxy settings block all traffic; skipping tracks cannot help.
    Blocked(String),
    Network,
    RateLimited,
    Transient(String),
}

impl PlayFailure {
    fn classify(e: &SlateError) -> Self {
        if let SlateError::ProxyBlocked { reason } = e {
            Self::Blocked(reason.clone())
        } else if e.is_network() {
            Self::Network
        } else if e.is_rate_limited() {
            Self::RateLimited
        } else if e.is_terminal_unplayable() {
            Self::Unplayable
        } else {
            Self::Transient(e.log_safe())
        }
    }
}

/// What a navigation call ended up doing.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// A track is now playing.
    Playing(Track),
    /// The current track restarted from 0.
    Restarted,
    /// Nothing left to play; playback stopped.
    Stopped,
    /// Several tracks in a row were unplayable; playback stopped.
    GaveUp,
    /// Playback could not start; the track stays queued for a retry.
    Failed(PlayFailure),
    /// Nothing to do (e.g. previous at the very start with no history).
    Nothing,
}

pub struct Controller {
    state: Arc<AppState>,
    queue: Mutex<Queue>,
    prefs: std::sync::Mutex<ContentPrefs>,
}

fn now_secs() -> i64 {
    i64::try_from(crate::now_secs()).unwrap_or(i64::MAX)
}

impl Controller {
    pub fn new(state: Arc<AppState>) -> Self {
        Self {
            state,
            queue: Mutex::new(Queue::new()),
            prefs: std::sync::Mutex::new(ContentPrefs::default()),
        }
    }

    pub fn set_content_prefs(&self, prefs: ContentPrefs) {
        if let Ok(mut g) = self.prefs.lock() {
            *g = prefs;
        }
    }

    fn prefs(&self) -> ContentPrefs {
        self.prefs.lock().map(|g| *g).unwrap_or_default()
    }

    fn classify(&self, t: &Track) -> Option<Skip> {
        if self.prefs().blocks(t) {
            Some(Skip::Blocked)
        } else if t.is_unavailable(now_secs()) {
            Some(Skip::Unavailable)
        } else {
            None
        }
    }

    /// Read-only access for rendering the queue view.
    pub async fn with_queue<R>(&self, f: impl FnOnce(&Queue) -> R) -> R {
        f(&*self.queue.lock().await)
    }

    // ---------- starting playback ----------

    /// Start `track` right now without touching queue contents. On success the
    /// previous track moves to history and `track` becomes current.
    async fn start(
        &self,
        q: &mut Queue,
        track: Track,
        chosen_by_user: bool,
    ) -> Result<(), PlayFailure> {
        let _ = chosen_by_user; // consumed by scrobbling in a later step
        let use_gain = q.use_track_gain;
        let mut attempt = 0;
        loop {
            match playback::play_tidal_track(&self.state, track.id, use_gain).await {
                Ok(_) => break,
                Err(SlateError::Audio(m)) if m == "device_busy" && attempt < DEVICE_MAX_RETRIES => {
                    attempt += 1;
                    tokio::time::sleep(DEVICE_RETRY_DELAY).await;
                }
                Err(e) => return Err(PlayFailure::classify(&e)),
            }
        }
        q.begin(track, false);
        Ok(())
    }

    /// Play a single track, discarding the queue.
    pub async fn play_single(&self, track: Track) -> Outcome {
        let mut q = self.queue.lock().await;
        if self.prefs().blocks(&track) {
            return Outcome::Failed(PlayFailure::Unplayable);
        }
        q.set_tracks(Vec::new(), None, false);
        self.finish_start(&mut q, track, true).await
    }

    /// Play `track` from `all`, queueing the remainder.
    pub async fn play_from_source(
        &self,
        track: Track,
        all: Vec<Track>,
        source: Option<Source>,
        album_mode: bool,
    ) -> Outcome {
        let mut q = self.queue.lock().await;
        if self.prefs().blocks(&track) {
            return Outcome::Failed(PlayFailure::Unplayable);
        }
        let all: Vec<Track> = all
            .into_iter()
            .filter(|t| !self.prefs().blocks(t))
            .collect();
        let track = q.play_from_source(track, all, source, album_mode, &mut rand::rng());
        self.finish_start(&mut q, track, true).await
    }

    /// Play everything from the top (or shuffled).
    pub async fn play_all(
        &self,
        all: Vec<Track>,
        source: Option<Source>,
        album_mode: bool,
        shuffle: bool,
    ) -> Outcome {
        let mut q = self.queue.lock().await;
        let eligible: Vec<Track> = all
            .into_iter()
            .filter(|t| self.classify(t).is_none())
            .collect();
        let Some(first) = q.play_all(eligible, source, album_mode, shuffle, &mut rand::rng())
        else {
            return Outcome::Nothing;
        };
        self.finish_start(&mut q, first, true).await
    }

    /// Start `track`; if it turns out to be unplayable, continue down the queue.
    async fn finish_start(&self, q: &mut Queue, track: Track, chosen: bool) -> Outcome {
        match self.start(q, track.clone(), chosen).await {
            Ok(()) => Outcome::Playing(track),
            Err(PlayFailure::Unplayable) => self.advance(q, true).await,
            Err(PlayFailure::RateLimited) => {
                q.requeue_head(track);
                Outcome::Failed(PlayFailure::RateLimited)
            }
            Err(f) => Outcome::Failed(f),
        }
    }

    pub async fn play_from_queue(&self, index: usize) -> Outcome {
        let mut q = self.queue.lock().await;
        let Some(track) = q.take_at(index) else {
            return Outcome::Nothing;
        };
        if self.classify(&track).is_some() {
            return Outcome::Failed(PlayFailure::Unplayable);
        }
        match self.start(&mut q, track.clone(), true).await {
            Ok(()) => Outcome::Playing(track),
            Err(f) => Outcome::Failed(f),
        }
    }

    // ---------- navigation ----------

    /// Track ended on its own (`explicit == false`) or the user pressed Next.
    pub async fn next(&self, explicit: bool) -> Outcome {
        if !explicit {
            crate::startup::on_track_finished(&self.state).await;
        }
        let mut q = self.queue.lock().await;
        // Stop first so a stale `track-finished` from the old pipeline cannot
        // advance the queue a second time.
        let _ = playback::stop_track(&self.state).await;
        self.advance(&mut q, explicit).await
    }

    async fn advance(&self, q: &mut Queue, explicit: bool) -> Outcome {
        let mut failures = 0u32;
        loop {
            let (next, dropped) = q.next(explicit, &mut rand::rng(), |t| self.classify(t));
            failures += u32::try_from(dropped.len()).unwrap_or(u32::MAX);
            if failures >= MAX_CONSECUTIVE_PLAY_FAILS {
                return Outcome::GaveUp;
            }
            match next {
                Next::Stop => return Outcome::Stopped,
                Next::ReplayCurrent => {
                    let Some(cur) = q.current.clone() else {
                        return Outcome::Stopped;
                    };
                    return match playback::play_tidal_track(&self.state, cur.id, q.use_track_gain)
                        .await
                    {
                        Ok(_) => Outcome::Playing(cur),
                        Err(e) => Outcome::Failed(PlayFailure::classify(&e)),
                    };
                }
                Next::NeedAutoplay => match self.autoplay(q).await {
                    Some(first) => match self.start(q, first.clone(), false).await {
                        Ok(()) => return Outcome::Playing(first),
                        Err(PlayFailure::Unplayable) => {
                            failures += 1;
                            continue;
                        }
                        Err(PlayFailure::RateLimited) => {
                            q.requeue_head(first);
                            return Outcome::Failed(PlayFailure::RateLimited);
                        }
                        Err(f) => return Outcome::Failed(f),
                    },
                    None => return Outcome::Stopped,
                },
                Next::Play {
                    track,
                    chosen_by_user,
                } => {
                    match self.start(q, track.clone(), chosen_by_user).await {
                        Ok(()) => return Outcome::Playing(track),
                        Err(PlayFailure::Unplayable) => {
                            failures += 1;
                            if failures >= MAX_CONSECUTIVE_PLAY_FAILS {
                                return Outcome::GaveUp;
                            }
                        }
                        Err(f) => {
                            // Network, proxy block, rate limit: the track is fine,
                            // so keep it at the head instead of draining the queue.
                            q.requeue_head(track);
                            return Outcome::Failed(f);
                        }
                    }
                }
            }
        }
    }

    async fn autoplay(&self, q: &mut Queue) -> Option<Track> {
        let current = q.current.as_ref()?;
        let mix_id = current
            .extra
            .get("mixes")
            .and_then(|m| m.get("TRACK_MIX"))
            .and_then(serde_json::Value::as_str)?
            .to_string();
        let result = pages::get_mix_items(&self.state, mix_id).await.ok()?;
        let radio: Vec<Track> = result
            .tracks
            .iter()
            .filter_map(|t| serde_json::to_value(t).ok())
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect();
        let prefs = self.prefs();
        q.install_autoplay(radio, |t| {
            if prefs.blocks(t) {
                Some(Skip::Blocked)
            } else if t.is_unavailable(now_secs()) {
                Some(Skip::Unavailable)
            } else {
                None
            }
        })
    }

    pub async fn previous(&self) -> Outcome {
        let mut q = self.queue.lock().await;
        let pos = playback::get_playback_position(&self.state)
            .await
            .unwrap_or(0.0);
        match q.previous(pos) {
            Previous::Restart => match playback::seek_track(&self.state, 0.0).await {
                Ok(()) => Outcome::Restarted,
                Err(e) => Outcome::Failed(PlayFailure::classify(&e)),
            },
            Previous::Nothing => Outcome::Nothing,
            Previous::Play(track) => {
                let _ = playback::stop_track(&self.state).await;
                // `previous` already moved the old current out; start without
                // pushing history again.
                match playback::play_tidal_track(&self.state, track.id, q.use_track_gain).await {
                    Ok(_) => {
                        q.begin(track.clone(), true);
                        Outcome::Playing(track)
                    }
                    Err(e) => Outcome::Failed(PlayFailure::classify(&e)),
                }
            }
        }
    }

    // ---------- queue editing ----------

    pub async fn add_to_queue(&self, track: Track, source: Option<Source>) {
        self.queue.lock().await.add_to_queue(track, source.as_ref());
    }

    pub async fn play_next_in_queue(&self, track: Track, source: Option<Source>) {
        self.queue
            .lock()
            .await
            .play_next_in_queue(track, source.as_ref());
    }

    pub async fn remove_from_queue(&self, index: usize) {
        self.queue.lock().await.remove_at(index);
    }

    pub async fn clear_queue(&self) {
        self.queue.lock().await.clear();
    }

    pub async fn toggle_shuffle(&self) -> bool {
        let mut q = self.queue.lock().await;
        q.toggle_shuffle(&mut rand::rng());
        q.shuffle
    }

    pub async fn cycle_repeat(&self) -> Repeat {
        self.queue.lock().await.cycle_repeat()
    }

    pub async fn set_modes(&self, shuffle: bool, repeat: Repeat, autoplay: bool) {
        let mut q = self.queue.lock().await;
        if q.shuffle != shuffle {
            q.toggle_shuffle(&mut rand::rng());
        }
        q.repeat = repeat;
        q.autoplay = autoplay;
    }

    /// Start the restored current track (loaded paused-in-UI after a restart).
    pub async fn resume_current(&self) -> Outcome {
        let q = self.queue.lock().await;
        let Some(cur) = q.current.clone() else {
            return Outcome::Nothing;
        };
        match playback::play_tidal_track(&self.state, cur.id, q.use_track_gain).await {
            Ok(_) => Outcome::Playing(cur),
            Err(e) => Outcome::Failed(PlayFailure::classify(&e)),
        }
    }

    // ---------- persistence ----------

    /// Shuffle / repeat / autoplay survive restarts too.
    pub async fn save_modes(&self) -> Result<(), SlateError> {
        let (shuffle, repeat, autoplay) = {
            let q = self.queue.lock().await;
            (q.shuffle, q.repeat.as_u8(), q.autoplay)
        };
        let json =
            serde_json::json!({ "shuffle": shuffle, "repeat": repeat, "autoplay": autoplay })
                .to_string();
        let state = Arc::clone(&self.state);
        tokio::task::spawn_blocking(move || state.write_state_file("modes.json", &json))
            .await
            .map_err(|e| SlateError::Io(e.to_string()))?
    }

    pub async fn load_modes(&self) -> (bool, Repeat, bool) {
        let state = Arc::clone(&self.state);
        let raw = tokio::task::spawn_blocking(move || state.read_state_file("modes.json"))
            .await
            .ok()
            .flatten();
        let v: serde_json::Value = raw
            .and_then(|r| serde_json::from_str(&r).ok())
            .unwrap_or_default();
        (
            v["shuffle"].as_bool().unwrap_or(false),
            Repeat::from_u8(v["repeat"].as_u64().unwrap_or(0) as u8),
            v["autoplay"].as_bool().unwrap_or(false),
        )
    }

    /// Write the queue to the encrypted state file. Blocking disk I/O: call
    /// from a blocking-safe context or accept the small write.
    pub async fn save(&self) -> Result<(), SlateError> {
        let json = self.queue.lock().await.snapshot().to_json()?;
        let state = Arc::clone(&self.state);
        tokio::task::spawn_blocking(move || playback::save_playback_queue(&state, json))
            .await
            .map_err(|e| SlateError::Io(e.to_string()))?
    }

    /// Restore the saved queue. The current track is returned (not started) so
    /// the UI can show it paused, as the original app does.
    pub async fn restore(&self) -> Result<Option<Track>, SlateError> {
        let state = Arc::clone(&self.state);
        let raw = tokio::task::spawn_blocking(move || playback::load_playback_queue(&state))
            .await
            .map_err(|e| SlateError::Io(e.to_string()))??;
        let Some(raw) = raw else { return Ok(None) };
        let snap = Snapshot::from_json(&raw)?;
        let prefs = self.prefs();
        let mut q = self.queue.lock().await;
        q.restore(snap, |t| !prefs.blocks(t));
        Ok(q.current.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_errors() {
        assert_eq!(
            PlayFailure::classify(&SlateError::Network("x".into())),
            PlayFailure::Network
        );
        assert_eq!(
            PlayFailure::classify(&SlateError::Api {
                status: 429,
                body: String::new()
            }),
            PlayFailure::RateLimited
        );
        assert_eq!(
            PlayFailure::classify(&SlateError::Api {
                status: 404,
                body: String::new()
            }),
            PlayFailure::Unplayable
        );
        assert!(matches!(
            PlayFailure::classify(&SlateError::ProxyBlocked { reason: "r".into() }),
            PlayFailure::Blocked(_)
        ));
    }

    #[test]
    fn content_prefs_block_flagged_tracks() {
        let mut t = Track::new(1);
        t.extra.insert("explicit".into(), true.into());
        assert!(!ContentPrefs::default().blocks(&t));
        let strict = ContentPrefs {
            allow_explicit: false,
            allow_ai: true,
        };
        assert!(strict.blocks(&t));
    }
}
