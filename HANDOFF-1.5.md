# 1.5.0 handoff

All seven features are committed on `main`; nothing is tagged or published. `cargo fmt --check`,
`cargo clippy --workspace --all-targets -D warnings` and `cargo test --workspace` pass.

Commits: content filter, window state, album shuffle, MPD server, party chat, then Piper voice and
DJ-from-playlist together (they share the DJ files), then release prep (version 1.5.0, CHANGELOG, MCP.md).

Review hints:
- `slate-core/src/mpd/` + `tests/mpd_net.rs` (server, limits); `slate-app/src/mpd_host.rs` (maps to Control/queue).
  A source guard rejects a type named `Client` with `::new(`: the MPD per-connection type is `Peer`.
- `slate-core/src/dj_voice.rs`: child processes, `track_changed()` hook in `Controller::start` and
  `on_track_advanced`; `Controller::dj_playlist`.
- `slate-core/src/party/chat.rs`; `window_state.rs` (core rules + app glue).

Measured: login screen, default settings, 1.4.1 vs 1.5.0 (CHANGELOG). NOT measured: playing, VRAM,
window-map time, "all features on" table. See CHANGELOG "Not tested" for the full honest list
(no real MPD client, no signed-in session, UI never clicked through, Windows untested).
Known: MPD playlist model quirks (CHANGELOG Notes); window check skipped for start-hidden on Linux.
