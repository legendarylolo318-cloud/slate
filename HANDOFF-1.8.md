# Handoff: 1.8 work on top of v1.7.0

Nothing was pushed, tagged or version-bumped. 9 commits on `main` after `53d00d8`, author "Slate dev", one per
item. Every commit passed `cargo fmt --all --check`, both `clippy -D warnings` runs (default and
`--no-default-features`) and `cargo test --workspace` (1,081 pass, 0 fail at the last commit).

```
237e8e9 perf: write tracks straight to the serializer (D, part 1)
0ca28e9 perf: release profile lto = "fat" (K)
6dd3419 perf: cap glibc malloc arenas at 2 (E)
90238eb feat: share card themes (#20)
2f82fb2 feat: party request limits and host-first ordering (#18)
034ef26 feat: compact now-playing strip (#17)
0726133 feat: export / import personal data (#16)
9e963b3 feat: fill the queue to N minutes (#14)
ace76e8 feat: resume points for long tracks (#3)
```

**What is NOT done** (so nobody assumes otherwise): performance items **A, B, C, F, H, I, J, L** were not
attempted, **D** is half done, **G** needed no change. Reasons are under "Performance" below. The features were
checked by unit tests and offscreen screenshots only; none was clicked through in a signed-in running app (see
"Could not test").

## Features

Defaults: all off or dormant. No thread, timer, socket or file read at start-up or idle was added; an idle build
measures the same as 1.7.0 (below).

### #3 Resume points (`ace76e8`)
- `slate-core/src/resume.rs` (pure rules + format), `slate-app/src/resume.rs` (glue), a pill above the player bar
  (`player_bar.slint`).
- Tracks of ≥ 20 min: when you leave one (next/skip/another track, Stop via MPRIS/MPD/MCP, quit) at > 1 min and
  > 1 min before the end, the position is stored; leaving within the last minute drops the point; leaving within the
  first minute keeps an older point. Starting that track again shows "Resume at mm:ss" (one click seeks). The offer
  goes away when the point is passed, on any seek, or after 20 s of playing. Nothing seeks by itself.
- 500 entries, least recently left dropped first. Stored through `AppState::write_state_file("resume.json")`, i.e.
  the **encrypted** state directory like `queue.json`. Read the first time a ≥ 20 min track starts or is left,
  written 0.7 s after the last change on a blocking thread, flushed at quit. Under 20 min nothing is touched.
- Sign-out: `resume::forget()` bumps an epoch (late work is ignored), cancels the debounce and clears memory; the
  core's `logout` now also deletes `resume.json`.
- Tests: 7 in core (rules, LRU, bounds, garbage). Screenshot: `SLATE_SHOTS=resume`.

### #14 Fill queue to N minutes (`9e963b3`)
- Queue "⋯" menu → "Fill queue to…" 15 / 30 / 45 / 60 / 90 / Custom… (dialog, 1–600). `queue_fill::select` is the
  pure rule: stops at the first track that would overshoot by more than half its length, and as soon as the target
  is reached. `Controller::fill_queue`: candidates are the context source's tracks not already queued, played or
  current (blocked/unavailable/video skipped), else the radio of the playing track (network call made without the
  queue lock). The target counts what is already upcoming; if it is already long enough nothing is added and the
  notice says so. One `edit` = one undo step.
- Tests: 4 for the rule. Screenshot: `SLATE_SHOTS=dialog-fill`.

### #16 Export / import personal data (`0726133`)
- Settings → General → Personal data. One JSON file `{schema: "slate-personal-data", version: 1, lyrics_offsets,
  tags}`. Import **merges**: a file's offset replaces the local one for that track (the file wins), tags are added
  to a track's tags, nothing is removed. Text goes through `clean_line` (controls and bidi overrides), tag
  normalisation, cover ids must be `[A-Za-z0-9-]{1,64}`, offsets are clamped, files over 16 MB and newer versions
  are refused with a sentence. Writes lyrics offsets under the existing offsets lock (`update_saved`).
- Tests: 4 in the app + 1 in core. Screenshot: `SLATE_SHOTS=settings` (General tab, tall window via the new
  `SLATE_SHOTS_SIZE=1000x1700`).
- Note: lyrics offsets and tags are not wiped at sign-out in 1.7.0 either; unchanged.

### #17 Compact now-playing strip (`034ef26`)
- Right-click the mini-player button → "Compact strip (open / close)". `strip.slint` (frameless 380×46: title – artist,
  3 px progress line, play/pause, next, close; drag by the body via winit `drag_window`). It is fed by the same
  `Backend::ui` hook as the mini player, so there is no timer or thread; closing drops the window.
- `always-on-top` on X11 and Windows. On Wayland (detected from the raw window handle) it is a normal window and a
  notice says so ("On Wayland the strip is a normal window…").
- Test: 1 (platform rule). Screenshots: `SLATE_SHOTS=strip` (scales 1, 1.5, 2).

### #18 Party request limits (`2f82fb2`)
- Settings → Party (host): "Requests per guest" Off / 1 / 2 / 3 / 5 / 10 (whole party, per guest *name*, a request
  the host turns down is given back; max 200 names counted) and "My picks play first" (guest requests are placed
  behind the host's own queued entries and the host's entries are kept ahead of them: `publish_queue` calls
  `party_order_queue`, one atomic read when off).
- Enforced on the host in `RequestBook::submit`; the refusal is the existing `RequestResult { ok: false, reason }`
  ("You have used your 2 requests for this party."), so 1.7.0 guests show it. **No wire change** (no new fields or
  messages), so 1.7.0 peers work both ways by construction.
- Tests: 4 new in `requests.rs` (+ the policy mapping test). Screenshot: `SLATE_SHOTS=settings` (Party tab).
- "Host's own picks" means the host's entries in "next up" (the manual queue); the context queue always plays after it.

### #20 Share card themes (`90238eb`)
- Right-click → "Share card theme" → Dark (unchanged), Light, Album colour (`cover_tint::dominant`, darkened; Dark
  when the cover is grey or absent), Minimal (flat page, no album line). Same 600×300 PNG, same pipeline and fonts.
  Last choice in `ui.json` (`share_theme`, absent = dark); "Copy share card" uses it.
- Tests: 5 (all palettes ≥ WCAG AA against both gradient ends over eight cover colours; keys; fallback; Minimal;
  all four render, same size, differ). Samples: `SLATE_CARD_DIR=… cargo test -p slate --release write_theme_samples -- --ignored`.

## Performance

Method as in 1.6/1.7: release builds, profile copy reset with `profreset`, 12 s warm-up, 60 s window, TOTO "Africa"
(539204) at 24/192 into a null PipeWire sink (created for the runs and removed afterwards; `streams_on_null=1` printed
for every run, so the audio really went there), headless KWin + Xwayland, interleaved runs. Scripts are copies of
`~/slate-bench` pointed at the new Xwayland display (`~/slate-bench/env`, with `ab.sh` for interleaving and
`threads.sh` for the per-thread breakdown). Machine: 24 cores, amdgpu, PipeWire. "head" = `53d00d8` + the six features
(`90238eb`); every comparison is against the build immediately before it.

**Baseline, idle** (head, release): 21 threads, 0 wakeups/s, 0 ticks, anonymous / total PSS 64.2 / 129.9 MB, VRAM
103.9 MB. These equal the 1.7.0 table (63.1 / 134.3 MB, 21, 0): the features cost nothing idle.

**Per-thread wakeups, playing, 20 s** (voluntary context switches/s; before = head, after = final tree `237e8e9`
equivalent build with E and K; no item that targets wakeups was done, so they are expected to match):

| | total | breakdown |
|---|---|---|
| before | 312 | threaded-ml 61, souphttpsrc 54, multiqueue0 41, queue1 34, multiqueue1 24, queue0 24, souphttpsrc0 23, tokio 18, UI 13, swapchain 16, mpris 2 |
| after | 327 | souphttpsrc 63, threaded-ml 57, queue1 45, multiqueue0 40, multiqueue1 26, queue0 24, souphttpsrc0 20, tokio 17, UI 14, swapchain 16, mpris 2 |

GStreamer queues and multiqueues are 120–135 of ~320 wakeups/s (item B's target); PipeWire's `threaded-ml` ~60 (item
A's). One run each; wakeups/s per run vary 280–339 in the tables below.

### E. `mallopt(M_ARENA_MAX, 2)` (Linux/glibc): KEPT (`6dd3419`)
Playing, anonymous PSS, interleaved:

| | runs | mean |
|---|---|---|
| head | 113.0, 114.6, 109.8, 113.4 | 112.7 |
| `MALLOC_ARENA_MAX=2` (env, same binary) | 104.3, 106.3, 103.1, 107.1 | 105.2 |
| `MALLOC_ARENA_MAX=4` (env) | 111.6, 110.6, 103.7, 112.7 | 109.7 (inconsistent; not chosen) |
| head (separate pass) | 117.3, 117.1, 111.7 | 115.4 |
| built-in `mallopt(2)` binary | 104.4, 106.6, 108.1 | 106.4 |

Ranges do not overlap: −7 to −9 MB anonymous (−7 to −8 MB total). CPU ticks head 85/90/79/77, arena 2: 84/85/83/86
(ticks vary ±35 between runs per the 1.7 notes); wakeups 286–300 in both. Idle was not re-measured with it (no idle
allocation change expected). `MALLOC_ARENA_MAX` set in the environment still overrides. Not measured: lock contention
under load beyond the CPU ticks above.

### K. `lto = "fat"` (no `panic = "abort"`): KEPT, with a cost (`0ca28e9`)
Compared with the build before it (thin LTO + E), four interleaved play runs, three idle, two × 7 launches:

| | thin | fat |
|---|---|---|
| binary | 78,823,496 B | 73,641,240 B (−6.6 %) |
| play anonymous / total PSS | 106.5, 107.0, 118.0, 106.3 / 177.3, 178.3, 189.2, 177.7 MB | 105.2, 105.8, 106.0, 105.2 / 175.1, 176.3, 175.5, 175.5 MB |
| play CPU ticks | 91, 89, 95, 93 | 91, 87, 90, 94 |
| play wakeups/s | 303, 280, 324, 313 | 303, 306, 324, 339 |
| idle total PSS | 120.7, 120.4, 121.1 MB | 119.9, 118.4, 119.0 MB |
| idle wakeups, threads | 0, 21 | 0, 21 |
| window mapped (median, range, 7 launches ×2) | 323 (315–325), 321 (319–324) ms | 319 (316–328), 320 (311–323) ms |
| **release build time** | **5 min 28 s** | **10 min 59 s** |

The win is small (about 2 MB total PSS, 5 MB of binary; one thin run at 118 MB anonymous is an outlier); CPU,
wakeups and start-up are inside noise. **The price is a doubled release build.** This was only built on Linux; the
Windows release job (Skia + D3D) was not tried with fat LTO and may take much longer or need more memory. If CI time
matters more than 5 MB, revert `0ca28e9` alone.

### D. Queue file: half done (`237e8e9`)
- Done: `Track`'s `Serialize` writes entries straight to the serializer instead of building a `serde_json::Map` per
  track (test: equal to `to_map` on the 1.6-style fixture and on a bare track; round-trips through text). 9,000
  tracks, 6,032,079 bytes (unchanged): **13.7, 13.8, 13.65 ms → 3.08, 3.01, 3.04 ms** (best of 5 per run,
  `cargo test -p slate-core --release time_a_big_snapshot -- --ignored --nocapture`). It runs on the debounced
  worker, so this is CPU on a worker thread, not a UI gain.
- **Not done:** storing the playing source's tracks as references to queue entries (the file is ~half duplicate:
  the 9,000-track snapshot has two copies). It changes the file schema and the loader and needs the 1.6/1.7 compat
  tests and a re-run of `bigq.sh`; not started. `bigq.sh` memory (after edits and undos) was not re-run for part 1
  (it needs a full release build and part 1 changes only what the file writer allocates transiently).

### G. No iroh/party objects before a party starts: nothing to change
Idle release build: 21 threads, none party/iroh (tokio workers are shared), **0 UDP sockets** (iroh is UDP), 25
socket fds (Wayland/X11/D-Bus/single-instance/HTTP), no `iroh` reference in start-up code. Already dormant; recorded,
no commit.

### Not attempted (no numbers; do not read absence as "no win")
Each was larger than a cheap A/B, and a wrong change touches audio or rendering:
- **A** sink buffer / PipeWire quantum: changes latency; the pause/volume/seek < 100 ms check needs a harness that
  does not exist in `~/slate-bench`.
- **B** removing queues/multiqueues: the per-thread breakdown above makes it the biggest wakeup target (about 40 %), but
  the branch queues are what let the next track pre-buffer behind `concat`; it needs the md5 gapless check at 24/192
  and 16/44 and a lookahead redesign. Not started.
- **C** Skia partial rendering, **H** Skia GPU cache/glyph-atlas caps, **I** freeing textures while minimized,
  **J** two swapchain images: these need Slint/Skia internals (partial rendering and swapchain image count are not
  exposed by Slint 1.18's public API as far as I looked; I did not dig into i-slint-renderer-skia) and VRAM/scrolling
  measurements with a 9,000-row list.
- **F** lazy CJK/fallback UI fonts: fonts are loaded by Slint/fontique, not by Slate; no hook found.
- **L** prefetch next stream URL 20 s before the end: `Controller` already arms the next track ahead of the end
  (`armed`, `attach_next_bin`); I did not measure skip latency, so I cannot say what is left to gain.

Nothing was reverted (no kept change turned out worse), so there are no new "Considered, not done" rejections beyond
the list above and `MALLOC_ARENA_MAX=4` (inconsistent, not chosen).

## Could not test
- **No feature was clicked through in a signed-in running app.** Resume offer, fill-queue (incl. the radio branch and
  the notice), personal-data export/import dialogs, the strip window, party limits and host-first (the ordering hook
  runs on every host queue change), and the share-card theme menu were covered by unit tests and offscreen
  renders only. A loopback party test of the new refusal text with a 1.7.0 guest was not written (no wire change; the
  rule is unit-tested in `requests.rs`).
- Strip: always-on-top on X11 and Windows, `drag_window` on each platform, and the Wayland notice were not seen live.
- Resume points: leave-on-quit and leave-on-stop paths are not covered by tests (the rules and store are).
- Windows and macOS: not built. New Linux-only items are `cfg`-gated (`mallopt`, `libc` dependency); the strip and
  the other features are platform-independent code that compiles on Windows only in CI.
- Fat LTO on Windows (see K); `mallopt` is glibc-only, musl builds skip it.
- Interface scale 1.5 and 2: screenshots were rendered at 1, 1.5, 2 for the strip and resume pill; the other pages at
  scale 1 (the settings and dialog shots use the standard `ui_shots` sizes at all three scales, checked visually at 1).
- Import of a file written by a *future* schema version is refused by design; a 1.6 profile's old files are untouched.
- `bigq.sh`, window-mapped time and idle numbers were taken on `head`/`e`/`k` builds, not re-run on the very last
  commit (`237e8e9` only changes how a queue file is written).
