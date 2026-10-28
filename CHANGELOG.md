# Changelog

## Unreleased (Slate — Slint port of SONE)

### Renamed
- Project renamed from SONE to Slate (crates, binary, config dir, identifiers). Update checker disabled until Slate has its own release feed.

### Added
- Queue model (manual + context queues, shuffle with order restore, repeat, history, autoplay, snapshot persistence) and playback controller in `slate-core`.
- MPRIS server and StatusNotifier tray (Linux); previous/next/shuffle/repeat in the player bar.

### Changed
- Cargo workspace: `crates/slate-core` (UI-free logic, extracted from `src-tauri`) and
  `crates/slate-app` (Slint application).
- Tauri `AppHandle` replaced by `slate_core::AppHandle` / `EventSink`; Tauri commands are
  plain functions over `&AppState`.
- OS media controls abstracted behind `slate_core::media::MediaSink`.
- `cargo fmt` and `cargo clippy --all-targets -- -D warnings` clean.

### Removed
- Tauri shell, webview-bound code (PKCE login window, WebKit proxy-auth, tray, MPRIS thread,
  Wayland idle-inhibit surface). Each is tracked in PORTING_PLAN.md for reimplementation.

Upstream history prior to the port: see https://github.com/lullabyX/sone/releases.
