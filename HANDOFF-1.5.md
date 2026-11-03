# 1.5.0 handoff (stopped early; not releasable yet)

Written in a hurry. Nothing is tagged or published.

## Committed (one feature per commit, on `main`)
1. `ffd1e4c` Content filter: Settings → Playback toggles "Hide explicit / AI-generated"; stored in the
   encrypted settings (`hide_explicit`, `hide_ai`), applied via `set_content_prefs`; rows dimmed with a
   "hidden by filter" note (`library::set_content_hide`, `TrackRow.blocked`). Lists already on screen
   update when reloaded.
2. `5ad6573` Window state: `slate-core/src/window_state.rs` (pure rules, 12 tests) +
   `slate-app/src/window_state.rs` (glue). Saved to `window.json`, debounced single-shot timer,
   written on the blocking pool; toggle in Settings → General (`UiPrefs.remember_window`, default on).
   Wayland: size/maximized only.
3. `c94c4ae` Album shuffle: `shuffle::album_shuffle`, Settings → Playback "Shuffle mode", right-click menu on
   the player-bar shuffle button (not on the full-screen player). Tests in `shuffle.rs`.

## MPD server (feature 4), committed
- Core: `slate-core/src/mpd/{mod,proto,server}.rs`, `commands/mpd.rs`, settings `mpd_*`, `AppState.mpd_*`,
  `Controller::clear_all/shuffle_upcoming`, `Queue::shuffle_upcoming`.
- Tests: `slate-core/tests/mpd_net.rs` (12 loopback tests) + `proto.rs` unit tests. fmt, clippy -D warnings and
  `cargo test --workspace` all passed on this commit.
- App: `mpd_host.rs`, hooks `Backend::mpd_changed()`, Settings → Integrations rows. README "Phone remote (MPD)" written.
- Not tested: any real MPD client, the settings UI, Windows.
- Quirks: MPD playlist = playing track (pos 0) + upcoming; `play N` leaves earlier entries queued; `clear` stops
  playback and drops the current track; `lsinfo/list/count` are empty; `mpd_port` has no UI.

## Not started
- 5 Party text chat, 6 Piper voice for the DJ, 7 DJ set from a playlist.
- docs-assets/MCP.md note, CHANGELOG 1.5.0 (Added/Changed/Fixed/Notes),
  "Performance (measured)" tables (default and all-features-on), version bump.

## Untested (be honest in the changelog)
- Window restore/off-monitor recentering on real X11/Wayland/Windows; start-hidden-to-tray on Linux skips the
  post-show monitor check.
- Album shuffle and content-filter UI paths were compiled and unit-tested only, never clicked.
- No resource measurements were taken.

Note: this file is `HANDOFF-1.5.md` because `HANDOFF.md` is the tracked architecture doc.
