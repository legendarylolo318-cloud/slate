//! The queue's track: a typed, compact record of the fields Slate reads.
//!
//! Until 1.6 a queue entry kept the whole TIDAL JSON of its track (about 9 KB each, so a
//! 9,408-track queue held ~85 MB). Only a few dozen fields are ever read; this keeps exactly
//! those, shares repeated strings and the artist and album records between tracks
//! ([`intern`]), and still reads and writes the JSON shape the original app and Slate 1.6 used:
//! old `queue` files and named queue snapshots load (unknown keys are ignored), and what is
//! written back is a subset of the old keys in the same camelCase shape.
//!
//! The runtime marks `_playingFrom` / `_contextFrom` (where a track was playing from, for
//! Previous) are never written; `_source` (the "Playing from" tag of a queued track) is.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex, Weak};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::queue::Source;

// ---------- sharing ----------

struct Pool<T> {
    map: HashMap<T, Weak<T>>,
    prune_at: usize,
}

impl<T: Hash + Eq + Clone> Pool<T> {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            prune_at: 1024,
        }
    }

    fn get(&mut self, value: T) -> Arc<T> {
        if let Some(a) = self.map.get(&value).and_then(Weak::upgrade) {
            return a;
        }
        let a = Arc::new(value.clone());
        self.map.insert(value, Arc::downgrade(&a));
        if self.map.len() > self.prune_at {
            self.map.retain(|_, w| w.strong_count() > 0);
            self.prune_at = (self.map.len() * 2).max(1024);
        }
        a
    }
}

struct StrPool {
    map: HashMap<Box<str>, Weak<str>>,
    prune_at: usize,
}

static STRINGS: Mutex<Option<StrPool>> = Mutex::new(None);
static ARTISTS: Mutex<Option<Pool<Artist>>> = Mutex::new(None);
static ALBUMS: Mutex<Option<Pool<Album>>> = Mutex::new(None);

/// One shared allocation per distinct short string (qualities, versions, tags, names).
pub fn intern(s: &str) -> Arc<str> {
    let mut g = STRINGS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let p = g.get_or_insert_with(|| StrPool {
        map: HashMap::new(),
        prune_at: 1024,
    });
    if let Some(a) = p.map.get(s).and_then(Weak::upgrade) {
        return a;
    }
    let a: Arc<str> = Arc::from(s);
    p.map.insert(s.into(), Arc::downgrade(&a));
    if p.map.len() > p.prune_at {
        p.map.retain(|_, w| w.strong_count() > 0);
        p.prune_at = (p.map.len() * 2).max(1024);
    }
    a
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Artist {
    pub id: Option<u64>,
    pub name: Arc<str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Album {
    pub id: Option<u64>,
    pub title: Arc<str>,
    pub cover: Option<Arc<str>>,
    pub release_date: Option<Arc<str>>,
}

fn shared_artist(a: Artist) -> Arc<Artist> {
    let mut g = ARTISTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    g.get_or_insert_with(Pool::new).get(a)
}

fn shared_album(a: Album) -> Arc<Album> {
    let mut g = ALBUMS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    g.get_or_insert_with(Pool::new).get(a)
}

// ---------- the track ----------

/// Where a track was playing from, kept on the track while it is current or history, so
/// Previous can restore "Playing from". Runtime only; never written to disk.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayingFrom {
    pub playback: Option<Source>,
    pub context: Option<Source>,
}

/// A queued item (audio track or video).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Track {
    pub id: u64,
    pub qid: Option<String>,
    pub title: Arc<str>,
    pub version: Option<Arc<str>>,
    /// The primary artist (`artist`).
    pub artist: Option<Arc<Artist>>,
    /// Every credited artist (`artists`).
    pub artists: Box<[Arc<Artist>]>,
    pub album: Option<Arc<Album>>,
    pub duration: f32,
    pub explicit: Option<bool>,
    pub ai: Option<bool>,
    pub isrc: Option<Arc<str>>,
    pub track_number: Option<u32>,
    pub volume_number: Option<u32>,
    pub audio_quality: Option<Arc<str>>,
    /// `mediaMetadata.tags`.
    pub media_tags: Box<[Arc<str>]>,
    pub audio_modes: Box<[Arc<str>]>,
    pub stream_ready: Option<bool>,
    pub allow_streaming: Option<bool>,
    pub stream_start_date: Option<Arc<str>>,
    pub item_type: Option<Arc<str>>,
    /// A video's thumbnail id.
    pub image_id: Option<Arc<str>>,
    /// `mixes.TRACK_MIX`, for autoplay radio.
    pub track_mix: Option<Arc<str>>,
    /// `_source`: the source a manually queued track came from.
    pub source: Option<Arc<Source>>,
    /// `_playingFrom` and `_contextFrom`.
    pub playing_from: Option<Arc<PlayingFrom>>,
}

fn str_of(m: &Map<String, Value>, k: &str) -> Option<Arc<str>> {
    m.get(k).and_then(Value::as_str).map(intern)
}

fn bool_of(m: &Map<String, Value>, k: &str) -> Option<bool> {
    m.get(k).and_then(Value::as_bool)
}

fn u32_of(m: &Map<String, Value>, k: &str) -> Option<u32> {
    m.get(k)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
}

fn artist_of(v: &Value) -> Option<Arc<Artist>> {
    let name = v.get("name").and_then(Value::as_str)?;
    Some(shared_artist(Artist {
        id: v.get("id").and_then(Value::as_u64),
        name: intern(name),
    }))
}

fn strings_of(v: Option<&Value>) -> Box<[Arc<str>]> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(intern).collect())
        .unwrap_or_default()
}

impl Track {
    pub fn new(id: u64) -> Self {
        Self {
            id,
            ..Self::default()
        }
    }

    /// Reads the JSON shape of a track; keys it does not use are ignored and mistyped values
    /// count as absent. `None` when there is no numeric `id`.
    pub fn from_map(m: &Map<String, Value>) -> Option<Self> {
        let id = m.get("id")?.as_u64()?;
        let qid = match m.get("_qid") {
            Some(Value::String(q)) => Some(q.clone()),
            Some(Value::Null) | None => None,
            Some(_) => return None,
        };
        let album = m.get("album").and_then(|a| {
            let title = a.get("title").and_then(Value::as_str);
            let id = a.get("id").and_then(Value::as_u64);
            let cover = a.get("cover").and_then(Value::as_str);
            let release = a.get("releaseDate").and_then(Value::as_str);
            if title.is_none() && id.is_none() && cover.is_none() && release.is_none() {
                return None;
            }
            Some(shared_album(Album {
                id,
                title: intern(title.unwrap_or("")),
                cover: cover.map(intern),
                release_date: release.map(intern),
            }))
        });
        Some(Self {
            id,
            qid,
            title: m
                .get("title")
                .and_then(Value::as_str)
                .map_or_else(|| Arc::from(""), Arc::from),
            version: str_of(m, "version"),
            artist: m.get("artist").and_then(artist_of),
            artists: m
                .get("artists")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(artist_of).collect())
                .unwrap_or_default(),
            album,
            duration: m.get("duration").and_then(Value::as_f64).unwrap_or(0.0) as f32,
            explicit: bool_of(m, "explicit"),
            ai: bool_of(m, "ai"),
            isrc: m.get("isrc").and_then(Value::as_str).map(Arc::from),
            track_number: u32_of(m, "trackNumber"),
            volume_number: u32_of(m, "volumeNumber"),
            audio_quality: str_of(m, "audioQuality"),
            media_tags: strings_of(m.get("mediaMetadata").and_then(|v| v.get("tags"))),
            audio_modes: strings_of(m.get("audioModes")),
            stream_ready: bool_of(m, "streamReady"),
            allow_streaming: bool_of(m, "allowStreaming"),
            stream_start_date: m
                .get("streamStartDate")
                .and_then(Value::as_str)
                .map(Arc::from),
            item_type: str_of(m, "itemType"),
            image_id: str_of(m, "imageId"),
            track_mix: m
                .get("mixes")
                .and_then(|x| x.get("TRACK_MIX"))
                .and_then(Value::as_str)
                .map(Arc::from),
            source: m
                .get("_source")
                .and_then(|v| serde_json::from_value::<Source>(v.clone()).ok())
                .map(Arc::new),
            playing_from: None,
        })
    }

    /// The JSON shape [`Track::from_map`] reads (what the queue files hold). The runtime
    /// marks are left out.
    pub fn to_map(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("id".into(), self.id.into());
        if let Some(q) = &self.qid {
            m.insert("_qid".into(), q.as_str().into());
        }
        m.insert("title".into(), (&*self.title).into());
        let put_s = |m: &mut Map<String, Value>, k: &str, v: &Option<Arc<str>>| {
            if let Some(v) = v {
                m.insert(k.into(), (&**v).into());
            }
        };
        put_s(&mut m, "version", &self.version);
        let artist = |a: &Artist| {
            let mut o = Map::new();
            if let Some(id) = a.id {
                o.insert("id".into(), id.into());
            }
            o.insert("name".into(), (&*a.name).into());
            Value::Object(o)
        };
        if let Some(a) = &self.artist {
            m.insert("artist".into(), artist(a));
        }
        if !self.artists.is_empty() {
            m.insert(
                "artists".into(),
                Value::Array(self.artists.iter().map(|a| artist(a)).collect()),
            );
        }
        if let Some(a) = &self.album {
            let mut o = Map::new();
            if let Some(id) = a.id {
                o.insert("id".into(), id.into());
            }
            o.insert("title".into(), (&*a.title).into());
            put_s(&mut o, "cover", &a.cover);
            put_s(&mut o, "releaseDate", &a.release_date);
            m.insert("album".into(), Value::Object(o));
        }
        m.insert("duration".into(), f64::from(self.duration).into());
        for (k, v) in [
            ("explicit", self.explicit),
            ("ai", self.ai),
            ("streamReady", self.stream_ready),
            ("allowStreaming", self.allow_streaming),
        ] {
            if let Some(v) = v {
                m.insert(k.into(), v.into());
            }
        }
        put_s(&mut m, "isrc", &self.isrc);
        for (k, v) in [
            ("trackNumber", self.track_number),
            ("volumeNumber", self.volume_number),
        ] {
            if let Some(v) = v {
                m.insert(k.into(), v.into());
            }
        }
        put_s(&mut m, "audioQuality", &self.audio_quality);
        if !self.media_tags.is_empty() {
            let tags: Vec<Value> = self.media_tags.iter().map(|t| (&**t).into()).collect();
            m.insert("mediaMetadata".into(), serde_json::json!({ "tags": tags }));
        }
        if !self.audio_modes.is_empty() {
            let modes: Vec<Value> = self.audio_modes.iter().map(|t| (&**t).into()).collect();
            m.insert("audioModes".into(), Value::Array(modes));
        }
        put_s(&mut m, "streamStartDate", &self.stream_start_date);
        put_s(&mut m, "itemType", &self.item_type);
        put_s(&mut m, "imageId", &self.image_id);
        if let Some(mix) = &self.track_mix {
            m.insert("mixes".into(), serde_json::json!({ "TRACK_MIX": &**mix }));
        }
        if let Some(s) = &self.source {
            if let Ok(v) = serde_json::to_value(&**s) {
                m.insert("_source".into(), v);
            }
        }
        m
    }

    /// Replaces every field but the id and queue id with the ones in `m` (the JSON shape).
    /// For building tracks in tests.
    #[doc(hidden)]
    pub fn fill_from(&mut self, mut m: Map<String, Value>) {
        m.insert("id".into(), self.id.into());
        if let Some(q) = &self.qid {
            m.insert("_qid".into(), q.as_str().into());
        }
        if let Some(t) = Self::from_map(&m) {
            *self = t;
        }
    }

    /// Builds a queue entry from any serializable item (a `TidalTrack`, a
    /// video). `None` when it does not serialize to an object with a numeric `id`.
    pub fn from_serialized<T: Serialize>(item: &T) -> Option<Self> {
        let Value::Object(m) = serde_json::to_value(item).ok()? else {
            return None;
        };
        Self::from_map(&m)
    }

    pub fn is_video(&self) -> bool {
        self.item_type.as_deref() == Some("video")
    }

    /// All credited artists, for scrobblers that take a combined string.
    pub fn artist_display(&self) -> String {
        if self.artists.is_empty() {
            self.artist_name().to_string()
        } else {
            self.artists
                .iter()
                .map(|a| &*a.name)
                .collect::<Vec<_>>()
                .join(", ")
        }
    }

    pub fn artist_name(&self) -> &str {
        self.artist
            .as_deref()
            .or_else(|| self.artists.first().map(|a| &**a))
            .map_or("", |a| &a.name)
    }

    /// TIDAL image id of the cover (album cover, or the video's own image).
    pub fn cover_id(&self) -> Option<&str> {
        self.album
            .as_ref()
            .and_then(|a| a.cover.as_deref())
            .or(self.image_id.as_deref())
    }

    pub fn duration_secs(&self) -> f32 {
        self.duration
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// `"Title (Version)"` as lists show it.
    pub fn version(&self) -> &str {
        self.version.as_deref().unwrap_or("")
    }

    pub fn album_title(&self) -> &str {
        self.album.as_ref().map_or("", |a| &*a.title)
    }

    pub fn album_id(&self) -> Option<u64> {
        self.album.as_ref().and_then(|a| a.id)
    }

    /// Metadata says the track cannot be streamed right now. Missing flags mean
    /// "available" so sparse responses do not grey everything out.
    pub fn is_unavailable(&self, now_epoch_secs: i64) -> bool {
        if self.is_video() {
            return false;
        }
        if self.stream_ready == Some(false) || self.allow_streaming == Some(false) {
            return true;
        }
        self.stream_start_date
            .as_deref()
            .and_then(crate::queue::parse_rfc3339_secs)
            .is_some_and(|ts| ts > now_epoch_secs)
    }
}

impl Serialize for Track {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.to_map().serialize(s)
    }
}

impl<'de> Deserialize<'de> for Track {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let m = Map::<String, Value>::deserialize(d)?;
        Self::from_map(&m).ok_or_else(|| serde::de::Error::custom("a track needs a numeric id"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::Snapshot;
    use serde_json::json;

    /// A queue file as 1.6 wrote it: whole TIDAL objects, nulls, `_playingFrom` on a
    /// history entry, a manual entry with a `_source` tag, and plenty of keys nothing reads.
    fn old_file() -> Value {
        let full = |id: u64, title: &str| {
            json!({
                "id": id, "_qid": format!("q{id}"), "title": title, "duration": 215,
                "version": null, "artist": { "id": 7, "name": "Toto", "picture": "x", "type": "MAIN" },
                "artists": [{ "id": 7, "name": "Toto", "type": "MAIN" }, { "id": 8, "name": "Guest" }],
                "album": { "id": 70, "title": "Toto IV", "cover": "aa-bb", "vibrantColor": "#fff",
                           "releaseDate": "1982-04-08" },
                "audioQuality": "HI_RES_LOSSLESS", "trackNumber": 2, "volumeNumber": 1,
                "isrc": "USSM18200001", "explicit": false, "ai": null, "popularity": 90,
                "replayGain": -5.1, "peak": 1.0, "copyright": "c", "url": "http://x",
                "streamReady": true, "allowStreaming": true, "premiumStreamingOnly": false,
                "streamStartDate": "1982-04-08T00:00:00.000+0000",
                "audioModes": ["STEREO"], "mediaMetadata": { "tags": ["LOSSLESS", "HIRES_LOSSLESS"] },
                "mixes": { "TRACK_MIX": "0012345" }, "itemType": "track", "someFutureKey": [1, 2, 3]
            })
        };
        let mut hist = full(2, "Rosanna");
        hist["_playingFrom"] = json!({ "type": "album", "id": 1, "name": "Toto IV",
                                       "tracks": [full(1, "Africa")] });
        hist["_contextFrom"] = Value::Null;
        let mut manual = full(3, "Africa");
        manual["_source"] =
            json!({ "type": "playlist", "id": "uuid-1", "name": "Mine", "tracks": [] });
        json!({
            "currentTrack": full(1, "Africa"),
            "queue": [full(4, "Waiting For Your Love")],
            "history": [hist],
            "originalQueue": null,
            "manualQueue": [manual],
            "playbackSource": { "type": "album", "id": 1, "name": "Toto IV", "mixType": null,
                                "tracks": [full(1, "Africa"), full(4, "Waiting For Your Love")] },
            "contextSource": null
        })
    }

    #[test]
    fn a_1_6_queue_file_still_loads_with_every_field_the_app_reads() {
        let snap = Snapshot::from_json(&old_file().to_string()).expect("old file loads");
        let cur = snap.current_track.as_ref().expect("current");
        assert_eq!(cur.id, 1);
        assert_eq!(cur.qid.as_deref(), Some("q1"));
        assert_eq!(cur.title(), "Africa");
        assert_eq!(cur.artist_name(), "Toto");
        assert_eq!(cur.artist_display(), "Toto, Guest");
        assert_eq!(cur.album_title(), "Toto IV");
        assert_eq!(cur.album_id(), Some(70));
        assert_eq!(cur.cover_id(), Some("aa-bb"));
        assert_eq!(cur.duration_secs(), 215.0);
        assert_eq!(cur.track_number, Some(2));
        assert_eq!(cur.isrc.as_deref(), Some("USSM18200001"));
        assert_eq!(cur.audio_quality.as_deref(), Some("HI_RES_LOSSLESS"));
        assert_eq!(
            &*cur.media_tags,
            &["LOSSLESS".into(), "HIRES_LOSSLESS".into()]
        );
        assert_eq!(cur.track_mix.as_deref(), Some("0012345"));
        assert_eq!(cur.explicit, Some(false));
        assert_eq!(cur.ai, None, "a null flag is absent");
        assert_eq!(
            cur.album.as_ref().and_then(|a| a.release_date.as_deref()),
            Some("1982-04-08")
        );
        assert!(!cur.is_unavailable(2_000_000_000));
        assert_eq!(snap.queue.len(), 1);
        assert_eq!(snap.history[0].id, 2);
        assert!(
            snap.history[0].playing_from.is_none(),
            "runtime marks are not read back"
        );
        let tag = snap.manual_queue[0].source.as_ref().expect("source tag");
        assert_eq!((tag.kind.as_str(), tag.name.as_str()), ("playlist", "Mine"));
        assert_eq!(
            snap.playback_source.as_ref().expect("source").tracks.len(),
            2
        );
    }

    #[test]
    fn what_is_written_loads_again_unchanged_and_is_a_subset_of_the_old_keys() {
        let snap = Snapshot::from_json(&old_file().to_string()).expect("old file loads");
        let json = snap.to_json().expect("json");
        let again = Snapshot::from_json(&json).expect("new file loads");
        assert_eq!(snap, again);
        let v: Value = serde_json::from_str(&json).expect("value");
        let t = &v["currentTrack"];
        for key in [
            "id",
            "_qid",
            "title",
            "artist",
            "artists",
            "album",
            "duration",
            "audioQuality",
        ] {
            assert!(t.get(key).is_some(), "{key} is written");
        }
        for key in [
            "popularity",
            "replayGain",
            "copyright",
            "url",
            "someFutureKey",
            "_playingFrom",
        ] {
            assert!(t.get(key).is_none(), "{key} is not kept");
        }
        assert!(v["manualQueue"][0]["_source"]["tracks"]
            .as_array()
            .is_some_and(Vec::is_empty));
        // Slate 1.6 reads the new shape too (it only needs `id` and the keys it looks up).
        assert_eq!(t["album"]["title"], "Toto IV");
        assert_eq!(t["artists"][1]["name"], "Guest");
    }

    #[test]
    fn mistyped_values_count_as_absent_and_a_missing_id_is_refused() {
        let t: Track = serde_json::from_value(json!({
            "id": 5, "title": 7, "duration": "long", "explicit": "yes", "album": 3, "artist": "x"
        }))
        .expect("loads");
        assert_eq!((t.title(), t.duration_secs(), t.explicit), ("", 0.0, None));
        assert!(t.album.is_none() && t.artist.is_none());
        assert!(serde_json::from_value::<Track>(json!({ "title": "x" })).is_err());
        assert!(serde_json::from_value::<Track>(json!({ "id": "5" })).is_err());
    }

    #[test]
    fn artists_and_albums_are_shared_between_tracks() {
        let mk = |id: u64| {
            Track::from_serialized(&json!({
                "id": id, "title": format!("T{id}"),
                "artist": { "id": 1, "name": "A" }, "artists": [{ "id": 1, "name": "A" }],
                "album": { "id": 2, "title": "Al", "cover": "c" }, "audioQuality": "LOSSLESS",
            }))
            .expect("track")
        };
        let (a, b) = (mk(1), mk(2));
        assert!(Arc::ptr_eq(
            a.artist.as_ref().unwrap(),
            b.artist.as_ref().unwrap()
        ));
        assert!(Arc::ptr_eq(&a.artists[0], b.artist.as_ref().unwrap()));
        assert!(Arc::ptr_eq(
            a.album.as_ref().unwrap(),
            b.album.as_ref().unwrap()
        ));
        assert!(Arc::ptr_eq(
            a.audio_quality.as_ref().unwrap(),
            b.audio_quality.as_ref().unwrap()
        ));
    }

    #[test]
    fn the_party_wire_carries_ids_not_tracks() {
        // Guests and hosts exchange ids (and a few display strings in `RequestTrack`); the queue's
        // track shape never crosses the wire, so the compact track cannot change what a 1.6 peer
        // reads. This pins that: the party code must not depend on the queue's track.
        for file in ["proto.rs", "requests.rs", "net.rs"] {
            let src = std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("src/party")
                    .join(file),
            )
            .expect("source");
            let code = src.split("#[cfg(test)]").next().unwrap_or("");
            assert!(
                !code.contains("queue::Track") && !code.contains("track::Track"),
                "{file} must not use the queue's Track"
            );
        }
        let m = crate::party::proto::HostMsg::Track {
            id: 9,
            position_ms: 1,
            playing: true,
            at_ms: 2,
        };
        assert_eq!(
            serde_json::to_value(&m).expect("json"),
            serde_json::to_value(
                json!({ "t": "track", "id": 9, "position_ms": 1, "playing": true, "at_ms": 2 })
            )
            .expect("json"),
        );
    }
}
