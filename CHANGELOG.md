# Changelog

## [1.0.0] - 2026-10-29

### Added
- Update notice: once a day Slate looks at its latest GitHub release and, if a newer final
  version exists, shows "Slate X.Y.Z is available" with a button that opens the release page.
  It never downloads or installs anything. Settings → General → Check for updates (on by
  default); closing the notice remembers that version.
- Export: playlists and Loved Tracks have an Export… button (CSV or M3U), with a save dialog,
  progress, and pages not yet loaded fetched one at a time.
- Import: New playlist → Import from file… turns a CSV or M3U into a new playlist. Slate's
  own exports and any file with TIDAL track links need no lookups; other files (Spotify
  exports via Exportify, M3U with `#EXTINF` lines) are matched by artist and title, one
  search at a time with a pause between them, stopping if TIDAL starts rate-limiting.
- Track credits: right-click a track → Credits lists producers, writers, engineers and
  musicians (one request, cached).
- Hi-Res, Lossless and Dolby Atmos badges in their own Quality column on track lists, on
  album pages and in the player bar, from the quality data TIDAL already sends.
- Settings → Integrations: Copy URL buttons for the MCP server and the OBS overlay, so the
  connection URL can be pasted into an AI client or OBS.
- Settings → General → Colours from album art (off by default): tints the player bar and
  full-screen player with the cover's main colour, adjusted so text stays readable.

### Changed
- TIDAL requests now run up to four at a time instead of one after another; pages load
  faster, rate-limit handling is unchanged, and background refreshes wait for the page on
  screen. Library lists (albums, artists, mixes) and the artist page fetch their parts at once.
- Images decode with one copy of the pixels; the disk cache does its file and encryption
  work off the async workers.
- Fewer idle wakeups (about 25 → 6 a second): the playback and MPRIS tickers sleep while
  nothing plays, and the theme-file watcher ticks four times a second instead of sixteen.
- The main screen no longer waits for the session lookup, queue restore and startup tasks.
- The volume slider changes loudness evenly along its whole length: every tenth of the slider
  is the same 5 dB step (50 dB range, fading to silence at the bottom), instead of the cubic
  curve that did most of its change near the bottom.
- The explicit "E" marker sits right after the track title instead of floating in the
  title column.

### Fixed
- Cached track credits were never read back, so each open asked TIDAL again.
- The signed-in user id was requested again whenever it was needed.
- Signing out while a token refresh was on the wire could bring the old session back.
- A request prepared just before a token refresh was sent with the old token, earning a
  401 and a second request.
- Creating or renaming a playlist and the profile edits (name, bio, links, picture) skipped
  the rate-limit cooldown and the in-flight cap.
- A background refresh that started before a favourite was added (or before signing out)
  could write the old list back into the disk cache.
- Export: navigating while the save dialog was open could put the new page's tracks in the
  file; names typed without an extension now get one; names no longer end in a dot or
  space (invalid on Windows).
- The credits panel reopened itself if closed before the credits arrived.
- A quick skip could leave the previous track's cover colour on the player bar.
- Tracks in TIDAL's old MQA tier were badged Hi-Res; they stream as 16-bit FLAC and are
  now badged Lossless.
- The request log included the account id from `/users/<id>/…` paths.
- MCP playback tools (play, pause, resume, skip, seek, volume, queue, repeat, shuffle, play a
  playlist/album/artist/mix) reported success but did nothing: the app never handled them.
  `get_now_playing` and `get_queue` always came back empty. Both now work.
- Creating a playlist (New playlist, Import) and renaming a private one failed with HTTP 400:
  Slate sent the access type `PRIVATE`, which TIDAL's playlist API does not accept; it is now
  `UNLISTED`.
- The OBS overlay never showed the playing track. It now updates on every track change,
  pause and seek, with cover and stream quality.

### Performance (measured)
Release builds, empty profile, login screen, 60 s after a 12 s warm-up, two runs each:
idle wakeups 20–25 → 6–8 a second, CPU 5 → 1–2 ticks a minute, memory (PSS) unchanged at
about 158 MB, `--self-test` startup unchanged (10.8 vs 11.1 ms, within noise). Playing and
signed-in numbers were not measured.

### Later
- The remaining idle wakeups are the theme-file watcher (4 a second, the debouncer
  library's floor) and one unattributed blocking thread (2 a second).
- About 120 MB of the 158 MB is file-backed (GPU driver, the binary, font caches); there is
  no easy cut in the 43 MB that is the app's own.

## [0.3.0] - 2026-10-29

### Added
- Browser sign-in has Open browser and Copy link buttons, for when the browser does not open
  by itself.
- Explore genre and mood tiles without artwork show their name on a coloured tile.

### Fixed
- Library → Mixes failed to load: TIDAL rejects pages larger than 50 for mixes, which now
  page in 50s. A mix that cannot be read no longer cuts the list short.
- Folders with more than 50 playlists (and more than 50 folders in the sidebar) showed only
  the first 50.
- A cover that failed to download (server error, rate limit) was cached as broken for up to
  90 days; failed downloads are no longer cached.
- Leaving a large library page stopped downloading its remaining covers, and a late cover can
  no longer appear on the next page.
- Two requests for the same image at once no longer download it twice.
- Short pages stay at the top of the window; "1 album", "1 mix" and so on are singular.

### Changed
- Release builds use thin LTO, one codegen unit and stripped symbols (smaller download).

## [0.2.0] - 2026-10-29

### Changed
- New icon set: clean vector icons replace the emoji and text symbols across the player bar,
  full-screen player, mini player, video player and dialogs. Larger buttons by default.
- The player bar can be resized: Settings → General → Player bar size (Small, Normal, Large,
  Extra large). Buttons, icons, cover art, text and sliders all scale, instantly.
- Lyrics can be resized: Settings → General → Lyrics size. Full-screen lyrics also grow with
  the window, and long lines wrap instead of being cut off.
- The seek bar keeps a usable length in narrower windows.

### Fixed
- Panels and docks showed a placeholder instead of Slate's icon when Slate was run as a plain
  AppImage or binary. Settings → General → "App menu and panel icon" → Add installs Slate's
  launcher and icons for your user (and makes global shortcuts available on Linux); the icon
  is also set on the window itself (X11, Windows).
- The tray icon was an old placeholder; it now uses the Slate icon.
- `slate --install-desktop-entry` / `--remove-desktop-entry` do the same from the command line.

## [0.1.0] - 2026-10-28

First release of Slate: a native TIDAL client written in Rust with a Slint UI, ported
from [SONE](https://github.com/lullabyX/sone) by lullabyX. No webview, about half the
memory of the original.

### Listening
- Lossless and Hi-Res streaming with a quality cap and automatic fallback; gapless playback
  (GStreamer 1.24+); ReplayGain; autoplay radio when the queue ends.
- Exclusive and bit-perfect output: direct ALSA with DAC format/rate matching on Linux,
  WASAPI exclusive on Windows. Signal Path panel shows what touches the audio.
- Synced lyrics, full-screen player, mini player, music videos (can shrink into the player
  bar), queue with shuffle/repeat/history that survives restarts.

### Library
- Home, Explore (genres, moods, new releases), search, albums, artists, mixes, playlists
  and folders; Loved Tracks and Loved Videos; sorting on every library list.
- Playlist and folder management, favourites, add to playlist, share links, profile editing.
- `tidal://` and tidal.com links open in the running Slate.

### Desktop
- Linux: MPRIS media controls, tray, sleep prevention. Windows: media keys and the volume
  flyout (SMTC), tray, sleep prevention, installer with Start Menu entry and `tidal://` handler.
- Optional global shortcuts (Ctrl+Alt+Space / → / ←), keyboard shortcut sheet (`?`),
  15 themes with a colour picker, interface scale.
- Scrobbling (Last.fm, Libre.fm, ListenBrainz), TIDAL play reporting, Discord Rich Presence.
- HTTP/HTTPS/SOCKS5 proxy that fails closed; MCP server and OBS overlay (both local-only,
  off by default).
- Renderer falls back automatically from Vulkan to OpenGL to software when a GPU driver is
  missing or broken, and remembers what worked.

### Security and privacy
- Settings encrypted with AES-256-GCM, key in the OS keyring (Windows Credential Manager,
  Secret Service on Linux); config files owner-only.
- Local servers bind to 127.0.0.1, reject foreign `Host` headers (DNS rebinding), and the
  overlay page runs under a strict content security policy.
- Single-instance socket private to the user on both platforms; no tokens or proxy
  passwords in logs.

Known: Windows builds are unsigned. Upstream history before the port:
https://github.com/lullabyX/sone/releases.
