//! Wake-up alarm (Settings → General). Rings at a chosen time on chosen days, fades in
//! from silence, and can be snoozed.
//!
//! Costs nothing while no alarm is set: no thread, no timer, no file is read at start-up
//! (`ui.json`, which is read anyway, says whether one is armed). While one is armed there is
//! exactly one sleeping task, set to the next ring — never a poll. It is checked again when
//! it wakes (`slate_core::alarm::check`): a computer that slept through the time skips the
//! ring with a notice, a clock that moved is followed. The alarm lives in the encrypted
//! state folder (`alarm.json`), survives restarts and is forgotten at sign-out.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use slate_core::alarm::{self, Alarm, Check, What};
use slate_core::commands::utility;
use slate_core::AppState;
use slint::ComponentHandle;
use tokio::task::AbortHandle;

use crate::backend::Backend;
use crate::bootstrap::SettingsState;
use crate::textutil::clean_line;

#[derive(Default)]
struct Rt {
    /// `None` until loaded (and again after sign-out).
    alarm: Option<Alarm>,
    timer: Option<AbortHandle>,
    ramp: Option<AbortHandle>,
    /// Volume before the alarm took over, put back on snooze or stop.
    volume_before: Option<f32>,
}

static RT: Mutex<Option<Rt>> = Mutex::new(None);
/// An alarm is ringing (from the ring until stop, snooze or ten minutes).
static RINGING: AtomicBool = AtomicBool::new(false);
static EPOCH: AtomicU64 = AtomicU64::new(0);
/// From sign-out until the next sign-in nothing is read or written. Held with `FILE` so a
/// write cannot land after the core deleted the file (#139/#144).
static SIGNED_OUT: AtomicBool = AtomicBool::new(false);
static FILE: Mutex<()> = Mutex::new(());

fn rt() -> std::sync::MutexGuard<'static, Option<Rt>> {
    RT.lock().unwrap_or_else(|e| e.into_inner())
}

/// The UTC offset in seconds at Unix time `t`.
fn local_offset_at(t: i64) -> i64 {
    #[cfg(unix)]
    {
        let tt = libc::time_t::try_from(t).unwrap_or(0);
        // SAFETY: `tm` is plain data that localtime_r fully initialises.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        if unsafe { libc::localtime_r(&tt, &mut tm) }.is_null() {
            return 0;
        }
        // c_long: i64 here, i32 on 32-bit targets.
        #[allow(clippy::useless_conversion)]
        let off = i64::from(tm.tm_gmtoff);
        off
    }
    #[cfg(windows)]
    {
        use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
        let _ = t;
        let mut tz = TIME_ZONE_INFORMATION::default();
        // SAFETY: `tz` is a valid out-pointer; the call fills it.
        let id = unsafe { GetTimeZoneInformation(&mut tz) };
        // 2 = daylight time; the answer is for now, and is checked again when the timer fires.
        let bias = tz.Bias
            + if id == 2 {
                tz.DaylightBias
            } else {
                tz.StandardBias
            };
        -i64::from(bias) * 60
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = t;
        0
    }
}

fn now() -> i64 {
    i64::try_from(slate_core::now_secs()).unwrap_or(0)
}

/// Sign-out: nothing of the old account stays, no timer is left, nothing is written.
pub fn forget() {
    let _file = FILE.lock().unwrap_or_else(|e| e.into_inner());
    SIGNED_OUT.store(true, Ordering::Release);
    EPOCH.fetch_add(1, Ordering::AcqRel);
    RINGING.store(false, Ordering::Release);
    if let Some(r) = rt().take() {
        for h in [r.timer, r.ramp].into_iter().flatten() {
            h.abort();
        }
    }
}

pub fn allow() {
    SIGNED_OUT.store(false, Ordering::Release);
}

pub fn ringing() -> bool {
    RINGING.load(Ordering::Acquire)
}

fn write_file(state: &AppState, a: &Alarm) {
    let _file = FILE.lock().unwrap_or_else(|e| e.into_inner());
    if SIGNED_OUT.load(Ordering::Acquire) {
        return;
    }
    if let Err(e) = state.write_state_file(alarm::FILE, &a.to_json()) {
        tracing::warn!("alarm not saved: {}", e.log_safe());
    }
}

fn read_file(state: &AppState) -> Alarm {
    if SIGNED_OUT.load(Ordering::Acquire) {
        return Alarm::default();
    }
    state
        .read_state_file(alarm::FILE)
        .and_then(|t| Alarm::parse(&t, &clean_line))
        .unwrap_or_default()
}

/// The alarm as set (read from disk once, only when Settings opens or an alarm is armed).
pub fn current(state: &AppState) -> Alarm {
    let mut g = rt();
    g.get_or_insert_with(Rt::default)
        .alarm
        .get_or_insert_with(|| read_file(state))
        .clone()
}

fn set_armed_pref(on: bool) {
    let mut p = crate::uiprefs::UiPrefs::load();
    if p.alarm_armed != on {
        p.alarm_armed = on;
        let _ = p.save();
    }
}

impl Backend {
    /// Start-up (after sign-in): arms the alarm if `ui.json` says one is set. Otherwise
    /// nothing at all happens here.
    pub(crate) fn alarm_start(&self) {
        if !crate::uiprefs::UiPrefs::load().alarm_armed {
            return;
        }
        let Some(state) = self.state() else { return };
        let this = self.clone();
        self.rt.spawn_blocking(move || {
            let a = current(&state);
            this.alarm_arm(a, true);
        });
    }

    /// Stores `a`, works out the next ring and sets the one timer for it.
    fn alarm_arm(&self, a: Alarm, startup: bool) {
        let Some(state) = self.state() else { return };
        if SIGNED_OUT.load(Ordering::Acquire) {
            return;
        }
        let mut a = a.clamped(&clean_line);
        let t = now();
        let mut note = None;
        let target = if !a.enabled {
            None
        } else if startup && a.next_fire > 0 && a.next_fire <= t {
            // Armed for a time that has gone by while the app was closed.
            match alarm::check(t, a.next_fire) {
                Check::Missed => {
                    note = Some(format!(
                        "Missed your {} alarm: Slate was closed or the computer was asleep",
                        a.time_text()
                    ));
                    alarm::next_fire(t, &a, local_offset_at)
                }
                _ => Some(a.next_fire),
            }
        } else if startup && a.next_fire > t {
            Some(a.next_fire)
        } else {
            alarm::next_fire(t, &a, local_offset_at)
        };
        a.next_fire = target.unwrap_or(0);
        let epoch = EPOCH.load(Ordering::Acquire);
        {
            let mut g = rt();
            let r = g.get_or_insert_with(Rt::default);
            if let Some(h) = r.timer.take() {
                h.abort();
            }
            r.alarm = Some(a.clone());
            if let Some(target) = target {
                let this = self.clone();
                let wait = u64::try_from((target - t).max(0)).unwrap_or(0);
                let task = self.rt.spawn(async move {
                    tokio::time::sleep(Duration::from_secs(wait)).await;
                    this.alarm_due(epoch, target);
                });
                r.timer = Some(task.abort_handle());
            }
        }
        write_file(&state, &a);
        set_armed_pref(a.enabled);
        let keep = a.enabled && a.keep_awake;
        let st = state.clone();
        self.rt.spawn(async move {
            let _ = if keep {
                utility::inhibit_idle(&st).await
            } else {
                utility::uninhibit_idle(&st).await
            };
        });
        if let Some(n) = note {
            self.alarm_bar(&n, false);
        }
        self.alarm_fill(&a);
    }

    /// The timer woke.
    fn alarm_due(&self, epoch: u64, target: i64) {
        if EPOCH.load(Ordering::Acquire) != epoch || SIGNED_OUT.load(Ordering::Acquire) {
            return;
        }
        let Some(state) = self.state() else { return };
        let a = current(&state);
        match alarm::check(now(), target) {
            Check::TooEarly => self.alarm_arm(a, false),
            Check::Missed => {
                let text = format!(
                    "Missed your {} alarm: the computer was asleep or Slate was not running",
                    a.time_text()
                );
                self.alarm_arm(a, false);
                self.alarm_bar(&text, false);
            }
            Check::Ring => {
                // The regular schedule goes on first, so a ring never costs the next one.
                self.alarm_arm(a.clone(), false);
                self.alarm_ring(&a);
            }
        }
    }

    /// Plays the source, from silence up to the chosen volume.
    fn alarm_ring(&self, a: &Alarm) {
        let Some(state) = self.state() else { return };
        RINGING.store(true, Ordering::Release);
        let before = f32::from_bits(self.shared.volume.load(Ordering::Relaxed));
        let epoch = EPOCH.load(Ordering::Acquire);
        {
            let mut g = rt();
            let r = g.get_or_insert_with(Rt::default);
            if let Some(h) = r.ramp.take() {
                h.abort();
            }
            // A second ring while one is up keeps the first volume to restore.
            r.volume_before.get_or_insert(before);
        }
        self.alarm_bar(
            &format!("Alarm {}: playing {}", a.time_text(), a.what.label()),
            true,
        );
        // Silent before the first sample, not persisted.
        self.shared.volume.store(0f32.to_bits(), Ordering::Relaxed);
        let _ = state.audio_player.set_volume(0.0);
        self.ui(|w| w.set_volume(0.0));
        match a.what.source() {
            Some((kind, id)) => self.mcp_play_source(kind.to_string(), id.to_string(), false),
            None => {
                let this = self.clone();
                self.rt.spawn(async move {
                    let Some(ctl) = this.controller() else { return };
                    let outcome = ctl.resume_current().await;
                    this.apply(outcome).await;
                });
            }
        }
        // The fade: one step a second for 30–120 s, then nothing again. Touching the volume
        // by hand (which counts `volume_seq`) hands it back at once.
        let this = self.clone();
        let (fade, target) = (a.fade_secs, a.volume);
        let seq0 = self.shared.volume_seq.load(Ordering::SeqCst);
        let task = self.rt.spawn(async move {
            for s in 1..=fade {
                tokio::time::sleep(Duration::from_secs(1)).await;
                if EPOCH.load(Ordering::Acquire) != epoch
                    || this.shared.volume_seq.load(Ordering::SeqCst) != seq0
                {
                    return;
                }
                let v = alarm::fade_volume(s as f32, fade as f32, target);
                this.shared.volume.store(v.to_bits(), Ordering::Relaxed);
                if let Some(st) = this.state() {
                    let _ = st.audio_player.set_volume(v);
                }
                this.ui(move |w| w.set_volume(v));
            }
            // Arrived: this is now the volume, saved like any other change.
            this.set_volume(target);
            if let Some(r) = rt().as_mut() {
                r.volume_before = None;
            }
        });
        if let Some(r) = rt().as_mut() {
            r.ramp = Some(task.abort_handle());
        }
        // The ringing window ends by itself after ten minutes.
        let this = self.clone();
        self.rt.spawn(async move {
            tokio::time::sleep(Duration::from_secs(alarm::SNOOZE_SECS as u64)).await;
            if EPOCH.load(Ordering::Acquire) == epoch && ringing() {
                this.alarm_end(false);
            }
        });
    }

    /// Ends the ringing state: the bar goes, the fade stops, and (with `restore`) the volume
    /// from before the alarm comes back.
    fn alarm_end(&self, restore: bool) {
        RINGING.store(false, Ordering::Release);
        let before = {
            let mut g = rt();
            let Some(r) = g.as_mut() else { return };
            if let Some(h) = r.ramp.take() {
                h.abort();
            }
            r.volume_before.take()
        };
        if restore {
            if let Some(v) = before {
                self.set_volume(v);
            }
        }
        self.ui(|w| {
            w.set_alarm_text("".into());
            w.set_alarm_ringing(false);
        });
    }

    /// Snooze: silence now, ring again in ten minutes.
    pub(crate) fn alarm_snooze(&self) {
        let Some(state) = self.state() else { return };
        if !ringing() {
            return;
        }
        let a = current(&state);
        self.alarm_end(true);
        self.alarm_pause();
        let epoch = EPOCH.load(Ordering::Acquire);
        let this = self.clone();
        let task = self.rt.spawn(async move {
            tokio::time::sleep(Duration::from_secs(alarm::SNOOZE_SECS as u64)).await;
            if EPOCH.load(Ordering::Acquire) != epoch || SIGNED_OUT.load(Ordering::Acquire) {
                return;
            }
            // The regular timer was replaced by this one: bring it back, then ring.
            let Some(state) = this.state() else { return };
            let a = current(&state);
            this.alarm_arm(a.clone(), false);
            this.alarm_ring(&a);
        });
        // Only the snooze waits now; arming again after it restores the schedule.
        if let Some(r) = rt().as_mut() {
            if let Some(h) = r.timer.replace(task.abort_handle()) {
                h.abort();
            }
        }
        self.alarm_bar(
            &format!(
                "Snoozed: rings again at {}",
                clock_text(now() + alarm::SNOOZE_SECS, &a)
            ),
            false,
        );
    }

    /// Stop (or dismiss a missed-alarm notice).
    pub(crate) fn alarm_stop(&self) {
        let was = ringing();
        self.alarm_end(true);
        if was {
            self.alarm_pause();
        }
    }

    fn alarm_pause(&self) {
        if self.shared.playing.load(Ordering::Relaxed) {
            self.toggle_play();
        }
    }

    fn alarm_bar(&self, text: &str, ringing: bool) {
        let text = text.to_string();
        self.ui(move |w| {
            w.set_alarm_text(text.into());
            w.set_alarm_ringing(ringing);
        });
    }

    /// Fills the Settings rows.
    fn alarm_fill(&self, a: &Alarm) {
        let a = a.clone();
        let when = (a.enabled && a.next_fire > 0).then(|| clock_text(a.next_fire, &a));
        self.ui(move |w| {
            let g = w.global::<SettingsState>();
            g.set_alarm_enabled(a.enabled);
            g.set_alarm_time(a.time_text().into());
            g.set_alarm_days(i32::from(a.days));
            g.set_alarm_what(a.what.label().into());
            g.set_alarm_fade_index(
                alarm::FADE_CHOICES
                    .iter()
                    .position(|s| *s == a.fade_secs)
                    .unwrap_or(1) as i32,
            );
            g.set_alarm_volume_index(
                alarm::VOLUME_CHOICES
                    .iter()
                    .position(|v| (*v - a.volume).abs() < 0.01)
                    .unwrap_or(1) as i32,
            );
            g.set_alarm_keep_awake(a.keep_awake);
            g.set_alarm_status(
                when.map(|t| format!("Next alarm: {t}"))
                    .unwrap_or_else(|| "No alarm set".into())
                    .into(),
            );
        });
    }

    /// Settings page opened: shows the alarm as set (read from disk once, here).
    pub(crate) fn alarm_settings_opened(&self) {
        let Some(state) = self.state() else { return };
        let this = self.clone();
        self.rt.spawn_blocking(move || {
            let a = current(&state);
            this.alarm_fill(&a);
        });
    }

    /// One edit from the Settings rows.
    pub(crate) fn alarm_edit(&self, key: &str, value: AlarmEdit) {
        let Some(state) = self.state() else { return };
        let key = key.to_string();
        let this = self.clone();
        self.rt.spawn_blocking(move || {
            let mut a = current(&state);
            match (key.as_str(), value) {
                ("alarm-enabled", AlarmEdit::Bool(v)) => a.enabled = v,
                ("alarm-keep-awake", AlarmEdit::Bool(v)) => a.keep_awake = v,
                ("alarm-day", AlarmEdit::Int(d)) if (0..7).contains(&d) => {
                    a.days ^= 1 << d;
                    if a.days & alarm::ALL_DAYS == 0 {
                        // At least one day: switching the last one off is refused.
                        a.days ^= 1 << d;
                        this.notify("An alarm needs at least one day");
                    }
                }
                ("alarm-fade", AlarmEdit::Int(i)) => {
                    a.fade_secs = usize::try_from(i)
                        .ok()
                        .and_then(|i| alarm::FADE_CHOICES.get(i))
                        .copied()
                        .unwrap_or(a.fade_secs)
                }
                ("alarm-volume", AlarmEdit::Int(i)) => {
                    a.volume = usize::try_from(i)
                        .ok()
                        .and_then(|i| alarm::VOLUME_CHOICES.get(i))
                        .copied()
                        .unwrap_or(a.volume)
                }
                ("alarm-time", AlarmEdit::Text(t)) => match alarm::parse_time(&t) {
                    Some(m) => a.minute_of_day = m,
                    None => {
                        this.notify("Enter the time as 07:30");
                        this.alarm_fill(&a);
                        return;
                    }
                },
                ("alarm-what", AlarmEdit::What(w)) => a.what = w,
                _ => return,
            }
            this.alarm_arm(a, false);
        });
    }

    /// "Use what is playing": the source of the queue as the alarm's.
    pub(crate) fn alarm_use_playing(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(ctl) = this.controller() else { return };
            let src = ctl.with_queue(|q| q.playback_source.clone()).await;
            let what = src.and_then(|s| {
                let id = match &s.id {
                    serde_json::Value::String(t) => t.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    _ => return None,
                };
                let name = s.name.clone();
                match s.kind.as_str() {
                    "playlist" => Some(What::Playlist { id, name }),
                    "album" => Some(What::Album { id, name }),
                    "mix" => Some(What::Mix { id, name }),
                    _ => None,
                }
            });
            match what {
                Some(w) => this.alarm_edit("alarm-what", AlarmEdit::What(w)),
                None => this.notify(
                    "What is playing is not a playlist, album or mix; paste a link instead",
                ),
            }
        });
    }

    /// A pasted TIDAL link as the alarm's source.
    pub(crate) fn alarm_use_link(&self, link: &str) {
        use crate::deeplink::DeepLink;
        let what = match crate::deeplink::parse(link) {
            Some(DeepLink::Playlist(id)) => Some(What::Playlist {
                id,
                name: "from a link".into(),
            }),
            Some(DeepLink::Album(n)) => Some(What::Album {
                id: n.to_string(),
                name: "from a link".into(),
            }),
            Some(DeepLink::Mix(id)) => Some(What::Mix {
                id,
                name: "from a link".into(),
            }),
            _ => None,
        };
        match what {
            Some(w) => self.alarm_edit("alarm-what", AlarmEdit::What(w)),
            None => self.notify("Paste a TIDAL playlist, album or mix link"),
        }
    }
}

pub(crate) enum AlarmEdit {
    Bool(bool),
    Int(i32),
    Text(String),
    What(What),
}

/// "Tue 07:30" for a Unix time, in local time.
fn clock_text(t: i64, _a: &Alarm) -> String {
    clock_text_at(t, local_offset_at(t))
}

fn clock_text_at(t: i64, offset: i64) -> String {
    let local = t + offset;
    let day = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
        [((local.div_euclid(86_400) + 3).rem_euclid(7)) as usize];
    let m = local.rem_euclid(86_400) / 60;
    format!("{day} {:02}:{:02}", m / 60, m % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_text_names_the_local_weekday_and_time() {
        // 2024-01-01 00:00 UTC was a Monday.
        assert_eq!(clock_text_at(1_704_067_200, 0), "Mon 00:00");
        assert_eq!(clock_text_at(1_704_067_200, -3600), "Sun 23:00");
        assert_eq!(
            clock_text_at(1_704_067_200 + 7 * 3600 + 1800, 0),
            "Mon 07:30"
        );
    }

    #[test]
    fn the_local_offset_is_a_sane_number() {
        // Whatever the machine's zone is, it is within the real range of UTC offsets.
        assert!((-14 * 3600..=14 * 3600).contains(&local_offset_at(now())));
    }
}
