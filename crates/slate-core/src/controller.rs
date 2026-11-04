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
use crate::sync::LockExt;
use crate::tidal_api::StreamInfo;
use crate::{AppState, SlateError};

/// How loud the music stays while a DJ line is spoken.
const DUCK_FACTOR: f32 = 0.3;
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
    /// The preferences for the two "hide" settings.
    pub fn hiding(explicit: bool, ai: bool) -> Self {
        Self {
            allow_explicit: !explicit,
            allow_ai: !ai,
        }
    }

    /// Both flags as bits (1 = hide explicit, 2 = hide AI), for a process-wide mirror.
    pub fn to_bits(self) -> u8 {
        u8::from(!self.allow_explicit) | (u8::from(!self.allow_ai) << 1)
    }

    pub fn from_bits(bits: u8) -> Self {
        Self::hiding(bits & 1 != 0, bits & 2 != 0)
    }

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
    /// Hidden by the content filter (Settings → Playback).
    Filtered,
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
    app_handle: crate::events::AppHandle,
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
    /// Track for which a crossfade was already decided, so it is decided once.
    fade_checked: std::sync::Mutex<Option<u64>>,
    /// Listener's latest explicit direction (consumed by the next DJ pick).
    dj_direction: std::sync::Mutex<Option<String>>,
    /// Why the current DJ block was chosen (fed back into the next prompt).
    dj_reason: std::sync::Mutex<Option<String>>,
    /// Last DJ attempt, in words a listener understands: success
    /// ("DJ is up a block — 'deep house'") or the reason nothing moved
    /// ("TIDAL had no results for '…'"). `None` until the DJ was asked.
    dj_status: std::sync::Mutex<Option<String>>,
    /// A DJ block fetched by `dj_prepare` without the queue lock, for the queue it
    /// was fetched for; `(key, None)` = the DJ was asked and failed (use the radio).
    dj_ready: std::sync::Mutex<Option<(DjKey, Option<DjPick>)>>,
}

/// A DJ move and the TIDAL tracks found for it.
struct DjPick {
    move_: crate::dj::DjMove,
    tracks: Vec<Track>,
}

/// Which queue a prepared DJ pick belongs to: its epoch and the playing track.
type DjKey = (u64, Option<u64>);

fn dj_key(q: &Queue) -> DjKey {
    (q.epoch, q.current.as_ref().map(|t| t.id))
}

/// Whether `name` is one of the track's artists, ignoring case ("Von" alone does not
/// match "King Von"; "Tyler, The Creator" stays one name).
fn by_artist(t: &Track, name: &str) -> bool {
    let name = name.trim().to_lowercase();
    if name.is_empty() {
        return false;
    }
    let listed = t
        .extra
        .get("artists")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|a| a.get("name").and_then(serde_json::Value::as_str));
    listed
        .chain(std::iter::once(t.artist_name()))
        .any(|a| a.trim().to_lowercase() == name)
}

/// A DJ block of `size` from per-artist track lists, no track twice. With `lead` (the
/// listener asked for the first artist) that artist fills about half the block,
/// alternating with the others; otherwise the artists take turns.
fn mix_block(groups: Vec<Vec<Track>>, size: usize, lead: bool) -> Vec<Track> {
    let mut seen = std::collections::HashSet::new();
    let mut groups: Vec<std::collections::VecDeque<Track>> = groups
        .into_iter()
        .map(|g| g.into_iter().filter(|t| seen.insert(t.id)).collect())
        .collect();
    let mut out = Vec::with_capacity(size);
    let mut turn = 0usize;
    while out.len() < size && groups.iter().any(|g| !g.is_empty()) {
        let pick = if lead && out.len() % 2 == 0 && !groups[0].is_empty() {
            0
        } else {
            // Round robin over the others (all of them when there is no lead).
            let start = usize::from(lead && groups.len() > 1);
            let others = groups.len() - start;
            let mut chosen = None;
            for k in 0..others {
                let i = start + (turn + k) % others;
                if !groups[i].is_empty() {
                    chosen = Some(i);
                    turn = turn + k + 1;
                    break;
                }
            }
            chosen.unwrap_or_default()
        };
        match groups[pick].pop_front() {
            Some(t) => out.push(t),
            None => break,
        }
    }
    out
}

fn now_secs() -> i64 {
    i64::try_from(crate::now_secs()).unwrap_or(i64::MAX)
}

impl Controller {
    pub fn new(state: Arc<AppState>, app_handle: crate::events::AppHandle) -> Self {
        Self {
            state,
            app_handle,
            queue: Mutex::new(Queue::new()),
            prefs: std::sync::Mutex::new(ContentPrefs::default()),
            armed: std::sync::Mutex::new(None),
            arm_failed: std::sync::Mutex::new(None),
            stream: std::sync::Mutex::new(None),
            armed_stream: std::sync::Mutex::new(None),
            autoplay_once: std::sync::atomic::AtomicBool::new(false),
            fade_checked: std::sync::Mutex::new(None),
            dj_direction: std::sync::Mutex::new(None),
            dj_reason: std::sync::Mutex::new(None),
            dj_status: std::sync::Mutex::new(None),
            dj_ready: std::sync::Mutex::new(None),
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
        f(&*self.lock_queue().await)
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
        let use_gain = q.track_gain();
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
        crate::dj_voice::track_changed();
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

    /// The queue, with the smart-shuffle setting applied (one atomic read).
    async fn lock_queue(&self) -> tokio::sync::MutexGuard<'_, Queue> {
        let mut q = self.queue.lock().await;
        q.spread = self
            .state
            .smart_shuffle
            .load(std::sync::atomic::Ordering::Relaxed);
        q.album_shuffle = self
            .state
            .album_shuffle
            .load(std::sync::atomic::Ordering::Relaxed);
        q
    }

    /// Play a single track, replacing the context queue (the manual queue stays).
    pub async fn play_single(&self, track: Track) -> Outcome {
        let mut q = self.lock_queue().await;
        if self.prefs().blocks(&track) {
            return Outcome::Failed(PlayFailure::Filtered);
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
        let mut q = self.lock_queue().await;
        if self.prefs().blocks(&track) {
            return Outcome::Failed(PlayFailure::Filtered);
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
        let mut q = self.lock_queue().await;
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
            Err(PlayFailure::Unplayable) => self.advance_context_first(q).await,
            Err(PlayFailure::RateLimited) => {
                // The request failed before the pipeline was touched, so the old
                // track is still sounding while the UI says nothing plays.
                let _ = playback::stop_track(&self.state).await;
                q.requeue_head(track);
                Outcome::Failed(PlayFailure::RateLimited)
            }
            Err(f) => Outcome::Failed(f),
        }
    }

    /// [`Self::advance`] for a clicked track that cannot play: continue in its own
    /// album or playlist before the manual queue, which plays when the collection
    /// has nothing left.
    async fn advance_context_first(&self, q: &mut Queue) -> Outcome {
        let held = q.hold_manual_for_context(|t| self.classify(t).is_none());
        if held.is_empty() {
            return self.advance(q, true).await;
        }
        let out = self.advance(q, true).await;
        q.release_manual(held);
        match out {
            Outcome::Stopped | Outcome::GaveUp => self.advance(q, true).await,
            out => out,
        }
    }

    pub async fn play_from_queue(&self, index: usize) -> Outcome {
        let mut q = self.lock_queue().await;
        let Some(track) = q.take_at(index) else {
            return Outcome::Nothing;
        };
        match self.classify(&track) {
            Some(Skip::Blocked) => return Outcome::Failed(PlayFailure::Filtered),
            Some(_) => return Outcome::Failed(PlayFailure::Unplayable),
            None => {}
        }
        match self.start(&mut q, track.clone(), true).await {
            Ok(()) => Outcome::Playing(track),
            Err(f) => Outcome::Failed(f),
        }
    }

    /// The full list of the source being played finished loading after playback
    /// began on a partial one. Ignored if the user has since moved elsewhere.
    pub async fn extend_source(&self, all: Vec<Track>, source: &Source) {
        let mut q = self.lock_queue().await;
        let same = q
            .playback_source
            .as_ref()
            .is_some_and(|s| s.kind == source.kind && s.id == source.id);
        if same {
            q.extend_context(all, &mut rand::rng());
        }
    }

    // ---------- navigation ----------

    /// Called from the playback tick. When crossfade is on and the current track is about to
    /// end, starts the next queued track as a fade instead of waiting for the end. Returns the
    /// outcome to show, or `None` when nothing was done (the usual case).
    pub async fn maybe_crossfade(&self, position: f32, duration: f32) -> Option<Outcome> {
        let settings = self.state.audio_player.crossfade_settings();
        if settings.secs == 0 || duration <= 0.0 {
            return None;
        }
        let remaining = duration - position;
        if remaining > settings.secs as f32 + 1.0 || remaining < 0.8 {
            return None;
        }
        let mut q = self.lock_queue().await;
        let current = q.current.as_ref()?;
        let id = current.id;
        {
            let mut done = self.fade_checked.lock().ok()?;
            if *done == Some(id) {
                return None;
            }
            *done = Some(id);
        }
        // Repeat-one replays the same track; an empty queue has nothing to fade into.
        if q.repeat == crate::queue::Repeat::One {
            return None;
        }
        let next = q.manual.first().or_else(|| q.queue.first())?;
        let album = |t: &Track| {
            t.extra
                .get("album")
                .and_then(|a| a.get("id"))
                .and_then(serde_json::Value::as_u64)
        };
        let s = self.state.load_settings().unwrap_or_default();
        let boundary = crate::crossfade::Boundary {
            exclusive_or_bit_perfect: s.exclusive_mode || s.bit_perfect,
            same_album: album(current).is_some() && album(current) == album(next),
            current_secs: duration as u32,
            next_secs: Some(next.duration_secs() as u32),
        };
        let crate::crossfade::Plan::Fade { secs } = crate::crossfade::plan(settings, boundary)
        else {
            return None;
        };
        // The fade ends a little before the old track would, so it never reaches its end.
        let player = &self.state.audio_player;
        player.arm_crossfade((secs as f32).min(remaining - 0.3).max(0.5));
        crate::startup::on_track_finished(&self.state).await;
        let outcome = self.advance(&mut q, false).await;
        player.disarm_crossfade();
        Some(outcome)
    }

    /// Track ended on its own (`explicit == false`) or the user pressed Next.
    pub async fn next(&self, explicit: bool) -> Outcome {
        if !explicit {
            crate::startup::on_track_finished(&self.state).await;
        }
        let prepared = self.dj_prepare(explicit).await;
        let mut q = self.lock_queue().await;
        if prepared.is_some_and(|key| key != dj_key(&q)) {
            // The user played something else while the DJ was thinking.
            return Outcome::Nothing;
        }
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
                    return match playback::play_tidal_track(&self.state, cur.id, q.track_gain())
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
                Next::NeedAutoplay => match self.autoplay_or_dj(q).await {
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

    /// Set the listener's next DJ direction (consumed by the next genre pick).
    pub fn dj_set_direction(&self, text: String) {
        if let Ok(mut g) = self.dj_direction.lock() {
            *g = Some(text);
        }
    }

    fn dj_take_direction(&self) -> Option<String> {
        self.dj_direction.lock().ok().and_then(|mut g| g.take())
    }

    fn dj_last_reason(&self) -> Option<String> {
        self.dj_reason.lock().ok().and_then(|g| g.clone())
    }

    /// The last DJ attempt in listener words — success or why nothing moved.
    /// `None` until the DJ was first asked (`dj_next` / autoplay).
    pub fn dj_status(&self) -> Option<String> {
        self.dj_status.lock().ok().and_then(|g| g.clone())
    }

    fn dj_note(&self, msg: impl Into<String>) {
        if let Ok(mut g) = self.dj_status.lock() {
            *g = Some(msg.into());
        }
    }

    /// What the DJ's model is told about the session (titles and artists only).
    fn dj_context(&self, q: &Queue) -> crate::dj::DjContext {
        let title = |t: &Track| {
            t.extra
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let artist = |t: &Track| t.artist_display();
        crate::dj::DjContext {
            current: q.current.as_ref().map(|t| (title(t), artist(t))),
            recent: q
                .history
                .iter()
                .rev()
                .take(20)
                .map(|t| (title(t), artist(t)))
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect(),
            direction: self.dj_take_direction(),
            last_reason: self.dj_last_reason(),
        }
    }

    /// The model picks the next genre and TIDAL search fills a block. Runs without the
    /// queue lock: a local model can take minutes (the timeout defaults to 300 s) and
    /// clicks, queue edits and the queue panel must not wait for it. `None` = the DJ is
    /// off or failed (the reason is in [`Self::dj_status`]); the caller falls back.
    async fn dj_fetch(&self, ctx: crate::dj::DjContext, forced: bool) -> Option<DjPick> {
        // The usual case (DJ off): one atomic read, no file access.
        if !forced
            && !self
                .state
                .dj_enabled
                .load(std::sync::atomic::Ordering::Relaxed)
        {
            return None;
        }
        let settings = match self.state.load_settings() {
            Some(s) => s,
            None => {
                self.dj_note("Could not read settings to run the DJ.");
                return None;
            }
        };
        if !settings.dj.enabled {
            self.dj_note("The DJ is disabled — enable it in Settings → Playback.");
            return None;
        }
        let dj = crate::dj::Dj::new(settings.dj, self.state.proxied_http.clone());
        let ctx_had_direction = ctx.direction.is_some();
        let move_ = match dj.decide(&ctx).await {
            Some(m) => m,
            None => {
                self.dj_note(dj.last_diag().unwrap_or_else(|| {
                    "The DJ endpoint did not answer, or the reply was not usable.".to_string()
                }));
                return None;
            }
        };

        let size = dj.block_size() as usize;
        let lead = ctx_had_direction;
        let mut tracks = self.dj_tracks_by_artist(&move_.artists, size, lead).await;
        if tracks.len() < size.min(3) {
            // Too few by artist (or no artists): the style phrase, as before.
            let mut client = self.state.tidal_client.lock().await;
            let results = match client.search(&move_.query, dj.block_size()).await {
                Ok(r) => r,
                Err(e) => {
                    drop(client);
                    self.dj_note(format!(
                        "TIDAL did not return results for the DJ's query '{}': {e}",
                        move_.query
                    ));
                    return None;
                }
            };
            drop(client);
            let seen: std::collections::HashSet<u64> = tracks.iter().map(|t| t.id).collect();
            tracks.extend(
                results
                    .tracks
                    .iter()
                    .filter_map(Track::from_serialized)
                    .filter(|t| !seen.contains(&t.id))
                    .take(size.saturating_sub(seen.len())),
            );
        }
        if tracks.is_empty() {
            self.dj_note(format!(
                "TIDAL matched the DJ's query '{}' but nothing of it could be loaded as a track.",
                move_.query
            ));
            return None;
        }
        Some(DjPick { move_, tracks })
    }

    /// Tracks by the DJ's artists: one TIDAL search per artist, keeping only tracks that
    /// artist is on (a name search also matches titles), mixed into a block of `size`.
    async fn dj_tracks_by_artist(&self, artists: &[String], size: usize, lead: bool) -> Vec<Track> {
        if artists.is_empty() || size == 0 {
            return Vec::new();
        }
        let per = u32::try_from(size.div_ceil(artists.len()) + 4).unwrap_or(10);
        let mut groups = Vec::with_capacity(artists.len());
        for name in artists {
            let mut client = self.state.tidal_client.lock().await;
            let found = client.search(name, per).await;
            drop(client);
            let tracks: Vec<Track> = match found {
                Ok(r) => r
                    .tracks
                    .iter()
                    .filter_map(Track::from_serialized)
                    .filter(|t| by_artist(t, name))
                    .collect(),
                Err(e) => {
                    log::debug!("[dj] artist search failed: {}", e.log_safe());
                    Vec::new()
                }
            };
            groups.push(tracks);
        }
        mix_block(groups, size, lead)
    }

    /// Installs a fetched block as the autoplay queue and announces it.
    fn dj_install(&self, q: &mut Queue, pick: DjPick) -> Option<Track> {
        let DjPick { move_, tracks } = pick;
        let prefs = self.prefs();
        let first = match q.install_autoplay(tracks, |t| {
            if prefs.blocks(t) {
                Some(Skip::Blocked)
            } else if t.is_unavailable(now_secs()) {
                Some(Skip::Unavailable)
            } else {
                None
            }
        }) {
            Some(first) => first,
            None => {
                self.dj_note("The DJ's best matches were blocked by your filters or unavailable.");
                return None;
            }
        };

        if !move_.reason.is_empty() {
            if let Ok(mut g) = self.dj_reason.lock() {
                *g = Some(move_.reason.clone());
            }
        }
        self.dj_note(format!(
            "DJ is up a block — '{}'{}",
            move_.query,
            if move_.reason.is_empty() {
                String::new()
            } else {
                format!(" ({}).", move_.reason)
            }
        ));
        self.app_handle
            .emit(
                "dj-branch",
                serde_json::json!({
                    "query": move_.query,
                    "reason": move_.reason,
                    "speaks": move_.speaks,
                    "line": move_.line,
                }),
            )
            .ok();
        if move_.speaks {
            self.dj_say(move_.line.as_deref());
        }
        Some(first)
    }

    /// "DJ this playlist": asks the model for a flowing order of `tracks` (titles and artists
    /// of up to 200 are sent) and plays that order. The request runs before the queue is
    /// locked. Any failure keeps the playlist's own order. Returns the outcome of starting,
    /// whether shuffle had to be switched off (it would scramble the set) and a sentence
    /// saying what happened.
    pub async fn dj_playlist(
        &self,
        tracks: Vec<Track>,
        source: Option<Source>,
    ) -> (Outcome, bool, String) {
        let settings = self.state.load_settings().unwrap_or_default().dj;
        // Tracks the filters (or availability) would skip are not sent: they would take
        // places among the 200 and their titles would leave the machine for nothing.
        let tracks: Vec<Track> = tracks
            .into_iter()
            .filter(|t| self.classify(t).is_none())
            .collect();
        let n = tracks.len().min(crate::dj::SET_MAX_TRACKS);
        let pairs: Vec<(String, String)> = tracks[..n]
            .iter()
            .map(|t| (t.title().to_string(), t.artist_name().to_string()))
            .collect();
        let asked = if crate::dj::is_configured(&settings) {
            let dj = crate::dj::Dj::new(settings, self.state.proxied_http.clone());
            dj.order_set(&pairs).await
        } else {
            Err("the DJ endpoint is not set up (DJ page)".to_string())
        };
        let (ordered, status) = match asked {
            Ok(order) => {
                let mut slots: Vec<Option<Track>> = tracks.iter().cloned().map(Some).collect();
                let mut out: Vec<Track> = order
                    .iter()
                    .filter_map(|&i| slots.get_mut(i).and_then(Option::take))
                    .collect();
                out.extend(slots.into_iter().flatten());
                let status = format!("DJ set ready: {} tracks in a new order.", out.len());
                (out, status)
            }
            Err(why) => (
                tracks,
                format!("Playing the playlist in its own order: {why}"),
            ),
        };
        self.dj_note(status.clone());
        let shuffle_off = {
            let mut q = self.lock_queue().await;
            let was = q.shuffle;
            if was {
                q.toggle_shuffle(&mut rand::rng());
            }
            was
        };
        let outcome = self.play_all(ordered, source, false, false).await;
        (outcome, shuffle_off, status)
    }

    /// Speaks a DJ line with the chosen voice. Piper ducks the music while it talks and says
    /// why when it cannot; the system voice is the original fire-and-forget one.
    fn dj_say(&self, line: Option<&str>) {
        let settings = self.state.load_settings().unwrap_or_default().dj;
        if crate::dj_voice::Voice::parse(&settings.voice) != crate::dj_voice::Voice::Piper {
            crate::dj::speak(line);
            return;
        }
        let Some(line) = line else { return };
        let duck: Option<crate::dj_voice::Duck> = (!settings.no_duck).then(|| {
            let state = Arc::clone(&self.state);
            Arc::new(move |on: bool| {
                let full = state.load_settings().map_or(1.0, |s| s.volume);
                let _ = state
                    .audio_player
                    .set_volume(if on { full * DUCK_FACTOR } else { full });
            }) as crate::dj_voice::Duck
        });
        if let Err(why) = crate::dj_voice::speak_piper(line, &settings.piper_model, duck, None) {
            self.dj_note(format!("DJ voice: {why}"));
        }
    }

    /// Before `next` takes the queue lock: if the queue is about to run out and the DJ
    /// is on, ask the model now, unlocked, and leave the result for [`Self::dj_autoplay`].
    /// Returns the queue the pick was made for, `None` when nothing was asked.
    async fn dj_prepare(&self, explicit: bool) -> Option<DjKey> {
        if !self
            .state
            .dj_enabled
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return None;
        }
        let (ctx, key) = {
            let q = self.lock_queue().await;
            if !q.would_autoplay(explicit) {
                return None;
            }
            (self.dj_context(&q), dj_key(&q))
        };
        let pick = self.dj_fetch(ctx, false).await;
        *self.dj_ready.lock_safe() = Some((key, pick));
        Some(key)
    }

    /// DJ autoplay: uses the block [`Self::dj_prepare`] fetched for this queue, if any,
    /// otherwise asks the model now (a dead clicked track that ends a collection).
    /// `None` = DJ unavailable/disabled → caller falls back to `autoplay`.
    async fn dj_autoplay(&self, q: &mut Queue) -> Option<Track> {
        let ready = self.dj_ready.lock_safe().take();
        let pick = match ready {
            Some((key, pick)) if key == dj_key(q) => pick,
            _ => {
                if !self
                    .state
                    .dj_enabled
                    .load(std::sync::atomic::Ordering::Relaxed)
                {
                    return None;
                }
                let ctx = self.dj_context(q);
                self.dj_fetch(ctx, false).await
            }
        };
        self.dj_install(q, pick?)
    }

    /// DJ if available, else the usual TRACK_MIX radio. Drop-in for `autoplay`.
    async fn autoplay_or_dj(&self, q: &mut Queue) -> Option<Track> {
        if let Some(first) = self.dj_autoplay(q).await {
            return Some(first);
        }
        self.autoplay(q).await
    }

    /// Force the DJ to pick a new block right now (manual trigger). The model is
    /// asked before the queue is locked.
    pub async fn dj_next(&self) -> Outcome {
        let ctx = {
            let q = self.lock_queue().await;
            self.dj_context(&q)
        };
        let Some(pick) = self.dj_fetch(ctx, true).await else {
            return Outcome::Nothing;
        };
        let mut q = self.lock_queue().await;
        let Some(first) = self.dj_install(&mut q, pick) else {
            return Outcome::Nothing;
        };
        match self.start(&mut q, first.clone(), true).await {
            Ok(()) => Outcome::Playing(first),
            Err(f) => Outcome::Failed(f),
        }
    }

    /// The listener steered the DJ ("deeper into house"): a block in that direction
    /// replaces what is up next (the manual queue stays) and plays when the current
    /// track ends; with nothing playing it starts now. The model is asked unlocked.
    pub async fn dj_steer(&self, direction: String) -> Outcome {
        self.dj_set_direction(direction);
        let ctx = {
            let q = self.lock_queue().await;
            self.dj_context(&q)
        };
        let Some(pick) = self.dj_fetch(ctx, true).await else {
            return Outcome::Nothing;
        };
        let mut q = self.lock_queue().await;
        let Some(first) = self.dj_install(&mut q, pick) else {
            return Outcome::Nothing;
        };
        if q.current.is_some() {
            q.unshift_autoplay(first);
            return Outcome::Nothing;
        }
        match self.start(&mut q, first.clone(), true).await {
            Ok(()) => Outcome::Playing(first),
            Err(f) => Outcome::Failed(f),
        }
    }

    pub async fn previous(&self) -> Outcome {
        let mut q = self.lock_queue().await;
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
                match playback::play_tidal_track(&self.state, track.id, q.track_gain()).await {
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
        self.lock_queue().await.add_to_queue(track, source.as_ref());
    }

    pub async fn play_next_in_queue(&self, track: Track, source: Option<Source>) {
        self.queue
            .lock()
            .await
            .play_next_in_queue(track, source.as_ref());
    }

    /// Makes `tracks` the upcoming queue (a restored snapshot), leaving what plays now alone.
    pub async fn replace_upcoming(&self, tracks: Vec<Track>) {
        let mut q = self.lock_queue().await;
        q.clear_manual();
        q.set_tracks(tracks, None, false);
    }

    /// Adds `tracks` after everything in "next up" (a restored snapshot, appended).
    pub async fn append_upcoming(&self, tracks: Vec<Track>) {
        let mut q = self.lock_queue().await;
        for t in tracks {
            q.add_to_queue(t, None);
        }
    }

    /// Inserts into the "next up" list at `index`.
    pub async fn insert_in_queue(&self, index: usize, track: Track) {
        self.lock_queue().await.insert_manual(index, track, None);
    }

    pub async fn remove_from_queue(&self, index: usize) {
        self.lock_queue().await.remove_at(index);
    }

    /// Drag-and-drop or an agent moving an upcoming track; false if the positions are invalid.
    pub async fn move_in_queue(&self, from: usize, to: usize) -> bool {
        self.lock_queue().await.move_upcoming(from, to)
    }

    /// After the agent-DJ wait: let the next empty queue start the autoplay radio.
    pub fn allow_autoplay_once(&self) {
        self.autoplay_once
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Empties the "next up" list but not the rest of the queue.
    pub async fn clear_next_up(&self) {
        self.lock_queue().await.manual.clear();
    }

    /// Empties the queue and forgets the playing track (the MPD `clear`); the caller stops audio.
    pub async fn clear_all(&self) {
        let mut q = self.lock_queue().await;
        q.clear();
        q.current = None;
    }

    /// Shuffles the upcoming tracks once, whatever the shuffle button says.
    pub async fn shuffle_upcoming(&self) {
        self.lock_queue().await.shuffle_upcoming(&mut rand::rng());
    }

    pub async fn clear_queue(&self) {
        self.lock_queue().await.clear();
    }

    pub async fn toggle_shuffle(&self) -> bool {
        let mut q = self.lock_queue().await;
        q.toggle_shuffle(&mut rand::rng());
        q.shuffle
    }

    /// The shuffle mode changed: shuffle the queue again if shuffle is on (one pass).
    pub async fn reshuffle(&self) {
        self.lock_queue().await.reshuffle(&mut rand::rng());
    }

    pub async fn cycle_repeat(&self) -> Repeat {
        self.lock_queue().await.cycle_repeat()
    }

    pub async fn set_modes(&self, shuffle: bool, repeat: Repeat, autoplay: bool) {
        let mut q = self.lock_queue().await;
        if q.shuffle != shuffle {
            q.toggle_shuffle(&mut rand::rng());
        }
        q.repeat = repeat;
        q.autoplay = autoplay;
    }

    /// Start the restored current track (loaded paused-in-UI after a restart).
    pub async fn resume_current(&self) -> Outcome {
        let q = self.lock_queue().await;
        let Some(cur) = q.current.clone() else {
            return Outcome::Nothing;
        };
        match playback::play_tidal_track(&self.state, cur.id, q.track_gain()).await {
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
            let q = self.lock_queue().await;
            let prefs = self.prefs();
            if !self.gapless_enabled() || q.current.as_ref().is_none_or(Track::is_video) {
                None
            } else {
                q.pick_gapless_next(now_secs(), |t| prefs.blocks(t))
                    .map(|t| (t.clone(), q.track_gain()))
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
        crate::dj_voice::track_changed();
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

        let mut q = self.lock_queue().await;
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
            let q = self.lock_queue().await;
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
        let json = self.lock_queue().await.snapshot().to_json()?;
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
        let mut q = self.lock_queue().await;
        q.restore(snap, |t| !prefs.blocks(t));
        Ok(q.current.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: u64, artists: &[&str]) -> Track {
        let mut t = Track::new(id);
        t.extra.insert(
            "artists".into(),
            serde_json::Value::Array(
                artists
                    .iter()
                    .map(|a| serde_json::json!({ "name": a }))
                    .collect(),
            ),
        );
        t
    }

    #[test]
    fn a_track_is_by_an_artist_only_when_listed() {
        assert!(by_artist(&track(1, &["King Von", "Lil Durk"]), "king von"));
        assert!(!by_artist(&track(2, &["King Von"]), "Von"));
        assert!(by_artist(
            &track(3, &["Tyler, The Creator"]),
            "Tyler, The Creator"
        ));
        assert!(!by_artist(&track(4, &["UK Bass Channel"]), "King Von"));
    }

    #[test]
    fn a_requested_artist_fills_half_the_block_alternating() {
        let a: Vec<Track> = (1..=10).map(|i| track(i, &["A"])).collect();
        let b: Vec<Track> = (11..=12).map(|i| track(i, &["B"])).collect();
        let c: Vec<Track> = (21..=22).map(|i| track(i, &["C"])).collect();
        let ids: Vec<u64> = mix_block(vec![a, b, c], 8, true)
            .iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(ids, vec![1, 11, 2, 21, 3, 12, 4, 22]);
    }

    #[test]
    fn without_a_request_artists_take_turns_and_nothing_repeats() {
        let a = vec![track(1, &["A"]), track(2, &["A"])];
        let b = vec![track(1, &["A", "B"]), track(3, &["B"])];
        let ids: Vec<u64> = mix_block(vec![a, b], 10, false)
            .iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(ids, vec![1, 3, 2]);
    }

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
        for (e, a) in [(false, false), (true, false), (false, true), (true, true)] {
            let p = ContentPrefs::hiding(e, a);
            assert_eq!(ContentPrefs::from_bits(p.to_bits()).to_bits(), p.to_bits());
        }
        assert_eq!(ContentPrefs::hiding(true, false).to_bits(), 1);
        assert_eq!(ContentPrefs::hiding(false, true).to_bits(), 2);
        let strict = ContentPrefs {
            allow_explicit: false,
            allow_ai: true,
        };
        assert!(strict.blocks(&t));
    }
}
