//! Wake-up alarm: when it next rings, whether a ring was missed, and the file format. Pure;
//! the app owns the one timer (armed only while an alarm is set) and what it plays.
//!
//! Local time comes in as a function from a Unix time to the UTC offset then, so the tests
//! can put a daylight-saving change in the way. The timer is armed from an estimate and
//! looked at again when it fires ([`check`]), so a clock change or a DST switch in between
//! cannot ring at the wrong hour.

use serde::{Deserialize, Serialize};

/// Name of the encrypted state file.
pub const FILE: &str = "alarm.json";
/// An alarm later than this (seconds) when the app notices it (asleep, closed) is skipped.
pub const MISSED_AFTER_SECS: i64 = 5 * 60;
/// Snooze length.
pub const SNOOZE_SECS: i64 = 10 * 60;
pub const FADE_MIN_SECS: u32 = 30;
pub const FADE_MAX_SECS: u32 = 120;
pub const FADE_CHOICES: [u32; 4] = [30, 60, 90, 120];
pub const VOLUME_CHOICES: [f32; 4] = [0.25, 0.5, 0.75, 1.0];
pub const ALL_DAYS: u8 = 0b111_1111;
/// A file bigger than this is not ours.
pub const MAX_FILE_BYTES: usize = 16 * 1024;
const MAX_NAME_CHARS: usize = 100;
const DAY: i64 = 86_400;

/// What rings. Ids are checked on the way in: a playlist is a UUID, an album a number, a mix
/// an alphanumeric id.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum What {
    /// Whatever the queue holds (the playing track again, or the first one).
    #[default]
    Queue,
    Playlist {
        id: String,
        name: String,
    },
    Album {
        id: String,
        name: String,
    },
    Mix {
        id: String,
        name: String,
    },
}

impl What {
    /// The `kind` string `play_source` takes, and the id.
    pub fn source(&self) -> Option<(&'static str, &str)> {
        match self {
            Self::Queue => None,
            Self::Playlist { id, .. } => Some(("playlist", id)),
            Self::Album { id, .. } => Some(("album", id)),
            Self::Mix { id, .. } => Some(("mix", id)),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Queue => "the queue".into(),
            Self::Playlist { name, .. } => format!("playlist “{name}”"),
            Self::Album { name, .. } => format!("album “{name}”"),
            Self::Mix { name, .. } => format!("mix “{name}”"),
        }
    }

    fn valid(&self) -> bool {
        let uuid = |s: &str| {
            !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
        };
        match self {
            Self::Queue => true,
            Self::Playlist { id, .. } => uuid(id),
            Self::Album { id, .. } => {
                !id.is_empty() && id.len() <= 20 && id.chars().all(|c| c.is_ascii_digit())
            }
            Self::Mix { id, .. } => {
                !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric())
            }
        }
    }

    fn cleaned(self, clean: &dyn Fn(&str) -> String) -> Self {
        let name = |n: String| {
            clean(&n.chars().take(MAX_NAME_CHARS * 4).collect::<String>())
                .chars()
                .take(MAX_NAME_CHARS)
                .collect()
        };
        match self {
            Self::Queue => Self::Queue,
            Self::Playlist { id, name: n } => Self::Playlist { id, name: name(n) },
            Self::Album { id, name: n } => Self::Album { id, name: name(n) },
            Self::Mix { id, name: n } => Self::Mix { id, name: name(n) },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Alarm {
    pub enabled: bool,
    /// Minutes after local midnight, 0..1440.
    pub minute_of_day: u16,
    /// Bit 0 Monday … bit 6 Sunday.
    pub days: u8,
    #[serde(default)]
    pub what: What,
    pub fade_secs: u32,
    /// The volume the fade-in reaches, 0.05..=1.
    pub volume: f32,
    /// Keep the computer awake while the alarm is armed (off by default).
    #[serde(default)]
    pub keep_awake: bool,
    /// The ring the timer is armed for (Unix seconds); 0 when none.
    #[serde(default)]
    pub next_fire: i64,
}

impl Default for Alarm {
    fn default() -> Self {
        Self {
            enabled: false,
            minute_of_day: 7 * 60,
            days: ALL_DAYS,
            what: What::Queue,
            fade_secs: 60,
            volume: 0.5,
            keep_awake: false,
            next_fire: 0,
        }
    }
}

impl Alarm {
    /// Values inside their ranges; an alarm with no valid source rings the queue, and one
    /// with no day rings every day.
    pub fn clamped(mut self, clean: &dyn Fn(&str) -> String) -> Self {
        self.minute_of_day = self.minute_of_day.min(24 * 60 - 1);
        self.days &= ALL_DAYS;
        if self.days == 0 {
            self.days = ALL_DAYS;
        }
        self.fade_secs = self.fade_secs.clamp(FADE_MIN_SECS, FADE_MAX_SECS);
        self.volume = if self.volume.is_finite() {
            self.volume.clamp(0.05, 1.0)
        } else {
            0.5
        };
        self.what = if self.what.valid() {
            self.what.cleaned(clean)
        } else {
            What::Queue
        };
        if self.next_fire < 0 {
            self.next_fire = 0;
        }
        self
    }

    pub fn parse(text: &str, clean: &dyn Fn(&str) -> String) -> Option<Self> {
        if text.len() > MAX_FILE_BYTES {
            return None;
        }
        serde_json::from_str::<Self>(text)
            .ok()
            .map(|a| a.clamped(clean))
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    /// "07:30".
    pub fn time_text(&self) -> String {
        format!(
            "{:02}:{:02}",
            self.minute_of_day / 60,
            self.minute_of_day % 60
        )
    }
}

/// "7:30" / "07:30" / "0730" → minutes after midnight.
pub fn parse_time(s: &str) -> Option<u16> {
    let s = s.trim();
    let (h, m) = match s.split_once(':') {
        Some((h, m)) => (h.trim(), m.trim()),
        None if s.len() == 4 => s.split_at(2),
        None => return None,
    };
    if h.is_empty() || h.len() > 2 || m.len() != 2 {
        return None;
    }
    let (h, m): (u16, u16) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

/// 0 = Monday … 6 = Sunday, for local seconds since 1970-01-01 00:00 (a Thursday).
fn weekday(local_secs: i64) -> u8 {
    ((local_secs.div_euclid(DAY) + 3).rem_euclid(7)) as u8
}

/// The next ring strictly after `now` (Unix seconds), at most a week ahead, or None for an
/// alarm that is off. `offset_at(t)` is the local UTC offset in seconds at Unix time `t`.
pub fn next_fire(now: i64, alarm: &Alarm, offset_at: impl Fn(i64) -> i64) -> Option<i64> {
    if !alarm.enabled || alarm.days & ALL_DAYS == 0 {
        return None;
    }
    let off_now = offset_at(now);
    let midnight = (now + off_now).div_euclid(DAY) * DAY;
    // One day before to one day after the week, so a DST jump cannot hide the answer.
    for d in 0..=8 {
        let local = midnight + d * DAY + i64::from(alarm.minute_of_day) * 60;
        if alarm.days & (1 << weekday(local)) == 0 {
            continue;
        }
        // The offset at the moment of ringing, which may differ from now's across a switch.
        let guess = local - off_now;
        let fire = local - offset_at(guess);
        if fire > now {
            return Some(fire);
        }
    }
    None
}

/// What to do when the timer fires (or the app starts) with a ring armed for `target`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// Ring now.
    Ring,
    /// Not yet (the clock moved back or a DST switch): arm again for the real time.
    TooEarly,
    /// Later than [`MISSED_AFTER_SECS`] (the computer slept, or the app was closed): skip it.
    Missed,
}

pub fn check(now: i64, target: i64) -> Check {
    if now < target - 30 {
        Check::TooEarly
    } else if now - target > MISSED_AFTER_SECS {
        Check::Missed
    } else {
        Check::Ring
    }
}

/// The volume at `elapsed` seconds of a `fade_secs` fade to `volume`: from silence, rising
/// smoothly (squared, which the ear takes for even), never above the target.
pub fn fade_volume(elapsed: f32, fade_secs: f32, volume: f32) -> f32 {
    let t = (elapsed / fade_secs.max(1.0)).clamp(0.0, 1.0);
    volume * t * t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(s: &str) -> String {
        s.trim().to_string()
    }

    /// 2024-01-01 (a Monday) 00:00 UTC.
    const MON: i64 = 1_704_067_200;

    fn alarm(h: u16, m: u16, days: u8) -> Alarm {
        Alarm {
            enabled: true,
            minute_of_day: h * 60 + m,
            days,
            ..Alarm::default()
        }
    }

    #[test]
    fn time_text_is_parsed_in_the_usual_forms() {
        assert_eq!(parse_time("07:30"), Some(450));
        assert_eq!(parse_time("7:05"), Some(425));
        assert_eq!(parse_time("0730"), Some(450));
        assert_eq!(parse_time(" 23:59 "), Some(1439));
        for bad in [
            "24:00", "7:5", "12:60", "", "ab:cd", "7", "007:30", "-1:30", "12:3a",
        ] {
            assert_eq!(parse_time(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn weekdays_count_from_monday() {
        assert_eq!(weekday(MON), 0);
        assert_eq!(weekday(MON + 6 * DAY), 6);
        assert_eq!(weekday(MON + 7 * DAY), 0);
        assert_eq!(weekday(0), 3, "1970-01-01 was a Thursday");
        assert_eq!(weekday(-1), 2, "and the second before it a Wednesday");
    }

    #[test]
    fn the_next_ring_is_today_if_it_is_still_ahead_else_the_next_day() {
        let a = alarm(7, 30, ALL_DAYS);
        assert_eq!(
            next_fire(MON + 3600, &a, |_| 0),
            Some(MON + 7 * 3600 + 1800)
        );
        assert_eq!(
            next_fire(MON + 8 * 3600, &a, |_| 0),
            Some(MON + DAY + 7 * 3600 + 1800)
        );
        // Exactly at the time is not "after now".
        assert_eq!(
            next_fire(MON + 7 * 3600 + 1800, &a, |_| 0),
            Some(MON + DAY + 7 * 3600 + 1800)
        );
    }

    #[test]
    fn only_the_chosen_days_ring() {
        let wed_fri = alarm(6, 0, 0b001_0100);
        assert_eq!(
            next_fire(MON, &wed_fri, |_| 0),
            Some(MON + 2 * DAY + 6 * 3600)
        );
        let after_fri = MON + 4 * DAY + 7 * 3600;
        assert_eq!(
            next_fire(after_fri, &wed_fri, |_| 0),
            Some(MON + 9 * DAY + 6 * 3600),
            "next Wednesday"
        );
        let only_today = alarm(23, 0, 1);
        assert_eq!(
            next_fire(MON + 23 * 3600 + 1, &only_today, |_| 0),
            Some(MON + 7 * DAY + 23 * 3600)
        );
    }

    #[test]
    fn local_time_uses_the_offset() {
        // UTC-5: 07:30 local is 12:30 UTC.
        let a = alarm(7, 30, ALL_DAYS);
        assert_eq!(
            next_fire(MON, &a, |_| -5 * 3600),
            Some(MON + 12 * 3600 + 1800)
        );
        // UTC+9 at 22:00 UTC Monday is 07:00 Tuesday local: 07:30 is ahead, same local day.
        assert_eq!(
            next_fire(MON + 22 * 3600, &a, |_| 9 * 3600),
            Some(MON + 22 * 3600 + 1800)
        );
    }

    #[test]
    fn a_daylight_saving_switch_between_now_and_the_ring_is_followed() {
        // UTC-5 until MON+DAY (standard), UTC-4 after: 07:30 local on Tuesday is 11:30 UTC.
        let off = |t: i64| if t < MON + DAY { -5 * 3600 } else { -4 * 3600 };
        let a = alarm(7, 30, ALL_DAYS);
        // Monday 08:00 local (13:00 UTC): the next ring is Tuesday 07:30 EDT = 11:30 UTC.
        assert_eq!(
            next_fire(MON + 13 * 3600, &a, off),
            Some(MON + DAY + 11 * 3600 + 1800)
        );
    }

    #[test]
    fn nothing_rings_when_off() {
        let mut a = alarm(7, 0, ALL_DAYS);
        a.enabled = false;
        assert_eq!(next_fire(MON, &a, |_| 0), None);
    }

    #[test]
    fn the_timer_is_trusted_only_near_its_time() {
        let t = 1_000_000;
        assert_eq!(check(t, t), Check::Ring);
        assert_eq!(check(t + 299, t), Check::Ring);
        assert_eq!(check(t + 300, t), Check::Ring);
        assert_eq!(check(t + 301, t), Check::Missed, "slept through it");
        assert_eq!(check(t - 29, t), Check::Ring);
        assert_eq!(check(t - 31, t), Check::TooEarly, "the clock moved back");
    }

    #[test]
    fn the_fade_starts_silent_rises_and_stops_at_the_chosen_volume() {
        assert_eq!(fade_volume(0.0, 60.0, 0.5), 0.0);
        let mut last = 0.0;
        for s in 1..=60 {
            let v = fade_volume(s as f32, 60.0, 0.5);
            assert!(v >= last && v <= 0.5);
            last = v;
        }
        assert_eq!(fade_volume(60.0, 60.0, 0.5), 0.5);
        assert_eq!(fade_volume(600.0, 60.0, 0.5), 0.5);
        assert_eq!(fade_volume(-5.0, 60.0, 0.5), 0.0);
    }

    #[test]
    fn a_file_is_read_as_hostile() {
        assert!(Alarm::parse("nope", &clean).is_none());
        assert!(Alarm::parse(&" ".repeat(MAX_FILE_BYTES + 1), &clean).is_none());
        let a = Alarm::parse(
            r#"{"enabled":true,"minute_of_day":9999,"days":0,"what":{"kind":"album","id":"12; DROP"},"fade_secs":1,"volume":9e9,"next_fire":-5}"#,
            &clean,
        );
        // An album without a name does not parse (name is required).
        assert!(a.is_none());
        let a = Alarm::parse(
            r#"{"enabled":true,"minute_of_day":9999,"days":0,"what":{"kind":"album","id":"12; DROP","name":"x"},"fade_secs":1,"volume":9e9,"next_fire":-5}"#,
            &clean,
        )
        .unwrap();
        assert_eq!(a.minute_of_day, 1439);
        assert_eq!(a.days, ALL_DAYS);
        assert_eq!(a.what, What::Queue, "a bad id falls back to the queue");
        assert_eq!(
            (a.fade_secs, a.volume, a.next_fire),
            (FADE_MIN_SECS, 1.0, 0)
        );
        let nan = Alarm::parse(
            r#"{"enabled":false,"minute_of_day":1,"days":1,"fade_secs":500,"volume":null}"#,
            &clean,
        );
        assert!(nan.is_none(), "null is not a number");
    }

    #[test]
    fn sources_check_their_ids_and_cut_their_names() {
        let ok = What::Playlist {
            id: "0a1b2c3d-1111-2222-3333-444455556666".into(),
            name: "x".repeat(500),
        };
        let a = Alarm {
            what: ok,
            ..Alarm::default()
        }
        .clamped(&clean);
        let What::Playlist { name, .. } = &a.what else {
            panic!("kept")
        };
        assert_eq!(name.chars().count(), MAX_NAME_CHARS);
        let bad = What::Playlist {
            id: "../../etc".into(),
            name: "n".into(),
        };
        assert_eq!(
            Alarm {
                what: bad,
                ..Alarm::default()
            }
            .clamped(&clean)
            .what,
            What::Queue
        );
        assert_eq!(
            What::Mix {
                id: "0123abc".into(),
                name: "m".into()
            }
            .source(),
            Some(("mix", "0123abc"))
        );
        assert_eq!(What::Queue.source(), None);
    }

    #[test]
    fn round_trips() {
        let a = Alarm {
            enabled: true,
            next_fire: 123,
            what: What::Album {
                id: "77".into(),
                name: "A".into(),
            },
            ..Alarm::default()
        };
        assert_eq!(Alarm::parse(&a.to_json(), &clean), Some(a));
    }
}
