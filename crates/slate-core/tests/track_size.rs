//! Memory per queued track, before and after the compact `Track` (1.7).
//!
//! "Before" is what a queue entry held until 1.6: the track's serialized JSON as a
//! `serde_json::Map` (`Track::extra`). "After" is the typed `Track`. Both are built from the
//! same 10,000 `TidalTrack`s (500 artists, 800 albums, about the spread of a large library) and
//! measured with a counting allocator, so the numbers are bytes actually kept on the heap.
//! Run with `cargo test -p slate-core --test track_size -- --nocapture` to see them.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::{json, Map, Value};
use slate_core::queue::{Queue, Source, Track};
use slate_core::tidal_api::TidalTrack;

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        LIVE.fetch_add(l.size(), Ordering::Relaxed);
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        System.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        if n >= l.size() {
            LIVE.fetch_add(n - l.size(), Ordering::Relaxed);
        } else {
            LIVE.fetch_sub(l.size() - n, Ordering::Relaxed);
        }
        System.realloc(p, l, n)
    }
}

#[global_allocator]
static A: Counting = Counting;

const N: u64 = 10_000;

fn tidal(i: u64) -> TidalTrack {
    let artist = |k: u64| {
        json!({ "id": 1000 + k, "name": format!("Artist number {k}"),
                "picture": format!("aaaaaaaa-bbbb-cccc-dddd-{k:012}"), "type": "MAIN" })
    };
    serde_json::from_value(json!({
        "id": 100_000 + i,
        "title": format!("A reasonably long track title number {i}"),
        "version": if i.is_multiple_of(5) { json!("Remastered 2011") } else { Value::Null },
        "duration": 180 + (i % 200),
        "artist": artist(i % 500),
        "artists": [artist(i % 500), artist((i * 7 + 1) % 500)],
        "album": { "id": 5000 + i % 800, "title": format!("An album called number {}", i % 800),
                   "cover": format!("11111111-2222-3333-4444-{:012}", i % 800),
                   "releaseDate": "2011-05-17" },
        "audioQuality": "HI_RES_LOSSLESS",
        "trackNumber": 1 + i % 14, "volumeNumber": 1,
        "isrc": format!("USABC{:07}", i),
        "explicit": i.is_multiple_of(3), "popularity": 40, "replayGain": -7.5, "peak": 0.98,
        "copyright": "(C) 2011 Some Record Company Ltd.",
        "url": format!("http://www.tidal.com/track/{}", 100_000 + i),
        "streamReady": true, "allowStreaming": true, "premiumStreamingOnly": false,
        "streamStartDate": "2011-05-17T00:00:00.000+0000",
        "audioModes": ["STEREO"],
        "mediaMetadata": { "tags": ["LOSSLESS", "HIRES_LOSSLESS"] },
        "mixes": { "TRACK_MIX": format!("00{i:030}") },
    }))
    .expect("tidal track")
}

fn live() -> usize {
    LIVE.load(Ordering::Relaxed)
}

#[test]
fn compact_tracks_are_far_smaller_than_the_json_they_replace() {
    let source: Vec<TidalTrack> = (0..N).map(tidal).collect();

    // Before: the serialized JSON object each entry kept (without id and _qid).
    let base = live();
    let old: Vec<Map<String, Value>> = source
        .iter()
        .map(|t| {
            let Value::Object(mut m) = serde_json::to_value(t).expect("value") else {
                unreachable!()
            };
            m.remove("id");
            m
        })
        .collect();
    let old_bytes = live() - base;
    drop(old);

    // After: typed tracks (artists and albums shared).
    let base = live();
    let new: Vec<Track> = source
        .iter()
        .map(|t| Track::from_serialized(t).expect("track"))
        .collect();
    let new_bytes = live() - base;
    let per_track = new_bytes / N as usize;

    println!(
        "10,000 tracks as the old JSON map: {:.1} MB ({} B each)",
        old_bytes as f64 / 1e6,
        old_bytes / N as usize
    );
    println!(
        "10,000 tracks as the compact Track: {:.1} MB ({} B each, struct itself {} B)",
        new_bytes as f64 / 1e6,
        per_track,
        std::mem::size_of::<Track>()
    );
    assert!(
        new_bytes * 3 < old_bytes,
        "compact tracks should be at least 3x smaller: {new_bytes} vs {old_bytes}"
    );

    // A whole queue: context queue + source list + the shuffle original, and a snapshot of it.
    let base = live();
    let mut q = Queue::new();
    q.shuffle = true;
    let src = Source {
        kind: "playlist".into(),
        id: json!("x"),
        name: "Big".into(),
        image: None,
        subtitle: None,
        mix_type: None,
        tracks: slate_core::queue::source_tracks(&new),
    };
    q.set_tracks(new, Some(src), false);
    q.toggle_shuffle(&mut rand::rng());
    let queue_bytes = live() - base;
    let base = live();
    let snap = q.snapshot();
    let snapshot_bytes = live() - base;
    println!(
        "queue with 10,000 tracks (queue + shuffle original + source list): {:.1} MB; a snapshot of it adds {:.2} MB",
        queue_bytes as f64 / 1e6,
        snapshot_bytes as f64 / 1e6
    );
    assert!(
        snapshot_bytes < queue_bytes / 20,
        "a snapshot must share the tracks, not copy them ({snapshot_bytes} B)"
    );
    let json = snap.to_json().expect("json");
    println!(
        "that snapshot as the queue file: {:.1} MB of JSON for 10,000 tracks (a 1.6 file held the full objects)",
        json.len() as f64 / 1e6
    );
    drop(snap);
    let _ = Arc::new(q);
}
