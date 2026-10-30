//! Playback controller: drives [`Queue`] against the audio engine.
//!
//! Owns the orchestration the React layer used to do around `playTrack` /
//! `playNext` / `playPrevious`: device-busy retry, skipping unplayable tracks
//! with a failure cap, re-queueing on transient failures, autoplay radio and
//! queue persistence. Frontends call these methods and render the result.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use crate::commands::{pages, playback, scrobble, utility};
use crate::queue::{Next, Previous, Queue, Repeat, Skip, Snapshot, Source, Track};
use crate::tidal_api::StreamInfo;
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
    /// The queue is empty and an agent is acting as DJ: wait a moment for it to add tracks
    /// before the autoplay radio starts.
    WaitForAgent,
}

pub struct Controller {
    state: Arc<AppState>,
    queue: Mutex<Queue>,
    prefs: std::sync::Mutex<ContentPrefs>,
    /// (track id, qid) currently armed for a gapless transition.
    armed: std::sync::Mutex<Option<(u64, String)>>,
    /// Track whose gapless arming failed recently, to avoid retry storms.
    arm_failed: std::sync::Mutex<Option<(u64, std::time::Instant)>>,
    /// How the current track is being streamed (quality, codec, bit depth).
    stream: std::sync::Mutex<Option<StreamInfo>>,
    /// Stream info of the gapless-armed next track.
    armed_stream: std::sync::Mutex<Option<(u64, StreamInfo)>>,
    /// Set when the agent-DJ wait ended: the next empty queue goes straight to autoplay.
    autoplay_once: std::sync::atomic::AtomicBool,
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
            armed: std::sync::Mutex::new(None),
            arm_failed: std::sync::Mutex::new(None),
            stream: std::sync::Mutex::new(None),
            armed_stream: std::sync::Mutex::new(None),
            autoplay_once: std::sync::atomic::AtomicBool::new(false),
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
        let use_gain = q.use_track_gain;
        let mut attempt = 0;
        loop {
            match playback::play_tidal_track(&self.state, track.id, use_gain).await {
                Ok(info) => {
                    self.set_stream(Some(info));
                    break;
                }
                Err(SlateError::Audio(m)) if m == "device_busy" && attempt < DEVICE_MAX_RETRIES => {
                    attempt += 1;
                    tokio::time::sleep(DEVICE_RETRY_DELAY).await;
                }
                Err(e) => return Err(PlayFailure::classify(&e)),
            }
        }
        q.begin(track.clone(), false);
        self.forget_armed();
        self.notify_started(q, &track, chosen_by_user).await;
        Ok(())
    }

    fn set_stream(&self, info: Option<StreamInfo>) {
        if let Ok(mut s) = self.stream.lock() {
            *s = info;
        }
    }

    /// Stream details of what is playing now, if a track has been started.
    pub fn stream_info(&self) -> Option<StreamInfo> {
        self.stream.lock().ok().and_then(|s| s.clone())
    }

    fn forget_armed(&self) {
        if let Ok(mut a) = self.armed.lock() {
            *a = None;
        }
    }

    /// Tells scrobblers and TIDAL play reporting a track began.
    async fn notify_started(&self, q: &Queue, track: &Track, chosen_by_user: bool) {
        let source = q.playback_source.as_ref();
        let album = track
            .extra
            .get("album")
            .and_then(|a| a.get("title"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.is_empty());
        let payload = scrobble::TrackStartedPayload {
            artist: track.artist_display(),
            artist_primary: track.artist_name().to_string(),
            title: track.title().to_string(),
            album,
            album_artist: None,
            duration_secs: track.duration_secs() as u32,
            track_number: track
                .extra
                .get("trackNumber")
                .and_then(serde_json::Value::as_u64)
                .map(|n| n as u32),
            chosen_by_user,
            isrc: track
                .extra
                .get("isrc")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            track_id: Some(track.id),
            source_type: source.map(|s| s.kind.clone()),
            source_id: source.map(|s| match &s.id {
                serde_json::Value::String(v) => v.clone(),
                other => other.to_string(),
            }),
        };
        if let Err(e) = scrobble::notify_track_started(&self.state, payload).await {
            log::debug!("[controller] scrobble start not recorded: {}", e.log_safe());
        }
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

    /// The full list of the source being played finished loading after playback
    /// began on a partial one. Ignored if the user has since moved elsewhere.
    pub async fn extend_source(&self, all: Vec<Track>, source: &Source) {
        let mut q = self.queue.lock().await;
        let same = q
            .playback_source
            .as_ref()
            .is_some_and(|s| s.kind == source.kind && s.id == source.id);
        if same {
            q.extend_context(all, &mut rand::rng());
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
                        Ok(info) => {
                            self.set_stream(Some(info));
                            Outcome::Playing(cur)
                        }
                        Err(e) => Outcome::Failed(PlayFailure::classify(&e)),
                    };
                }
                Next::NeedAutoplay
                    if crate::mcp::hooks::dj_waiting(&self.state)
                        && !self
                            .autoplay_once
                            .swap(false, std::sync::atomic::Ordering::SeqCst) =>
                {
                    return Outcome::WaitForAgent;
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
            .filter_map(Track::from_serialized)
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
                    Ok(info) => {
                        self.set_stream(Some(info));
                        q.begin(track.clone(), true);
                        self.forget_armed();
                        self.notify_started(&q, &track, true).await;
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

    /// Drag-and-drop or an agent moving an upcoming track; false if the positions are invalid.
    pub async fn move_in_queue(&self, from: usize, to: usize) -> bool {
        self.queue.lock().await.move_upcoming(from, to)
    }

    /// After the agent-DJ wait: let the next empty queue start the autoplay radio.
    pub fn allow_autoplay_once(&self) {
        self.autoplay_once
            .store(true, std::sync::atomic::Ordering::SeqCst);
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
            Ok(info) => {
                self.set_stream(Some(info));
                self.forget_armed();
                self.notify_started(&q, &cur, true).await;
                Outcome::Playing(cur)
            }
            Err(e) => Outcome::Failed(PlayFailure::classify(&e)),
        }
    }

    // ---------- gapless ----------

    fn gapless_enabled(&self) -> bool {
        use std::sync::atomic::Ordering::Relaxed;
        utility::get_gapless_supported()
            && self.state.gapless.load(Relaxed)
            && !self.state.exclusive_mode.load(Relaxed)
            && !self.state.bit_perfect.load(Relaxed)
    }

    /// Keeps the engine's "next track" slot in step with the queue. Call after
    /// anything that can change what plays next (start, edits, shuffle, repeat).
    pub async fn refresh_gapless(&self) {
        const FAILURE_MEMO: Duration = Duration::from_secs(10);
        let wanted = {
            let q = self.queue.lock().await;
            let prefs = self.prefs();
            if !self.gapless_enabled() || q.current.as_ref().is_none_or(Track::is_video) {
                None
            } else {
                q.pick_gapless_next(now_secs(), |t| prefs.blocks(t))
                    .map(|t| (t.clone(), q.use_track_gain))
            }
        };
        let Some((next, use_gain)) = wanted else {
            let had = self.armed.lock().ok().and_then(|mut a| a.take()).is_some();
            if had {
                let _ = playback::clear_next_track(&self.state);
            }
            return;
        };
        let qid = next.qid.clone().unwrap_or_else(|| next.id.to_string());
        if self
            .armed
            .lock()
            .ok()
            .is_some_and(|a| a.as_ref() == Some(&(next.id, qid.clone())))
        {
            return;
        }
        let recently_failed = self
            .arm_failed
            .lock()
            .ok()
            .and_then(|f| *f)
            .is_some_and(|(id, at)| id == next.id && at.elapsed() < FAILURE_MEMO);
        if recently_failed {
            return;
        }
        match playback::set_next_track(&self.state, next.id, qid.clone(), use_gain).await {
            Ok(info) => {
                if let Ok(mut a) = self.armed.lock() {
                    *a = Some((next.id, qid));
                }
                if let Ok(mut s) = self.armed_stream.lock() {
                    *s = Some((next.id, info));
                }
            }
            Err(e) => {
                log::debug!("[controller] could not arm next track: {}", e.log_safe());
                if let Ok(mut f) = self.arm_failed.lock() {
                    *f = Some((next.id, std::time::Instant::now()));
                }
            }
        }
    }

    /// The engine switched to the armed track by itself (`track-advanced`).
    /// Audio reality wins: reconcile the queue and adopt it. Returns the track now playing.
    pub async fn on_track_advanced(&self, payload: &serde_json::Value) -> Option<Track> {
        use std::sync::atomic::Ordering::Relaxed;
        let track_id = payload.get("trackId").and_then(serde_json::Value::as_u64)?;
        let qid = payload
            .get("qid")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let rg = payload
            .get("replayGain")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(f64::NAN);
        let peak = payload
            .get("peakAmplitude")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(f64::NAN);
        // So a live volume-normalisation toggle is right for the new track.
        self.state.last_replay_gain.store(rg.to_bits(), Relaxed);
        self.state
            .last_peak_amplitude
            .store(peak.to_bits(), Relaxed);
        let armed = self.armed_stream.lock().ok().and_then(|mut s| s.take());
        self.set_stream(armed.filter(|(id, _)| *id == track_id).map(|(_, i)| i));
        self.forget_armed();
        // The outgoing track finished: finalise its scrobble / play report.
        crate::startup::on_track_finished(&self.state).await;

        let mut q = self.queue.lock().await;
        let track = match q.adopt_advanced(track_id, qid) {
            Some(t) => t,
            None => {
                // Armed track no longer in the queue: audio is playing it anyway.
                let meta = crate::commands::metadata::get_track(&self.state, track_id)
                    .await
                    .ok()?;
                let t: Track = serde_json::from_value(meta).ok()?;
                q.begin(t.clone(), false);
                t
            }
        };
        self.notify_started(&q, &track, false).await;
        Some(track)
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
