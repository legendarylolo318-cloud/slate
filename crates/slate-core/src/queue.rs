//! Playback queue model: context queue, manual ("play next") queue, history,
//! shuffle with order restoration, repeat modes and persistence.
//!
//! Ported from the React layer (`atoms/playback.ts`, `hooks/usePlaybackActions.ts`).
//! This module is pure state transitions — no I/O, no audio — so every rule can
//! be unit-tested. The side effects (starting playback, skipping unplayable
//! tracks, fetching autoplay radio) live in the caller, which reports back what
//! happened (for instance via [`Queue::requeue_head`]).
//!
//! Tracks are kept as their JSON shape plus a queue id (`_qid`), so snapshots
//! stay compatible with the ones the original app wrote.

use std::sync::atomic::{AtomicU64, Ordering};

use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const MAX_HISTORY_TRACKS: usize = 500;
/// Seconds into a track after which "previous" restarts it instead of going back.
pub const RESTART_THRESHOLD_SECS: f32 = 3.0;

static QID_COUNTER: AtomicU64 = AtomicU64::new(0);

fn next_qid() -> String {
    format!("q{}", QID_COUNTER.fetch_add(1, Ordering::Relaxed) + 1)
}

/// Make sure freshly minted ids never collide with restored ones.
fn advance_counter_past(tracks: &[Track]) {
    for t in tracks {
        let n = t
            .qid
            .as_deref()
            .and_then(|q| q.strip_prefix('q'))
            .and_then(|n| n.parse::<u64>().ok());
        if let Some(n) = n {
            QID_COUNTER.fetch_max(n, Ordering::Relaxed);
        }
    }
}

/// A queued item (audio track or video). Unknown fields round-trip untouched.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: u64,
    #[serde(rename = "_qid", default, skip_serializing_if = "Option::is_none")]
    pub qid: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Track {
    pub fn new(id: u64) -> Self {
        Self {
            id,
            qid: None,
            extra: Map::new(),
        }
    }

    pub fn is_video(&self) -> bool {
        self.extra.get("itemType").and_then(Value::as_str) == Some("video")
    }

    /// All credited artists, for scrobblers that take a combined string.
    pub fn artist_display(&self) -> String {
        let names: Vec<&str> = self
            .extra
            .get("artists")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.get("name").and_then(Value::as_str))
                    .collect()
            })
            .unwrap_or_default();
        if names.is_empty() {
            self.artist_name().to_string()
        } else {
            names.join(", ")
        }
    }

    pub fn artist_name(&self) -> &str {
        self.extra
            .get("artist")
            .and_then(|a| a.get("name"))
            .and_then(Value::as_str)
            .or_else(|| {
                self.extra
                    .get("artists")
                    .and_then(|a| a.get(0))
                    .and_then(|a| a.get("name"))
                    .and_then(Value::as_str)
            })
            .unwrap_or("")
    }

    /// TIDAL image id of the cover (album cover, or the video's own image).
    pub fn cover_id(&self) -> Option<&str> {
        self.extra
            .get("album")
            .and_then(|a| a.get("cover"))
            .and_then(Value::as_str)
            .or_else(|| self.extra.get("imageId").and_then(Value::as_str))
    }

    pub fn duration_secs(&self) -> f32 {
        self.extra
            .get("duration")
            .and_then(Value::as_f64)
            .unwrap_or(0.0) as f32
    }

    pub fn title(&self) -> &str {
        self.extra
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
    }

    /// Metadata says the track cannot be streamed right now. Missing flags mean
    /// "available" so sparse responses do not grey everything out.
    pub fn is_unavailable(&self, now_epoch_secs: i64) -> bool {
        if self.is_video() {
            return false;
        }
        if self.extra.get("streamReady").and_then(Value::as_bool) == Some(false)
            || self.extra.get("allowStreaming").and_then(Value::as_bool) == Some(false)
        {
            return true;
        }
        self.extra
            .get("streamStartDate")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_secs)
            .is_some_and(|ts| ts > now_epoch_secs)
    }

    fn stamped(mut self) -> Self {
        if self.qid.is_none() {
            self.qid = Some(next_qid());
        }
        self
    }

    fn restamped(mut self) -> Self {
        self.qid = Some(next_qid());
        self
    }

    fn source_tag(&self) -> Option<Source> {
        self.extra
            .get("_source")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    fn strip_runtime(mut self) -> Self {
        self.extra.remove("_playingFrom");
        self.extra.remove("_contextFrom");
        self
    }
}

/// Minimal RFC 3339 → epoch seconds (`YYYY-MM-DDTHH:MM:SS` with optional
/// fraction and zone offset). Enough for TIDAL's `streamStartDate`.
fn parse_rfc3339_secs(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |a: usize, z: usize| s.get(a..z)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, se) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    let mut offset = 0i64;
    let rest = &s[19..];
    let rest = rest.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    if let Some(sign) = rest.chars().next().filter(|c| *c == '+' || *c == '-') {
        let oh = rest.get(1..3)?.parse::<i64>().ok()?;
        let om = rest
            .get(4..6)
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        offset = (oh * 3600 + om * 60) * if sign == '+' { 1 } else { -1 };
    }
    // days from civil (Howard Hinnant)
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + se - offset)
}

/// Where the current queue came from ("Playing from …").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: Value,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    #[serde(rename = "mixType", default, skip_serializing_if = "Option::is_none")]
    pub mix_type: Option<String>,
    /// Every track of the source, used to rebuild the queue for repeat-all.
    #[serde(default)]
    pub tracks: Vec<Track>,
}

impl Source {
    fn tag(&self) -> Self {
        Self {
            tracks: Vec::new(),
            ..self.clone()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Repeat {
    #[default]
    Off,
    All,
    One,
}

impl Repeat {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::All,
            2 => Self::One,
            _ => Self::Off,
        }
    }

    pub fn as_u8(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::All => 1,
            Self::One => 2,
        }
    }

    pub fn cycled(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }
}

/// Why a candidate track was passed over while advancing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    /// Filtered by content preferences; dropped silently.
    Blocked,
    /// Known unplayable; dropped, counts toward the failure cap.
    Unavailable,
}

/// What `next` decided.
#[derive(Debug, Clone, PartialEq)]
pub enum Next {
    /// Repeat-one: replay the current track in place.
    ReplayCurrent,
    /// Start this track. `chosen_by_user` is false for autoplay radio.
    Play { track: Track, chosen_by_user: bool },
    /// Queue exhausted and autoplay is enabled: fetch radio for `current`,
    /// then call [`Queue::install_autoplay`].
    NeedAutoplay,
    /// Nothing left.
    Stop,
}

/// What `previous` decided.
#[derive(Debug, Clone, PartialEq)]
pub enum Previous {
    Restart,
    Play(Track),
    /// No history and the track just started: nothing to do.
    Nothing,
}

#[derive(Debug, Default, Clone)]
pub struct Queue {
    pub current: Option<Track>,
    /// Context queue (after the manual queue).
    pub queue: Vec<Track>,
    pub manual: Vec<Track>,
    pub history: Vec<Track>,
    /// Pre-shuffle order of `queue`; `Some` only while shuffle is on.
    pub original: Option<Vec<Track>>,
    pub shuffle: bool,
    pub repeat: Repeat,
    pub autoplay: bool,
    pub playback_source: Option<Source>,
    pub context_source: Option<Source>,
    /// Album order (album replay gain) vs mixed context (track replay gain).
    pub use_track_gain: bool,
    /// Bumped whenever the context queue is replaced wholesale.
    pub epoch: u64,
    autoplay_ids: Vec<u64>,
}

fn stamp_all(tracks: Vec<Track>) -> Vec<Track> {
    tracks.into_iter().map(Track::restamped).collect()
}

impl Queue {
    pub fn new() -> Self {
        Self {
            use_track_gain: true,
            ..Self::default()
        }
    }

    // ---------- building ----------

    /// Replace the context queue with `tracks` (in order).
    pub fn set_tracks(&mut self, tracks: Vec<Track>, source: Option<Source>, album_mode: bool) {
        self.epoch += 1;
        self.use_track_gain = !album_mode;
        self.original = None;
        self.manual.clear();
        self.context_source = None;
        self.playback_source = source.map(|s| Source {
            tracks: stamp_all(s.tracks),
            ..s
        });
        self.queue = stamp_all(tracks);
        self.autoplay_ids.clear();
    }

    /// Replace the context queue with a shuffled `tracks`.
    pub fn set_shuffled_tracks(
        &mut self,
        tracks: Vec<Track>,
        source: Option<Source>,
        album_mode: bool,
        rng: &mut impl rand::Rng,
    ) {
        self.set_tracks(tracks, source, album_mode);
        let ordered = self.queue.clone();
        self.original = self.shuffle.then(|| ordered.clone());
        self.queue.shuffle(rng);
    }

    /// "Play this track from a list": queue the rest, rotated to start after it.
    /// Returns the track to start (already removed from the queue).
    pub fn play_from_source(
        &mut self,
        track: Track,
        all: Vec<Track>,
        source: Option<Source>,
        album_mode: bool,
        rng: &mut impl rand::Rng,
    ) -> Track {
        let idx = all.iter().position(|t| t.id == track.id);
        let rest: Vec<Track> = match idx {
            Some(i) => all[i + 1..]
                .iter()
                .chain(all[..i].iter())
                .cloned()
                .collect(),
            None => all.into_iter().filter(|t| t.id != track.id).collect(),
        };
        if self.shuffle {
            self.set_shuffled_tracks(rest, source, album_mode, rng);
        } else {
            self.set_tracks(rest, source, album_mode);
        }
        track
    }

    /// The full list of the playing source arrived after playback already started
    /// on a partial one. In order: rebuild the queue as `play_from_source` would
    /// have (everything after the current track, then the wrap-around). Shuffled:
    /// append only tracks not seen yet, shuffled. The manual queue is untouched.
    pub fn extend_context(&mut self, all: Vec<Track>, rng: &mut impl rand::Rng) {
        let Some(current_id) = self.current.as_ref().map(|t| t.id) else {
            return;
        };
        if let Some(src) = self.playback_source.as_mut() {
            src.tracks = stamp_all(all.clone());
        }
        if self.shuffle {
            let seen: std::collections::HashSet<u64> = self
                .queue
                .iter()
                .chain(self.history.iter())
                .map(|t| t.id)
                .chain(std::iter::once(current_id))
                .collect();
            let fresh: Vec<Track> = all.into_iter().filter(|t| !seen.contains(&t.id)).collect();
            self.append(fresh, true, rng);
        } else {
            let rest: Vec<Track> = match all.iter().position(|t| t.id == current_id) {
                Some(i) => all[i + 1..]
                    .iter()
                    .chain(all[..i].iter())
                    .cloned()
                    .collect(),
                None => all.into_iter().filter(|t| t.id != current_id).collect(),
            };
            self.queue = stamp_all(rest);
            self.original = None;
        }
        self.epoch += 1;
    }

    /// "Play all" / "Shuffle all". Returns the first track, or `None` if empty.
    pub fn play_all(
        &mut self,
        all: Vec<Track>,
        source: Option<Source>,
        album_mode: bool,
        shuffle: bool,
        rng: &mut impl rand::Rng,
    ) -> Option<Track> {
        if all.is_empty() {
            return None;
        }
        if shuffle || self.shuffle {
            let mut all = all;
            let first = all.remove(rng_index(rng, all.len()));
            self.set_shuffled_tracks(all, source, album_mode, rng);
            Some(first)
        } else {
            let mut it = all.into_iter();
            let first = it.next();
            self.set_tracks(it.collect(), source, album_mode);
            first
        }
    }

    /// Append to the manual queue (plays before the context queue).
    pub fn add_to_queue(&mut self, track: Track, source: Option<&Source>) {
        self.manual.push(Self::tagged(track, source).stamped());
    }

    /// Insert at the head of the manual queue ("play next").
    pub fn play_next_in_queue(&mut self, track: Track, source: Option<&Source>) {
        self.manual.insert(0, Self::tagged(track, source).stamped());
    }

    fn tagged(mut track: Track, source: Option<&Source>) -> Track {
        if let Some(s) = source {
            if let Ok(v) = serde_json::to_value(s.tag()) {
                track.extra.insert("_source".into(), v);
            }
        }
        track
    }

    /// Append to the context queue, optionally shuffling the new tracks in.
    pub fn append(&mut self, tracks: Vec<Track>, shuffle: bool, rng: &mut impl rand::Rng) {
        let mut fresh = stamp_all(tracks);
        if let Some(orig) = self.original.as_mut() {
            orig.extend(fresh.iter().cloned());
        }
        if shuffle {
            fresh.shuffle(rng);
        }
        self.queue.extend(fresh);
    }

    /// Remove by position in the combined list (manual first, then context).
    pub fn remove_at(&mut self, index: usize) -> Option<Track> {
        if index < self.manual.len() {
            return Some(self.manual.remove(index));
        }
        let i = index - self.manual.len();
        if i >= self.queue.len() {
            return None;
        }
        let t = self.queue.remove(i);
        self.drop_from_original(&t);
        Some(t)
    }

    /// Take the track at `index` of the combined list so the caller can play it.
    pub fn take_at(&mut self, index: usize) -> Option<Track> {
        self.remove_at(index)
    }

    pub fn clear(&mut self) {
        self.epoch += 1;
        self.queue.clear();
        self.manual.clear();
        self.original = None;
        self.playback_source = None;
        self.context_source = None;
        self.autoplay_ids.clear();
    }

    /// Replace both queues after a drag-and-drop reorder, keeping queue ids.
    pub fn reorder(&mut self, tracks: Vec<Track>, manual_count: usize) {
        let stamped: Vec<Track> = tracks.into_iter().map(Track::stamped).collect();
        let mc = manual_count.min(stamped.len());
        self.manual = stamped[..mc].to_vec();
        self.queue = stamped[mc..].to_vec();
    }

    fn drop_from_original(&mut self, track: &Track) {
        if let Some(orig) = self.original.as_mut() {
            orig.retain(|t| t.qid != track.qid);
        }
    }

    // ---------- shuffle / repeat ----------

    pub fn toggle_shuffle(&mut self, rng: &mut impl rand::Rng) {
        if !self.shuffle {
            self.original = Some(self.queue.clone());
            self.queue.shuffle(rng);
            self.shuffle = true;
        } else {
            if let Some(orig) = self.original.take() {
                // Match on qid, not id, so duplicate tracks survive.
                let live: std::collections::HashSet<_> =
                    self.queue.iter().filter_map(|t| t.qid.clone()).collect();
                self.queue = orig
                    .into_iter()
                    .filter(|t| t.qid.as_ref().is_some_and(|q| live.contains(q)))
                    .collect();
            }
            self.shuffle = false;
        }
    }

    pub fn cycle_repeat(&mut self) -> Repeat {
        self.repeat = self.repeat.cycled();
        self.repeat
    }

    // ---------- starting a track ----------

    /// Record `track` as now playing; the previous one goes to history.
    pub fn begin(&mut self, track: Track, skip_history_push: bool) {
        if let Some(prev) = self.current.take() {
            if !skip_history_push {
                self.history.push(prev);
                if self.history.len() > MAX_HISTORY_TRACKS {
                    let excess = self.history.len() - MAX_HISTORY_TRACKS;
                    self.history.drain(..excess);
                }
            }
        }
        let mut track = track.stamped();
        // Remember where this was playing from so Previous can restore it.
        if let Ok(v) = serde_json::to_value(&self.playback_source) {
            track.extra.insert("_playingFrom".into(), v);
        }
        if let Ok(v) = serde_json::to_value(&self.context_source) {
            track.extra.insert("_contextFrom".into(), v);
        }
        self.current = Some(track);
    }

    /// A start attempt failed for a transient reason: put the track back where
    /// it came from so a retry plays it instead of skipping it.
    pub fn requeue_head(&mut self, track: Track) {
        self.queue.insert(0, track);
    }

    // ---------- advancing ----------

    /// Decide what plays next. `skip` classifies candidates so blocked or
    /// unavailable tracks are dropped here; the returned `Vec` is the tracks
    /// that were dropped as [`Skip::Unavailable`] (for toasts and failure caps).
    ///
    /// On `Next::Play` the track has been removed from its queue but `begin`
    /// has not run; the caller starts it and then calls `begin`.
    pub fn next(
        &mut self,
        explicit: bool,
        rng: &mut impl rand::Rng,
        mut skip: impl FnMut(&Track) -> Option<Skip>,
    ) -> (Next, Vec<Track>) {
        let mut dropped = Vec::new();

        if self.repeat == Repeat::One && !explicit && self.current.is_some() {
            return (Next::ReplayCurrent, dropped);
        }

        // Manual queue first.
        while !self.manual.is_empty() {
            let track = self.manual.remove(0);
            match skip(&track) {
                Some(Skip::Blocked) => continue,
                Some(Skip::Unavailable) => {
                    dropped.push(track);
                    continue;
                }
                None => {}
            }
            if let Some(tag) = track.source_tag() {
                if self.context_source.is_none() {
                    self.context_source = self.playback_source.clone();
                }
                self.playback_source = Some(tag);
            }
            let mut track = track;
            track.extra.remove("_source");
            return (
                Next::Play {
                    track,
                    chosen_by_user: explicit,
                },
                dropped,
            );
        }

        // Manual queue exhausted: restore the stashed context source.
        if let Some(src) = self.context_source.take() {
            self.playback_source = Some(src);
        }

        while !self.queue.is_empty() {
            let track = self.queue.remove(0);
            let is_autoplay = self.autoplay_ids.contains(&track.id);
            self.autoplay_ids.retain(|id| *id != track.id);
            self.drop_from_original(&track);
            match skip(&track) {
                Some(Skip::Blocked) => continue,
                Some(Skip::Unavailable) => {
                    dropped.push(track);
                    continue;
                }
                None => {}
            }
            return (
                Next::Play {
                    track,
                    chosen_by_user: !is_autoplay,
                },
                dropped,
            );
        }

        if self.repeat == Repeat::All {
            if let Some(first) = self.rebuild_for_repeat_all(rng, &mut skip) {
                return (
                    Next::Play {
                        track: first,
                        chosen_by_user: true,
                    },
                    dropped,
                );
            }
            return (Next::Stop, dropped);
        }

        if self.autoplay && self.current.is_some() {
            return (Next::NeedAutoplay, dropped);
        }
        (Next::Stop, dropped)
    }

    fn rebuild_for_repeat_all(
        &mut self,
        rng: &mut impl rand::Rng,
        skip: &mut impl FnMut(&Track) -> Option<Skip>,
    ) -> Option<Track> {
        let source = self
            .context_source
            .as_ref()
            .or(self.playback_source.as_ref());
        let from_source = source.map(|s| s.tracks.clone()).filter(|t| !t.is_empty());
        let has_source = from_source.is_some();
        let raw = from_source.unwrap_or_else(|| {
            self.history
                .iter()
                .cloned()
                .chain(self.current.clone())
                .collect()
        });
        let mut all: Vec<Track> = stamp_all(raw)
            .into_iter()
            .filter(|t| skip(t).is_none())
            .collect();
        if all.is_empty() {
            return None;
        }
        if self.shuffle {
            all.shuffle(rng);
        }
        let first = all.remove(0);
        self.original = self.shuffle.then(|| {
            let mut o = all.clone();
            o.sort_by_key(|t| t.qid.clone());
            o
        });
        self.queue = all;
        // A source-backed loop keeps history so Previous survives the wrap.
        if !has_source {
            self.history.clear();
        }
        Some(first)
    }

    /// Install autoplay radio after `Next::NeedAutoplay`. Tracks already heard
    /// or filtered out are dropped. Returns the first track to play.
    pub fn install_autoplay(
        &mut self,
        radio: Vec<Track>,
        mut skip: impl FnMut(&Track) -> Option<Skip>,
    ) -> Option<Track> {
        let mut heard: std::collections::HashSet<u64> = self.history.iter().map(|t| t.id).collect();
        if let Some(c) = &self.current {
            heard.insert(c.id);
        }
        let mut fresh: Vec<Track> = radio
            .into_iter()
            .filter(|t| !heard.contains(&t.id) && skip(t).is_none())
            .collect();
        if fresh.is_empty() {
            return None;
        }
        let first = fresh.remove(0);
        self.epoch += 1;
        self.autoplay_ids = fresh.iter().map(|t| t.id).collect();
        self.queue = stamp_all(fresh);
        self.original = None;
        self.use_track_gain = true;
        Some(first)
    }

    /// Decide what "previous" does at `position_secs` into the current track.
    pub fn previous(&mut self, position_secs: f32) -> Previous {
        if position_secs > RESTART_THRESHOLD_SECS {
            return Previous::Restart;
        }
        let Some(mut prev) = self.history.pop() else {
            return Previous::Nothing;
        };
        // The track we are leaving goes to the head of the manual queue, tagged
        // with its source so going forward restores "Playing from".
        if let Some(cur) = self.current.take() {
            let cur = cur.strip_runtime();
            let tagged = Self::tagged(cur, self.playback_source.as_ref());
            self.manual.insert(0, tagged);
        }
        if let Some(from) = prev.extra.get("_playingFrom") {
            self.playback_source = serde_json::from_value(from.clone()).ok().flatten();
            self.context_source = prev
                .extra
                .get("_contextFrom")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .flatten();
        }
        prev = prev.strip_runtime();
        Previous::Play(prev)
    }

    /// The track to pre-arm for a gapless transition, or `None` when the boundary
    /// must go through the normal track-finished path: repeat-one, nothing queued,
    /// unavailable/blocked/video heads, or a head from a different source (so
    /// "Playing from" never goes wrong).
    pub fn pick_gapless_next(
        &self,
        now_epoch_secs: i64,
        blocked: impl Fn(&Track) -> bool,
    ) -> Option<&Track> {
        if self.repeat == Repeat::One {
            return None;
        }
        let head = self.manual.first().or_else(|| self.queue.first())?;
        if head.is_unavailable(now_epoch_secs) || blocked(head) || head.is_video() {
            return None;
        }
        if let Some(tag) = head.source_tag() {
            let current = self.playback_source.as_ref().map(|s| &s.id);
            if current != Some(&tag.id) {
                return None;
            }
        }
        Some(head)
    }

    /// The engine already switched to a pre-armed track: reconcile the queues and
    /// make it current. Removes exactly one entry, matched by `qid` (falling back
    /// to the track id), so duplicates elsewhere in the list survive.
    pub fn adopt_advanced(&mut self, track_id: u64, qid: &str) -> Option<Track> {
        let by_qid = |t: &Track| t.qid.as_deref() == Some(qid);
        let track = if let Some(i) = self.manual.iter().position(by_qid) {
            self.manual.remove(i)
        } else {
            let i = self
                .queue
                .iter()
                .position(by_qid)
                .or_else(|| self.queue.iter().position(|t| t.id == track_id))?;
            let t = self.queue.remove(i);
            self.drop_from_original(&t);
            self.autoplay_ids.retain(|id| *id != t.id);
            t
        };
        let mut track = track;
        track.extra.remove("_source");
        self.begin(track.clone(), false);
        Some(track)
    }

    /// Everything currently queued, manual first (what the queue view shows).
    pub fn upcoming(&self) -> impl Iterator<Item = &Track> {
        self.manual.iter().chain(self.queue.iter())
    }

    // ---------- persistence ----------

    pub fn snapshot(&self) -> Snapshot {
        let strip = |v: &[Track]| {
            v.iter()
                .cloned()
                .map(Track::strip_runtime)
                .collect::<Vec<_>>()
        };
        let start = self.history.len().saturating_sub(MAX_HISTORY_TRACKS);
        let strip_src = |s: &Option<Source>| {
            s.as_ref().map(|s| Source {
                tracks: strip(&s.tracks),
                ..s.clone()
            })
        };
        Snapshot {
            current_track: self.current.clone().map(Track::strip_runtime),
            queue: strip(&self.queue),
            history: strip(&self.history[start..]),
            original_queue: self.original.as_deref().map(strip),
            manual_queue: strip(&self.manual),
            playback_source: strip_src(&self.playback_source),
            context_source: strip_src(&self.context_source),
        }
    }

    /// Restore a snapshot. `keep` filters out tracks that should not come back
    /// (content preferences, no longer playable).
    pub fn restore(&mut self, snap: Snapshot, keep: impl Fn(&Track) -> bool) {
        let filt = |v: Vec<Track>| v.into_iter().filter(|t| keep(t)).collect::<Vec<_>>();
        let queue = filt(snap.queue);
        let history = filt(snap.history);
        let manual = filt(snap.manual_queue);
        let original = snap.original_queue.map(filt);
        for list in [&queue, &history, &manual] {
            advance_counter_past(list);
        }
        if let Some(o) = &original {
            advance_counter_past(o);
        }
        self.current = snap.current_track;
        self.queue = queue.into_iter().map(Track::stamped).collect();
        self.manual = manual.into_iter().map(Track::stamped).collect();
        self.history = history;
        self.original = original.filter(|_| self.shuffle);
        self.playback_source = snap.playback_source;
        self.context_source = snap.context_source;
        self.epoch += 1;
    }
}

fn rng_index(rng: &mut impl rand::Rng, len: usize) -> usize {
    use rand::RngExt;
    rng.random_range(0..len)
}

/// Persisted shape; identical to the original app's `PlaybackSnapshot`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    #[serde(default)]
    pub current_track: Option<Track>,
    #[serde(default)]
    pub queue: Vec<Track>,
    #[serde(default)]
    pub history: Vec<Track>,
    #[serde(default)]
    pub original_queue: Option<Vec<Track>>,
    #[serde(default)]
    pub manual_queue: Vec<Track>,
    #[serde(default)]
    pub playback_source: Option<Source>,
    #[serde(default)]
    pub context_source: Option<Source>,
}

impl Snapshot {
    pub fn to_json(&self) -> Result<String, crate::SlateError> {
        Ok(serde_json::to_string(self)?)
    }

    pub fn from_json(json: &str) -> Result<Self, crate::SlateError> {
        Ok(serde_json::from_str(json)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    fn rng() -> StdRng {
        StdRng::seed_from_u64(7)
    }

    fn t(id: u64) -> Track {
        Track::new(id)
    }

    fn ids(v: &[Track]) -> Vec<u64> {
        v.iter().map(|t| t.id).collect()
    }

    fn never(_: &Track) -> Option<Skip> {
        None
    }

    fn src(name: &str, tracks: Vec<Track>) -> Source {
        Source {
            kind: "album".into(),
            id: Value::from(1),
            name: name.into(),
            image: None,
            subtitle: None,
            mix_type: None,
            tracks,
        }
    }

    fn play(q: &mut Queue, explicit: bool) -> Option<u64> {
        let (n, _) = q.next(explicit, &mut rng(), never);
        match n {
            Next::Play { track, .. } => {
                let id = track.id;
                q.begin(track, false);
                Some(id)
            }
            _ => None,
        }
    }

    #[test]
    fn play_from_source_rotates_after_chosen_track() {
        let mut q = Queue::new();
        let first = q.play_from_source(t(3), vec![t(1), t(2), t(3), t(4)], None, false, &mut rng());
        assert_eq!(first.id, 3);
        assert_eq!(ids(&q.queue), vec![4, 1, 2]);
    }

    #[test]
    fn manual_queue_plays_before_context() {
        let mut q = Queue::new();
        q.set_tracks(vec![t(1), t(2)], None, false);
        q.add_to_queue(t(9), None);
        q.play_next_in_queue(t(8), None);
        assert_eq!(play(&mut q, true), Some(8));
        assert_eq!(play(&mut q, true), Some(9));
        assert_eq!(play(&mut q, true), Some(1));
        assert_eq!(play(&mut q, true), Some(2));
        assert_eq!(play(&mut q, true), None);
    }

    #[test]
    fn repeat_one_replays_unless_explicit() {
        let mut q = Queue::new();
        q.set_tracks(vec![t(2)], None, false);
        q.begin(t(1), false);
        q.repeat = Repeat::One;
        assert_eq!(q.next(false, &mut rng(), never).0, Next::ReplayCurrent);
        assert_eq!(play(&mut q, true), Some(2));
    }

    #[test]
    fn repeat_all_rebuilds_from_source() {
        let mut q = Queue::new();
        let all = vec![t(1), t(2), t(3)];
        let s = src("A", all.clone());
        let first = q.play_from_source(t(1), all, Some(s), false, &mut rng());
        q.begin(first, false);
        q.repeat = Repeat::All;
        assert_eq!(play(&mut q, false), Some(2));
        assert_eq!(play(&mut q, false), Some(3));
        // exhausted: wraps to the start, history is kept for source-backed loops
        assert_eq!(play(&mut q, false), Some(1));
        assert_eq!(ids(&q.queue), vec![2, 3]);
        assert!(!q.history.is_empty());
    }

    #[test]
    fn repeat_all_without_source_uses_history_and_clears_it() {
        let mut q = Queue::new();
        q.begin(t(1), false);
        q.begin(t(2), false);
        q.repeat = Repeat::All;
        let (n, _) = q.next(false, &mut rng(), never);
        assert!(matches!(n, Next::Play { ref track, .. } if track.id == 1));
        assert!(q.history.is_empty());
        assert_eq!(ids(&q.queue), vec![2]);
    }

    #[test]
    fn stops_when_empty_and_autoplay_requests_radio() {
        let mut q = Queue::new();
        q.begin(t(1), false);
        assert_eq!(q.next(false, &mut rng(), never).0, Next::Stop);
        q.autoplay = true;
        assert_eq!(q.next(false, &mut rng(), never).0, Next::NeedAutoplay);
        let first = q.install_autoplay(vec![t(1), t(5), t(6)], never);
        assert_eq!(first.map(|t| t.id), Some(5));
        assert_eq!(ids(&q.queue), vec![6]);
        // autoplay tracks are not "chosen by user"
        let (n, _) = q.next(false, &mut rng(), never);
        assert!(matches!(
            n,
            Next::Play {
                chosen_by_user: false,
                ..
            }
        ));
    }

    #[test]
    fn skip_reports_unavailable_and_drops_blocked() {
        let mut q = Queue::new();
        q.set_tracks(vec![t(1), t(2), t(3)], None, false);
        let (n, dropped) = q.next(true, &mut rng(), |t| match t.id {
            1 => Some(Skip::Blocked),
            2 => Some(Skip::Unavailable),
            _ => None,
        });
        assert!(matches!(n, Next::Play { ref track, .. } if track.id == 3));
        assert_eq!(ids(&dropped), vec![2]);
    }

    #[test]
    fn shuffle_toggle_restores_order_minus_played_and_keeps_duplicates() {
        let mut q = Queue::new();
        q.set_tracks(vec![t(1), t(2), t(1), t(3), t(4)], None, false);
        let before = ids(&q.queue);
        q.toggle_shuffle(&mut rng());
        assert!(q.shuffle && q.original.is_some());
        let mut shuffled = ids(&q.queue);
        shuffled.sort();
        let mut sorted = before.clone();
        sorted.sort();
        assert_eq!(shuffled, sorted);
        // play one, then un-shuffle: the played track must not come back
        let played = q.queue.remove(0);
        q.drop_from_original(&played);
        q.toggle_shuffle(&mut rng());
        assert!(!q.shuffle && q.original.is_none());
        assert_eq!(q.queue.len(), 4);
        let mut got = ids(&q.queue);
        got.sort();
        let mut want = before;
        if let Some(i) = want.iter().position(|id| *id == played.id) {
            want.remove(i);
        }
        want.sort();
        assert_eq!(got, want);
    }

    #[test]
    fn previous_restarts_after_threshold_then_walks_history() {
        let mut q = Queue::new();
        q.begin(t(1), false);
        q.begin(t(2), false);
        assert_eq!(q.previous(10.0), Previous::Restart);
        match q.previous(1.0) {
            Previous::Play(p) => assert_eq!(p.id, 1),
            other => panic!("unexpected {other:?}"),
        }
        // the track we left is next in line
        assert_eq!(q.manual.first().map(|t| t.id), Some(2));
        assert!(q.history.is_empty());
        let mut empty = Queue::new();
        empty.begin(t(1), false);
        assert_eq!(empty.previous(1.0), Previous::Nothing);
    }

    #[test]
    fn previous_restores_playing_from_source() {
        let mut q = Queue::new();
        q.playback_source = Some(src("First", vec![]));
        q.begin(t(1), false);
        q.playback_source = Some(src("Second", vec![]));
        q.begin(t(2), false);
        let Previous::Play(_) = q.previous(0.0) else {
            panic!("expected play");
        };
        assert_eq!(
            q.playback_source.as_ref().map(|s| s.name.as_str()),
            Some("First")
        );
        // going forward re-applies the source recorded on the manual entry
        let (n, _) = q.next(true, &mut rng(), never);
        assert!(matches!(n, Next::Play { ref track, .. } if track.id == 2));
        assert_eq!(
            q.playback_source.as_ref().map(|s| s.name.as_str()),
            Some("Second")
        );
    }

    #[test]
    fn history_is_capped() {
        let mut q = Queue::new();
        for i in 0..(MAX_HISTORY_TRACKS as u64 + 20) {
            q.begin(t(i), false);
        }
        assert_eq!(q.history.len(), MAX_HISTORY_TRACKS);
        assert_eq!(q.history[0].id, 19);
    }

    #[test]
    fn skip_history_push_leaves_history_alone() {
        let mut q = Queue::new();
        q.begin(t(1), false);
        q.begin(t(2), true);
        assert!(q.history.is_empty());
    }

    #[test]
    fn remove_and_take_by_combined_index() {
        let mut q = Queue::new();
        q.set_tracks(vec![t(1), t(2)], None, false);
        q.add_to_queue(t(9), None);
        assert_eq!(q.remove_at(0).map(|t| t.id), Some(9));
        assert_eq!(q.take_at(1).map(|t| t.id), Some(2));
        assert_eq!(q.take_at(5), None);
    }

    #[test]
    fn snapshot_round_trips_and_matches_original_field_names() {
        let mut q = Queue::new();
        let mut tr = t(1);
        tr.extra.insert("title".into(), Value::from("Song"));
        q.set_tracks(vec![tr, t(2)], Some(src("A", vec![t(1), t(2)])), false);
        q.begin(t(7), false);
        let json = q.snapshot().to_json().expect("json");
        for key in [
            "currentTrack",
            "manualQueue",
            "playbackSource",
            "originalQueue",
        ] {
            assert!(json.contains(key), "missing {key} in {json}");
        }
        let mut r = Queue::new();
        r.restore(Snapshot::from_json(&json).expect("parse"), |_| true);
        assert_eq!(ids(&r.queue), ids(&q.queue));
        assert_eq!(r.queue[0].title(), "Song");
        assert_eq!(r.current.as_ref().map(|t| t.id), Some(7));
        assert_eq!(
            r.playback_source.as_ref().map(|s| s.name.as_str()),
            Some("A")
        );
    }

    #[test]
    fn restore_filters_and_advances_qid_counter() {
        let snap = Snapshot::from_json(
            r#"{"currentTrack":null,"queue":[{"id":1,"_qid":"q999"},{"id":2}],"history":[]}"#,
        )
        .expect("parse");
        let mut q = Queue::new();
        q.restore(snap, |t| t.id != 2);
        assert_eq!(ids(&q.queue), vec![1]);
        assert_eq!(q.queue[0].qid.as_deref(), Some("q999"));
        assert!(Track::new(5)
            .stamped()
            .qid
            .as_deref()
            .is_some_and(|s| s != "q999"));
        let n: u64 = Track::new(5).stamped().qid.unwrap_or_default()[1..]
            .parse()
            .unwrap_or(0);
        assert!(n > 999);
    }

    #[test]
    fn gapless_prediction_rules() {
        let mut q = Queue::new();
        let now = 1_700_000_000;
        assert!(q.pick_gapless_next(now, |_| false).is_none());
        q.set_tracks(vec![t(1), t(2)], Some(src("A", vec![])), false);
        assert_eq!(q.pick_gapless_next(now, |_| false).map(|t| t.id), Some(1));
        // manual head wins
        q.add_to_queue(t(9), None);
        assert_eq!(q.pick_gapless_next(now, |_| false).map(|t| t.id), Some(9));
        // blocked head: no gapless (the normal path drains it)
        assert!(q.pick_gapless_next(now, |t| t.id == 9).is_none());
        // repeat-one
        q.repeat = Repeat::One;
        assert!(q.pick_gapless_next(now, |_| false).is_none());
        q.repeat = Repeat::Off;
        // a head from another source is not gapless
        let other = Source {
            id: Value::from(99),
            ..src("B", vec![])
        };
        q.manual.clear();
        q.add_to_queue(t(7), Some(&other));
        assert!(q.pick_gapless_next(now, |_| false).is_none());
        // video heads are not gapless
        q.manual.clear();
        let mut v = t(8);
        v.extra.insert("itemType".into(), Value::from("video"));
        q.manual.push(v.stamped());
        assert!(q.pick_gapless_next(now, |_| false).is_none());
    }

    #[test]
    fn adopt_advanced_removes_exactly_one_by_qid() {
        let mut q = Queue::new();
        q.set_tracks(vec![t(1), t(2), t(1)], None, false);
        q.begin(t(0), false);
        let second_one = q.queue[2].qid.clone().unwrap_or_default();
        let got = q.adopt_advanced(1, &second_one).expect("adopted");
        assert_eq!(got.id, 1);
        assert_eq!(ids(&q.queue), vec![1, 2]);
        assert_eq!(
            q.queue[0].qid.as_deref(),
            q.queue.first().and_then(|t| t.qid.as_deref())
        );
        assert_eq!(q.current.as_ref().map(|t| t.id), Some(1));
        assert_eq!(ids(&q.history), vec![0]);
        // unknown qid falls back to the first track with that id
        let again = q.adopt_advanced(2, "q-nope").expect("fallback");
        assert_eq!(again.id, 2);
        assert!(q.adopt_advanced(42, "q-nope").is_none());
    }

    #[test]
    fn extending_a_partial_source_in_order_rebuilds_after_the_current_track() {
        let mut q = Queue::new();
        let s = src("A", vec![]);
        // started on a partial list of 1..=3 from track 2
        let first = q.play_from_source(t(2), vec![t(1), t(2), t(3)], Some(s), false, &mut rng());
        q.begin(first, false);
        assert_eq!(ids(&q.queue), vec![3, 1]);
        q.add_to_queue(t(99), None);
        q.extend_context(vec![t(1), t(2), t(3), t(4), t(5)], &mut rng());
        assert_eq!(ids(&q.queue), vec![3, 4, 5, 1]);
        assert_eq!(ids(&q.manual), vec![99], "manual queue untouched");
        assert_eq!(q.playback_source.as_ref().map(|s| s.tracks.len()), Some(5));
    }

    #[test]
    fn extending_while_shuffled_only_appends_unseen_tracks() {
        let mut q = Queue::new();
        q.shuffle = true;
        let first = q.play_from_source(
            t(1),
            vec![t(1), t(2)],
            Some(src("A", vec![])),
            false,
            &mut rng(),
        );
        q.begin(first, false);
        q.extend_context(vec![t(1), t(2), t(3), t(4)], &mut rng());
        let mut got = ids(&q.queue);
        got.sort();
        assert_eq!(got, vec![2, 3, 4], "current track is not re-queued");
    }

    #[test]
    fn rejects_tracks_without_id() {
        assert!(Snapshot::from_json(r#"{"queue":[{"title":"x"}]}"#).is_err());
    }

    #[test]
    fn unavailability_rules() {
        let mut a = t(1);
        assert!(!a.is_unavailable(0));
        a.extra.insert("allowStreaming".into(), Value::Bool(false));
        assert!(a.is_unavailable(0));
        let mut b = t(2);
        b.extra.insert(
            "streamStartDate".into(),
            Value::from("2030-01-01T00:00:00.000+0000"),
        );
        assert!(b.is_unavailable(1_700_000_000));
        assert!(!b.is_unavailable(2_000_000_000));
        let mut v = t(3);
        v.extra.insert("itemType".into(), Value::from("video"));
        v.extra.insert("streamReady".into(), Value::Bool(false));
        assert!(!v.is_unavailable(0));
    }

    #[test]
    fn rfc3339_parsing() {
        assert_eq!(parse_rfc3339_secs("1970-01-02T00:00:00Z"), Some(86_400));
        assert_eq!(parse_rfc3339_secs("1970-01-01T01:00:00+01:00"), Some(0));
        assert_eq!(parse_rfc3339_secs("nope"), None);
    }
}
