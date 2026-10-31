# Architecture and threading

Notes for anyone working on Slate's code. What changed per release is in
[CHANGELOG.md](CHANGELOG.md); the MCP interface is in [docs-assets/MCP.md](docs-assets/MCP.md).

## Crates
- `slate-core`: no UI dependencies. `AppState` owns the audio engine, the TIDAL client, the
  disk cache, scrobbling, Discord, the MCP and OBS servers and the listening party protocol.
  Frontends implement `EventSink` (events) and `MediaSink` (OS media controls) and call
  `slate_core::commands::*`.
- `slate-app`: the Slint UI and platform glue (MPRIS and tray on Linux; SMTC, tray and sleep
  prevention on Windows; global shortcuts on both). `main.rs` scrubs the environment before
  any thread starts (proxy variables, `GST_PLUGIN_PATH`), then `bootstrap.rs` starts tracing
  and a tokio multi-thread runtime, picks a renderer and opens the window.

## Threading
- All network, disk and GStreamer work runs on the tokio runtime or the audio worker thread.
  The UI thread only builds and updates Slint models.
- `Backend` (cheap to clone) forwards UI callbacks to the runtime and publishes results with
  `Weak<AppWindow>::upgrade_in_event_loop`, wrapped in `Backend::ui`.
- Track lists use a lazy `slint::Model` (`models::LazyTracks`); results for a page carry the
  page generation and are dropped if the page changed meanwhile.
- Every TIDAL request goes through `TidalClient::send`: at most 4 in flight, a shared
  rate-limit cooldown, one token refresh for many 401s. Background refreshes yield to
  on-screen requests. Playlist edits take a per-client write lock; long edits from the UI run
  as one paced, cancellable job at a time (`bulk.rs`).
- Optional subsystems (MCP, listening party, crossfade, agent DJ, suggestions) start nothing
  until they are switched on; their hooks in playback code are a single atomic or `None`
  check, and a source-guard test keeps it that way for the MCP hooks.

## Things to know
- Settings, cache and logs live in `~/.config/slate` (`%APPDATA%\slate` on Windows); the
  session is encrypted with AES-256-GCM and the key is in the OS keyring. Slate does not read
  a SONE install's data. Point `XDG_CONFIG_HOME` / `XDG_CACHE_HOME` at a temporary directory
  to run a build without touching your own profile.
- Renderer fallback: Slint fixes its platform once per process, so if the chosen renderer
  fails within 5 s of start-up the app relaunches itself with the next one
  (`SLATE_RENDERER_INDEX`).
- The interface scale uses `SLINT_SCALE_FACTOR` at launch, so changing it needs a restart.
- `slate --self-test --network` checks GStreamer plugins and HTTPS streaming, then exits;
  `slate --self-test --party-relay` checks that the party's iroh relay is reachable.
- Party rules are pure and tested in `slate-core/src/party/requests.rs` (caps, duplicates, fair
  queue, vote skip, handoff); `proto.rs` is the versioned wire format (protocol 2 since 1.3) and
  `net.rs` the transport. `party_ui.rs` in the app only connects them to the queue and the UI.
  Everything a peer can grow is bounded: 4 KiB messages, 60 messages per 10 s per guest
  (`proto::Flood`), 8 pre-auth handshakes, 30 waiting requests, 64 tracks of "cannot play"
  reports; a guest bounds what it reads during the handshake the same way.
- The equalizer (`eq.rs`) adds its GStreamer elements to the pipeline only while it is on and
  the output is not exclusive or bit-perfect; with it off the chain is unchanged. A crossfade's
  incoming worker is handed the EQ (`FadeState::eq`), or it would play on without it.
- The shared-mode sink runs with 40 ms `latency-time` (GStreamer's default is 10 ms) to cut
  sound-server wakeups; `buffer-time` is GStreamer's 200 ms.
- The theme.json watcher is a plain inotify watcher plus a thread that waits in `recv()`; do
  not bring back a ticking debouncer (it cost 4 idle wakeups a second).
