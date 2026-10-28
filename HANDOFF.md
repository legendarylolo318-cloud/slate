# Reviewer handoff — Slate (port of lullabyX/sone)

**State: substantial but incomplete port.** The Slint app covers login, library, search, home,
album/artist/playlist/mix pages, queue, lyrics, full-screen player, mini-player, settings, themes,
signal path and music video on Linux. Gaps are listed in `PORTING_PLAN.md` §11. Everything was
developed and smoke-tested on one Linux/Wayland machine with one TIDAL account.

## What is verified
- `cargo test --workspace`: ~400 core tests, ~37 app tests and the source guards pass (Linux).
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- Exercised live: sign-in, playback, library/playlist pages, artwork, queue, folders listing, settings
  page, themes. **Not exercised live**: music video, miniplayer, signal-path panel, scrobbling,
  Discord, MCP/overlay toggles, proxy, exclusive/bit-perfect output, any Windows build.

## Architecture
- `slate-core` — no UI/Tauri deps. `AppState` owns audio, client, caches, scrobbling,
  Discord, MCP/overlay servers. Frontends implement `EventSink` (events) and
  `MediaSink` (OS media controls) and call `slate_core::commands::*`.
- `slate-app` — `main.rs` runs the pre-thread environment scrub (proxy vars,
  `GST_PLUGIN_PATH`), then `bootstrap.rs` starts tracing, a tokio multi-thread runtime,
  chooses a renderer, opens the window.

## Threading model (intended)
Tokio on background threads; UI mutation only via `invoke_from_event_loop` /
`Weak::upgrade_in_event_loop`. Not yet exercised since there are no UI callbacks.

## Decisions / things I was unsure about
1. `AppHandle::state()` aborts the process if called before `AppState` is installed
   (programmer error); chosen over `expect` to honour the no-`expect` rule while keeping
   the ~100 inherited call sites unchanged.
2. Queue/shuffle/repeat/autoplay/LRC/theme-validation/presets live in the TS frontend, not
   Rust — they must be ported, not moved. Largest remaining core task.
3. The OBS overlay page embeds a `<script>`; the brief permits only static HTML/CSS.
   Needs a product decision.
4. Embedded-webview PKCE login cannot exist without a webview; login falls back to
   device-code (+QR) or the browser PKCE flow with code paste.
5. The no-`unwrap`/`expect`/`panic!` rule is enforced by clippy deny lints (see PORTING_PLAN §13). Some upstream style lints remain allowed crate-wide (`lib.rs`).
6. `docs/` is gitignored upstream, so the reference copies of the removed Tauri files
   (`docs/legacy-tauri/`) exist only locally; they are also in git history (`a34fd6f`).
7. Commits were authored with a per-command identity (`SONE port (Claude Code)`); rewrite
   authorship as you see fit. Nothing was pushed or published.
8. Renderer fallback: Slint fixes its platform once per process, so if the chosen renderer
   fails within 5 s of startup the app re-launches itself with the next one
   (`SLATE_RENDERER_INDEX`), passing back the proxy env vars the launch scrub removed.
   Only Skia/Vulkan was exercised.
9. Running the binary uses the real config dir unless `XDG_CONFIG_HOME` is overridden; on my
   first run it touched `~/.config/sone` (cache rebuild) and connected Discord RPC.
10. **Rename sone → Slate.** Crates `slate-core` / `slate-app`, binary `slate`, error type
    `SlateError`, env vars `SLATE_LOG` / `SLATE_RENDERER_INDEX`, MPRIS/tray/desktop id `slate`.
    Credit to lullabyX/sone is kept (README, Cargo `authors`, LICENSE, MusicBrainz UA).
    Consequences you should know about:
    - Config/cache now live in `~/.config/slate`, keyring service is `slate`, and the encrypted-file
      magic is `SLAT`: **Slate does not read or touch a SONE install's data** (which also means no
      shared login).
    - The update checker is **disabled** (no Slate release feed); pointing it at upstream would
      announce SONE's versions as Slate updates.
    - **Discord Rich Presence still uses upstream's Discord application id** (name shown there is
      "SONE") and upstream's logo URL. A Slate Discord application must be registered by the
      maintainer, then `APPLICATION_ID` swapped.
    - Not yet renamed: `data/` (desktop file, metainfo), `build-scripts/`, the checkout directory,
      README body (still upstream text) — handled in the final cleanup.
11. Threading model (as built): `tokio` multi-thread runtime owns all network, disk and GStreamer
    work; `Backend` (cheaply cloneable) forwards UI callbacks to it and publishes results with
    `Weak<AppWindow>::upgrade_in_event_loop` (wrapped in `Backend::ui`). Lists use a custom lazy
    `slint::Model` (`models::LazyTracks`); async results find it by downcasting the window's model.
    MPRIS runs on its own current-thread runtime. Video frames are dropped (not queued) when the UI
    thread is behind.
12. Renderer fallback relaunches the process; if a renderer "starts" but draws nothing, that is not
    detected.
13. `build-scripts/`, `flake.nix`, metainfo and screenshots under `data/` are still upstream's and
    describe the Tauri build; they need replacing before any packaging.
14. The interface-scale setting uses `SLINT_SCALE_FACTOR` at launch, so it needs a restart.
15. Global shortcuts are not implemented. `tidal://` + single instance use `interprocess` local sockets; on Windows the URL scheme registration must be done by the installer.
16. Gapless and scrobble submission are implemented but were only unit-tested, not heard / observed against live services.
