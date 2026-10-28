# Slate (Slint port of SONE): plan and status

Slate is a port of [lullabyX/sone](https://github.com/lullabyX/sone) (Tauri 2 + React 19) to a
100 % Rust app with a native Slint UI. Below, "SONE" / "upstream" means the original project. Baseline: upstream `a34fd6f` (v0.22.1).

Status legend: `done` · `in-progress` · `todo` · `blocked (reason)`.

The original frontend (`src/`, ~46 k lines TS/TSX, 242 files) and its JS tooling are
**still in the tree on purpose** as the porting reference. They are deleted in the
final cleanup commit (see §8). The Tauri shell (`src-tauri/`) is already gone.

## 0. Key findings from the study

1. **`src-tauri` is ~35 k lines of Rust, 177 `#[tauri::command]`s, 22 emitted events,
   one managed state (`AppState`).** Business logic is mostly Tauri-independent. Coupling
   was `AppHandle` (emit + `state::<AppState>()`), webview windows (PKCE login, tray,
   decorations, idle-inhibit surface) and the MPRIS thread.
2. **Important: queue / shuffle / repeat / history / autoplay / gapless-prefetch /
   LRC parsing / content filtering / theme presets+validation / shortcuts are NOT in Rust.**
   They live in the React layer (`hooks/usePlaybackActions.ts` 1.8 k lines, `atoms/*`,
   `lib/*`). The brief says "reuse existing Rust"; for these there is nothing to reuse and
   they are **ports from TypeScript to Rust** into `slate-core`. This is the largest piece
   of net-new core work.
3. The Rust backend only owns: ReplayGain *normalisation gain* (`compute_norm_gain`),
   persisted queue blob (`save_playback_queue` / `load_playback_queue`, opaque JSON), and
   audio/pipeline.
4. Upstream already depends on `rmcp` for MCP (streamable HTTP) rather than a hand-rolled
   axum server; kept as is (axum is its transport).

## 1. Architecture mapping (done in step 1)

| Original | New | Status |
|---|---|---|
| `src-tauri/src/*` (core logic) | `crates/slate-core/src/*` | done |
| `tauri::AppHandle` (emit, state) | `slate_core::AppHandle` + `EventSink` trait (`events.rs`) | done |
| `#[tauri::command] fn x(State<AppState>, ..)` | plain `pub fn/async fn x(&AppState, ..)` in `slate_core::commands::*` | done |
| MPRIS thread owned by core | `slate_core::media::{MediaCommand, MediaSink, MediaHub}`; app registers the sink | core side done; sinks todo |
| `main.rs` pre-thread env scrub (proxy, `GST_PLUGIN_PATH`) | `crates/slate-app/src/main.rs` (source guards updated to scan it) | done |
| WebKit-only env (`WEBKIT_DISABLE_DMABUF_RENDERER`, `SOUP_FORCE_HTTP1`) | dropped, no webview | done |
| `tauri-plugin-window-state` | todo: persist window pos/size in app | todo |
| `tauri-plugin-single-instance` + `deep-link` | `single-instance` + `tidal://` in app | todo |
| `tauri-plugin-global-shortcut` | todo (evaluate `global-hotkey` crate) | todo |
| `tauri-plugin-opener` | `open` crate | todo |
| `tauri-plugin-oauth` (localhost redirect) | not needed: native uses code-paste / browser flow | see §4 |

## 2. Backend commands (`#[tauri::command]` → `slate_core::commands`)

All callable as plain Rust today. "UI wiring" = Slint callback + model glue (not started
unless noted). Command bodies were moved, not rewritten.

| Module (orig. `src-tauri/src/commands/`) | Commands | Core | UI wiring |
|---|---|---|---|
| `auth.rs` | load_saved_auth, get_saved/default_credentials, has_pkce_defaults, parse_token_data, import_session, start/poll_device_auth, refresh_tidal_auth, start/complete_pkce_auth, logout, consume_legacy_auth_notice, get_session_user_id, start/complete_pkce_browser_login, get_user_profile | done | todo |
| `auth.rs` | **start_pkce_login_window** (embedded webview) | **blocked: needs a webview; removed.** Replaced by browser PKCE flow (`start_pkce_browser_login`) + device-code login with QR | n/a |
| `library.rs` | playlists CRUD, folders, favourites (tracks/albums/artists/playlists/mixes/videos), recommendations, paged playlist tracks | done | todo |
| `pages.rs` | home, explore/page sections, album, artist (+bio, top tracks, view-all), mix | done | todo |
| `search.rs` | search_tidal, get_suggestions | done | todo |
| `metadata.rs` | get_stream_url, get_playlist_details, get_track, get_track_lyrics, get_track_credits | done | todo |
| `feed.rs` | get_feed, mark_feed_seen | done | todo |
| `profile.rs` | get/update profile meta, bio, links, upload/delete picture | done | todo |
| `playback.rs` | play_tidal_track, set/clear_next_track, stream/video info, pause/resume/stop, volume, seek, position, is_track_finished, save/load_playback_queue | done | todo |
| `playback.rs` | update_mpris_* (5) | done (now forward to `MediaHub`) | sink todo |
| `scrobble.rs` | notify_track_*, status, queue size, connect Last.fm / Libre.fm / ListenBrainz, disconnect | done | todo |
| `utility.rs` | signal path, image bytes, cache stats/clear, all audio/proxy/discord/report settings, list_audio_devices, idle inhibit, logging toggle | done | todo |
| `utility.rs` | quit_app, update_tray_tooltip, install_webview_proxy_auth | removed (window/tray/webview); app owns | todo |
| `utility.rs` | set_decorations | persistence kept in core; window call moves to app | todo |
| `mcp.rs`, `overlay.rs` | connection info, enable, port/host, token regen, publish state/theme | done | todo |
| `updates.rs` | check_for_update | done | todo |

Events the core emits (now through `EventSink`): `audio-error`, `audio-resampled`,
`track-finished`, `signal-path-changed`, `scrobble-auth-error`, `theme-file-changed`
(+ `pkce-*` removed with the webview, `tray:*` / `mpris:*` become app-internal).

## 3. Core features

| Feature | Original | New | Status |
|---|---|---|---|
| TIDAL client, device-code login, token refresh | `tidal_api.rs`, `commands/auth.rs` | `slate-core` same | done (tests exist) |
| QR code for login | `qrcode.react` | `qrcode` crate → Slint `Image` (`images.rs`) | done (unit-tested) |
| AES-256-GCM config/cache, keyring | `crypto.rs`, `cache.rs` | same | done (tests exist) |
| Playback engine (playbin3, gapless, exclusive/bit-perfect ALSA) | `audio.rs` | same | done on Linux; **Windows wasapi2sink exclusive path todo** |
| ReplayGain (`compute_norm_gain`, album/track select) | `commands/playback.rs` + atoms | same | done (Rust half); selection policy in TS todo |
| Max-quality cap + fallback | `quality_tiers` | same | done (tests exist) |
| Queue / shuffle / repeat / history / persistence | `usePlaybackActions.ts`, `atoms/playback.ts`, `lib/playbackSource.ts`, `lib/qid.ts` | `slate-core::queue` (model, 18 tests) + `controller` (orchestration) | done; snapshot format identical to the original. Queue *view* (list, drag reorder) todo with step 4 |
| Autoplay (radio after queue ends) | TS | `queue` + `controller` | done (logic tested; live fetch unverified); no UI toggle yet |
| Gapless prefetch / prediction | `useGaplessPrefetch.ts`, `lib/gaplessPredict.ts` | `slate-core::controller` (`set_next_track`) | **todo** — queue exposes the next track but arming is not wired, so there is a gap between tracks |
| `notify_track_started` payload (feeds scrobbling) | `buildTrackStartedPayload` | controller | **todo (step 9)** — no scrobbles are sent yet |
| Content filter prefs (explicit/AI) | `lib/contentFilter.ts` | `controller::ContentPrefs` | logic done; settings UI + persistence todo |
| Scrobbling Last.fm / Libre.fm / ListenBrainz + MusicBrainz | `scrobble/*` | same | done (tests exist) |
| TIDAL play reporting (toggle) | `tidal_report/*` | same | done |
| Discord Rich Presence | `discord*.rs` | same | done |
| Proxy HTTP/HTTPS/SOCKS5 | `proxy*.rs` | same | done (tests exist) |
| MCP server 127.0.0.1:5577, token, default off | `mcp/*` (rmcp+axum) | same | done; `ensure_mcp_started` now `pub` |
| OBS overlay 127.0.0.1:5578, default off | `overlay/*` | same + embedded HTML asset | done; **open question:** the embedded overlay page (`overlay/server.rs`) contains an inline `<script>` (live updates). The brief allows only static HTML/CSS; a JS-free alternative (meta-refresh / SSE-less polling is not possible in pure CSS) needs a decision — see HANDOFF |
| Signal path probing | `signal_path.rs`, `pipeline_probe.rs` | same | done |
| `theme.json` read/write/validate + watcher | `theme_config.rs` (file IO) + `lib/themeFile.ts` (validation) | `slate-core::theme` | IO done; **validation + 15 presets are TS → todo with rejection-case tests** |
| Lyrics fetch + LRC timing | `get_track_lyrics` + `lib/lrc.ts` | `slate-core::lyrics` | fetch done; **LRC parse/seek todo** |
| Content filter (explicit/AI hide) | `lib/contentFilter.ts`, `useContentPrefs.ts` | core | todo |
| Idle inhibit | `idle_inhibit/*` | same | D-Bus + X11 done; **Wayland-native layer blocked (needs wl_surface from winit; D-Bus covers it meanwhile)** |
| Logging | flexi_logger (core) | app uses `tracing` + `tracing-log` bridge | app side done; core still `log` |

## 4. Platform shells (`slate-app`)

| Feature | Plan | Status |
|---|---|---|
| Window, renderer fallback (Skia-Vulkan → Skia-GL → FemtoVG → software; `SLINT_BACKEND` override) | `bootstrap.rs`: select + startup-failure respawn with next renderer | done; Skia/Vulkan verified on this machine (Wayland), fallbacks untested |
| MPRIS / SMTC | Linux: `mpris-server` (shuffle/loop/seek needed, souvlaki lacks them) in `platform/mpris.rs` — **done, registered on D-Bus, shuffle property round-trips**. Windows SMTC via `souvlaki`: todo |
| Tray | Linux `ksni` in `platform/tray.rs` — **done** (registers; menu clicks not exercised). Close-to-tray via `on_close_requested`. Windows `tray-icon`: todo |
| Deep links `tidal://`, single instance | .desktop MimeType + IPC forward | todo |
| Linux audio: DAC detection `/proc/asound` | in core `audio.rs` | done |
| Windows audio: `wasapi2sink exclusive=true`; hide ALSA-only options | todo |
| Windows build verification (MSVC + GStreamer MSVC) | **blocked: no Windows host available in this environment** | blocked |

## 5. Views / components (React → Slint)

All UI work is **todo**. Original line counts shown to indicate size.

| Feature | Original | New (planned) |
|---|---|---|
| Layout: sidebar + content + player bar, title bar, resize edges | Layout, Sidebar(985), TitleBar, PlayerBar(772), ResizeEdges | `ui/shell.slint`, `ui/player_bar.slint` |
| Login: code + QR, PKCE fallback, proxy banner | Login(1384), ProxyNoticeBanner | `ui/login.slint` |
| Home / Explore / Feed | Home(608), HomeSection(974), ExplorePage, ExploreSubPage, FeedPage | `ui/home.slint` |
| Search (Top Hits, Tracks/Albums/Artists/Playlists/Videos), suggestions | SearchView(1060), SearchBar(699) | `ui/search.slint` |
| Library: playlists+folders, albums, artists, mixes, loved tracks/videos, search+pagination, tab-aware Play/Shuffle | FavoritesView, LibraryViewAll, ViewAllPage, PlaylistView, FolderContextMenu, MoveToFolderMenu, AddToPlaylistMenu | `ui/library.slint` |
| Album / Artist / Playlist / Mix pages | AlbumView, ArtistPage, ArtistTracksPage, PlaylistView, MixPage | `ui/pages/*.slint` |
| Track list (virtualised, 5 k+ rows) | TrackList(986) | `ui/track_list.slint` + custom lazy `Model` in Rust |
| Profile + edit (bio, links, avatar crop) | ProfilePage, ProfileEditModal, AvatarCropper, SocialLinks | `ui/profile.slint` |
| Now-playing drawer, synced lyrics, credits | NowPlayingDrawer(1797) | `ui/drawer.slint` |
| Full-screen player | MaximizedPlayer(1059) | `ui/fullscreen.slint` |
| Miniplayer (separate window) | MiniPlayer(1411), miniplayer-main.tsx, useMiniplayer* | second Slint `Window` |
| Music video (fullscreen / in-bar) | VideoPlayer(723), hls.js | GStreamer appsink → `SharedPixelBuffer` |
| Context menus, toasts, tooltips, share (copy TIDAL link) | MediaContextMenu, TrackContextMenu, ToastContext, TooltipLayer | `ui/menus.slint` |
| Settings (General, Playback, Themes, Network, Scrobble, Discord, MCP, Overlay, Utilities) | components/settings/* | `ui/settings/*.slint` |
| Signal Path panel + PRISTINE verdict | SignalPathPanel, signal-path/* | `ui/signal_path.slint` |
| Theme: Theme global, 15 presets, light/dark, colour picker | lib/theme.ts, ColorPicker, ThemesTab | `ui/theme.slint` + `slate-core::theme` |
| Shortcut overlay + all shortcuts | lib/shortcuts.ts(426) | `ui/shortcuts.slint` + app key handling |
| Image cache (disk + bounded LRU, off-thread decode) | `get_image_bytes` + TidalImage | `slate-app::images` |
| Update toast, about modal | AboutModal, updates | `ui/about.slint` |

## 6. Rules compliance status

| Rule | Status |
|---|---|
| `cargo fmt` + `cargo clippy --all-targets -- -D warnings` | passing (style lints inherited from upstream allowed at crate level; see `lib.rs`) |
| No `unwrap()/expect()` outside tests | **NOT yet met in inherited core code** (many `unwrap`/`expect`/`panic!`, e.g. `AppState::new`, scrobble/overlay, tests guards). Audit + fix todo. New code avoids them (one deliberate `process::abort` in `AppHandle::state`) |
| thiserror in core / anyhow in app | core `SoneError` already thiserror; app uses anyhow |
| Never log tokens | inherited `log_safe()` conventions retained; audit todo |
| Tests: auth/token parsing, queue, theme, crypto, ReplayGain, lyrics | auth/token, crypto, ReplayGain norm, proxy, scrobble exist (367 tests). **queue, theme.json, lyrics todo** (logic not in Rust yet). wiremock not yet used |
| No JS in final tree | **pending** — `src/`, `package.json`, vite/ts/eslint configs, `pnpm-lock.yaml` removed in final cleanup |

## 7. Build order progress

1. Workspace + `slate-core` extracted, tests passing, Tauri removed — **done** (367 unit + 16 integration tests pass)
2. Slint window → login → play one track → player bar — **code complete, partly verified**: login screen renders on Skia/Vulkan; device-code + QR flow, track-ID play box, player bar (play/pause, seek, volume, cover) implemented but **not exercised against live TIDAL** (needs an interactive account approval)
3. Queue + persistence, media controls, tray — **done on Linux**: 387 core tests pass; app starts, registers MPRIS + tray, saves `queue.json`/`modes.json` on clean exit. **Not verified with a real logged-in playback session** (next/prev/autoplay/restore against live TIDAL)
4. Library + pages + lazy models + image cache — **in progress**: sidebar (Loved Tracks + all user playlists), paged lazy track list (`models::LazyTracks`, 2 tests), double-click-to-play, Play / Shuffle (whole list fetched in 6 parallel pages on demand). Still todo: albums, artists, mixes, folders, Album/Artist/Playlist-detail pages, cover thumbnails + bounded LRU image cache, loved videos tab, search/pagination UI, track context menu
5. Search + explore — todo
6. Drawer + lyrics — todo
7. Theme system — todo
8. Miniplayer, fullscreen, video — todo
9. Scrobbling/Discord/MCP/OBS/proxy/signal path/deep links wiring — todo (core done)
10. Windows build — blocked (no Windows host)

## 8. Final cleanup checklist

- [ ] delete `src/`, `package.json`, `pnpm-*`, `vite.config.*`, `tsconfig*`, `eslint*`, `index.html`, `miniplayer.html`, `sync-version.mjs`, `knip*`, `public/`
- [ ] update `build-scripts/` (PKGBUILD, AppImage, etc.) and `data/*.desktop` for the new binary
- [ ] README (fork attribution), CHANGELOG, HANDOFF
