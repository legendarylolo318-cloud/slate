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
- The share_card tests load only the fonts they need (about 1 s; they scanned every system font before the review).
- Edits made within 0.6 s of each other are one undo step (so a multi-select "add to queue" is one step).
- Tags are written 0.7 s after the last edit, and at quit (fixed in review, #127).
- Not pushed, no tag, no PR (still true after the review until the release step).

## 1.6 review (2026-11-05)
Findings and fixes: REVIEW.md #121–138; measured numbers and what was not tested: CHANGELOG 1.6.0.
Everything listed above as "NOT tested in the running app" was clicked through on a signed-in profile
copy except saving the party set (it would create a playlist on the real account) and anything with a
second party member.

- Queue undo (`queue_undo.rs`): the stack keeps the queue as the last edit left it ("settled") and,
  before every edit/undo/redo, replays what changed meanwhile (played, autoplay, party requests) onto
  every stored state; an epoch change that is not an edit clears it. Captures share `Arc<Track>`s with
  the settled state — never capture with `None`, that copied the whole queue (85 MB per undo).
- Share card: never call `fontdb::Database::load_system_fonts` (12 s and every font file on a machine
  with many fonts); `font_db` asks fontique for the few families needed. Text is split into per-script
  and emoji `<tspan>`s because usvg drops fallback glyphs when glyph counts differ.
- Clipboard: always go through `clipboard.rs` (one kept handle); dropping an `arboard::Clipboard`
  loses the copy on X11.
- Dialog text fields use `autofocus: true`; closing a dialog refocuses `fs` (the shortcut scope).
- Driving the UI (as in 1.5): headless `kwin_wayland --virtual` + rootful `Xwayland :88`, keep the
  runtime-dir path short (Wayland socket paths are limited to 108 bytes), python-xlib XTEST, and set the
  input focus on the Slate window explicitly (no window manager, so winit drops keys otherwise).
  `SLINT_BACKEND=winit-software` for screenshots (Vulkan frames on the nested X server can be stale).
  Profile copies for measurements: clear every setting but the sign-in (`Settings { auth_tokens,
  client_id, client_secret, auth_method, ..Default }`) and delete each copy after its run (the 1.6
  review filled /tmp once).
