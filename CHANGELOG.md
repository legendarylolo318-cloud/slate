# Changelog

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
