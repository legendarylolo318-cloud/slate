# Review log

Last updated 2026-10-29. Linux checks run on Arch (Wayland/Hyprland, AMD GPU); Windows checks run on GitHub's `windows-latest` (MSVC, GStreamer 1.24.13).

## Build and test status
| Check | Linux | Windows |
|---|---|---|
| `cargo fmt --all --check` | pass | pass (CI) |
| `cargo clippy --workspace --all-targets -- -D warnings` | pass | pass (CI) |
| `cargo test --workspace` | 488 pass | pass (CI); 3 `curlhttpsrc` tests skipped, no curl plugin in GStreamer's Windows packages |
| `cargo deny check` (advisories, bans, licenses, sources) | pass | n/a |
| `cargo audit` | pass, with the exceptions below | n/a |

## Bugs found and fixed
| # | Severity | Finding | Fix |
|---|---|---|---|
| 1 | High | OBS overlay host was user-configurable (`0.0.0.0` accepted), exposing an unauthenticated server to the LAN | Non-loopback hosts are rejected by `overlay_set_host` and clamped to 127.0.0.1 at bind time; tests added |
| 2 | High | AES master key was always also written to plaintext `slate.key` beside the ciphertext | File is only written when the keyring is unusable (0600 at creation); existing file is migrated into the keyring and removed once a round-trip verifies |
| 3 | Medium | MCP URL token (the credential) was written to the log | Log shows `<token>` placeholder |
| 4 | Medium | Logout left the previous account's `queue.json`/`modes.json` on disk | Removed on logout |
| 5 | Low | Settings/state files were written non-atomically | Write to temp file, then rename |
| 6 | Low | `.gitignore` did not cover local account/session files | Added settings/key/queue entries |
| 7 | Medium | A failed page load left its error banner up on every later page | Banner cleared on navigation (`pages.rs`) |
| 8 | Medium | Volume drag spawned one blocking settings write per pointer event, possibly out of order | Persistence debounced (250 ms), newest value wins |
| 9 | High | MCP and overlay servers accepted any `Host` header, so a web page could use DNS rebinding to read the overlay's now-playing data (and reach MCP if the token leaked) | `http_util::require_loopback_host` middleware: non-loopback `Host` → 403 on both servers. Tests: `overlay::…::dns_rebinding_host_is_refused`, `tests/mcp_auth.rs` |
| 10 | Medium | Overlay page had no CSP | CSP allows only the page's own inline script by SHA-256 hash, no remote script/connect/frame; plus `nosniff`, `no-referrer`. Test checks the header and that the page has exactly one script |
| 11 | Medium | Linux single-instance socket was in the abstract namespace (no permissions): any local user could send links or take the name first and stop Slate starting | Socket file `$XDG_RUNTIME_DIR/slate.sock`, only used when that dir is owned by us and mode 0700; stale socket removed. Live-tested: second launch forwards in 6 ms. Windows pipe hardening: #8 |
| 12 | Medium | A system proxy URI captured from the environment (may contain `user:pass@`) was logged verbatim when unusable | Logged through `proxy::redact_userinfo`; test |
| 13 | Medium | Response bodies quoted in errors were cut with `&body[..body.len().min(500)]`: panics when a multi-byte UTF-8 character straddles the cut (23 sites in `tidal_api.rs`, `pages.rs`); same pattern for `client_id` in `import_session`'s log | `http_util::clip` cuts at a char boundary; test |
| 14 | High | Windows build did not compile (Linux-only ALSA writer, tray/MPRIS plumbing unused), and rust-skia has no prebuilt Windows binary with D3D + Vulkan, forcing a Skia source build | Linux-only items gated; Skia/Vulkan is Linux-only, Windows uses Skia's Direct3D path first |
| 15 | High | Windows bundle: `gst-plugin-scanner.exe` was in `gst-plugins\`, where Windows cannot find the GStreamer DLLs it needs; `libsoup` (dlopened by `gstsoup`) and GIO's TLS module were never bundled, so HTTPS streams could not play | Scanner beside `slate.exe`, libsoup and `gio\modules\gioopenssl.dll` bundled, `GIO_MODULE_DIR` set at startup |
| 16 | High | AppImage: same runtime-loaded libsoup / GIO TLS gap (worked on the build machine only because the host had them) | Both bundled, `GIO_MODULE_DIR` set by the AppRun hook; CI self-test runs with the host's GStreamer plugins, GIO modules and libsoup hidden |
| 17 | High | With no usable Vulkan driver the app reported `skia-vulkan` active and showed **no window** (Skia creates its surface lazily, after the fallback check) | Vulkan probed with `ash` (instance + physical device) before selection |
| 18 | High | Same silent failure for GL renderers when EGL is unusable | First-frame watchdog on GL renderers: no frame in 6 s → window hidden, next renderer. Verified: no Vulkan + no EGL ends in the software renderer with one window and one MPRIS name |
| 19 | Low | Wayland `app_id` was empty, so docks could not match `slate.desktop` (wrong icon, no grouping) | `slint::set_xdg_app_id("slate")`, also under `SLINT_BACKEND` |
| 20 | Medium | Long track lists kept a decoded thumbnail (~100 KB) per row ever shown, so scrolling a 9,000-track playlist could pin hundreds of MB beyond the image cache budget | Model keeps covers for the 6 pages nearest the viewport and refetches on return; image cache 96 → 64 MiB. Unit test |
| 21 | Low | Closing the music video dropped the pipeline (`set_state(Null)`, joins streaming threads) on the UI thread while holding a mutex | Teardown moved to a blocking worker outside the lock |
| 22 | Low | Workspace `repository` pointed at upstream SONE; crates publishable by accident | Points at Slate; `publish = false` |

## Security verification
- **Loopback only, off by default:** MCP binds `127.0.0.1` (hard-coded), overlay clamps to loopback; both default off (`defaults::mcp_enabled/overlay_enabled`).
- **MCP auth:** `tests/mcp_auth.rs` runs a real server: exact token → 200; missing, wrong, truncated, extended and upper-cased tokens → 404 with no MCP response; right token with a foreign `Host` → 403. Token is a 128-bit UUIDv4.
- **Overlay:** no CORS headers (test), `Host` check, strict CSP. Decision: **keep the inline script.** It is the overlay's only dynamic part (SSE updates for OBS), it uses `textContent` only (no `innerHTML`), and the CSP pins it by hash so nothing else can run.
- **Secrets in logs:** grep of all `log::`/`tracing::` calls; checked the real log files for token, auth-header, `sk=`, `api_sig`, signed-URL and `user:pass@` patterns: none. Only `api.tidal.com` URLs appear, without query secrets. Fixed #12.
- **On disk:** `settings.json` is AES-256-GCM (`SLAT` magic). It is created 0644; contents are encrypted, so that is acceptable, but 0600 would be tidier. `decrypt` passes plaintext through for migration, so anyone who can write the config dir can plant plaintext settings (they could equally replace the binary's config; not treated as a boundary).
- **AES-GCM:** a fresh 96-bit nonce from `OsRng` per `encrypt`; no counter or stored nonce, so no reuse path. Volume is settings saves plus cache entries, far below the ~2³² random-nonce limit per key. No rotation: the key lives in the OS keyring for the install's lifetime; deleting the keyring entry and config is the re-key path.
- **Token refresh:** both refresh paths (`TidalClient::api_request` on 401, and `tidal_report::refresh_snapshot`) call `refresh_token()` on the client held under the `tidal_client` mutex, so they serialise. Two callers that both saw a 401 can refresh twice in a row; harmless, since the second uses the already-updated refresh token.
- **Deep links / IPC:** `deeplink::parse` accepts only `track|album|artist|video` numeric IDs, hex UUID playlists ≤ 64 chars and alphanumeric mix IDs ≤ 64; no paths reach the filesystem. IPC lines are capped at 2 KiB. Socket permissions: fixed #11.
- **Panics:** clippy denies `unwrap`/`expect`/`panic!` in production code. Remaining slice-index risks are fixed (#13); other slices use `find()` results or lengths checked beforehand. Time math: timestamps are `i64`, float→int casts saturate.

### Dependency audit
`deny.toml` allows only GPLv3-compatible licences (MIT, Apache-2.0, BSD, ISC, Zlib, MPL-2.0, LGPL-2.1+, …). Slint is triple-licensed; with the `LicenseRef-Slint-*` licences not allowed it resolves to **GPL-3.0-only**. Lockfile refresh fixed crossbeam-epoch, quick-xml, anyhow, tokio and event-listener advisories. Accepted exceptions (`deny.toml`, `.cargo/audit.toml`):

| Advisory | Crate | Why accepted |
|---|---|---|
| RUSTSEC-2026-0258 | h2 0.3 (via reqwest 0.11) | Client-side DoS from a malicious server; Slate talks to fixed HTTPS hosts. Migration: #6 |
| RUSTSEC-2025-0134 | rustls-pemfile 1 | Unmaintained, via reqwest 0.11 (#6) |
| RUSTSEC-2024-0436 | paste | Unmaintained proc-macro, via gstreamer-rs |
| RUSTSEC-2026-0192 | ttf-parser | Unmaintained, via winit's Wayland decorations |
| RUSTSEC-2026-0370/0371/0372 | zvariant, zbus_names, zvariant_derive | Unmaintained, via accesskit/zbus |
| RUSTSEC-2025-0141 | bincode | Unmaintained; in the lockfile, not in the built graph |

## Memory and CPU
Linux/Wayland, logged in, Skia/Vulkan, measured from `/proc/<pid>/smaps_rollup`, CPU over the stated window.

| Scenario | PSS | RSS | Threads | FDs | CPU |
|---|---|---|---|---|---|
| Original SONE 0.22.1, idle Home (Tauri + WebKit, all processes) | ~343 MB | ~609 MB | | | 1.5–2.6 % |
| Slate, idle Home, 60 s after start | 180–183 MB | 239–243 MB | 39 | 52 | 0.10 % (10 s) |
| Slate, 9,408-track public playlist open, settled | 174 MB | 233 MB | 39 | 58 | 0.00 % (10 s) |
| Slate, playing (FLAC, normal output) | 218 MB | 283 MB | 61 | 71 | 1.85 % (60 s) |

Idle RAM stays about 47 % below the original (hard requirement met).

**Not measured:** scrolling the long playlist (no input injection here; the model test covers the memory bound), 1 h+ playback, rapid skipping (the run was stopped when the account got rate-limited), music-video frame memory. Code review of the video path: frames are dropped, not queued, when the UI is behind (`video.rs`), and one `Image` is replaced per frame.

**Event-loop review:** network, disk, image decode and GStreamer pipeline build/teardown run on tokio or blocking workers; results come back through `upgrade_in_event_loop`. The UI thread still calls `set_state(Playing/Paused)` and a flushing seek on the video pipeline; both are asynchronous in GStreamer. Fixed #21.

## Renderer fallback (offline profile, `XDG_CONFIG_HOME` pointed at an empty dir)
| Scenario | Result |
|---|---|
| Default | Skia/Vulkan |
| `VK_ICD_FILENAMES=/nonexistent` | Vulkan probe fails → Skia/OpenGL |
| Also `__EGL_VENDOR_LIBRARY_FILENAMES=/nonexistent` | Skia/OpenGL draws nothing → femtovg draws nothing → software, about 12 s total |
| `SLINT_BACKEND=winit-software` / `winit-femtovg` | Override honoured, no fallback attempted |

Remaining: links forwarded during a fallback session are dropped, and the working renderer is not remembered (#9).

## Feature parity (original README → Slate)
"Code" = implemented and unit-tested, not exercised live in this review. Live tests were stopped after the TIDAL account was rate-limited.

| Feature | Status |
|---|---|
| Lossless/Hi-Res streaming, quality cap and fallback | Works (live playback earlier) |
| Bit-perfect | Code: decoder → audioconvert (dither/noise-shaping off) → appsink → ALSA writer, no resampler or volume element; negotiated caps not inspected live |
| Exclusive ALSA, DAC matching, "device busy" message | Code (Linux only); Windows: #3 |
| Signal Path panel | Code, unverified live |
| ReplayGain album/track switching | Code (queue context decides), unverified live |
| Autoplay | Works: skipping past the queue end continued with radio tracks |
| Gapless (1.24+ with fallback) | Code, unit-tested; not heard |
| GStreamer errors in UI | Code (`audio-error` event → toast) |
| Music video | Code; HLS starts; minimise into bar missing (#15) |
| Loved videos, Videos tabs | Missing (#12) |
| Themes (15 presets, picker, light/dark) | Works |
| Lyrics, full-screen player, miniplayer | Code; not exercised in this review |
| Queue persistence | Works |
| MPRIS | Works (play, pause, next, volume via `playerctl`) |
| Tray | Linux code; Windows #2 |
| Keyboard shortcuts | In-app work; overlay missing (#13); global #5 |
| Library + folders | Works; sorting missing (#14) |
| Share links | Code (copy link to clipboard) |
| Deep links + single instance | Works (`tidal://album/…`, `tidal://playlist/…`) |
| Profile editing | Code |
| Explore | Missing (#11) |
| MCP server | Works (integration test) |
| OBS overlay | Works (tests) |
| Scrobbling, play reporting | Code; live submission unverified |
| Discord presence | Works, but under SONE's app ID (#7) |
| Proxy (HTTP/HTTPS/SOCKS5, auth, fail-closed) | Code, 135 tests; not tested against a live proxy |
| Auth: device code, PKCE browser, expiry, logout | Device code works (existing session); wiremock tests cover refresh/expiry; logout wipe tested |

## Windows status
CI builds, lints and tests pass. Packaging dry run: the AppImage job passes (including the bundled-plugins/HTTPS self-test with host libraries hidden); Windows packaging is being iterated. Known gaps, all filed: SMTC (#1), tray (#2), WASAPI exclusive (#3), sleep inhibit (#4), global shortcuts (#5), pipe ACL (#8).

## Open issues
#1–#15 on GitHub; see the tables above for which finding each one tracks.
