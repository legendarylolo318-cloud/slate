//! Crossfade decisions and curves. Pure logic only: this module does not touch GStreamer.
//! It fixes when a fade may happen, how long it is, and the equal-power gain curves for the
//! outgoing and incoming tracks; `audio.rs` runs the fade with them.

pub const MIN_SECS: u32 = 1;
pub const MAX_SECS: u32 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    /// 0 = off.
    pub secs: u32,
    /// Keep consecutive tracks of one album gapless.
    pub skip_same_album: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            secs: 0,
            skip_same_album: true,
        }
    }
}

impl Settings {
    pub fn clamped(self) -> Self {
        Self {
            secs: if self.secs == 0 {
                0
            } else {
                self.secs.clamp(MIN_SECS, MAX_SECS)
            },
            ..self
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    Off,
    /// Exclusive or bit-perfect output must not be mixed or scaled.
    BitPerfectOutput,
    SameAlbum,
    TrackTooShort,
    NoNextTrack,
}

impl Why {
    /// The note shown next to the setting.
    pub fn note(self) -> &'static str {
        match self {
            Self::BitPerfectOutput => {
                "Crossfade is off while exclusive or bit-perfect output is on."
            }
            _ => "",
        }
    }
}

/// What the next track change should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    Fade { secs: u32 },
    Gapless(Why),
}

#[derive(Debug, Clone, Copy)]
pub struct Boundary {
    pub exclusive_or_bit_perfect: bool,
    pub same_album: bool,
    pub current_secs: u32,
    pub next_secs: Option<u32>,
}

pub fn plan(s: Settings, b: Boundary) -> Plan {
    let s = s.clamped();
    if s.secs == 0 {
        return Plan::Gapless(Why::Off);
    }
    if b.exclusive_or_bit_perfect {
        return Plan::Gapless(Why::BitPerfectOutput);
    }
    let Some(next) = b.next_secs else {
        return Plan::Gapless(Why::NoNextTrack);
    };
    if s.skip_same_album && b.same_album {
        return Plan::Gapless(Why::SameAlbum);
    }
    // A fade never takes more than a third of either track.
    let cap = (b.current_secs.min(next)) / 3;
    if cap == 0 {
        return Plan::Gapless(Why::TrackTooShort);
    }
    Plan::Fade {
        secs: s.secs.min(cap),
    }
}

/// Equal-power curve: gain of the outgoing track at `t` in 0..=1 through the fade.
pub fn fade_out(t: f64) -> f64 {
    (t.clamp(0.0, 1.0) * std::f64::consts::FRAC_PI_2).cos()
}

/// Gain of the incoming track at `t`.
pub fn fade_in(t: f64) -> f64 {
    (t.clamp(0.0, 1.0) * std::f64::consts::FRAC_PI_2).sin()
}

/// With ReplayGain both tracks run through one shared gain stage set for the outgoing track,
/// so the incoming track is scaled by `incoming / outgoing` to land at its own level.
pub fn relative_gain(outgoing_norm: f64, incoming_norm: f64) -> f64 {
    if outgoing_norm <= 0.0 {
        1.0
    } else {
        incoming_norm / outgoing_norm
    }
}

/// Where in the outgoing track the fade starts.
pub fn fade_start_secs(current_secs: u32, fade_secs: u32) -> u32 {
    current_secs.saturating_sub(fade_secs)
}

/// The signal path reads MODIFIED while a fade is happening.
pub fn signal_path_modified(fading: bool) -> bool {
    fading
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b() -> Boundary {
        Boundary {
            exclusive_or_bit_perfect: false,
            same_album: false,
            current_secs: 200,
            next_secs: Some(180),
        }
    }

    #[test]
    fn off_by_default() {
        assert_eq!(Settings::default().secs, 0);
        assert!(Settings::default().skip_same_album);
        assert_eq!(plan(Settings::default(), b()), Plan::Gapless(Why::Off));
    }

    #[test]
    fn gates_in_order() {
        let on = Settings {
            secs: 6,
            skip_same_album: true,
        };
        assert_eq!(plan(on, b()), Plan::Fade { secs: 6 });
        assert_eq!(
            plan(
                on,
                Boundary {
                    exclusive_or_bit_perfect: true,
                    ..b()
                }
            ),
            Plan::Gapless(Why::BitPerfectOutput)
        );
        assert_eq!(
            plan(
                on,
                Boundary {
                    same_album: true,
                    ..b()
                }
            ),
            Plan::Gapless(Why::SameAlbum)
        );
        assert_eq!(
            plan(
                Settings {
                    skip_same_album: false,
                    ..on
                },
                Boundary {
                    same_album: true,
                    ..b()
                }
            ),
            Plan::Fade { secs: 6 }
        );
        assert_eq!(
            plan(
                on,
                Boundary {
                    next_secs: None,
                    ..b()
                }
            ),
            Plan::Gapless(Why::NoNextTrack)
        );
        assert_eq!(
            plan(
                on,
                Boundary {
                    current_secs: 2,
                    ..b()
                }
            ),
            Plan::Gapless(Why::TrackTooShort)
        );
        assert!(Why::BitPerfectOutput.note().contains("bit-perfect"));
    }

    #[test]
    fn short_tracks_shorten_the_fade_and_the_setting_is_clamped() {
        let on = Settings {
            secs: 12,
            skip_same_album: true,
        };
        assert_eq!(
            plan(
                on,
                Boundary {
                    current_secs: 30,
                    ..b()
                }
            ),
            Plan::Fade { secs: 10 }
        );
        assert_eq!(
            Settings {
                secs: 99,
                skip_same_album: true
            }
            .clamped()
            .secs,
            MAX_SECS
        );
        assert_eq!(
            Settings {
                secs: 0,
                skip_same_album: true
            }
            .clamped()
            .secs,
            0
        );
    }

    #[test]
    fn curves_are_equal_power() {
        for i in 0..=20 {
            let t = f64::from(i) / 20.0;
            let p = fade_out(t).powi(2) + fade_in(t).powi(2);
            assert!((p - 1.0).abs() < 1e-9, "{t}");
        }
        assert!((fade_out(0.0) - 1.0).abs() < 1e-12 && fade_out(1.0).abs() < 1e-12);
        assert!(fade_in(0.0).abs() < 1e-12 && (fade_in(1.0) - 1.0).abs() < 1e-12);
        assert_eq!(fade_in(-1.0), 0.0);
        assert!((fade_out(5.0)).abs() < 1e-12);
    }

    #[test]
    fn replaygain_and_timing() {
        assert!((relative_gain(0.5, 0.25) - 0.5).abs() < 1e-12);
        assert_eq!(relative_gain(0.0, 0.7), 1.0);
        assert_eq!(fade_start_secs(200, 6), 194);
        assert_eq!(fade_start_secs(3, 6), 0);
        assert!(signal_path_modified(true) && !signal_path_modified(false));
    }
}
