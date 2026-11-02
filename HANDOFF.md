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
- 1.4: the multi-selection lives in `models::LazyTracks` (shown-row indices, cleared when the shown rows
  change; `slate_core::selection` is the click logic) and in `selection_ui::QUEUE_SEL` for the queue
  panel. `Queue::set_tracks` no longer clears the manual queue; `Queue::play_all` does, except for an
  artist source. `Queue::requeue_head` puts the track at the head of the manual queue.
- 1.4 party: `HostMsg::Heir` (to the successor only, key redacted in `Debug`) and `HostMsg::Successor`
  (to everyone) are sent on every roster change and to each joiner after it subscribes;
  `Host::start_as` continues a party under that key and secret. `reactions.rs` holds the emoji set (indexes
  on the wire, appending is compatible) and the 5-per-10-s limit.
- 1.4 audio: exclusive-writer teardown never waits more than `WRITER_SHUTDOWN_TIMEOUT`; the appsink
  callback gives up on a full writer queue once `sink_closing` is set (not on a generation bump, a seek
  bumps it too). Tests that wait on a thread run it under `within()`.
- The DJ (`dj.rs`) is called from `Controller::autoplay_or_dj`; `AppState::dj_enabled` is the only thing
  the autoplay path reads while it is off. Never ask the model with the queue locked: `next` calls
  `dj_prepare` first (model + TIDAL search unlocked, result keyed on epoch and current track) and returns
  `Outcome::Nothing` if the user played something meanwhile.
- Party handoff key: `HostInner::heir` is regenerated whenever the successor changes; a kick sends no
  heir/secret message itself (the roster change after the kicked guest leaves does), and the guest loop's
  `select!` is `biased` so the kick branch wins.
- `blocking` is pinned to 1.6.2 in Cargo.lock: 1.7.0's idle pool thread never exits and woke 2/s for the
  whole session (AccessKit's AT-SPI connect starts it). Check before letting `cargo update` move it.
