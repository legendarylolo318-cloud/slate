# Review log (in progress)

Verified on Linux: `cargo build --release`, `cargo test --workspace` (all pass), `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`.

## Bugs found and fixed
| # | Severity | Finding | Fix |
|---|---|---|---|
| 1 | High | OBS overlay host was user-configurable (`0.0.0.0` accepted), exposing an unauthenticated server to the LAN | Non-loopback hosts are rejected by `overlay_set_host` and clamped to 127.0.0.1 at bind time; tests added |
| 2 | High | AES master key was always also written to plaintext `slate.key` beside the ciphertext | File is only written when the keyring is unusable (0600 at creation); existing file is migrated into the keyring and removed once a round-trip verifies |
| 3 | Medium | MCP URL token (the credential) was written to the log | Log shows `<token>` placeholder |
| 4 | Medium | Logout left the previous account's `queue.json`/`modes.json` on disk | Removed on logout |
| 5 | Low | Settings/state files were written non-atomically (crash could corrupt them) | Write to temp file, then rename |
| 6 | Low | `.gitignore` did not cover local account/session files | Added settings/key/queue entries |

## Confirmed OK
- Token refresh is serialised by the `tidal_client` mutex (no concurrent refresh).
- MCP binds 127.0.0.1 only; token is a UUIDv4 path segment; wrong token gives 404. Both servers default off.
- AES-GCM uses a fresh random 96-bit nonce per encryption.

## Not yet reviewed / known gaps
Audio bit-perfect caps, renderer fallback, memory/CPU measurement, packaging (AppImage, Windows), CI, cargo audit/deny, clean-container tests. Windows cannot be tested from this machine.

## UI bug found while taking screenshots
| 7 | Medium | A failed page load (e.g. a 404 album) left its red error banner up on every later page, even healthy ones, until restart | The banner is cleared whenever the user navigates (`pages.rs`, `render`) |

## Memory and CPU (measured 2026-10-29, Linux/Wayland, logged in, Home page idle, ~1 min after start)
| App | Proportional (PSS) | Resident (RSS) sum | CPU idle |
|---|---|---|---|
| Original SONE 0.22.1 (Tauri + WebKit: main + web + network processes) | ~343 MB | ~609 MB | ~1.5–2.6 % |
| Slate | ~175 MB | ~243 MB | ~2 % |

Slate uses about half the memory of the original. Its decoded-image cache is capped at 96 MiB. Not yet measured: a 5,000-track playlist and long sessions.
