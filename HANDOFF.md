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
- `slate --self-test --network` checks GStreamer plugins and HTTPS streaming, then exits.
