# Slate 1.7.0 handoff (performance: RAM, wakeups, VRAM)

Goal: lower RAM, CPU and VRAM with no change to audio output and no visible behaviour change; no new
settings or features. Nothing is pushed, tagged or version-bumped (the version is still 1.6.1; the release
commit is yours). One commit per item, each with `cargo fmt`, `cargo clippy --workspace --all-targets -- -D
warnings` and `cargo test --workspace` green. Commits use the repository's "Slate dev" author.

| # | Commit | Item | Status |
|---|---|---|---|
| 1 | `dd98274` | Compact queue tracks | done |
| 2 | `fe23c29` | 3 s decoded reservoir in the gapless branches | done |
| 3 | none | Burst download for non-DASH streams | **not done, on purpose** (see below) |
| 4 | `140dccb` | Covers as RGB8, sizes cut from one decode | done |
| 5 | `0e6e532` | Lyrics backdrop on its own cached layer | done, effect on a GPU not measured |
| 6 | `ca2e678` | Tokio blocking pool limits | done |
| 7 | `8def0e3` | Debounced, off-thread queue persistence | done |

## 1. Compact queue tracks (`dd98274`)

- New `slate-core/src/track.rs`: `Track` is a typed record (id, qid, title, version, artist, artists, album
  {id, title, cover, releaseDate}, duration, explicit, ai, isrc, trackNumber, volumeNumber, audioQuality,
  media tags, audio modes, streamReady, allowStreaming, streamStartDate, itemType, imageId, `mixes.TRACK_MIX`,
  `_source`). These are exactly the keys read anywhere in both crates (grepped every `.extra` use: queue,
  controller, shuffle, export, quality, panels, library, filter, tags, share card, Discord, MPD,
  party UI, remote, DnD, backend). `popularity`, `replayGain`, `peak`, `copyright`, `url`, `premiumStreamingOnly`
  and `dateAdded` are not read by anything and are not kept.
- `Arc<str>` for repeated strings (`intern`), and artists/albums are shared `Arc<Artist>` / `Arc<Album>`
  records, interned through weak pools (pruned when they grow), so 10,000 tracks of 500 artists / 800 albums
  hold 500 + 800 records, not 10,000.
- `queue`, `manual`, `history`, `original`, `current`, `Source::tracks` (`Arc<[Arc<Track>]>`), `Snapshot`, the
  undo states and `Next`/`Previous`/`Outcome` all hold `Arc<Track>` (`queue::TrackRef`). Moving, reordering
  and shuffling copy pointers; `snapshot()` copies pointers (only tracks that carry a runtime mark are copied).
- `_playingFrom` / `_contextFrom` used to be a full JSON copy of the playing source *including its whole track
  list* inside the current track and every history entry (the real source of the ~85 MB). They are now
  `Track::playing_from: Option<Arc<PlayingFrom>>`, runtime only (never serialized, as before: `snapshot`
  stripped them), sharing the source's `Arc<[Arc<Track>]>`.
- Serde: `Track` serializes the camelCase shape (a subset of the old keys, nulls and empty arrays omitted) and
  deserializes from any object with a numeric `id`, ignoring unknown keys and treating mistyped values as
  absent (same leniency as the old `.get().and_then(as_str)` reads). `serde` got the `rc` feature (no new crates).
- Tests: `track::tests` (a 1.6-style queue file with full TIDAL objects, `_playingFrom`, `_source`, unknown keys
  loads with every field; written file reloads equal and keeps only known keys; mistyped values; sharing;
  party wire), `tests/track_size.rs` (allocator-counted).

Numbers (10,000 `TidalTrack`s, 500 artists / 800 albums, counting allocator, debug build, bytes kept on the heap):

| | before (1.6 `extra` map) | after |
|---|---|---|
| 10,000 tracks | 75.3 MB (7,534 B each) | 5.7 MB (569 B each; the struct is 264 B) |
| a queue of 10,000 (queue + shuffle original + source list) | 3 x 75 MB deep copies, ~225 MB (arithmetic, not measured) | 6.5 MB |
| `snapshot()` of that queue | another deep copy | +0.16 MB |
| queue file for that queue | (not measured; full objects) | 14.1 MB JSON |

The real 1.6 figure of ~85 MB for 9,408 tracks was larger per track than my synthetic track (7.5 KB) because
of the `_playingFrom` copies; the 1.7 number does not depend on that.

**Party compatibility:** the party wire never carried queue tracks: `HostMsg::Track` has ids and times,
`HostMsg::Queue` ids and names, `RequestTrack` its own few strings. A test pins that `party/{proto,requests,net}.rs`
do not use the queue's `Track` and pins the `track` message JSON. So 1.6 peers interoperate unchanged.
Not run against a real 1.6 peer (no second machine).

**Old files:** `queue.json` (encrypted) and named queue snapshots written by 1.6 load (tested with a synthetic
1.6-shaped file, not with a real one from a profile). A file written by 1.7 is **not** loadable with full
fidelity by 1.6 in every corner (it lacks keys 1.6 never read, so it works, but downgrade was not tried).

Risks: any code path that serialized a `Track` and read a key not in the list above would silently lose it; I
found none (grepped `to_value`/`to_string` over tracks, `serde_json::from_value::<TidalTrack>`), and the
overlay/MCP code uses its own state structs. `fill_from` is `#[doc(hidden)]` and meant for tests only.

## 2. Decoded reservoir (`fe23c29`)

- `audio.rs`: the `queue` after each gapless branch's decoder (main branch and prerolled next track) holds
  `DECODED_RESERVOIR_NS` = 3 s instead of 15 s. The 12 s it gave up are added to the branch `uridecodebin`'s
  `buffer-duration` (`BRANCH_LOOKAHEAD_EXTRA_NS`), and its byte cap is 20 MiB (`BRANCH_BUFFER_SIZE_BYTES`) so
  hi-res FLAC can hold the longer window compressed. Total lookahead is identical: 20 s progressive, 30 s DASH
  and next-track branches (unit test). Worst case held per stream: 20 MiB + 4.6 MB of 24/192 PCM, against 6 MiB
  + 23 MB before (compile-time assert).
- The exclusive / bit-perfect direct path (`make_stream_decodebin(.., Lookahead::Direct)`, the appsink to ALSA
  writer) keeps its old 5/15 s and 6 MiB; sinks, caps, audioconvert/dithering, ALSA, WASAPI, EQ, crossfade and
  ReplayGain are untouched.
- Measured with `gst-launch` as a stand-in (24/192 FLAC served by a local HTTP server, same `uridecodebin`
  properties, sync fakesink, RSS after 14 s): anonymous 36.5 MB before, 22.1 MB after. This is not the app.

**Not verified:** playback through the real app (needs a signed-in account and a sound device): gapless
handover, seek, crossfade and a DASH hi-res track were *not* run end to end. `slate --self-test` (offline)
passes, and the unit tests that build the concat/branch pipelines pass. The reasoning for gapless: the next
branch's queue is full (3 s) when `concat` switches, and the rest of the lookahead sits compressed in its
multiqueue. If a hi-res track ever underruns right after a seek, raise `DECODED_RESERVOIR_NS` first (one line).
Buffering messages: `use-buffering` stays on, so a longer compressed window also means the buffering
low-watermark logic sees a larger queue; unobserved on a slow connection.

## 3. Burst download (not done)

Stopped on this item and made no code change. Reasons:

1. **It would not help the measured case.** The 1.6 review's ~430 wakeups/s were measured on a 24/192 track,
   which is DASH; the spec leaves DASH as it is. The burst would only touch progressive (lossy AAC and plain
   FLAC) streams.
2. **The wakeups are not the source's.** Stand-in test (local server, 120 s of 44.1 kHz stereo, the branch
   pipeline of item 2, voluntary context switches over all threads, 20 s window): FLAC over HTTP 87/s, the same
   file through `filesrc` 129/s; AAC 129/s over HTTP and 129/s from file. Reading from a file is not quieter
   than `souphttpsrc` once the compressed buffer is full (and the 1.6 review already found a bigger
   `blocksize` changed nothing).
3. **Routing is not reproducible faithfully.** `souphttpsrc`/`curlhttpsrc` take their proxy from
   `AudioProxy::route_for(Capability::Lossy)` per pipeline generation, with fail-closed refusals at every
   build, and in the `NoProxy` case libsoup uses GIO's proxy resolver (desktop proxy settings), which the
   reqwest client (environment capture only) does not see. A download in the command layer would have to
   re-implement that or egress differently from the current source. That is a security-relevant behaviour
   change for a win I could not show.
4. It needs a temp-file lifetime tied to `NextBinState`, the current branch, skip, quit and the route
   generation checks in a 6,000-line file I could only test offline.

If you still want it: only for `route_for(Lossy) == Ok(NoProxy)` with an empty captured system proxy and a
`gio::ProxyResolver::default().lookup()` of `direct://`, only for the gapless-prerolled next track (start latency
unchanged), file owned by `NextBinState`, Linux only.

## 4. Covers (`140dccb`)

- `images::Pixels` = `Rgb(SharedPixelBuffer<Rgb8Pixel>) | Rgba(..)`; `decode_pixels` keeps opaque images
  (all JPEGs, PNG without alpha, grey) as RGB, images with alpha as RGBA. `imgcache::Pixels` re-exports it.
  `byte_size()` counts 3 or 4 bytes a pixel; the 64 MB budget now fits a third more RGB covers (tested).
  Consumers that need RGBA (colour pick, blur backdrop, share card) use `to_rgba()` (a transient copy).
  `Image::from_rgba8(px)` became `px.into_image()` at the call sites. The GPU texture is RGBA either way, so
  **VRAM is unchanged by this item** (RAM only).
- The cache keys `(url, size rank)` and remembers the ranks it holds per URL. A request for a smaller size is cut
  from a bigger one that is cached or in flight (the in-flight fetch is awaited and shared), not re-read from the
  disk cache and decoded again. A bigger request than anything held still decodes. The original-size request
  (`max_edge` 0) ranks above all numbers.
- Differences: a size cut from an already shrunk image (say 160 from a 320 decode of a 640 source) is resampled
  twice with the `image` crate's thumbnail filter; for a full-size master it is identical to decoding small
  (tested bit-for-bit). Not measured: the share of requests that hit the new path in real use, or its effect on
  RAM in a long session (no profile was run).

## 5. Lyrics backdrop layer (`0e6e532`)

`ui/lyrics_full.slint`: the backdrop image and its 0xa0 dimming sit in one `Rectangle` with
`cache-rendering-hint: true`. Offscreen screenshots (`SLATE_SHOTS=lyrics-full`, all five sizes at 1x/1.5x/2x)
are pixel-identical before and after. The software renderer ignores the hint, so **the caching itself was not
exercised**; on Skia/FemtoVG the layer is a window-sized texture, i.e. it **costs** W x H x 4 bytes of VRAM
(7.5 MB at 1880x1000@1x, 30 MB at 2x) only while the lyrics view is open, in exchange for not scaling the
128 px image up under every changed line. A line changes every few seconds (plus the 280 ms slide), so the CPU
saving is small. This trades against the VRAM goal; if a measurement on your GPU shows no CPU gain, drop the
hint (one line). I could not measure per-process VRAM here.

## 6. Tokio blocking pool (`ca2e678`)

`max_blocking_threads(16)`, `thread_keep_alive(2 s)` (defaults: 512, 10 s). Every `spawn_blocking` in both crates
is a short job (cover decode, cache and file writes, share card, pipeline build/teardown, `drop(session)`); none
`block_on`s or waits for another blocking job; MPD, party and the scrobblers run on async tasks, not the blocking
pool. Peak concurrent demand is about 8 decodes plus a few writes/teardowns. A job past the cap just waits.
Not measured: thread counts while playing (the 1.5/1.6 notes show 40-48; expect a few fewer idle threads).

## 7. Queue persistence (`8def0e3`)

- `Backend::save()` (called from ~25 places after every change) now only schedules a write: 1 s after the last
  request (`debounce::Debounce`, tested), and `Backend::save_now()` writes immediately; the shutdown path calls
  `save_now()` inside `block_on`, so quitting writes what is pending. Any exit that does not go through
  `slint::quit_event_loop` (SIGKILL, a crash, power loss) can lose up to 1 s of queue changes (before: none).
- `Controller::save()` takes the snapshot (pointer copies) under the queue lock, then JSON + AES-GCM + temp file
  and rename (`write_atomic`) run in `spawn_blocking`; a gate (`save_gate`) held from snapshot to disk keeps two
  saves ordered. Modes (`modes.json`) take the same gate.
- The file holds only the compact fields (item 1). Format is the same `Snapshot` JSON; no migration step.

## Not verified / risks

- No end-to-end playback (no signed-in session driven, no sound device): items 2 and 7 touch the audio path's
  buffering and the persistence of the playing queue respectively. Please play a hi-res (DASH) album gaplessly,
  seek in it, enable crossfade, restart and check the queue comes back.
- No release build or A/B of RAM/CPU/VRAM was made (a release build takes ~8 minutes and the machine had no
  signed-in profile copy prepared); the numbers above are allocator counts and a `gst-launch` stand-in.
  Expected in the app: the queue savings scale with queue length (a 9k-track queue should drop by tens of MB),
  playing RSS drops with the reservoir (up to ~20 MB per hi-res stream, ~40 MB while the next one prerolls),
  idle RSS barely moves.
- Windows and macOS were not built.
- `cargo check --no-default-features` was already broken in 1.5.0 (`party_off.rs`); not touched.
- `cargo update` was not run; `blocking` is still pinned to 1.6.2 (see HANDOFF.md).
