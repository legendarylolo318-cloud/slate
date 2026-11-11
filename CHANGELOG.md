# Changelog

## [2.0.1] - 2026-11-10

Bug fixes only. Audio output, playback and memory use are unchanged. Details in REVIEW.md
#165–172.

### Fixed
- Sign-in and token refresh: when TIDAL's sign-in server rate-limited a token refresh, every
  request that was waiting for that refresh sent its own, each one rate-limited again. Now one
  refresh is sent and the rest wait out the cooldown. The same cooldown now covers the auth
  server, not just the API.
- Device-code sign-in kept polling at the same pace after TIDAL asked it to slow down.
- MCP `create_playlist` with `track_ids`: the tracks were added the moment the playlist
  existed, when TIDAL often does not know it yet, so the agent got an error for a playlist that
  had been made and could create it again. It now waits for the playlist as the app's own
  New playlist does, adds long lists in batches of 50, and reports a failed add as a warning
  beside the new playlist's id.
- A notice (the small line above the player bar) shown within 3 s of another disappeared early.
- `ui.json` (theme, sizes, renderer choice, alarm flag) and the saved window geometry were
  written in place: a crash or power cut mid-write could reset every interface preference.
  They are now written atomically, and a failed `ui.json` save is logged.
- Party guests: when the host edited the queue several times quickly, each guest kept looking
  up the tracks of queues that had already been replaced.
- Starting Slate within a second of booting could panic (MPD server and MusicBrainz lookups).

## [2.0.0] - 2026-11-08

Six features, all off or dormant until used, and a smaller queue file. Audio output with
default settings is unchanged (crossfeed off adds nothing to the pipeline; gapless output is
bit-identical to the source, as in 1.9.0). The work was reviewed before release; the fixes
are under "Found in review".

### Added
- Headphone crossfeed (Settings → Playback): Off / Light / Medium / Strong, through GStreamer's
  `bs2b`. Off is the default and then nothing is added to the audio path. A level change applies
  at once; switching it on from Off applies from the next track (which then starts its own
  pipeline instead of following on gaplessly). Off while exclusive or bit-perfect output is on.
  The Signal Path lists it and reads MODIFIED. The row hides itself, with a sentence, where
  GStreamer has no `bs2b`; `slate --self-test` says whether it is there.
- Queue "⋯" → Copy queue link: the queue (at most 500 tracks; the notice says when it was cut)
  as a `slate://queue/…` link. Opening one asks Replace queue / Add to queue / Cancel and skips
  tracks that are unavailable or hidden by your content filter, with a count. Links are read as
  hostile: at most 16 KiB, 64 KiB unpacked, numbers only for tracks, a version check.
- Remember volume and equalizer per output device (Settings → General, off by default). The
  device is looked at when a track starts and when you pick an exclusive device, never on a
  timer; a device plugged in mid-track is noticed at the next track. Linux: the PipeWire /
  PulseAudio default sink, or the exclusive ALSA device. Windows: the exclusive WASAPI device
  only (shared mode has no device name to go by yet). At most 50 devices, kept in the encrypted
  state folder (`devices.json`), deleted at sign-out.
- Wake-up alarm (Settings → General): a time, days of the week, what to play (the queue, a
  playlist, album or mix from a link or from what is playing), a 30 s–2 min fade-in from silence
  to a chosen volume, Snooze 10 min from the bar or by pausing while it rings. Nothing runs while
  no alarm is set; while one is, a timer looks at the wall clock once a minute. An alarm missed
  by more than 5 minutes (Slate closed, the computer asleep) is skipped with a notice. Optional
  "keep the computer awake" (off by default; it also keeps the screen on). `alarm.json` in the
  encrypted state folder, deleted at sign-out.
- Import from Spotify or Apple Music (Settings → General): Spotify's `YourLibrary.json` and
  `Playlist*.json`, Exportify CSV, Apple Music `Library.xml` and Apple's privacy-export CSV.
  Tracks are matched by ISRC, then title, artist and length (±3 s), about three a second, and
  you review matched / uncertain / not found per playlist before anything is created as an
  unlisted playlist. Up to 50 MB, 200 playlists and 20,000 tracks per import; cancellable.
- Accessibility: Reduce motion (Settings → Appearance: Off / On / Follow system, where "Follow
  system" reads GNOME's animations switch on Linux; Windows does not follow the system yet), a
  High Contrast theme preset (yellow on black, every text pair above 7:1), and screen-reader
  names on every icon-only button.

### Changed
- The queue file stores the playing source's tracks as references to the same tracks in the
  queue (format version 2): a 9,000-track queue file is half the size (6.0 → 3.1 MB before
  encryption) and loads about 40 % faster. Older files still load. A queue file written by 2.0
  read by 1.9 or older loses the source's track list (used for repeat-all), not the queue.
- Theme presets: error red on light themes, and the text colour on mid-tone accent colours,
  now pass WCAG AA (a test holds every preset to it).
- Binary about 2.2 MB larger (flate2, roxmltree, gstreamer-base).

### Fixed
- Older bugs found in this review: the equalizer and Signal Path since 1.3 (the panel dropped
  the equalizer after a gapless track change, and switching the equalizer on during an album
  never took effect until playback restarted), and long exclusive-device names drawn over
  each other in Settings → Playback (since 1.x).

### Found in review before release
Each was fixed in its own commit; details in REVIEW.md #151–164.
- The alarm's timer ran on a clock that stops while the computer is suspended: after a
  suspend it fired hours late and the ring was then skipped as missed (a laptop asleep from
  23:30 to 06:50 would not have rung at 07:00). It now checks the wall clock once a minute.
- Windows: across a daylight-saving change the alarm rang an hour early or late.
- An alarm time like "1é1" crashed the settings task instead of being refused.
- Crossfeed: never shipped in the Windows bundle (the plugin was not on its list); switching
  it on during an album did not apply until playback restarted; the Signal Path dropped it
  after a gapless track change. The self-test now says whether each bundle has `bs2b`.
- Import: a hostile file under the 50 MB limit used 0.8–1.2 GB of memory before the 20,000
  track limit was checked (48 MB of empty Spotify entries, a 50 MB line of CSV commas). Now
  refused at about 110 MB, the file itself.
- Sign-out did not end an import or queue link begun before it: matching could run on at the
  login screen and its review be offered to the next account; a waiting link survived.
  Sign-out also left the alarm flag set for the next account.
- Closing the import review or the queue-link dialog kept what it held in memory.
- A queue link's "Add to queue" put its tracks ahead of the rest of the playing playlist
  instead of after everything queued, as the dialog says.
- Per-device memory restored a device's equalizer without showing it on the equalizer page.
- A queue-link test said it checked deep nesting but never reached the parser.

### Performance (measured)
Measured on this machine (24 cores, AMD Radeon RX 9070 with amdgpu, PipeWire 1.6.10, GStreamer
1.28.8) with the 1.9 method: release builds of 1.9.0, 2.0 as handed over (`6117841`) and the
final tree, each started 12 s before a 60 s window, on a copy of a signed-in profile reset to
default settings, with its own `XDG_RUNTIME_DIR`, headless KWin with a nested Xwayland,
Skia/Vulkan. Playing = TOTO "Africa" (539204), FLAC 24-bit/192 kHz, into a null PipeWire sink.
Interleaved runs: two idle, four playing. VRAM is `drm-memory-vram` from amdgpu's fdinfo; it
reads about 137.0 or 138.4 MB from run to run for every build, so no VRAM difference is claimed.

| Default settings | 1.9.0 | 2.0 as handed over | 2.0.0 |
|---|---|---|---|
| Idle: threads, open files, wakeups/s, CPU ticks in 60 s | 21, 54, 0, 0 (both runs) | 21, 54, 0, 0 | 21, 54, 0, 0 |
| Idle: anonymous / total PSS | 57.4 / 121.7, 56.3 / 121.2 MB | 56.6 / 121.8, 56.9 / 122.4 MB | 56.3 / 121.6, 56.1 / 121.7 MB |
| Playing: threads (start / end of window), open files | 42 / 38 (one run 43), 73 / 72 | 42 / 38, 73 / 72 | 42 / 38, 73 / 72 |
| Playing: wakeups/s | 336, 282, 270, 307 | 263, 292, 284, 282 | 273, 283, 290, 297 |
| Playing: CPU ticks in 60 s | 84, 83, 95, 75 | 89, 82, 80, 85 | 80, 83, 85, 85 |
| Playing: anonymous PSS | 102.2, 102.9, 105.7, 108.4 MB | 107.4, 102.6, 107.4, 107.6 MB | 104.5, 110.7, 107.3, 106.6 MB |
| Playing: total PSS | 174.5, 175.2, 178.1, 180.3 MB | 180.8, 175.4, 180.3, 180.4 MB | 177.0, 183.6, 180.1, 179.3 MB |
| Window mapped (7 launches ×2, median and range) | 299 (297–302), 295 (293–301) ms | 297 (289–304), 301 (293–305) ms | 297 (296–304), 298 (296–310) ms |
| Binary | 73,639,192 B | 75,831,192 B | 75,899,672 B |
| 9,000-track queue restored / after shuffle, two edits, three undos: anonymous PSS | 68.1 / 144.5 MB | 68.4 / 87.0 MB | 69.3 / 125.7 MB |
| Hidden to the tray and restored (idle and playing): VRAM; restore time | unchanged; 47–50 ms | unchanged; 48–51 ms | unchanged; 46–51 ms |
| Track start (MPD stop → play → stream running), six starts | 141, 231, 137, 127, 138, 146 ms | 167, 140, 133, 244, 131, 132 ms | 134, 136, 133, 172, 137, 143 ms |
| Pause / resume (MPD → stream corked / running), seek (MPD → new position) | 0–2 / 0–3 / 2–4 ms | 0–1 / 1–3 / 2–3 ms | 0–2 / 0–3 / 2–3 ms |

Default settings cost nothing new: same idle threads and open files, no idle wakeups or CPU,
and no new timer, thread or file read at start-up (each new spawn and timer was read: the alarm
timer exists only while an alarm is armed, device memory and crossfeed only when switched on).
While playing, wakeups, CPU and threads are inside run-to-run spread; anonymous memory is
2–3 MB higher in the medians (104.3 → 107.0 MB), inside the spread of single runs (the ranges
overlap); the binary is 2.2 MB larger (flate2, roxmltree, gstreamer-base).

Per thread while playing (wakeups/s, final build, same in all three): PipeWire `threaded-ml`
52–58, `souphttpsrc` 40–81 and its pad task 19–31, GStreamer queues and multiqueues about 100,
the tokio runtime 17–18, the UI thread 11–14, the Vulkan swapchain threads 16, MPRIS 2.

- P7, the queue file with a source's tracks as references: kept. 9,000 tracks (the source
  holding all of them), best of 5, three interleaved runs: 6,032,079 → 3,060,025 bytes;
  writing 3.05–3.83 ms before vs 3.35–3.76 ms after (the same); reading 24.8, 24.8, 24.8 ms vs
  14.5, 14.7, 14.6 ms. The 9,000-track queue in the app restores at the same memory (68–69 MB);
  after edits and undos it varies between runs, as in 1.9.0.
- Throttled connection (`thrott_proxy.py`), final build: 24/192 at 1 MB/s (66.3 MB through the
  proxy in 69.5 s) and 16/44 at 250 kB/s each played 68.9 s of audio in 70 s with no buffering
  drop after the start. The null sink reports no underruns itself; the position kept pace.
- Gapless: a 24/192 and a 16/44 pair through the branch pipeline's elements (crossfeed off adds
  none) is bit-identical to the source (md5).
- Control latency stays far under 100 ms; restore from the tray shows the current track and
  position (checked on screen). Restore and track-start times are measured from a separate
  process and include its own start-up; they are for comparing the columns, and differ from the
  1.9.0 notes' method.

### Considered, not done
- P1–P6 (the brief's other performance items): not started in the handed-over work, so
  there is nothing to measure or keep; only P7 (the queue file) was implemented.
- An Apple `Library.xml` at the 2,000,000-node cap peaks at 157 MB while it is read
  (roxmltree's tree). A real 20,000-track library is about 600,000 nodes; the cap was left.
- Following a device change mid-track would need a sink-change notification Slate does not
  have in shared mode; the device is looked at when a track starts instead.

### Not tested before release
- Windows and macOS: not run here. The Windows-only alarm offset (#152) and the `bs2b`
  bundling are built and self-tested by CI only; Windows shared-mode output has no device
  name, so per-device memory works there only in exclusive mode; "Follow system" for reduce
  motion does not read the Windows setting.
- A real suspend and resume with an alarm armed (not possible on this machine); the wall-clock
  wait is unit-tested and an alarm across an app restart and a missed alarm were checked live.
- A real output-device change: simulated with a `pactl` stand-in that reports another
  default sink (switching this machine's real default output was not acceptable).
- Signal Path PRISTINE in exclusive / bit-perfect mode needs a real DAC (ALSA's `null` device
  has no clock); that path's code is unchanged since 1.7.0 apart from crossfeed being kept
  out of it.
- The Spotify / Apple import through its file dialog (it opens on the desktop portal, not the
  headless display) and the matcher against real TIDAL search results; the readers were run on
  real hostile files, the matcher in unit tests.
- Reduce motion was checked by reading every animation (all go through one setting) and
  switching it on live, not frame by frame on every page.
- A 2.0 queue file read by 1.9 or older loses the source's track list (repeat-all), not the
  queue; not run.
- The crossfade incoming player's Signal Path shows no decoded format (known since 1.3; not
  changed here).

## [1.9.0] - 2026-11-08

The reviewed release of the 1.8 work: six features, all off or dormant until used, and two
memory and size changes. 1.8.0 was published from the same work before its review; 1.9.0 is
that work with the fixes below. Audio output is unchanged.

### Added
- Resume points: a track of 20 minutes or more that you leave part-way (skip, another track,
  Stop from MPRIS / MPD / MCP, quit) offers "Resume at mm:ss" when it starts again. One click
  seeks; nothing seeks by itself. The offer goes after 20 s of playing or on any seek. Leaving
  in the first minute keeps an older point, leaving in the last minute drops it. At most 500
  tracks, kept in the encrypted state folder (`resume.json`), deleted at sign-out.
- Queue "⋯" → Fill queue to… 15 / 30 / 45 / 60 / 90 minutes or Custom (1–600): adds tracks
  from the playing album or playlist that have not been queued or played, else from the
  playing track's radio, after everything already queued. One undo step.
- Settings → General → Personal data: export the lyrics timing offsets and tags to one JSON
  file, and import one. Import adds and never removes; text is cleaned like typed tags, sizes
  are bounded, other files are refused with a sentence.
- Compact now-playing strip: right-click the mini-player button → Compact strip. A 380×46
  frameless window (title – artist, progress line, play / pause, next, close), kept on top on
  X11 and Windows; on Wayland a normal window, and a notice says why.
- Party host: "Requests per guest" (Off / 1 / 2 / 3 / 5 / 10 for the whole party; a request you
  turn down does not count) and "My picks play first". Enforced by the host with the existing
  refusal message, so guests on 1.7 and older see it; no protocol change.
- Share card themes: right-click → Share card theme → Dark, Light, Album colour, Minimal. The
  last choice is kept (`share_theme` in `ui.json`) and used by Copy share card.

### Changed
- Linux (glibc): malloc arenas capped at 2 (`MALLOC_ARENA_MAX` still wins): about 7 MB less
  memory while playing and 8 MB idle.
- Release builds use `lto = "fat"`: the binary is 6.6 % smaller (5.2 MB); release builds take
  about twice as long (10 min 50 s here; the Windows release job 38 min instead of about 17).
- The queue file is written about 4.5 times faster (9,000 tracks: 13.6–14.0 ms → 3.0–3.3 ms,
  same bytes), on the worker thread that writes it.

### Fixed
- Mini player (since 0.1): the expand button left the mini window on screen, detached, and the
  next open made a second one.

### Found in review before release
Each was fixed in its own commit; details in REVIEW.md #144–150.
- The compact strip could not be closed: its close button and the menu toggle left the window
  on screen (no longer updated), and every reopen added another window and its open files.
  Closing it from the window manager made the next toggle do nothing.
- Fill queue put its tracks in "next up", ahead of the rest of the album or playlist.
- Importing a large or hostile personal-data file took time growing with the square of its
  tags: a 9.4 MB file of 800,000 tags would have taken over an hour with the Tags page frozen.
  Now linear, with at most 32 tags added per track and 1,000 different tags; that file takes
  under a second.
- Resume points were not shut during sign-out: a long track left while signing out could put
  the old account's `resume.json` back after it was deleted (found by reading; not reproduced).
- With "My picks play first", MPD and MCP clients were told the queue order before it was
  applied.
- Two quick lyrics-offset nudges could report their values out of order, leaving the older
  offset on screen (the personal-data change had moved the report outside the file lock).

### Performance (measured)
Measured on this machine (24 cores, AMD Radeon RX 9070 with amdgpu, PipeWire, GStreamer 1.28.8)
with the 1.7 method: release builds of 1.7.0, 1.8.0 as handed over (`c3dad4e`, the published
1.8.0) and 1.9.0, each started 12 s before a 60 s window, on a copy of a signed-in profile reset
to default settings, headless KWin with a nested Xwayland, Skia/Vulkan. Playing = TOTO "Africa"
(539204), FLAC 24-bit/192 kHz, into a null PipeWire sink. Interleaved runs: two idle, four
playing. VRAM is `drm-memory-vram` from amdgpu's fdinfo; on this display it reads 110.3 or
138.3 MB from run to run for every build, so no VRAM difference is claimed.

| Default settings | 1.7.0 | 1.8.0 as handed over | 1.9.0 |
|---|---|---|---|
| Idle: threads, wakeups/s, CPU ticks in 60 s | 21, 0, 0 (both runs) | 21, 0, 0 | 21, 0, 0 |
| Idle: anonymous / total PSS | 63.2 / 131.2, 62.0 / 129.5 MB | 55.9 / 122.4, 54.6 / 121.1 MB | 54.7 / 121.5, 54.1 / 120.4 MB |
| Playing: threads (start / end of window) | 43 / 38, 42 / 38 | 42 / 38 | 42 / 38 |
| Playing: wakeups/s | 293, 301, 301, 272 | 298, 298, 301, 296 | 287, 291, 282, 269 |
| Playing: CPU ticks in 60 s | 82, 88, 81, 84 | 85, 88, 82, 79 | 81, 91, 89, 94 |
| Playing: anonymous PSS | 110.7, 111.7, 112.3, 111.6 MB | 107.5, 101.9, 104.6, 104.7 MB | 105.8, 104.5, 104.8, 109.6 MB |
| Playing: total PSS | 187.7, 187.8, 185.0, 188.4 MB | 182.1, 176.2, 178.8, 179.0 MB | 180.3, 179.0, 179.6, 184.5 MB |
| Window mapped (7 launches ×2, median and range) | 302 (295–314), 306 (290–318) ms | 304 (298–311), 306 (301–312) ms | 304 (303–312), 303 (296–308) ms |
| Binary | 78,180,632 B | 73,632,536 B | 73,639,192 B |
| 9,000-track queue restored / after shuffle, two edits, three undos: anonymous PSS | 73.4 / 119.1, 72.3 / 117.4 MB | 67.6 / 108.1, 68.3 / 143.0 MB | 67.9 / 145.0, 69.2 / 105.1 MB |
| VRAM shown / hidden in the tray / after restore, idle and playing; restore time | 84.4 / 84.4 / 84.4, 84.3 / 84.3 / 84.3 MB; 3–4 ms | 85.8 / 85.8 / 85.8, 84.3 / 84.3 / 84.3 MB; 4–7 ms | 84.4 / 84.4 / 84.4, 84.3 / 84.3 / 84.3 MB; 4 ms |
| Track start (MPD play → audio running), six starts | 615, 627, 629, 639, 611, 632 ms | 613, 789, 754, 613, 606, 620 ms | 623, 636, 740, 625, 613, 615 ms (separate run) |

Default settings cost nothing new: same idle threads, no idle wakeups or CPU, no new file read,
timer or thread at start-up (checked by reading every new spawn and timer: each runs only when
a feature is used or a track of 20 minutes or more changes). Wakeups and CPU while playing are
inside run-to-run spread. Memory is lower than 1.7.0 by about 6–8 MB idle and playing (the
arena cap).

- E, malloc arenas, A/B on the same 1.9.0 binary while playing (anonymous PSS): capped
  108.3, 106.3, 107.3, 105.5 MB vs glibc's default arenas (`MALLOC_ARENA_MAX=192`) 112.8, 112.1, 118.6, 116.7 MB; wakeups and CPU the
  same. Kept.
- K, `lto = "fat"`, A/B against the same tree with thin LTO: binary 73,639,192 vs 78,819,656 B;
  playing anonymous PSS 105.8, 104.5, 104.8, 109.6 vs 107.5, 104.5, 106.9, 116.4 MB and idle
  total 121.5, 120.4 vs 121.0, 123.5 MB (inside noise); CPU and wakeups the same. The size
  win reproduces, the memory win does not clearly. Kept for the size, at the build-time cost
  above.
- D, queue file writer: 9,000 tracks, 6,032,079 bytes either way, best of 5 per run: 13.6, 14.0,
  13.7 ms before vs 3.0, 3.3, 3.1 ms after. Kept.
- Pause and resume reach PipeWire (stream corked / uncorked) in about 20 ms and a seek lands in
  about 30 ms, the same as 1.7.0; no redraw artefacts or tearing were seen in the screenshots
  taken during the review.
- The 9,000-track queue after edits and undos varies between runs (105–145 MB for both 1.8.0 and
  1.9.0); restored it is 5 MB below 1.7.0 (the arena cap). The edits and undos themselves did not
  change (only the file writer did).
- VRAM does not change when the window goes to the tray (no texture is freed while hidden, as
  in 1.7.0); restoring from the tray takes under 10 ms.

### Considered, not done
- A (sink buffer / PipeWire quantum), B (fewer GStreamer queues), C (Skia partial rendering),
  F (lazy CJK / fallback UI fonts), H (Skia GPU cache and glyph-atlas caps), I (freeing
  textures while minimised), J (two swapchain images), L (prefetching the next stream URL):
  not attempted. C, F, H, I and J have no setting or hook in Slint 1.18 / fontique and would
  need a patched renderer; A and B change the audio path and need the gapless and latency
  checks first; L is partly there already (the next track is armed before the end). G (no
  party objects before a party) needed no change: an idle build has no UDP socket and no party
  thread.
- D part 2, storing the playing source's tracks as references into the queue (the file is
  about half duplicates): changes the file format; not started.
- `MALLOC_ARENA_MAX=4`: inconsistent between runs in the handed-over measurements; 2 kept.

### Not tested before release
- A per-thread wakeup breakdown was not re-taken (stopped to ship sooner); total wakeups while
  playing are in the table, and no change targets wakeups.
- Windows and macOS: not built in this review; the release workflow builds and self-tests
  Windows (fat LTO on Windows builds: the 1.8.0 release job passed with it).
- Signal Path PRISTINE in exclusive / bit-perfect mode: not checkable here without a real DAC
  (on ALSA's `null` device the writer has no clock and races through the track, in 1.7.0 and
  1.9.0 alike); that code is unchanged since 1.7.0. Gapless was checked: a 24/192 and a 16/44
  pair through the branch pipeline's elements is bit-identical to the source (md5).
- Personal-data export / import through the file dialogs (they open on the desktop portal,
  not the headless display); the code behind them was run on real hostile files (16 MB+,
  binary, 100,000-deep nesting, wrong schema, bidi and CJK text).
- The strip's always-on-top and dragging on a real X11 window manager, Windows and Wayland.
- Share cards: a **bold** Arabic title is still drawn as boxes on every theme (known since 1.6).
- The party request limit counts guests by name: a guest who rejoins under another name gets
  a new allowance.

## [1.8.0] - 2026-11-07

### Added
- Resume points: tracks of 20 minutes or more offer "Resume at mm:ss" when started again after you left them part-way.
- Queue menu → Fill queue to… 15 / 30 / 45 / 60 / 90 minutes or a custom length (one undo step).
- Settings → General → Personal data: export / import lyrics offsets and tags as one file (import merges).
- Compact now-playing strip (right-click the mini-player button); kept on top on X11 and Windows, a normal window on Wayland.
- Party host: requests per guest for the whole party, and "My picks play first". No protocol change.
- Share card themes: Dark, Light, Album colour, Minimal.

### Changed
- Linux: glibc malloc arenas capped at 2 (about 8 MB less memory while playing).
- Release builds use `lto = "fat"`: binary 6.6 % smaller, release build takes about twice as long.
- Writing the queue file is about 4x faster (9,000 tracks: 13.7 ms to 3.0 ms).

Published from the handed-over tree before its review. Use 1.9.0: it is the same work after
review, with eight fixes (see "Found in review" under 1.9.0), including a strip window that
could not be closed.

## [1.7.0] - 2026-11-07

A resource release: less memory for long queues and while hi-res plays, with no change to audio output or
to anything you see. No new settings or features.

### Changed
- The queue keeps a compact record of each track (the fields Slate reads, with artists, albums and repeated
  strings shared) instead of the whole TIDAL object, and the queue, history, shuffle order, undo states and
  playing source share one copy of each track. A 9,000-track queue now takes about 75 MB of memory instead
  of about 410 MB, and editing and undoing it no longer copies it (1.6.1 grew past 1.2 GB after a shuffle,
  two edits and three undos).
- While a gapless stream plays, 3 s of decoded audio is held ahead (was 15 s); the rest of the lookahead is
  kept compressed, so the total time buffered ahead is unchanged (20 s progressive, 30 s DASH). The
  exclusive / bit-perfect output path is unchanged.
- Covers without transparency are kept as RGB (3 bytes a pixel instead of 4), so the 64 MB cover cache
  holds a third more; a smaller size of a cover already decoded at full resolution is cut from it instead of
  being read and decoded again.
- The background worker pool keeps at most 16 threads and lets idle ones go after 2 s (was 512 and 10 s).
- The queue file is written 1 s after the last change, on a worker thread, instead of after every change on
  the spot (a quit still writes it straight away). A crash or `kill -9` can lose the last second of queue
  changes.

### Fixed
- `cargo check --no-default-features` (a build without the listening party) failed since 1.5.0.

### Found in review before release
Each was fixed in its own commit; details in REVIEW.md #139–142.
- Signing out within about a second of a queue change could put the old account's queue file back on disk
  after sign-out deleted it, and the next sign-in would restore it (from the delayed queue save).
- A cover size cut from a cover that had itself been shrunk was resampled twice and came out softer than
  1.6's, depending on the order covers were requested and the interface scale.
- Every MPD `status` poll copied every track in the queue.
- The full-screen lyrics backdrop was moved onto a cached layer; measured, it cost one window-sized
  texture of VRAM (about 4.5 MB here) and saved no CPU, so it was reverted.

### Performance (measured)
Measured on this machine (24 cores, AMD Radeon RX 9070 with amdgpu, PipeWire 1.6.10, GStreamer 1.28.8),
with the 1.6 method: release builds of 1.6.1, 1.7.0 as handed to review (`065396b`) and 1.7.0 before the
lyrics revert, each started 12 s before a 60 s window, on a copy of a signed-in profile reset to default
settings (only the sign-in kept), headless KWin with a nested Xwayland, Skia/Vulkan. Playing = TOTO
"Africa" (track 539204), FLAC 24-bit/192 kHz (DASH), into a null PipeWire sink at s24le/192 kHz. Wakeups
are voluntary context switches over all threads. Two interleaved runs each. VRAM is the process's
`drm-memory-vram` from amdgpu's fdinfo. A different machine from the 1.6 notes: compare the columns, not
with 1.6's numbers. The last variant build was linking on one core during the first runs.

| Default settings | 1.6.1 | 1.7.0 as handed over | 1.7.0 |
|---|---|---|---|
| Idle main screen: threads, wakeups/s, CPU ticks in 60 s | 21, 0, 0 | 21, 0, 0–1 | 21, 0, 0 |
| Idle main screen: anonymous / total PSS | 61.9 / 132.7, 64.0 / 135.5 MB | 62.9 / 134.1, 65.9 / 136.9 MB | 63.1 / 134.3, 64.2 / 135.7 MB |
| Playing: threads (start / end of window) | 43 / 39, 43 / 39 | 42 / 38, 42 / 38 | 43 / 38, 42 / 38 |
| Playing: wakeups/s | 281, 258 | 276, 274 | 279, 240 |
| Playing: CPU ticks in 60 s | 97, 113 | 95, 141 | 167, 129 |
| Playing: anonymous / total PSS | 114.6 / 194.4, 110.5 / 190.3 MB | 117.1 / 197.3, 106.0 / 185.9 MB | 115.8 / 196.2, 117.5 / 197.9 MB |
| VRAM idle / playing | 102.6–103.9 / 103.8 MB | 102.6–106.5 / 103.8 MB | 102.6–103.9 / 103.8–106.4 MB |
| Window mapped (7 launches, median and range) | 300 ms (294–311) | 296 ms (290–305) | 300 ms (290–305) |
| Binary | 78,158,232 B | 78,181,912 B | 78,180,888 B |
| 9,000-track queue restored: anonymous PSS | 409.2, 409.0 MB | 76.8, 77.2 MB | 74.6, 75.1 MB |
| …after shuffle on, two edits (MPD) and three Ctrl+Z | 1,672.8, 1,290.6 MB | 122.8, 122.7 MB | 115.8, 120.4 MB |

The big queue is a 1.6-format `queue.json` of 9,000 full TIDAL-shaped track objects restored at start-up
(this account has no playlist that large); the edits went through MPD, the undos through the keyboard,
and the queue state was checked after them (shuffle back off, the deleted track back).

Default settings are within run-to-run noise of 1.6.1: same idle threads, no idle wakeups, playing memory
and wakeups inside the spread of repeated runs, the same window-mapped time and VRAM. CPU ticks while
playing vary from run to run (95–167 over all builds) with no pattern between them.

Decoded reservoir, A/B in the app (1.7.0 vs the same tree with the reservoir change reverted), playing,
30 s windows, two runs each, anonymous PSS:

| | 1.7.0 | Reservoir reverted |
|---|---|---|
| 24/192 (DASH), fast connection | 106.4, 108.3 MB | 112.4, 106.2 MB |
| 16/44 (progressive), fast connection | 70.1, 71.1 MB | 70.7, 71.6 MB |
| 24/192 through a proxy throttled to 1 MB/s | 84.7, 84.8 MB | 94.5, 94.1 MB |
| 16/44 through a proxy throttled to 250 kB/s | 68.9, 69.0 MB | 69.0, 69.5 MB |

Wakeups and CPU are the same in every pair. Audio: a gapless pair (24/192 and 16/44, split mid-block)
decoded through the gapless branch pipeline with the 1.6 and the 1.7 buffer settings is bit-identical to
the source with no gap (md5 of the joined output equals the original). A `gst-launch` stand-in of a
*progressive* 24/192 stream on a throttled connection held more with the 1.7 settings (58 vs 32 MB); the
app's 24/192 streams are DASH and measured lower, as above.

### Considered, not done
- Burst download of the next track to a temporary file (handed over as not done): no measured wakeup
  saving (a file source woke as often as the HTTP source in the stand-in), it would need its own copy of
  the proxy and fail-closed routing, and it would put whole decrypted audio files on disk. Not done.
- Full-screen lyrics backdrop on a cached layer: reverted (see Found in review). With lyrics open, VRAM
  108.3 / 108.3 MB with it vs 103.8 / 106.4 MB without, CPU ticks 77 / 74 vs 73 / 78.
- mimalloc as the global allocator: anonymous PSS idle 93.6 / 92.7 MB vs 65.3 / 63.7 MB, playing 140.0 /
  142.4 MB vs 109.9 / 108.2 MB, CPU the same. About 30 MB more; not kept.
- Release profile `lto = "fat"` with `panic = "abort"`: binary 63,218,072 B vs 78,180,888 B (−19 %), total
  PSS about 7 MB lower (the smaller binary), anonymous memory and CPU the same. Not kept for this release:
  with `panic = "abort"` a panic in any background task (a cover decode, a pipeline teardown), which today
  fails that task alone, would end the whole app, and the two were not measured apart. `lto = "fat"` on its
  own is worth its own A/B.

### Not tested before release
- Windows and macOS: not built in this review; the release workflow builds and self-tests Windows.
- Underruns on a slow connection were not observed directly (the null sink does not report them); memory,
  wakeups and CPU through the throttled proxy were the same as with the 1.6 buffer sizes.
- A party between a 1.6.1 and a 1.7.0 instance was not run: the party messages carry track ids, not queue
  tracks, and `party/` did not change (a test pins the wire format).
- Signal Path in exclusive / bit-perfect mode was not checked live (that path's code is unchanged).
- The cover change at interface scale 1.5 and 2 in the running app (unit tests only); a queue file written
  by 1.7 loaded by 1.6.1 (downgrade).
- Old named queue snapshots, `tags.json` and party sets from a real 1.6 profile (a synthetic 1.6 queue file
  is tested).

## [1.6.1] - 2026-11-05

One change on top of 1.6.0; nothing else in the app or the audio path changed.

### Added
- Start at half volume (Settings → General, **on by default**): every launch starts at 50 % volume
  instead of the last level, set once at start-up before anything plays. Changing the volume afterwards
  (slider, keys, MPRIS, MPD) works as before for the rest of the session. Turn it off to keep the last
  level as in 1.6.0 and earlier.

### Notes
- Settings key `start_half_volume` in `ui.json`; a `ui.json` written before it existed counts as on.

### Not tested before release
- Windows: built and self-tested by the release workflow, not used on a Windows desktop.
- A volume set through MPRIS (`playerctl volume`) was not kept across a restart in one test, in 1.6.0
  as well; not investigated (the slider and keys are the normal path).

## [1.6.0] - 2026-11-05

Resource rule for the whole release: with default settings nothing new is allocated, spawned, polled or
redrawn. Every feature below is off or idle until it is used: the party set is kept only while hosting,
the share card and the lyrics backdrop exist only while they are made, `tags.json` is read on first use,
and the undo stack holds shared track handles only. Audio path, buffers, gapless and bit-perfect/exclusive
behaviour are unchanged.

### Added
- Save party set: while hosting, the tracks that played are kept in memory (a track counts once it has
  played 30 s or half its length, so tracks skipped through do not; the track already playing when the
  party starts counts too; at most 500, no repeats, no videos or tracks hidden by the content filter,
  nothing for guests). "End party" offers "Save set as playlist" (default name "Party – <date>"), and the
  Party tab offers it until the next party starts or you sign out. It makes an unlisted TIDAL playlist
  through the existing playlist code.
- Share card: right-click a track, a queue row or the now-playing area → "Copy share card". A 600×300 PNG
  (cached cover, title, artist, album, `https://tidal.com/browse/track/<id>`) is drawn on a worker thread
  in about a quarter of a second and copied as an image; then every buffer is dropped. The cover is the
  cached thumbnail only, nothing is fetched. Only the fonts the card needs are loaded, found through the
  system font index. Emoji are drawn in colour; Japanese, Chinese, Korean and Hebrew use the system's
  font for that script; right-to-left titles are right-aligned; long titles shrink and end in "…".
  Without image clipboard support the PNG is saved in the Pictures folder and the notice says where.
- Full-screen lyrics: a button on the lyrics panel and a shortcut (default F11, rebindable; also works
  from the full-screen player, Esc goes back). Big centred lyrics, current line highlighted, over the cover
  blurred and darkened once per track from a thumbnail of at most 128 px on a worker thread. The only
  redraw trigger is a line change (a 280 ms slide). "No lyrics" state; follows the lyrics size setting;
  click a line to seek.
- Personal tags: right-click track(s) → "Tags…" (works on a multi-selection) adds or removes free-text tags
  with autocomplete from your existing ones. A "Tags" sidebar entry lists them with counts; a tag opens as
  a track page (Play, Shuffle, queue actions, Ctrl+F filter) that needs no API call. Tags can be renamed
  (renaming to an existing tag merges them) or deleted from the sidebar.
- Queue undo / redo: Ctrl+Z and Ctrl+Shift+Z (rebindable) and ↶ ↷ buttons in the queue panel step through
  your edits: remove, clear, move, shuffle on/off, add, play next, load snapshot, DJ this playlist. The last
  20 states are kept; a new edit clears redo; edits within 0.6 s count as one step. Undo never interrupts
  playback and never brings back a track that has already played; tracks that arrived meanwhile (autoplay,
  party requests, DJ blocks) stay. Playing a new album or playlist, or autoplay starting a radio, starts a
  new history.

### Changed
- "Copy TIDAL link" is now "Copy link" and copies `https://tidal.com/browse/track/<id>`.
- The sidebar has a Tags entry.

### Removed
- The party QR code. The `slate://party/…` link is shown as text with its Copy link button. The `qrcode`
  crate stays: the device-code sign-in still draws its QR code with it.

### Fixed
- Older bugs found in the 1.6 review: on X11 without a clipboard manager every copy (Copy link, party,
  MCP and overlay links, the sign-in link) was gone before it could be pasted (since 1.0); the text field
  of a dialog (rename, queue snapshots) never took the keyboard focus, so typing went to the field behind
  it.

### Found in review before release
Each was fixed in its own commit (#125–127 and #131–132 share one, being one code path); details in
REVIEW.md #121–138.
- The handed-over tree failed `cargo fmt --check` and `clippy -D warnings`: CI and the release workflow
  would have refused it.
- Queue undo: each Ctrl+Z kept a full copy of the queue: 85 MB per undo of a 9,408-track queue, 1.7 GB
  after 20.
- Queue undo: undoing after tracks had played put them back (and the playing one), and dropped autoplay
  tracks and guests' requests that had arrived since; undoing a shuffle toggle left the shuffle button
  (and MPRIS, MPD) saying "on".
- Sign-out kept the undo history and the party set, so the next account could undo into the previous
  one's queue or save its party.
- Tags: an unreadable `tags.json` was taken as empty and the next edit wrote over it, losing every tag.
  It is now kept as `tags.json.damaged-<time>` and you are told. Writes are ordered (two could
  interleave), owner-only like the other state files, and a quit within 0.7 s of an edit no longer
  loses it.
- Share card: one card took 12–29 s here (every system font was loaded, 10,007 faces, and font files were
  re-read for every measurement); now about 0.25 s. Emoji drew as boxes, and Hebrew, Korean and mixed
  titles came out in a monospaced or serif font.
- Full-screen lyrics: F11 from the full-screen player opened them underneath it; a track without a cover
  kept the previous track's backdrop.
- Party set: every skipped track was saved; the track playing when the party started was not.
- Tags dialog: a big gap above the field; typing went to the search box behind it, and after closing
  it (or any dialog) no shortcut worked until something was clicked.
- "1 tracks" on a tag page and under the party set.

### Notes
- New file: `tags.json` (next to `ui.json`, owner-only): `{"tracks": {"<id>": {title, artist, artist_id,
  album, album_id, cover, duration, tags: [...]}}}`. Written through a temporary file, 0.7 s after the last
  edit (and at quit), on a blocking thread. Tags are one line of at most 40 characters; control and
  bidirectional-override characters are stripped (as in party chat); "Jazz" and "jazz" are one tag.
- New shortcut actions (Settings → Shortcuts): `lyrics-fullscreen` (F11), `queue-undo` (Ctrl+Z),
  `queue-redo` (Ctrl+Shift+Z). No new settings keys.
- Dependencies: no new crate. The share card uses `resvg` (already linked by Slint) with its
  `system-fonts` and `memmap-fonts` switches, and `fontique` and `skrifa` (already built for Slint and
  resvg, same versions and features) as direct dependencies; `arboard` gets `image-data`; `dirs` moved
  from Linux-only to all platforms.
- After the first copy Slate keeps one clipboard connection for the session, so on X11 the copy stays
  pasteable until another application copies something.

### Performance (measured)
Measured in the 1.6 review on this machine (16 cores, NVIDIA, PipeWire 1.6), with the 1.5 method:
release builds of 1.5.0, 1.6.0 as handed to review (`a658880`) and the final tree, each started 12 s
before a 60 s window, on a copy of a signed-in profile reset to default settings (only the sign-in kept),
headless KWin with a nested Xwayland, Skia/Vulkan. Playing = TOTO "Africa" (track 539204), FLAC
24-bit/192 kHz, into a null PipeWire sink at s24le/192 kHz. Wakeups are voluntary context switches over
all threads. Two interleaved runs each. The numbers are not comparable with the 1.5 notes' (another
day, another profile copy); compare the columns.

| Default settings | 1.5.0 | 1.6.0 as handed over | 1.6.0 |
|---|---|---|---|
| Idle main screen: threads, wakeups/s, CPU ticks in 60 s | 21, 0.1, 0 | 21, 0.1, 0 | 21, 0.1, 0 |
| Idle main screen: anonymous / total PSS | 79.0 / 230.0, 79.0 / 230.2 MB | 78.1 / 229.7, 78.4 / 230.2 MB | 78.7 / 230.7, 77.7 / 228.9 MB |
| Playing: threads (start / end of window) | 47 / 43, 44 / 40 | 44 / 40, 45 / 41 | 44 / 40, 48 / 44 |
| Playing: wakeups/s | 442, 449 | 442, 438 | 440, 429 |
| Playing: CPU ticks in 60 s | 115, 114 | 118, 116 | 118, 115 |
| Playing: anonymous / total PSS | 131.4 / 294.2, 123.0 / 285.4 MB | 125.7 / 290.2, 120.0 / 284.5 MB | 126.2 / 290.7, 132.8 / 297.9 MB |
| Window mapped (7 launches, median and range) | 450 ms (446–457) | 460 ms (454–470) | 449 ms (446–453) |
| Binary | 76,773,512 B | 78,202,648 B | 78,121,368 B |

Default settings are within run-to-run noise of 1.5.0: same idle threads, no idle wakeups, playing memory
inside the spread of repeated runs (two more runs of the final binary: 129.3 and 130.6 MB anonymous,
437 and 438 wakeups/s, 111 and 115 ticks). The binary is 1,347,856 bytes (1.8 %) larger than 1.5.0. VRAM:
`nvidia-smi` gives no per-process figure here and the nested display hides the driver's own number, so
no VRAM figure is claimed.

| 1.6 features in use (final 1.6.0, playing) | Anonymous / total PSS | Wakeups/s | CPU ticks |
|---|---|---|---|
| None (same session type) | 122.9 / 286.6 MB | 429 | 126 |
| Share card copied | 125.6 / 290.5 MB | 444 | 125 |
| Tag added, tag page open | 124.8 / 291.2 MB | 432 | 126 |
| Shuffle toggled and undone | 124.7 / 289.7 MB | 423 | 131 |
| Full-screen lyrics open | 131.9 / 284.7 MB | 420 | 132 |
| Hosting a party (1.3 feature; the 1.6 set adds a list of ids) | 151.8 / 327.0 MB | 413 | 157 |
| All of the above at once | 163.2 / 340.3 MB | 432 | 190 |

Each new 1.6 feature is inside the +10 MB budget on its own (full-screen lyrics +9.0 MB, the most:
the renderer's full-window layers and large glyphs; the backdrop itself is one ≤128 px image). All
four together on top of a party are about +11 MB, at the edge of the budget and within the ±5 MB
spread of single runs; this was one run each, not repeated. The party's own cost (+29 MB here, 23 MB in
the 1.5 review) is not new in 1.6. CPU: unchanged except while hosting a party (ticks 157 vs 126, the
party's sync work, as in 1.5) and with everything on at once (190, one run; the full-screen lyrics view
redraws each line change at full-window size).

Where the ~430 wakeups/s while playing come from (per thread, 20 s, final 1.6.0): `souphttpsrc` 144 and
its pad task 29, GStreamer queues and multiqueues 119, PipeWire's `threaded-ml` 64, Vulkan swapchain
threads 20, the tokio runtime 17, the UI thread 15, MPRIS 2. About 360 of them are GStreamer and
PipeWire; the UI tick is 2 Hz and runs only while playing.

### Optimizations kept
- The share card's font loading (#131): 12–29 s → about 0.25 s per card, measured on seven sample cards.
  Kept as a bug fix, not as a tweak.

### Considered, not done
- Slowing the UI progress tick when unfocused, stopping it when minimized or in the tray: it is already
  2 Hz and only while playing, and the UI thread accounts for 15 of ~430 wakeups/s; most of those come
  from the same tick's position, lyrics, crossfade and MPRIS work, which must keep running. Not worth a
  behaviour change.
- `malloc_trim(0)` after the share card and when hiding to the tray: measured after a share card,
  131.5 vs 132.2 MB anonymous PSS (with and without), inside noise. Removed.
- A 32 KiB `blocksize` on the HTTP source (4 KiB default) to cut `souphttpsrc` wakeups: `souphttpsrc`
  still woke 154 times a second (libsoup paces on socket readiness, not on the read size); total
  420–439 vs 437–438 wakeups/s. Removed.
- Not tried in this review, no numbers: decode-once cover cache with an LRU byte budget, capping Skia's
  GPU resource cache and glyph atlas, lazy CJK/fallback fonts in the UI, `Arc<str>` track metadata and
  in-place `VecModel` updates, a ListView audit of long lists, tokio blocking-pool limits, reqwest client
  reuse and gzip, GStreamer pipeline reuse between tracks and queue limits, prefetching the next stream
  URL 20 s before the end, debouncing encrypted-settings writes during slider drags (volume already is,
  #8), checking iroh and MPD sockets are fully dormant without peers, caching the Vulkan probe per GPU and
  driver, and the release profile A/B (`lto = "fat"`, `panic = "abort"`, `opt-level`). Left for 1.7.

### Not tested before release
- Windows: not built or run in this review; the release workflow builds and self-tests it. The new
  clipboard handle, the share card's font lookup (DirectWrite through fontique) and the dialog focus
  change have not been used on Windows.
- Saving the party set as a playlist was not run (it would create a playlist on the reviewer's real
  account); the dialog, the default name and the count were checked live. A playlist API failure was
  checked by reading the code (the message is shown and the set kept), not by forcing one.
- Handing a party over to a new host, guests at all, multi-monitor, mixed DPI, a real Wayland compositor
  session (the click-through ran on X11; Wayland clipboard checked with a small test program on headless
  KWin), real clipboard managers, interface scale 1.5 and 2 in the running app (offscreen renders only),
  the mini player open with full-screen lyrics, 5,000 tagged tracks in the running app (the store was
  timed in a unit test: adding a tag to 5,000 selected tracks 1.6 ms, a 1.2 MB file in 6 ms), and long
  sessions.
- Known: a **bold** Arabic title on a share card is drawn as boxes (the artist and album lines are
  fine); resvg drops fallback glyphs when the ligature count differs. Not fixed.

## [1.5.0] - 2026-11-03

Resource rule for the whole release: with default settings nothing new is allocated, spawned or
polled. Window-state restore is the only feature on by default and works only at launch and when a
window stops moving or closes. Everything else is lazy and off by default. No crate added, the audio
path, buffers and bit-perfect/exclusive behaviour are unchanged.

### Added
- Content filter (Settings → Playback): "Hide explicit tracks" and "Hide AI-generated tracks". Filtered
  tracks are skipped by the queue, autoplay, radio and the DJ, and shown dimmed with "hidden by filter"
  in lists (lists already on screen update when reloaded). Stored in the encrypted settings. Off by default.
- Remember window size and position (Settings → General, on by default): size, position and maximized
  state of the main window and the mini player, saved after a move or resize stops (one single-shot
  timer, written off the UI thread) and on close, restored at launch. A saved position that is on no
  monitor any more puts the window in the centre of the primary one, and a size bigger than the screen
  is shrunk to fit (checked once the window exists: two single-shot timers at launch, nothing repeats).
  Sizes are stored logically, so the
  interface scale and per-monitor DPI are respected. On Wayland only size and maximized are restored
  (clients cannot set their position). Turning it off deletes `window.json`.
- Album shuffle (Settings → Playback "Shuffle mode", or right-click the shuffle button): shuffles whole
  albums and keeps each album's track order, so gapless playback works and each album plays at its album
  ReplayGain; the album that is playing carries on from the playing track, and "Shuffle play" starts an
  album at its first track. With smarter shuffle on, the same artist is avoided on
  consecutive albums. One pass when shuffling.
- MPD server (Settings → Integrations, off by default): a minimal MPD protocol server (no crate) so
  M.A.L.P., MaximumMPD, Rigelian and `mpc` can control Slate. Loopback `127.0.0.1:6600` by default;
  "Allow LAN" listens on every interface and requires a password (generated, with Copy). Queue,
  transport, volume, modes, `idle`, TIDAL playlists (`listplaylists`, `load`), TIDAL track search and
  cover art are supported; anything else gets an `ACK` error. 8 clients, 60 commands per 10 s each,
  64 KiB lines, idle connections closed after 10 minutes, TCP keepalive so a phone that leaves the
  network frees its slot within about 3 minutes. See the README, "Phone remote (MPD)".
- Party text chat: guests and host send plain-text messages of up to 200 characters (5 per person per
  10 s, counted by the existing 60-message flood rule). The Party tab has a collapsible chat panel and
  the latest message shows beside the player like reactions. The last 100 messages are kept in memory
  only and are gone when the party ends. The host can mute a guest. Protocol version stays 2: 1.3 and
  1.4 peers ignore the new message.
- DJ voice (DJ page): choose espeak-ng / say or Piper, pick a voice `.onnx` file, "Test voice". Piper is
  started only while a line is spoken (the text goes in on stdin), played through a one-off
  `paplay`/`aplay` (PowerShell on Windows), ducks the music while it talks (can be turned off) and is
  killed when a new line or the next track interrupts it. A missing binary or voice file is named on
  the page.
- DJ this playlist (right-click a playlist): the model gets the titles and artists of up to 200 tracks
  and returns a flowing order, which is queued. Returned numbers are deduplicated, out-of-range ones
  dropped and missing ones appended; any failure plays the playlist in its own order. The request runs
  without the queue lock. Shuffle is switched off for the set.

### Changed
- The DJ page's disclosure line mentions the 200 tracks "DJ this playlist" sends.

### Fixed
- Older bugs found while clicking through the app in the 1.5 review: a long settings search pushed the
  player bar off the bottom of the window (since 1.1); the queue snapshot dialog's buttons overlapped
  (1.4); long error messages spilled out of the error bar over the sidebar; after "End party" the host's
  page still said it was hosting (1.3).

### Found in review before release
Each was fixed in its own commit; details in REVIEW.md #105–120.
- MPD: a client without the password could sit in `idle` for ever (and eight of them locked every phone out);
  command lists waited 10 minutes before the password.
- MPD: switching "Allow LAN" or making a new password failed with "address in use" (every time in the test).
- MPD: a phone that left the Wi-Fi while waiting in `idle` held its connection slot for ever; connections now
  use TCP keepalive (~3 min to notice a dead peer).
- MPD: the settings-page info printed the LAN password in debug output.
- Piper: each line cut off by a track change or a new line left two zombie processes; a failure halfway left
  `piper` running; a failed start could leave the music ducked.
- Album shuffle: starting from track 5 of an album played tracks 1–4 right after it; "Shuffle play" started
  halfway through an album (both seen live on a real playlist).
- Album shuffle played at track gain; it now uses album gain, as promised.
- Window restore: the "on no monitor any more" and "bigger than the screen" checks never ran at launch (the
  window did not exist yet), so a window saved on an unplugged monitor opened off-screen; under an interface
  scale above 1 the correction was half size. Found by running it.
- Party chat and names kept bidirectional-override characters (text that reads backwards).
- "DJ this playlist" sent tracks hidden by the content filter to the model.
- A track hidden by the content filter said "Track unavailable." instead of naming the filter.
- The four older bugs under Fixed.

### Notes
- Files: `window.json` (window geometry, plain JSON next to `ui.json`), settings keys `hide_explicit`,
  `hide_ai`, `album_shuffle`, `mpd_*`, and `dj.voice` / `dj.piper_model` / `dj.no_duck`.
- The MPD playlist is the playing track followed by the queue; `clear` stops playback; `play N` leaves
  earlier entries queued; `lsinfo`, `list` and `count` answer empty (no local library).
- The MPD port has no settings UI (`mpd_port` in the settings file, default 6600).

### Performance (measured)
Measured in the 1.5 review on this machine (16 cores, NVIDIA, PipeWire 1.6): release builds of 1.4.1
(`194e36b`), 1.5.0 as handed to review (`fa47f36`) and the final tree, each started 12 s before a 60 s
window, on a copy of a signed-in profile with default settings. The window ran on a headless KWin with a
nested Xwayland (not the desktop; the 1.4.1 notes' numbers came from Hyprland), Skia/Vulkan, so compare the
columns with each other, not with older notes. Playing = TOTO "Africa", FLAC 24-bit/192 kHz, into a null
PipeWire sink at s24le/192 kHz. Wakeups are voluntary context switches over all threads.

| Default settings | 1.4.1 | 1.5.0 as handed over | 1.5.0 |
|---|---|---|---|
| Idle main screen: threads, wakeups/s, CPU ticks in 60 s | 24, 0.0, 0 | 24, 0.0, 0 | 24, 0.0, 0 |
| Idle main screen: anonymous / total PSS | 72.8 / 206.2 MB | 73.3 / 207.5 MB | 72.4 / 207.1 MB, 72.1 / 204.6 MB |
| Playing: threads (start / end of window) | 51 / 47 | 51 / 47 | 50 / 46, 51 / 47 |
| Playing: wakeups/s | 416 | 380 | 402, 393 |
| Playing: CPU ticks in 60 s | 90 | 88 | 93, 87 |
| Playing: anonymous / total PSS | 127.4 / 273.3 MB | 132.3 / 279.1 MB | 136.4 / 282.7 MB, 128.6 / 274.8 MB |
| Window mapped (7 launches, median and range) | 375 ms (137–479) | 372 ms (142–484) | 375 ms (141–476) |
| Binary | 75,277,768 B | 76,714,968 B | 76,773,384 B |

Default settings are within run-to-run noise of 1.4.1: the same thread names and counts (four extra runs
each: 45–50 threads playing for both, the difference is tokio's short-lived blocking pool), no idle
wakeups, memory inside the spread of repeated runs (anonymous PSS while playing varied 128.6–136.4 MB
between two runs of the same build). Window-state work happens only at launch, after a move/resize stops
and on close. The binary is 1,495,616 bytes (2.0 %) larger than 1.4.1. VRAM: `nvidia-smi` gives no per-process figure here
and the nested display hides the driver's own number, so no VRAM figure is claimed.

| All 1.5 features on (1.5.0) | Default settings, playing | All on, playing |
|---|---|---|
| Setup | as above | MPD on with LAN and a client connected waiting in `idle`, both content filters, album shuffle, DJ connected (mock endpoint) with the Piper voice selected |
| Threads | 46–51 | 44 / 40 |
| Wakeups/s, CPU ticks in 60 s | 393–402, 87–93 | 442, 86 |
| Anonymous / total PSS | 128.6–136.4 / 274.8–282.7 MB | 129.1 / 246.8 MB |

Party chat, measured on an idle host with a guest joined: 95.4 MB anonymous PSS with three chat messages
and an MPD client in `idle`, 95.6 MB with neither, so chat adds nothing measurable; a party itself (1.3)
costs about 23 MB on the host (95.6 vs 72.4 MB idle). Within the +10 MB budget for every 1.5 feature.

Optimizations kept (each its own commit):
- Playback tick: with MCP off, the twice-a-second "now playing" publish no longer clones the playing
  track's strings and spawns a task. Not measurable against GStreamer's own work (wakeups and CPU stayed in
  noise); kept because with default settings nothing should run for a feature that is off.
- Release profile check, `codegen-units = 1` (kept) against 16, same tree and lockfile, playing as above,
  two runs each: binary 76,773,384 vs 92,082,248 bytes (16 is 20 % larger), total PSS 290–294 vs
  298–305 MB, CPU ticks 89–90 vs 90–92, window mapped 373 vs 377 ms median. 16 units only win on build
  time (an app-only rebuild took 242 s instead of about 390 s), so release builds keep 1.

### Considered, not done
- Moving slate-core to reqwest 0.13 (iroh already pulls it in): measured −688,128 bytes (−0.9 %). Not
  done: `proxy::system_proxy_from_env` deliberately mirrors reqwest 0.12.28's proxy environment rules, and a
  source-guard test refuses the bump until that mirror is re-checked against the new hyper-util. That is a
  proxy behaviour change, not a size tweak.
- Using iroh's `lru` 0.18 instead of 0.19 to drop a duplicate crate: the binary did not get smaller.
- Skipping the start-up Vulkan probe (`vulkan_usable`): it costs 52–85 ms per launch here (2.3 s once, on a
  cold driver cache), measured with a C program doing the same calls. Not done: it is what stops an
  invisible window on machines without working Vulkan, and changing renderer selection needs an A/B on
  machines this review does not have.
- Decoding each cover once for all sizes: the image cache keys entries by size, so a cover shown at
  three sizes is fetched from the disk cache and decoded three times. Not measured as a cost; left alone.
- Allocation counts per tick: an `LD_PRELOAD` malloc counter crashed the app at start, and heaptrack,
  bytehound and perf are not installed (no root), so no allocation profile was taken.
- No audio path, buffer or renderer fallback change was made, so no PulseAudio / ALSA exclusive / WASAPI
  A/B was needed.

### Not tested before release
- Windows: nothing was built or run on Windows in this review (the release workflow builds and self-tests).
- Real phone MPD apps (M.A.L.P., MaximumMPD, Rigelian): the protocol was walked with a scripted client using
  their command sequences, against the real signed-in app over loopback and the LAN address (search through
  TIDAL, playlists, cover art, idle, the password gate), but no phone app was connected. A real half-open
  connection (needs a second machine) was not produced; keepalive is set and accepted by the OS.
- Piper audio: Piper is not installed here; the "not installed" message was checked live, the process
  handling with stand-in scripts.
- Multi-monitor and mixed-DPI window restore: one virtual monitor only. Off-screen, too-large, interface
  scale 1.5 and 2, mini player and Wayland (size only) were checked live.
- VRAM, map time and wakeups on the real desktop compositor; the A/B above used a nested display.
- A 1.4.1 party host for comparison (the two-instance runs on 1.4.1 failed to set up: it has no window
  restore, so both windows opened on top of each other).
- Long sessions (hours), rapid skipping, music videos.

## [1.4.1] - 2026-11-02

Maintenance release: bug fixes and resource reductions only. No new features, no setting or
default changed, no crate added, audio path unchanged (bit-perfect and exclusive output, buffers
and the 6 MiB hi-res stream buffer as in 1.4.0). Details in REVIEW.md #95–104.

### Fixed
- DJ: an error reply from the model's endpoint with non-ASCII text could crash Slate (the text was
  cut in the middle of a character).
- DJ: the "seconds to wait for the model" setting was waited out twice when the endpoint did not
  answer; a refused request (an HTTP error) is still retried without the JSON schema as before.
- Party handoff: a guest joining at the same moment another left could leave the handoff key with
  a guest who was no longer next in line, so the other guests looked for a party nobody hosted.
- Lyrics timing: two quick −100 / +100 ms nudges could lose one of them.
- "Add to new playlist" held up every other TIDAL request (the next track's stream included) for
  up to 4.6 s while it waited for TIDAL to know the new playlist.
- If the Slate core could not start (e.g. a keyring error) with "Start minimized to tray" on,
  Slate ran with no window and no tray; it also checked for the core 10 times a second forever.
  The window now opens with the error.
- Closing a music video while it was still loading let it play on, heard but not seen.
- The last music-video frame (up to ~3.7 MB and its texture) stayed in memory after the video
  closed.
- The DJ API key and TIDAL tokens can no longer reach a log line through debug printing.
- Windows: starting the media controls no longer ties up a runtime thread for up to 5 s.

### Changed (resources)
- The async runtime uses at most 4 worker threads instead of one per CPU core.

### Performance (measured)
Release builds of 1.4.0 and 1.4.1 on the same machine, 12 s warm-up then a 60 s window (1.4.0 two
runs, 1.4.1 one). Playing: a copy of a signed-in profile, TOTO "Africa" (hi-res, S24LE 2ch 192 kHz
at the PipeWire sink in both).

| | 1.4.0 | 1.4.1 |
|---|---|---|
| Threads (login / playing) | 38 / 64, 65 | 18 / 45 |
| Login screen: wakeups/s, CPU ticks in 60 s | 0.7, 0 | 0.7, 0 |
| Login screen: anonymous PSS | 29.7, 29.3 MB | 29.3 MB |
| Login screen: GPU memory (VRAM) | 54.2 MB | 54.3 MB |
| Playing: wakeups/s | 286.2, 252.9 | 251.8 |
| Playing: CPU ticks in 60 s | 84, 82 | 78 |
| Playing: anonymous PSS | 126.6, 133.8 MB | 130.1 MB |
| Playing: GPU memory (VRAM) | 93.4 MB | 93.4 MB |
| Time until the window is mapped | 293–298 ms | 290–306 ms |

Wakeups, CPU and memory while playing are within run-to-run noise; nearly all wakeups are
GStreamer's streaming threads. VRAM was sampled every second and never moved, so the one-off
167.6 MB reading in 1.4.0's notes was not reproduced (a re-tiling test was cut short).

Considered, not done:
- A larger first read size for the HTTP source (64 KiB): no measurable change, so it was taken out.
- Releasing the renderer's GPU memory while hidden: Slint already drops the window surface on hide
  and has no public call to free more.
- Skipping progress updates while hidden: at most 2 wakeups/s; the mini player reads the same values.
- Sending audio to PipeWire directly instead of through its PulseAudio layer (~50 wakeups/s), a
  smaller decoded buffer, or dropping fallback renderers: each changes the audio path, buffering or
  start-up on some machines.

## [1.4.0] - 2026-11-01

### Added
- Multi-select in track lists (album, playlist, Loved Tracks, mixes, smart playlists, and the queue
  panel): Ctrl-click toggles a row, Shift-click selects a range, Ctrl+A selects the list (rebindable),
  Esc clears. A plain click still plays. Right-click on a selection: Add to queue, Play next, Add to
  playlist… (tracks already in it are left out and counted), Suggest to party (guests; the host takes
  3 requests a minute, so 3 are sent), Remove (from your own playlist, or the queue). Search results
  are a card strip, not a list, so they have no multi-select.
- Smarter shuffle (Settings → Playback, off by default): shuffle avoids the same artist, then the
  same album, twice in a row when it can. One pass when the queue is shuffled; nothing runs while
  playing.
- Like current track: a global shortcut (Settings → Shortcuts, unbound by default; on Linux your
  desktop's shortcut settings can bind it), and "Like / Unlike Current Track" in the tray menu. A toast
  says what happened. MPRIS has no "like" action, so it is not there.
- Tray: "Start minimized to tray" (the window opens normally if there is no tray); middle-click the icon
  toggles play / pause (Linux and Windows); the mouse wheel over the icon changes the volume (Linux:
  Windows sends no wheel messages to notification-area icons). The wheel over the in-app volume slider
  changes the volume on both.
- Party reactions: while a party runs, guests and the host can send 👍 ❤️ 🔥 😂 🎉 👏 on the current
  track. The latest shows next to the player for 4 seconds, and the host sees the counts for the track.
  5 reactions per person per 10 seconds. Protocol version stays 2: Slate 1.3 peers ignore the new
  messages.
- Party handoff moves the guests: the host tells everyone where the party continues (the successor's
  public id and the invite secret) and gives the successor, privately, the key to host under. When the
  successor takes over, the other guests keep trying to join it for 2 minutes after the 60 second grace
  period, with no new link. If they cannot reach it, they are told to ask for a new link as before.
- Lyrics timing: −100 ms / +100 ms in the lyrics panel and two rebindable shortcuts (unbound by
  default) shift synced lyrics for the playing track. The offset is saved per track in
  `lyrics-offsets.json` (500 tracks at most) and read only when lyrics are on screen.
- DJ mixer (Settings → Playback, off by default), written by Hsky17 in the
  [slate-dj fork](https://github.com/Hsky17/slate-dj/tree/feat/dj-mixer) and ported to this version:
  when the queue runs out, a chat model you configure (any OpenAI-compatible endpoint, e.g. Ollama)
  picks the next genre, Slate searches TIDAL for a block of tracks and queues them; any failure falls back to the radio.
  Direction box, "DJ up a block" button, endpoint test, readable failures, request timeout, optional
  spoken line through `espeak-ng` / `say`. The titles and artists of the last 20 tracks go to the
  endpoint you set; nothing is sent until the DJ is on.
- DJ page (the next step in Hsky17's handoff), a "DJ" item in the sidebar:
  - Connect a model there: one-click Ollama, LM Studio, OpenAI and OpenRouter presets, "Detect
    local" (looks for Ollama or LM Studio on this computer), "Find models" (lists what the endpoint
    offers to pick from), an API key field, and "Connect", which asks the model to answer exactly
    as the DJ will and saves and switches the DJ on only when it does. The page says what a hosted
    endpoint is sent.
  - Once connected, opening the page turns the DJ on. It shows the genre the DJ is mixing and why,
    the model's spoken transition line, whether this machine can read it aloud (it says so when
    `espeak-ng` / `say` is missing), a few facts about the genre from a small built-in list, the
    direction box, "DJ up a block", on/off, "Change endpoint" and the live queue beside it.
    "Send" on a direction picks a new block that way at once; it replaces the DJ's queued tracks
    (your own "play next" queue stays) and starts after the current track.
  - Nothing on it is built, read or fetched until it is opened.

### Changed
- Clicking a track, playing a single track and playing an artist no longer discard what you queued
  with "Play next" / "Add to queue" (as in TIDAL). Albums, playlists, Loved Tracks and mixes still
  replace everything.
- Hi-res streams buffer up to 6 MiB per track instead of GStreamer's hidden 2 MB, which cut their
  15 s buffer to 3–5 s, so a weak connection rides out dropouts longer (at most +8 MiB while the
  next track is prerolled). CD-quality streams were never limited by it and are unchanged.
- "Add to playlist…" on a track already in the playlist opens the "already in this playlist / Add
  anyway" dialog instead of an error.
- A quality filter (All / Lossless / Hi-Res / Atmos, remembered per page type) already exists on track
  lists and in search since 1.2; it needed no change.

### Fixed
- Playlist creation (details in REVIEW.md #73–75): "Add to new playlist" could leave an empty playlist
  behind a success message because TIDAL's edit host does not know a playlist for a moment after it is
  created; the add now waits for it and says when it still failed. New playlists show in the sidebar
  at once even when TIDAL's list lags, and saving a queue or a smart/merged playlist reports "N of M
  tracks" instead of failing or claiming success.
- "Add anyway" for a duplicate track really adds it now.
- A click that hit a rate limit left the old track playing while the player showed it paused; it now
  stops, and the clicked track plays next. A dead clicked track moves on within its own album or
  playlist before your queued tracks.
- Exclusive-output (ALSA) hangs: the audio thread could block forever on a writer that stopped reading
  or was being torn down (sone 0.23.0 fixes, table below).
- DJ speech: the speech program was never found (PATH was split on `/`), the model's text could be
  read as an option by `say`, and `espeak-ng` was called with a `--text` option it does not have (it
  printed its usage and said nothing); the line now goes to `espeak-ng --stdin`.
- DJ blocks were full of unrelated tracks ("UK Bass Channel", "Bass Solo (Live)"): the DJ searched TIDAL
  for its style phrase, which matches song titles, and it was told never to name an artist, so "play
  King Von" could not work. The model now also names 3–6 artists for the block (one you ask for
  first); Slate searches for them and keeps only tracks they are on, and an artist you asked for
  fills about half the block. The style phrase is the fallback when artists give too few tracks.
- Playlist suggestions: "Hide" at the bottom of a playlist brought the suggestions straight back
  (hiding shrank the section, which counted as reaching the bottom again). Hidden now stays hidden
  until "Show suggestions" or the playlist is opened again.
- One fewer thread and 2 fewer wakeups a second at idle: a thread pool that a dependency starts for
  the accessibility bus kept waking every 500 ms forever (an older version of it is used, which
  lets the idle thread exit).
- A crash that could hit when two equalizers were created for the first time at the same moment
  (GStreamer's equalizer registers a type without locking); seen in the test suite, possible in the
  app with the equalizer and crossfade both on. The first one is now created once, before any other.
- Found in review before release (details in REVIEW.md, #83–92):
  - Party: kicking a guest could hand that guest the new invite link's secret, and a guest who
    had been next in line to host (then kicked, gone, or replaced by a co-host) kept the key to
    take the party over after the host left. The key now changes with the successor, and the new
    secret reaches only the guests who stay.
  - DJ: while the model was thinking (up to 5 minutes with the default timeout), clicks, Next and
    queue edits waited for it. The model is now asked without holding the queue; a track you play
    meanwhile wins.
  - "Start minimized to tray" could leave Slate with no window and no tray icon when the tray
    failed quickly at start.

### Notes
- Resource budget: every new feature is lazy (nothing allocated, spawned, loaded or fetched until it is
  opened or switched on). No new threads, timers or polls; no crates added. The only always-on additions
  are one atomic read in the queue lock (smart shuffle), one in autoplay (DJ), and the reaction/handoff
  fields of a running party. The larger stream buffer is the one deliberate RAM change (above).
- Discord "Join party" button: not built. A Rich Presence button needs a public URL; putting the party
  invite (which contains the join secret) in your presence would let anyone who sees your profile join
  and would defeat "a kick makes a new link". The Discord join-secret / ask-to-join flow needs a
  Discord application review and a new crate, so the item was skipped.
- The DJ keeps its own switch (the original design); it is not behind the MCP setting because it is not
  an MCP feature: Slate calls your model, nothing calls Slate, and it adds no MCP tools. Its intent is
  kept as written (genre blocks, direction, radio fallback, readable failures, generous timeout);
  the review changed only the speech lookup, the speech text and when the queue is locked. Thanks to
  Hsky17 for the work.
- Handoff needs both guests and the new host to reach iroh's address lookup; with a custom relay or
  relays off ("none") a guest may not find the new host, and the old "share a new link" text appears.

#### sone 0.23.0 fixes
| Commit | Subject | Result |
|---|---|---|
| 1d83331 | auth: sign-in window camera prompt under Flatpak | Not applicable: Slate signs in with a device code or the system browser, there is no embedded web view |
| 5edf7ee, c3abdaf | playlists: prompt instead of silently skipping duplicate adds; "Add anyway" | Applied: the 1.2 dialog existed for drag-and-drop only and "Add anyway" used `onDupes=FAIL`, so it could never add. Now `OnDupes::{Fail,Skip,Add}`, the track menu opens the dialog, and a multi-select add counts what it left out |
| 5988a82 | audio: exclusive sink blocking on a writer being torn down | Applied (`send_chunk`) |
| afea702 | audio: never hang waiting on an exclusive writer | Applied (`shutdown_writer`, stop flag, `lingering_writers`) |
| 2a15fa1 | audio: let a stopping writer leave its retry loops | Applied (`retry_resume`, `write_silence` with the stop flag) |
| c7c59c9 | audio: give a lingering writer time to release the device | Applied (`reap_lingering_writers`) |
| 583b1e5 | audio: give up only when the pipeline closes | Applied (`sink_closing`) |
| 90bc6b9 | audio: no fault reports from a stopping writer | Applied (`report_fault`) |
| 576f73d | audio: exit the writer when a flush drains its shutdown | Applied |
| ce68fc4 | playback: hidden 2 MB stream buffer cap | Applied with a bound: 6 MiB (`STREAM_BUFFER_SIZE_BYTES`), not sone's 40 MiB and 30 s, to stay inside the RAM budget; 24/96 now fills its 15 s window, 24/192 holds ~8 s; 16/44.1 (~18 s per 2 MB) was never byte-bound |
| 426dd72, 3edc437 | queue: stop / clear / new play cancel a pending rate-limit retry | Not applicable: Slate has no timer-driven retry. A rate limit stops playback and queues the track; nothing runs later that could need cancelling |
| f2bf30a | queue: retry the clicked track after a rate limit | Applied in Slate's form: the clicked track is put at the head of the queue and the old track is stopped (no automatic timer, see above) |
| 13cba72 | queue: skip a dead clicked track within its collection | Applied (needed once the manual queue survives a click) |
| f6e61ae | queue: keep queued tracks on a single track or artist | Applied |
| undici / smol-toml bump | JS dependencies | Not applicable |
| AppStream metainfo | refresh | `data/…metainfo.xml` has a 1.4.0 release entry |

All 20 upstream commits after Slate's porting baseline (`a34fd6f`) are in this table; the exclusive-writer
port was re-checked against upstream line by line (same stop checks, fault reports and timeouts).

### Performance (measured)
Release builds of 1.3.0 and 1.4.0 from the same disk, 12 s warm-up then a 60 s window, two runs each,
optional subsystems off as they are by default. Login screen: empty profile. Playing: a copy of a
signed-in profile, Play pressed over MPRIS, a lossless track through PipeWire (s16le 2ch 44.1 kHz at
the sink). This machine's setup differs from the 1.3.0 measurements (another profile and track,
GStreamer 1.28), so both versions were measured again side by side; compare the columns, not this
table with 1.3.0's. The 1.4.0 column is the released build (DJ page included).

| | 1.3.0 | 1.4.0 |
|---|---|---|
| Login screen: wakeups/s (all threads) | 2.7, 2.7 | 0.7, 0.7 |
| Login screen: CPU ticks in 60 s | 0, 0 | 0, 0 |
| Login screen: anonymous PSS | 30.5, 30.5 MB | 30.5, 30.5 MB |
| Login screen: GPU memory (VRAM) | 52.4, 52.4 MB | 52.4, 52.4 MB |
| Playing: wakeups/s | 149.7, 139.7 | 141.5, 138.5 |
| Playing: CPU ticks in 60 s | 55, 57 | 60, 56 |
| Playing: anonymous PSS | 96.5, 98.5 MB | 99.9, 96.0 MB |
| Playing: GPU memory (VRAM) | 92.9, 91.5 MB | 167.6 (see below), 91.5 MB |
| Threads (login / playing) | 45 / 70, 72 | 44 / 74, 73 |
| Network sockets at the login screen | none | none |
| Time until the window is mapped | 297–431 ms | 303–368 ms |
| Binary | 73.0 MB | 75.3 MB |

The 1.4.0 login wakeups and the one thread fewer are the `blocking` fix (before it, 1.4.0 read the same
2.7 wakeups/s and 45 threads as 1.3.0). The idle remainder, 0.7 a second, is on the UI thread in both
versions. One 1.4.0 playing run read 167.6 MB VRAM against 91.5 MB in every other playing run of both
versions (six in all); it was not reproduced and is reported as measured. Total PSS is left out: a
stray 1.3.0 test window ran during part of the last series and shared library pages with it, which
lowers PSS for the runs it overlapped; anonymous PSS is not affected. While playing, the playing-thread
count varies by a few between runs of the same build (GStreamer's streaming threads), and the wakeups
are GStreamer's: libpulse's mainloop ~48/s, the decoded queue ~23/s, two multiqueues ~11/s each, the UI
thread ~10.5/s (two progress updates a second and their redraws). Everything is inside the budget
(+10 MB RAM, +10 MB VRAM, CPU and threads unchanged); nothing was cut.

Measured separately:
- Playing with the window hidden ("Start minimized to tray", 1.4.0): 23 CPU ticks a minute, anonymous
  PSS 61.3 MB, VRAM 32.4 MB, 63 threads, the UI thread 2 wakeups/s. Skipping the progress updates
  while hidden would save at most those 2 of ~129 wakeups a second, so it was not done.
- Tried and not kept: a byte cap on the 15 s decoded queue, which only matters for hi-res. Runs with a
  Hi-Res track reached PipeWire as 16/44.1 and the decoded rate could not be confirmed, so the saving
  was not measured and the change was taken out.
- Not measured: a running party with reactions (the reaction state is six counters and at most five
  timestamps per guest; the party's own cost was measured for 1.3.0), the DJ with a model (no thread
  or timer while on; one HTTP request when the queue runs out; the DJ page builds nothing until opened)
  and the larger stream buffer at hi-res (its bound is +8 MiB).

### Not tested before release
- The playlist-creation fix against a live account. It is built from reading the code and a
  mock-server test of the 404-then-200 sequence (1.3.0's call fails that same sequence); the real
  failure was never reproduced, so the bug the reporter saw may be another one.
- Exclusive-output fixes: unit tests, source guards and a line-by-line comparison with upstream; not
  heard on a DAC, and the hang itself was never reproduced.
- Party reactions, handoff and the two review fixes (#83, #84): tested over loopback only; not with two
  machines, a relay, or iroh's address lookup (guests finding a new host by its id alone). Emoji
  drawing in the player strip was not looked at on screen.
- Tray: middle click and the wheel (Linux), the Windows tray and "Start minimized" were not clicked on
  a real desktop (the hidden start was measured, not used); Slint's wheel direction on the volume bar
  was not checked.
- Multi-select, lyrics nudge, smarter shuffle and the new settings rows: compiled and their logic
  unit-tested; the screens themselves were not looked at.
- DJ mixer and DJ page: the model's reply was checked against a local Ollama model (gemma4:e4b, ~7 s,
  usable JSON) and the sidebar item was seen on screen; the page was opened by hand only by the
  maintainer before release, and speech was not heard (no `espeak-ng` on the development machine).
  The unlocked fetch is covered by a queue test, not by a run with a slow model.
- The larger stream buffer with a hi-res track, and the cost of a running party and of the DJ (see
  "Performance").
- Windows: the new code was compiled, linted and tested in CI only; nothing was run on a Windows
  desktop.

## [1.3.0] - 2026-10-31

### Added
- Listening party, guest requests (Settings → Party). First, a fix: in 1.2 the host could approve
  or deny suggestions but a guest had no way to send one. Now guests can use "Suggest to party" in
  any track's right-click menu, or the ＋ on a search result, and see a read-only "Up next" list
  that says who suggested each track.
  - Host: a request tray with guest name, track and quality, Approve / Deny and Approve all /
    Deny all. Approved requests go where you choose: Play next, Add to end, or Fair queue
    (guests take turns, so one guest cannot flood the queue); the choice is remembered.
  - Host modes: Approval required (default), Auto-approve, Requests off. Each guest can have 3
    requests waiting (1 / 2 / 3 / 5 / 10 in settings). A track that is already waiting, already
    queued or was just played is turned down with a message to the guest, and "No explicit
    tracks" turns down explicit ones.
  - A request shows "unavailable for N guests" when guests report they cannot play it (region).
    A guest whose account cannot play a track tries another version of the song and otherwise
    skips it for themselves only; the party carries on.
  - Co-host: the host can promote a guest, who can then approve and deny requests, skip and
    reorder. Vote skip: optional, the host picks the share of guests needed (25 / 50 / 75 %).
  - Host handoff: a guest that loses the host keeps the party for 60 seconds and tries to
    reconnect; after that the co-host (or the guest who joined first) is offered the party and
    can take it over with a new invite link.
  - A guest who joins late gets the current track, position and the next tracks straight away.
  - Protocol version 2. A 1.2 guest joining a 1.3 host (or the reverse) is told which side is
    newer instead of failing; messages from a future version are ignored.
  - MCP: `get_party_requests`, `approve_request` (can be undone from the activity log) and
    `deny_request` (needs `confirm: true` and the in-app Allow / Deny).
- Queue panel "⋯" menu: "Save queue as playlist" (an unlisted TIDAL playlist, like the 1.2
  tools) and "Queue snapshots…": save the queue under a name, restore it (replacing the queue
  or adding to it), rename and delete. Snapshots are kept on your computer (50 at most, 1000
  tracks each) and the list is read from disk only when the dialog opens.
- Credits: people in the Credits panel are clickable. Producers, writers and engineers open their
  artist page when TIDAL gives an artist id for them, otherwise a search for the name. There is
  a "Credits" button on the full-screen player as well as in the track right-click menu.
- Parametric equalizer (Settings → Playback → Equalizer, off by default): up to 10 bands (peak,
  low shelf, high shelf with frequency, gain and Q) and a preamp, and import of an AutoEq
  `ParametricEQ.txt` file. Slate ships no presets. The equalizer is added to the audio pipeline
  only while it is on; with it off the pipeline is the same as in 1.2. While it is on the Signal
  Path reads MODIFIED. It switches itself off while exclusive or bit-perfect output is on and the
  page says why.
- `slate --self-test --party-relay` checks that the default iroh relay (or your custom one) is
  reachable, prints the result and exits.

### Changed
- Party settings moved into the Party page: request mode, placement, per-guest limit, explicit
  filter and vote skip replace the single "Let guests suggest tracks" switch. Your guest name can
  be set next to the Join field (it used to be "Guest").
- Switching the equalizer on or off applies from the next track; changing a band applies to the
  track that is playing.
- Lighter while idle and while playing (numbers under "Performance"): the theme-file watcher no
  longer wakes 4 times a second to check for changes (it waits for the file system to report
  one), and the sound server is fed in 40 ms instead of 10 ms segments, a quarter of the
  wakeups, with the same 200 ms buffer. One dependency (`notify-debouncer-full`) is gone.

### Fixed
- Guests could never send a track suggestion in 1.2 (the host side was finished, the guest side
  was not reachable from the interface).
- Found in review before release (details in REVIEW.md, #61–72):
  - Party: a guest could send "cannot play" reports for any track without limit, growing the
    host's memory and re-sending the request list each time; repeated skip votes re-announced
    themselves to everyone; any number of connections could wait in the handshake. Reports now
    count only for tracks the party is playing, queuing or deciding on (64 at most), a guest
    sending more than 60 messages in 10 seconds is disconnected, and at most 8 handshakes run
    at once.
  - Party: two quick clicks on Start or Join started two parties (the first kept running in
    the background); Leave while a start, join or reconnect was still under way was undone when
    it finished; a guest could end up playing an older track when two arrived close together.
  - Party: a 1.3 guest joining a 1.2 host now reads "The host is on an older version of Slate"
    instead of "protocol version 2 is not supported (need 1)".
  - Equalizer: it switched itself off for every track after the first crossfade; switching
    exclusive or bit-perfect output on mid-track left it acting on the sound until the next
    track (it is now flat at once); an AutoEq file with `Preamp: NaN` was accepted.
  - The equalizer switch and the party choices no longer read and write settings files on the
    UI thread.
  - Queue snapshots are written owner-only like the rest of the profile, and saving a queue of
    more than 1000 tracks says that only the first 1000 were kept.
- README, `main.rs` and `PORTING_PLAN.md` disagreed about Windows media controls and the tray;
  they are implemented (not yet used on a real Windows desktop, see below), and the text now
  says so.

### Notes
- Resource budget: every new feature is lazy. Nothing is allocated, spawned, loaded or fetched
  until it is opened or switched on, and no new thread, timer or poll runs otherwise (the
  thread count is the same as 1.2's at the login screen and while playing; the only renamed
  thread is the theme watcher's). The
  party extras live behind the existing `party` feature. No crates were added; one was removed.
  Measured against 1.2 below.
- Host handoff keeps the party alive and offers it, but the other guests are not moved
  automatically: the new host shares a new link.
- A guest's plan quality (a track above their subscription) is not detected separately: TIDAL
  streams the best quality the plan allows, and a track that cannot be played at all is
  reported as unavailable.
- Queue snapshots stay on the computer when you sign out; they hold track details only.

### Performance (measured)
Release builds of 1.2.0 and 1.3.0 from the same btrfs disk, 12 s warm-up then a 60 s window,
two runs each, optional subsystems (MCP, party, crossfade, suggestions, equalizer) off as they
are by default. Login screen: empty profile. Playing: a copy of a signed-in profile resuming a
lossless track through PipeWire.

| | 1.2.0 | 1.3.0 |
|---|---|---|
| Login screen: wakeups/s (all threads) | 6, 6 | 2, 2 |
| Login screen: CPU ticks in 60 s | 1, 1 | 0, 0 |
| Login screen: anonymous PSS | 29.8, 29.8 MB | 29.8, 29.7 MB |
| Login screen: PSS | 95.4, 96.0 MB | 97.5, 97.5 MB |
| Login screen: GPU memory (VRAM) | 52.4, 52.4 MB | 52.4, 52.4 MB |
| Playing: wakeups/s | 269, 259 | 181, 198 |
| Playing: CPU ticks in 60 s | 80, 85 | 73, 74 |
| Playing: anonymous PSS | 118.5, 124.3 MB | 120.8, 129.9 MB |
| Playing: GPU memory (VRAM) | 154.6, 154.6 MB | 155.9, 154.6 MB |
| Threads (login / playing) | 45 / 75 | 45 / 74 |
| Network sockets at the login screen | none | none |
| Time until the window is mapped | 295–313 ms | 295–299 ms |
| Binary | 71.4 MB | 73.0 MB |

Idle wakeups: 4 a second came from the theme watcher's debouncer tick; the remaining 2 are in
zbus (accessibility / D-Bus). While playing, the sound-server thread went from ~100 to ~25
wakeups a second. Anonymous PSS while playing varies by about 6 MB between runs of the same
build (buffered audio), so the two versions are within noise of each other, inside the
+10 MB budget. Some login-screen runs were disturbed by other activity on the desktop
(1.2.0: 23 and 16 wakeups/s, 1.3.0: 9 and 13) and are not in the table.

The cost of the new features when used, not on by default:
- Listening party, host with one guest over loopback, measured in a separate process that runs
  both ends: +7–8 MB anonymous memory and no extra threads while running. After ending, the
  memory goes back to the allocator; about 3–5 MB stays mapped (one-time TLS/QUIC setup and
  allocator fragmentation), and three parties in a row did not grow it further.
- Equalizer on (10 bands, while playing, two runs): anonymous PSS 120.9, 120.9 MB (no
  change), VRAM unchanged, 222 wakeups/s and 114, 149 CPU ticks a minute against 73, 74 with it
  off, i.e. about 1 % of one core for the filtering. With it off the pipeline has no EQ
  elements at all.
- Credits panel, queue snapshots: not measured (a list model of a few dozen rows; snapshots are
  read from disk only while the dialog is open).

### Not tested before release
- Almost everything with a screen. The Equalizer section and the Party page were rendered
  offscreen with sample data and looked right; nothing was clicked in a running app. The queue
  "⋯" menu, the snapshot dialog, clickable credits, the full-screen Credits button, the ＋ on
  search results and the "Suggest to party" menu item were compiled but never rendered.
- A party with two real Slates, on one machine or on different networks. The loopback tests
  cover messages, requests, caps, co-host, version refusal and the flood limit, but not hole
  punching, relays or a lost host coming back; a real 1.2 ↔ 1.3 pair was not tried (the 1.2
  host's refusal text is matched in a unit test); the host-handoff reconnect and "Take over" flow have unit tests for the rules,
  not an end-to-end run.
- Fair-queue placement and approve-then-play with real TIDAL tracks, and the region fallback to
  another version.
- The MCP tools `get_party_requests`, `approve_request` and `deny_request` against a real
  agent.
- The equalizer through the speakers: the GStreamer element is tested for the gain it applies,
  but nobody has listened to it. It is untested with Dolby Atmos tracks and on Windows (the
  `equalizer` plugin is in the Windows bundle; the AppImage bundle was not checked).
- Importing AutoEq files from the wild beyond the common `PK` / `LSC` / `HSC` format.
- The cost of a running party, the credits panel and queue snapshots inside the running app:
  this session had no way to click, so the party was measured in a separate process (see
  "Performance") and credits and snapshots not at all. Track-list scrolling, page navigation
  and the thumbnail cache were not profiled for the same reason (nothing in them changed).
- The 40 ms sound-server segments by ear (wakeups and CPU were measured while a track played;
  nobody listened for glitches), and with PulseAudio proper rather than PipeWire's.
- The equalizer surviving a crossfade in a running app (the fix is in the worker hand-over;
  there is no test that plays two tracks).
- Windows (everything above), and Windows media controls and tray, which were already untested.

## [1.2.0] - 2026-10-30

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
  combines with the filter, and is remembered per kind of page. Search results have the same
  chips for their Tracks section.
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
  ends it at once, and pausing finishes it (the new track waits at full level). The second player
  exists only for the length of a fade; with crossfade off nothing changes in playback.
- Album and playlist headers show the total length (for example "13 tracks · 1 h 1 min").
- Settings has a search box: type a word and pick the setting from the list to jump to its page.
- The sidebar uses one set of line icons instead of mixed symbols; icon-only buttons in the player
  bar show a tooltip on hover, and buttons show a focus ring and can be used with Tab, Space and
  Enter.
- Empty playlists show a short "Nothing here yet" message instead of a blank page, and the UI
  now has one shared set of spacing, radius and text sizes (`Metrics`) for new work.

### Changed
- Long playlist edits (remove duplicates, merge from Compare, adding missing tracks, saving a
  smart playlist, replacing a version) run one at a time, paced, with a Cancel button on the
  notice bar. Signing out stops them.
- The app has its own `party` Cargo feature (on by default). Built without it, the Party page
  says so and no party code is compiled.

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

### Notes
- The MCP server is now a stateful Streamable-HTTP session (same URL, still loopback-only with
  the Host check), so client names and resource subscriptions work. Its older
  `create_playlist` tool still makes public playlists; the new tools make unlisted ones.
- Ctrl+F works on playlist, album, Loved Tracks and mix pages, not on an artist's top tracks.
- Suggested tracks are a panel under the list rather than rows inside it.
- The Discord "Ask to join" button for parties is not there: the presence library Slate uses
  cannot subscribe to Discord's join events.
- Not tested before release: crossfade by ear on real tracks, a party across two different
  networks (hole punching and relay fallback; only loopback is covered by tests), the Windows
  global-shortcut code, and the full scripted session below on a signed-in account.

### Performance (measured)
Release builds of 1.1.0 and 1.2.0 from the same disk, empty profile, login screen, 12 s
warm-up then a 60 s window, two runs each, optional subsystems (MCP, party, crossfade,
suggestions) off as they are by default:

| | 1.1.0 | 1.2.0 |
|---|---|---|
| Wakeups/s (voluntary context switches, all threads) | 6, 6 | 6, 6 |
| CPU ticks in 60 s | 2, 1 | 2, 1 |
| PSS | 177.7, 177.6 MB | 173.6, 173.8 MB |
| Anonymous PSS | 42.4, 42.3 MB | 43.0, 43.1 MB |
| Threads | 34 | 34 (same names) |
| Network sockets | none | none (no MCP listener, no iroh endpoint) |
| Binary | 52 MB | 72 MB (iroh and the MCP tools) |

PSS depends on where the binary sits: run from tmpfs, 1.1.0 measured 154 MB, because btrfs
maps more of the binary per page fault. Both columns above are from the same btrfs disk.
Not measured for this release: the signed-in scripted session and GPU memory, and the cost of
MCP with a client, a running party and a crossfade (all off by default).

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
