//! Headphone crossfeed (GStreamer `bs2b`). Pure: the element is built in `audio.rs`, and only
//! while a level other than Off is chosen, so with it off the pipeline has no new element.

use serde::{Deserialize, Serialize};

/// GStreamer element that does the work.
pub const FACTORY: &str = "bs2b";
/// Name of the element in the pipeline, so a live change can find it.
pub const ELEMENT_NAME: &str = "slate-crossfeed";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    #[default]
    Off,
    Light,
    Medium,
    Strong,
}

impl Level {
    pub const ALL: [Level; 4] = [Level::Off, Level::Light, Level::Medium, Level::Strong];

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Light => "Light",
            Self::Medium => "Medium",
            Self::Strong => "Strong",
        }
    }

    pub fn index(self) -> i32 {
        Self::ALL.iter().position(|l| *l == self).unwrap_or(0) as i32
    }

    pub fn from_index(i: i32) -> Self {
        usize::try_from(i)
            .ok()
            .and_then(|i| Self::ALL.get(i).copied())
            .unwrap_or_default()
    }

    /// bs2b `(fcut in Hz, feed in dB/10)`; None for Off. The feed is how far the crossed
    /// signal sits below the direct one, so a LOWER number is a STRONGER effect (measured in
    /// `audio::crossfeed_bleeds_…`). Light is Jan Meier's setting (little change), Medium the
    /// library's own default (virtual speakers at 30°), Strong feeds 3 dB.
    pub fn params(self) -> Option<(i32, i32)> {
        match self {
            Self::Off => None,
            Self::Light => Some((650, 90)),
            Self::Medium => Some((700, 45)),
            Self::Strong => Some((700, 30)),
        }
    }

    /// Whether crossfeed is in the audio path: a level is chosen and output is not exclusive
    /// or bit-perfect (which must not be altered).
    pub fn in_path(self, exclusive: bool, bit_perfect: bool) -> bool {
        self != Self::Off && !exclusive && !bit_perfect
    }
}

/// Whether this GStreamer has the `bs2b` plugin. Looked up once, and only when the setting is
/// shown or a level is chosen.
pub fn available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        gstreamer::init().is_ok() && gstreamer::ElementFactory::find(FACTORY).is_some()
    })
}

/// Why the option is missing, for the Settings page.
pub const UNAVAILABLE_REASON: &str =
    "Crossfeed needs GStreamer's bs2b plugin (gst-plugins-bad with libbs2b), which this install does not have.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_has_no_parameters_and_is_never_in_the_path() {
        assert_eq!(Level::Off.params(), None);
        assert!(!Level::Off.in_path(false, false));
    }

    #[test]
    fn a_stronger_level_has_a_lower_feed_number_and_stays_inside_the_element_range() {
        let feeds: Vec<i32> = [Level::Light, Level::Medium, Level::Strong]
            .iter()
            .map(|l| l.params().unwrap().1)
            .collect();
        assert!(feeds.windows(2).all(|w| w[0] > w[1]), "{feeds:?}");
        for l in Level::ALL {
            if let Some((f, d)) = l.params() {
                assert!((300..=2000).contains(&f) && (10..=150).contains(&d));
            }
        }
    }

    #[test]
    fn exclusive_and_bit_perfect_override() {
        assert!(Level::Medium.in_path(false, false));
        assert!(!Level::Medium.in_path(true, false));
        assert!(!Level::Medium.in_path(false, true));
    }

    #[test]
    fn index_round_trips_and_bad_index_is_off() {
        for l in Level::ALL {
            assert_eq!(Level::from_index(l.index()), l);
        }
        assert_eq!(Level::from_index(-1), Level::Off);
        assert_eq!(Level::from_index(99), Level::Off);
    }

    #[test]
    fn unknown_saved_value_is_not_a_panic() {
        assert!(serde_json::from_str::<Level>("\"strong\"").is_ok());
        assert!(serde_json::from_str::<Level>("\"loud\"").is_err());
    }
}
