# Slate 1.6.0 handoff

All seven items are committed on `main` (one commit per feature, plus QR removal, plus a release commit
with the changelog, README and version bump to 1.6.0). Git has no identity configured on this machine;
commits used the existing "Slate dev" author through environment variables.

## Verified
- `cargo test --workspace`: all pass (192 app tests, 752 core tests, integration tests).
- `cargo build --release -p slate` on Linux: builds (7m46s).
- The release binary starts on the real signed-in profile and ran 10 s without crashing or logging a new error.
- Unit tests: party set, share card layout (long and RTL titles, markup escaping, real render), blur
  backdrop, tags store (load/save/rename/merge/delete, bidi stripping), undo/redo stack, keymap defaults.
- Visual: sample share card PNG, and offscreen screenshots of the full-screen lyrics (synced layout only).

## NOT tested in the running app (needs clicking through with a signed-in account)
- Party: End party -> "Save set as playlist" dialog and the playlist it creates; the Party-tab button.
- Share card / Copy link: the three menus, the real clipboard on Wayland, X11 and Windows, and the Pictures
  folder fallback.
- Full-screen lyrics: F11, the panel button, Esc, "No lyrics" and unsynced layouts, the real blur on a real cover.
- Tags: the Tags dialog (autocomplete, multi-select), the sidebar list, rename/delete, the tag page with Play,
  Shuffle, Ctrl+F. The tags dialog and tag page have not been seen at all, not even in a screenshot.
- Queue undo: Ctrl+Z / Ctrl+Shift+Z and the buttons against a real queue; the guest refusal.
- Windows and macOS were not built.
- Memory/VRAM budget (+10 MB each) was not measured.

## Known issues / notes
- `cargo check --no-default-features` (no `party` feature) already failed on 1.5.0: `party_off.rs` lacks
  `party_chat_send` / `party_chat_mute`. Not fixed.
- The share_card tests scan system fonts and take about 15-40 s in debug.
- Edits made within 0.6 s of each other are one undo step (so a multi-select "add to queue" is one step).
- Tags are written 0.7 s after the last edit; quitting inside that window loses the last edit.
- Not pushed, no tag, no PR.
