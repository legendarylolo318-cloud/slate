# Changelog

## Unreleased (Slint port)

### Changed
- Cargo workspace: `crates/sone-core` (UI-free logic, extracted from `src-tauri`) and
  `crates/sone-app` (Slint application).
- Tauri `AppHandle` replaced by `sone_core::AppHandle` / `EventSink`; Tauri commands are
  plain functions over `&AppState`.
- OS media controls abstracted behind `sone_core::media::MediaSink`.
- `cargo fmt` and `cargo clippy --all-targets -- -D warnings` clean.

### Removed
- Tauri shell, webview-bound code (PKCE login window, WebKit proxy-auth, tray, MPRIS thread,
  Wayland idle-inhibit surface). Each is tracked in PORTING_PLAN.md for reimplementation.

Upstream history prior to the port: see https://github.com/lullabyX/sone/releases.
