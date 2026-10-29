# Changelog

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
