# Slate 2.0 handoff (end of day 1)

Base: v1.9.0 (`c7879ca`). Nothing pushed, tagged or version-bumped. Author on every commit: "Slate dev".
The original brief (hard rules, F1–F6, P1–P7, measurement method, final `HANDOFF-2.0.md` with tables) still applies in full.
**This file is a working note, not the final `HANDOFF-2.0.md`.** Write that one at the end.

## Commits so far (4 of 12 items)

| Commit | Item | What it does |
|---|---|---|
| `7e762d2` | F1 crossfeed | Settings → Playback: Off / Light / Medium / Strong via `bs2b`. No element when Off. Off on a running pipeline bypasses it with `set_passthrough` (gstreamer-base added). Hidden with a sentence when the plugin is missing; greyed in exclusive / bit-perfect. Signal Path row and MODIFIED. Crossfade workers inherit it. `Segmented` gained `enabled`. |
| `f211eaf` | F2 queue link | Queue "⋯" → Copy queue link. `core/queue_link.rs`: `slate://queue/<b64url>`, deflate over 1 KiB, max 500 tracks (notice says when cut), 16 KiB link cap, 64 KiB unpack cap, numeric ids only, version check. Opening it shows Replace / Add / Cancel and skips unavailable or blocked tracks with a count. The single-instance socket line cap went from 2048 to 16 KiB + 64. `flate2` added. |
| `1bd1fd8` | F3 device memory | Opt-in toggle (`ui.json: device_memory`, default off). Volume and EQ per device, max 50 (LRU), `devices.json` in the encrypted state folder, deleted at sign-out and not written after it. Restores serialise under one lock. |
| `d76dab0` | F4 alarm | Settings → General rows. `core/alarm.rs` is pure and tested, including a DST case. `app/alarm.rs` runs one sleeping task only while armed. `ui.json: alarm_armed` avoids a file read at start-up when unused. Missed more than 5 min → skipped with a bar notice. Fade-in 30–120 s. Snooze 10 min from the bar, or Pause/PlayPause while ringing. Keep-awake via `inhibit_idle`. |

All four passed the full gate. F1's tests also passed 200 times pinned with `taskset -c 0`.

## Later commits (F5, F6 done)

| Commit | Item | What it does |
|---|---|---|
| `0f09ba1` | F5 import | `core/import_lib.rs` (Spotify JSON, Exportify/Apple CSV, Apple `Library.xml` via roxmltree, `judge()` matcher; 18 tests incl. XXE, entity bomb, node cap, 20k-track linear time) and `app/service_import.rs` (picker, paced ISRC-then-title matching, review dialog, unlisted playlists). Matcher never run against real TIDAL results. |
| `a86ae09` | F6 accessibility | Reduce motion Off/On/Follow system (`Motion.dur` on all animate sites; Linux follow via gsettings; **no Windows follow**). High Contrast preset. WCAG AA test over all presets found and fixed two real failures in `derive()` (error red on light themes; white on mid-tone accents). `IconButton` takes its label from `tip`; 33 buttons got `accessible-label`; a test scans the .slint sources. |

All six features F1–F6 are committed and passed the full gate. Re-run F1's thread tests pinned to one core only if touched again; F4/F5 tests do not use threads or locks beyond what is described.

## Gate script

`/tmp/claude-1000/-home-lolo-slate/22ca04b6-e35c-4ac2-850b-5040897d84f0/scratchpad/gate.sh` runs fmt, clippy three ways, and `cargo test --workspace`. That path is session-scoped and may be gone tomorrow. It is just those commands in order:
```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --no-default-features -- -D warnings
cargo clippy --workspace --all-targets --no-default-features --features slate/renderer-skia-vulkan,slate/renderer-femtovg,slate/renderer-software -- -D warnings
cargo test --workspace
```
The script does not stop on failure. Read the output for `error`, not just the last line. `ls` is aliased on this machine (eza), so use `find` / `command ls` in scripts.

## Not started

(F6 was done after this note's plan below, kept for reference.) **F6 accessibility.** Plan:
- A `Motion` global in `theme.slint` with a `dur(d)` function, applied to the 13 `animate` sites (pages 1, fullscreen 4, video 2, lyrics_full 1, library 1, player_bar 1, widgets 2).
- Reduce-motion setting as Off / On / Follow system, default Off. Follow on Linux via `gsettings get org.gnome.desktop.interface enable-animations`, queried only when "Follow" is chosen. Skip the Windows OS follow (can't compile-test it here) and say so in the handoff.
- A High-contrast preset in `core/theme.rs`. `PRESETS` is `[_; 15]` and must match `theme_config::PRESET_NAMES`, so update both and the Settings grid.
- A test that every preset passes WCAG AA for text. `custom_theme.rs` already has `contrast_ratio` and `contrast_warnings`, and `derive()` in `theme.rs` makes the palette. Some existing presets may fail, e.g. `text_muted` `#a6a6a6` on the light themes, or accent on `on_accent`. Fix the palette or presets rather than loosening the test.
- `accessible-label` on every icon-only `IconButton` (about 50 uses, none have one now), plus a test that scans the `.slint` sources and fails on an `IconButton` without one. `settings_search.rs` already reads `settings.slint` the same way.

**P1–P7 performance A/Bs and the whole measurement table.** None started. They need release builds (`lto=fat`, about 11 min each), `~/slate-bench` (`run.sh`, `profreset`, `bigq.sh`, `gen.py`, `thrott_proxy.py`) and a headless KWin with nested Xwayland on `:88`. Rules from the brief: never launch Slate on the real profile (`XDG_CONFIG_HOME=<copy>`), interleave runs, delete profile copies after each run, keep a change only if the win reproduces.

## Open questions and risks to put in the final handoff

- **F3 can't see a device change without polling.** Slate has no default-sink change notification in shared mode. I look at the device at track start, exclusive toggle and exclusive-device pick (one `pactl info` per track start, only when the setting is on). A device plugged in mid-track is noticed at the next track. This is a deliberate reading of "existing notifications only". Say so.
- **bs2b in bundles is unverified.** The AppImage should pick up `libgstbs2b` and `libbs2b` via the Ubuntu `gstreamer1.0-plugins-bad` package through linuxdeploy-plugin-gstreamer. I could not check the Windows GStreamer MSI. The option hides itself when the plugin is missing.
- **Windows-only code is uncompiled here (no rustup / cross target).** `alarm.rs` calls `GetTimeZoneInformation` (windows 0.62; features `Win32_Foundation` and `Win32_System_Time` added to `slate-app/Cargo.toml`). I read the crate source for the signature. Windows CI is the real test. Windows shared-mode device memory returns no key (exclusive WASAPI only).
- **Never clicked in the running app:** every UI piece (F1–F5 Settings rows, queue link dialog, alarm bar, import review). Only unit-tested and compiled. F1's engine is tested at element level only (bleed per level, bypass untouched). The alarm timer, ring, fade and snooze flow has never run. A live smoke test on an isolated profile copy is still owed.
- **F1 finding:** bs2b's `feed` is dB below the direct signal, so a lower number is a stronger effect. My first mapping was backwards; a test caught it. Levels are now Light 650 Hz / 9.0 dB, Medium 700 / 4.5, Strong 700 / 3.0.
- **F3 / F4 persistence design:** both use a flag in `ui.json` (already read at start-up) so default start-up reads nothing new.
- **F2:** `Controller::playable()` is new and public. Link loading uses `metadata::get_track` per id, 4 at a time.
- **Idle numbers (21 threads, 0 wakeups/s, 0 ticks) were not re-measured** after any of these changes. Reasoning only: nothing new spawns at start-up with default settings. Verify in the final table.
- The `lookathisclaude/` directory in the repo is untouched.
