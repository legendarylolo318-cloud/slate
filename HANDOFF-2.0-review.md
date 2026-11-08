# Slate 2.0 review — handoff (did not finish)

Stopped mid-review at the user's request. Nothing is pushed, tagged or version-bumped.
Base v1.9.0 = `c7879ca`; Sonnet's 2.0 work as handed over = `6117841`.
Note: the brief calls the handoff `HANDOFF-2.0.md`; the file in the tree is `HANDOFF.md`
(Sonnet's working note). Both it and this file must be deleted before release.

## Commits so far (all local, author "Slate dev", one finding each, row in REVIEW.md)

| # | Commit | Finding |
|---|---|---|
| 151 | `c13fa09` | High. Alarm slept on tokio's monotonic clock: a suspend delayed the ring by the suspend's length, then it was skipped as missed. Now wall-clock steps of ≤ 60 s (1 wakeup/min while armed only). Also snooze. |
| 152 | `baa5835` | Windows alarm used *now's* UTC offset for future times → an hour off across DST. Now `SystemTimeToTzSpecificLocalTime` at the asked time. **Compiled only by Windows CI (not yet run).** |
| 153 | `ee2e018` | `parse_time("1é1")` panicked (`split_at` inside a char). Confirmed with a standalone `split_at`. |
| 154 | `eb74587` | Gapless advance cleared EQ/crossfeed rows in Signal Path though the pipeline kept them (EQ since 1.3). **Reproduced live on the handed-over build.** |
| 155 | `fd26bb4` | EQ/crossfeed switched on mid-album never applied (gapless reuses the pipeline; next track is prerolled at track *start*, seen in logs). Now `stage_missing` declines/detaches the preroll. **Reproduced live on handed-over; fix not yet checked live.** |
| 156 | `c911989` | Windows bundle never shipped `gstbs2b` (explicit plugin list). Added; `--self-test` prints an `info crossfeed (bs2b)` line. |
| 157 | `0991e80` | Import / queue link begun before sign-out could act after it. Account epoch (`Backend::account_epoch/same_account`). |
| 158 | `9b959ba` | Device memory restored EQ but the EQ page kept the old bands. |
| 159 | `0afe5c6` | Alarm flag re-read `ui.json` at sign-in; sign-out left `alarm_armed` set. **REVIEW row text needs correcting:** inotify counts showed `ui.json` is opened 10–13 times at start-up in *both* 1.9.0 and 2.0, so the extra read is inside noise; keep the fix, reword the row (the sign-out half is the real part). |
| 160 | `031bc8f` | Closing the import review / queue-link dialog kept their data in memory. |
| 161 | `5b9d14c` | Medium. Import caps checked after parsing: 48 MB of `{}` → 798 MB, 50 MB CSV of commas → 1,211 MB peak. Now an element budget (`Capped`) and CSV column/cell caps → 108–125 MB (the input itself). |

`cargo clippy --workspace --all-targets -D warnings` and `cargo test -p slate` / the touched
core tests passed after these; the full gate (fmt, clippy ×3, `cargo test --workspace`) has
**not** been run on the final tree.

## Uncommitted in the working tree

- `crates/slate-app/ui/widgets.slint`: `overflow: elide` on Segmented labels. Pre-existing bug
  (not 2.0): Settings → Playback → Exclusive device with 3 long device names draws them on top
  of each other (seen live). Would be #162; **not yet checked in a screenshot**.
- `README.md`: 2.0 feature bullets, 16 presets, bs2b build note. Ready, uncommitted.

## Measured so far (this machine, headless KWin + Xwayland :88, null sink `slate-null`)

- Binaries: v1.9.0 73,639,192 B (identical to the 1.9.0 changelog); 2.0 as handed over
  75,831,192 B (+2.19 MB). `~/slate-bench/slate-190`, `slate-200h`.
- Gapless md5 through concat → audioconvert → audioresample → volume ×2 (crossfeed off adds
  nothing): 24/192 and 16/44 both equal to the source (`78c5b696…`, `1f22bd4b…`).
- P7 (queue file v2), release, interleaved ×3 (`time_a_big_snapshot` + load timing patch):
  size 6,032,079 → 3,060,025 B; save 3.05–3.83 vs 3.35–3.76 ms (same); load 24.8 ms →
  14.5–14.7 ms. Win reproduces → keep. Downgrade: a v2 file read by 1.9 loses the source's
  track list (repeat-all), not the queue — say so in the changelog.
- **P1–P5 do not exist**: Sonnet's handoff says none were started; only P7 has code.
- One idle 2.0-handed-over run: 22 threads, 0 wakeups/s, 0 ticks, 54.4/126.8 MB (not a table run).
- Hostile import files: see #161. Apple `Library.xml` at the 2M-node cap peaks at 157 MB
  (left; mention under Considered/Not tested).
- Signal Path panel is a snapshot taken when opened (does not update while open) — pre-existing,
  by design; don't mistake it for a bug when testing.

## Not done (everything below is still owed)

1. Final release build: `cargo build --release -p slate` was running in `target/` (≈10 min, fat
   LTO); it does **not** include the widgets.slint change. Rebuild after committing.
2. Perf table, interleaved, columns v1.9.0 / handed over / final, 1.9 rows + FDs + per-thread
   wakeups + pause/volume/seek latency: `review2/run.sh` (idle ×2, play ×4), `maptime.py`,
   `bigq.sh` (P7 restored RAM), tray VRAM/restore, MPD track-start. None run yet.
3. Live feature checks on the final build: crossfeed mid-track/gapless/crossfade (verify #155),
   queue link (copy via Queue ⋯, xclip; hostile links via `link.sh`), device switch
   (use `review2/fakebin/pactl` with `FAKE_SINK_FILE` + PATH — never change the real default
   sink), alarm ring/snooze/restart/missed, sign-out wipes devices/alarm/queue files, reduce
   motion, High Contrast, mini/strip close-reopen FDs, throttled proxy 1 MB/s & 250 kB/s
   (`thrott_proxy.py`, profreset `proxy:PORT`; check no `buffering` <100 % after start and
   MPD elapsed tracks wall time), party 1.9 ↔ 2.0 both ways (Settings → Party → Start; guest =
   second instance launched with the link, separate `RUNID`).
4. 200 single-core passes: `review2/stress.sh` (157 thread/lock tests, plan from
   `threadtests.py` + `mkstress.py`; ~49 s/pass ≈ 2.7 h). Started and stopped at pass 0 — rerun.
   Don't rebuild debug tests in `target/` while it runs.
5. Full gate three ways; read CI log for Windows (#152 code).
6. CHANGELOG 2.0.0 (draft Added/Changed in `review2/cl_added.md`), README commit, version
   bump to 2.0.0, delete `HANDOFF.md` and this file, push main, wait for CI (Ubuntu + Windows),
   then tag v2.0.0 and watch the release (check the self-test `info crossfeed` line in both jobs).

## Hard rules learned this session

- `/run/user/1000/slate.sock` exists (stale) — every test launch must use its own
  `XDG_RUNTIME_DIR` (the review2 scripts do: private dir with pipewire-0/pulse symlinks).
- Profile copies live in `~/slate-bench/runs/p.XXXXXX`; delete each by literal path after use
  (none left now). Never launch on `~/.config/slate`.
- GitHub binary downloads 504 here: new target dirs can't fetch Skia; clone an existing one
  (`cp -a --reflink=auto ~/slate-bench/tgt-final …`).
- `review2/display.sh` starts KWin (`--socket wl-bench`) + `Xwayland :88`; both still running.
  Null sinks `slate-null` (s24le/192k) and `slate-null2` are loaded in the user's PipeWire —
  unload (`pactl unload-module`) when done.
