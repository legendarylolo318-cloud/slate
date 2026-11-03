# 1.5.0 handoff (paused mid-fix)

## State
- Seven 1.5 features, release prep and the 10 review fixes are committed on `main` (not tagged).
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings` and `cargo test --workspace`
  all pass on the final commit.

## The 10 review fixes
1. `dj_voice.rs`: each spoken line owns its processes (`Procs`), only the current line restores volume;
   new test `a_cut_off_line_does_not_touch_the_one_that_replaced_it`.
2/3. `mpd_host.rs`: Delete/Move/PlayPos/Stop/Clear/Resume are awaited (no fire-and-forget), so multi-delete
   order is right and `seek N t` seeks after the new song has started.
4. `mpd/mod.rs`: `Secret` newtype (Debug shows `<set>`), `Settings.mpd_password` uses it; custom `Debug`
   for `mpd::Config`.
5. `window_state.rs`: first observation while maximized remembers 80 % of the screen as the restored size.
6. `mpd/server.rs`: `PREAUTH_TIMEOUT` 30 s for peers without the password (`read_timeout`). Only the constant
   is unit-tested, not a real timeout.
7. `dj_voice.rs`: Windows temp wav name includes the line's generation (untested on Windows).
8. `party/net.rs` + `chat.rs`: late joiners get the last 20 chat messages (`Chat::recent`); loopback test extended.
9. Chat panel draws `chat-recent` (newest 8) and `chat-count` (replaces `chat-lines`).
10. `tests/party_net.rs`: mute is tested on a guest who has not chatted, no 10 s sleep.

## Still to do
- Measurements: only the default login-screen table exists (CHANGELOG). Playing, VRAM and the
  "all features on" table were not measured. Nothing was tried with a real MPD client or a signed-in account.
- Weakly covered: the 30 s pre-auth timeout (constant only), the Windows temp-file name (Windows untested).
- Do not tag or publish.

Note: this file is `HANDOFF-1.5.md` (not `handoff.md`) because `HANDOFF.md` is the tracked architecture doc.
