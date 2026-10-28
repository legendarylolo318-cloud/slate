# Reviewer handoff — Slate (port of lullabyX/sone)

**State: partial port.** Steps 1–2 are implemented: login (code + QR), a track-ID box and a
player bar. There are no library/search/queue screens yet, so the app is not usable day to day. See
`PORTING_PLAN.md` for the per-feature table.

## What is verified
- `cargo test --workspace`: 367 core unit tests + 16 integration/guard tests pass (Linux).
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- Not verified: running the Slint window (no display in the authoring environment), any
  Windows build, any live TIDAL call.

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
5. Inherited core still contains `unwrap`/`expect`/`panic!` outside tests (rule not yet
   met) and some style lints are allowed crate-wide (`lib.rs`).
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
