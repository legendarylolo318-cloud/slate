# Slate 1.6.0 handoff (work in progress)

Branch `main`, commits on top of 1.5.0 (author "Slate dev"; git has no identity configured here, so commits used env vars).

## Done (one commit each)
1. **QR removal**: party QR code, UI and `party-qr` property gone; invite link text + Copy button stay. The `qrcode` crate stays (device-code login still uses it). README updated.
2. **Party set**: `party_set.rs` (cap 500, no repeats, skips videos and content-filtered tracks, host only). End party opens a "Save set as playlist" dialog (`dialog-kind` `party-set`); the Party tab shows a button until the next party. 4 unit tests.
3. **Share card + Copy link**: `share_card.rs` (resvg text layout, cached cover only, arboard image, falls back to saving a PNG in Pictures). Menus: track rows, queue rows, player-bar info area. "Copy link" now copies `https://tidal.com/browse/track/<id>`. New deps: resvg `system-fonts` (adds fontconfig-parser), arboard `image-data`, `dirs` moved to all platforms. 6 tests; `write_sample` (ignored) writes a sample PNG.
4. **Fullscreen lyrics**: `ui/lyrics_full.slint`, `lyrics_full.rs` (blur from <=128 px thumbnail, once per track, worker thread), shortcut `lyrics-fullscreen` (F11, in `keymap.rs`), button in the lyrics panel, Esc exits. Committed after a debug `cargo check` and unit tests only. NOT looked at on screen: a release `ui_shots` run (`SLATE_SHOTS=lyrics-full`, `lyrics-full-plain`) was started and its result not checked. The `lyrics-full` branch in `ui_shots.rs` has not been compiled.

## Not started
5. **Personal tags** (tags.json next to ui.json, Tags… menu with multi-select, sidebar Tags page with counts/rename/delete, lazy load, debounced off-thread write; use `textutil::clean_line` to strip bidi in tags; tests).
6. **Queue undo/redo** (Ctrl+Z / Ctrl+Shift+Z rebindable in `keymap.rs`, 20 states as `Arc<[TrackId]>`, buttons in queue panel, no undo for autoplay/radio/DJ additions, guests excluded; tests).
7. **Docs**: 1.6.0 CHANGELOG (Added/Changed/Removed/Fixed/Notes; note new setting/shortcut keys, the Copy link URL change, and the new deps), README features, version bump to 1.6.0 in `Cargo.toml`.
8. **Verification**: `cargo build --release`, full `cargo test`, launch the app and click through every feature; list untested items. Nothing has been run in the real app yet (needs a TIDAL login); share card clipboard (Wayland/X11/Windows) and the party-set dialog are untested live.

## Known issues
- `cargo check --no-default-features` (no `party`) already failed on 1.5.0: `party_off.rs` lacks `party_chat_send` / `party_chat_mute`. Not caused by 1.6 work.
- share_card tests take ~15-40 s in debug (system font scan).
