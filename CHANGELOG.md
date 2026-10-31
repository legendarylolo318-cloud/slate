# Changelog

## [Unreleased]

### Added
- Playlists you own have a "Remove duplicates…" button. It reads the whole playlist (with
  progress), lists the repeated tracks (same ISRC, or the same TIDAL track when there is no
  ISRC), and after you confirm removes every copy except the first.
- Go to album / Go to artist on queue rows (right-click), and clicking the title or artist
  in the player bar, full-screen player or mini-player opens that album or artist. When a
  track has several artists you pick one from a list.
- Settings → Shortcuts: every in-app shortcut and the three global media shortcuts can be
  rebound. Click one and press the new combination; a clash names the action already using
  it, each item has Reset, and "Reset all" restores the defaults. The `?` sheet shows your
  current bindings, and global shortcuts are re-registered without a restart.
- Ctrl+F on a playlist, album, Loved Tracks or mix opens a filter box (Esc clears and closes
  it). It matches title, artists and album ignoring case and accents. If the list is not
  fully loaded Slate says "Loading N more tracks to search…" and fetches the rest. Playing a
  track from a filtered list plays the filtered tracks.
- A chip row (All / Lossless / Hi-Res / Atmos) on the same pages narrows the list by quality,
  combines with the filter, and is remembered per kind of page.
- Settings → Appearance → Your themes: make your own themes. "New theme" opens an editor for
  every colour role (accent, background, surfaces, text, error…) with the colour picker and
  a live preview in the real app; text pairs below a 4.5:1 contrast ratio get a warning that
  names the pair. "Generate from album cover" builds a palette from the playing cover.
  Themes can be used, edited, duplicated, renamed and deleted, exported to a `.slatetheme`
  file and imported again (invalid or oversized files give a clear message). Dropping a
  `.slatetheme` on the window imports it.
- Right-click a playlist → "Compare with…" and pick another: tabs for tracks only in A,
  only in B and in both (matched by ISRC, else track), with counts. Make a new playlist from
  any tab, merge both into a new de-duplicated playlist, or add the missing tracks to
  whichever of the two you own.
- Smart playlists: a "Smart" section in the sidebar with "＋" to make one. Rules (all or any)
  on loved, artist, album, quality (Lossless / Hi-Res / Atmos), explicit, duration, release
  year, how long ago it was added to Loved Tracks, and membership of a named playlist, plus
  sorting and a limit. They are stored on your computer and worked out only when opened,
  from Loved Tracks and the playlists the rules name. Play, shuffle, queue and export work
  as on any playlist, and "Save as TIDAL playlist" makes an unlisted snapshot.
- Discord Rich Presence text is yours to set: two template lines with `{title}`, `{artist}`,
  `{album}`, `{quality}`, `{playlist}` and `{year}`, a live preview and Reset (Settings →
  Scrobbling, under Discord). Lines are tidied and cut to Discord's 128 characters. "Hide
  presence for playlists that are not public" and a per-playlist "Hide from / show on
  Discord" in the playlist right-click menu keep chosen playlists off Discord entirely.
- Suggested tracks: under a playlist you own, a panel offers tracks TIDAL recommends for it
  (never ones already in the playlist). It loads only after you scroll to the end of the
  list, or when you press "Show suggestions" — never when the page opens. "＋" adds a track
  (the next suggestion takes its place), the right-click menu and play work as usual,
  "Refresh" gets a new batch and "Show more" the next. Settings → General → Suggested
  tracks under playlists: Off / 5 / 10 / 20 (default 10); Off never asks TIDAL.
- Other versions: right-click a track (in a list or the queue) → "Other versions…" lists the
  explicit / clean, remastered, live and Dolby Atmos versions of the song with year, quality
  and length. Play or queue one, or "Replace" the track in the playlist (same position) or in
  the queue. Album pages have "Other versions" for deluxe / remaster / clean editions.
- Drag and drop: drag tracks from any list onto a playlist in the sidebar to add them (a
  highlight shows the target; a track that is already there asks "Already in playlist —
  Add anyway / Skip"). Drag rows to reorder a playlist you own (shown in its own order) or
  the queue, and drag playlists into and out of sidebar folders. Esc cancels a drag.
- Listening party (Settings → Party): host a session and share a `slate://party/…` link or
  QR code; guests join from the link and everyone plays from their own TIDAL account. Only
  playback state is synced (track, play / pause, seek, the next few tracks for prefetch), peer to
  peer and encrypted, with no port forwarding; up to 10 guests, kick and end. Guests can suggest
  tracks that you approve or deny. A custom `iroh-relay` URL can be set. Nothing runs or is
  opened until a party starts, and ending it closes everything.
- MCP for AI agents (full list in docs-assets/MCP.md): new tools for the queue (`add_to_queue`,
  `play_next`, `move_queue_item`, `get_history`), search and browsing (`search_albums`,
  `search_artists`, `search_playlists`, `get_mixes`, `get_explore`, `get_similar_artists`,
  `get_track_credits`, `get_album_info`, `get_artist_info`, `get_other_versions`), playlists
  (`reorder_playlist`, `dedupe_playlist`, `compare_playlists`, `match_tracks`,
  `create_playlist_from_tracks`, `get_playlist_suggestions`) and context (`get_current_lyric_line`,
  `get_signal_path`, `get_listening_stats`, smart playlists, `get_party_status`). Queue, playlist,
  album and mix tools report `track_count` and `total_duration_seconds`; `get_queue` also gives
  `remaining_seconds`, `get_now_playing` gives `remaining_seconds` and `ends_at`. `play_query`
  understands plain requests ("the new Kendrick album") and only plays when it is confident;
  `batch` runs up to 20 calls. Now-playing, the queue and `track_skipped` / `track_replayed` /
  `queue_low` events are MCP resources you can subscribe to.
- Agent safety: an optional access token, read-only mode, `confirm: true` plus an in-app
  Allow / Deny for destructive calls (no answer in 60 s denies), an activity log of everything an
  agent changed with Undo, and "Let an agent DJ", which gives an agent 10 s to refill an empty
  queue before the autoplay radio.

- Crossfade (Settings → Playback, off by default): blend the end of a track into the next, 2–12 s,
  with equal-power curves. Consecutive tracks of one album stay gapless by default, and it
  switches itself off while exclusive or bit-perfect output is on. While two tracks mix the signal
  path reads MODIFIED. Each track keeps its own ReplayGain level; skipping or seeking during a fade
  ends it at once, and pausing pauses both. The second player exists only for the length of a
  fade; with crossfade off nothing changes in playback.
- Album and playlist headers show the total length (for example "13 tracks · 1 h 1 min").
- Settings has a search box: type a word and pick the setting from the list to jump to its page.
- The sidebar uses one set of line icons instead of mixed symbols; icon-only buttons in the player
  bar show a tooltip on hover, and buttons show a focus ring and can be used with Tab, Space and
  Enter.
- Empty playlists show a short "Nothing here yet" message instead of a blank page, and the UI
  now has one shared set of spacing, radius and text sizes (`Metrics`) for new work.

### Fixed
- Track-page header: the extra buttons (export, remove duplicates, sort, save as TIDAL playlist,
  other versions) no longer run off the edge of a narrow window; they are in a "⋯" menu next to
  Play, Shuffle and Save.
- The sidebar's "Smart" section sits below the playlists instead of mislabeling them.
- "Add to playlist" (and Compare, Move to folder) showed gaps and overlapping text when you had
  folders: the lists now contain only what can be picked.
- Suggested tracks show their cover art and have a Hide button.
- Player bar in narrow windows: previous / play / next / shuffle / repeat get their own
  centred row and the seek bar its own line, so they never disappear; the less-used buttons
  (add to playlist, mini-player, signal path, full screen) drop out first.

## [1.1.0] - 2026-10-30

### Added
- Import: New playlist → Import from file… turns a CSV or M3U into a new playlist. Slate's
  own exports and any file with TIDAL track links need no lookups; other files (Spotify
  exports via Exportify, M3U with `#EXTINF` lines) are matched by artist and title, one
  search at a time with a pause between them, stopping if TIDAL starts rate-limiting.

### Changed
- Card pages (library albums, artists, mixes, videos, folders, Explore and search grids) only
  build the rows on screen and load covers as their rows scroll into view. Covers far from
  the viewport are dropped (at most 240 kept) and fetched again on return; resizing the
  window regroups the rows without fetching anything.
- Thumbnails are decoded at the size they are shown at (times the display's scale factor)
  instead of the CDN size: card covers 152 px, track-row covers 34 px, sidebar 40 px.
  Moving the window to a display with another scale factor decodes the visible covers again
  at the new size, from the disk cache.
- Smaller build: `image` in the core is built with JPEG, PNG and WebP only (no AVIF/EXR
  encoders or rayon), and two unused dependencies (`walkdir`, `id3`) are gone.
- Track lists collected for playback (play all, autoplay radio, search, top tracks) are
  converted without a second JSON pass; API items are parsed without copying them first.

### Fixed
- MCP playback tools (play, pause, resume, skip, seek, volume, queue, repeat, shuffle, play a
  playlist/album/artist/mix) reported success but did nothing: the app never handled them.
  `get_now_playing` and `get_queue` always came back empty. Both now work.
- Creating a playlist (New playlist, Import) and renaming a private one failed with HTTP 400:
  Slate sent the access type `PRIVATE`, which TIDAL's playlist API does not accept; it is now
  `UNLISTED`.
- The OBS overlay never showed the playing track. It now updates on every track change,
  pause and seek, with cover and stream quality.

### Performance (measured)
Same scripted session (Explore → genre page scrolled to the end, a 174-track playlist
scrolled to the end, Home), fresh launch, 1884×988, Skia/Vulkan, AMD: GPU memory 183 → 95 MiB,
resident memory 349 → 225 MiB. FemtoVG lands in the same range (84–106 MiB VRAM).

## [1.0.0] - 2026-10-29

### Added
- Update notice: once a day Slate looks at its latest GitHub release and, if a newer final
  version exists, shows "Slate X.Y.Z is available" with a button that opens the release page.
  It never downloads or installs anything. Settings → General → Check for updates (on by
  default); closing the notice remembers that version.
- Export: playlists and Loved Tracks have an Export… button (CSV or M3U), with a save dialog,
  progress, and pages not yet loaded fetched one at a time.
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
