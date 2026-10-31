//! Parametric equalizer settings and the AutoEq `ParametricEQ.txt` importer. Pure: the
//! GStreamer element is built in `audio.rs`, and only while the equalizer is on.

use serde::{Deserialize, Serialize};

pub const MAX_BANDS: usize = 10;
pub const MIN_FREQ: f32 = 20.0;
pub const MAX_FREQ: f32 = 20_000.0;
/// The range of GStreamer's `equalizer-nbands` band gain.
pub const MIN_GAIN_DB: f32 = -24.0;
pub const MAX_GAIN_DB: f32 = 12.0;
pub const MIN_Q: f32 = 0.1;
pub const MAX_Q: f32 = 20.0;
/// The Q of a shelf when a file gives none (the usual 12 dB/octave shelf).
const SHELF_Q: f32 = 0.707;
/// AutoEq files are small; anything bigger is not one.
pub const MAX_FILE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterKind {
    #[default]
    Peak,
    LowShelf,
    HighShelf,
}

impl FilterKind {
    /// The value of the band's `type` property in `equalizer-nbands`.
    pub fn gst_nick(self) -> &'static str {
        match self {
            Self::Peak => "peak",
            Self::LowShelf => "low-shelf",
            Self::HighShelf => "high-shelf",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Peak => "Peak",
            Self::LowShelf => "Low shelf",
            Self::HighShelf => "High shelf",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Band {
    #[serde(default)]
    pub kind: FilterKind,
    pub freq: f32,
    pub gain_db: f32,
    pub q: f32,
}

impl Band {
    pub fn clamped(self) -> Self {
        let fin = |v: f32, d: f32| if v.is_finite() { v } else { d };
        Self {
            kind: self.kind,
            freq: fin(self.freq, 1000.0).clamp(MIN_FREQ, MAX_FREQ),
            gain_db: fin(self.gain_db, 0.0).clamp(MIN_GAIN_DB, MAX_GAIN_DB),
            q: fin(self.q, 1.0).clamp(MIN_Q, MAX_Q),
        }
    }

    /// GStreamer describes a band's width in Hz, not as a Q.
    pub fn bandwidth_hz(&self) -> f32 {
        self.freq / self.q.max(MIN_Q)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub preamp_db: f32,
    #[serde(default)]
    pub bands: Vec<Band>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            preamp_db: 0.0,
            bands: Vec::new(),
        }
    }
}

impl Settings {
    /// Every value inside what the element accepts, at most [`MAX_BANDS`] bands.
    pub fn clamped(mut self) -> Self {
        self.bands.truncate(MAX_BANDS);
        self.bands = self.bands.into_iter().map(Band::clamped).collect();
        self.preamp_db = if self.preamp_db.is_finite() {
            self.preamp_db.clamp(MIN_GAIN_DB, MAX_GAIN_DB)
        } else {
            0.0
        };
        self
    }

    /// Whether the equalizer is in the audio path: switched on, something to apply, and not
    /// overridden by exclusive or bit-perfect output (which must not be altered).
    pub fn in_path(&self, exclusive: bool, bit_perfect: bool) -> bool {
        self.enabled
            && !exclusive
            && !bit_perfect
            && (!self.bands.is_empty() || self.preamp_db != 0.0)
    }

    /// Linear gain of the preamp.
    pub fn preamp_amplitude(&self) -> f64 {
        10f64.powf(f64::from(self.preamp_db) / 20.0)
    }
}

/// The note shown when the equalizer is on but cannot act.
pub const BLOCKED_NOTE: &str =
    "The equalizer is off while exclusive or bit-perfect output is on, which must not alter the sound.";

/// What an AutoEq file gave.
#[derive(Debug, Clone, PartialEq)]
pub struct Imported {
    pub preamp_db: f32,
    pub bands: Vec<Band>,
    /// Lines that were understood but not used, for a short message to the user.
    pub skipped: Vec<String>,
}

impl Imported {
    /// "Imported 10 bands, preamp −6.2 dB (1 filter skipped)".
    pub fn summary(&self) -> String {
        let n = self.bands.len();
        let mut s = format!(
            "Imported {n} band{}, preamp {:.1} dB",
            if n == 1 { "" } else { "s" },
            self.preamp_db
        );
        if !self.skipped.is_empty() {
            s.push_str(&format!(
                " ({} filter{} skipped)",
                self.skipped.len(),
                if self.skipped.len() == 1 { "" } else { "s" }
            ));
        }
        s
    }
}

fn number_after(tokens: &[&str], key: &str) -> Option<f32> {
    let i = tokens.iter().position(|t| t.eq_ignore_ascii_case(key))?;
    tokens
        .get(i + 1)?
        .to_ascii_lowercase()
        .trim_end_matches("db")
        .trim_end_matches("hz")
        .parse()
        .ok()
}

/// Parses an AutoEq `ParametricEQ.txt`:
///
/// ```text
/// Preamp: -6.2 dB
/// Filter 1: ON PK Fc 31 Hz Gain 6.4 dB Q 1.41
/// Filter 2: ON LSC Fc 105 Hz Gain 4.5 dB Q 0.70
/// ```
///
/// Peak (`PK`), low shelf (`LS`, `LSC`) and high shelf (`HS`, `HSC`) filters that are `ON` are
/// used, up to ten. Filters that are `OFF` are ignored; other types (low/high pass, notch…)
/// are skipped and counted. Values are clamped to what the equalizer accepts.
pub fn parse_autoeq(text: &str) -> Result<Imported, String> {
    if text.len() > MAX_FILE_BYTES {
        return Err("That file is too large to be an equalizer preset.".into());
    }
    let mut preamp_db: f32 = 0.0;
    let mut bands = Vec::new();
    let mut skipped = Vec::new();
    let mut saw_filter = false;
    for line in text.lines() {
        let line = line.trim().trim_start_matches('\u{feff}');
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("preamp:") {
            let tokens: Vec<&str> = rest.split_whitespace().collect();
            if let Some(v) = tokens
                .first()
                .and_then(|t| t.trim_end_matches("db").parse().ok())
            {
                preamp_db = v;
            }
            continue;
        }
        if !lower.starts_with("filter") {
            continue;
        }
        let Some((_, rest)) = line.split_once(':') else {
            continue;
        };
        saw_filter = true;
        let tokens: Vec<&str> = rest.split_whitespace().collect();
        let on = tokens.first().is_some_and(|t| t.eq_ignore_ascii_case("on"));
        if !on {
            continue;
        }
        let kind = match tokens.get(1).map(|t| t.to_ascii_uppercase()).as_deref() {
            Some("PK" | "PEQ") => FilterKind::Peak,
            Some("LS" | "LSC") => FilterKind::LowShelf,
            Some("HS" | "HSC") => FilterKind::HighShelf,
            _ => {
                skipped.push(line.to_string());
                continue;
            }
        };
        let Some(freq) = number_after(&tokens, "Fc") else {
            skipped.push(line.to_string());
            continue;
        };
        let gain_db = number_after(&tokens, "Gain").unwrap_or(0.0);
        let q = number_after(&tokens, "Q").unwrap_or(if kind == FilterKind::Peak {
            1.0
        } else {
            SHELF_Q
        });
        if bands.len() >= MAX_BANDS {
            skipped.push(line.to_string());
            continue;
        }
        bands.push(
            Band {
                kind,
                freq,
                gain_db,
                q,
            }
            .clamped(),
        );
    }
    if !saw_filter && preamp_db == 0.0 {
        return Err("That does not look like an AutoEq ParametricEQ.txt file.".into());
    }
    if bands.is_empty() {
        return Err("That file has no usable filters (peak and shelf filters that are ON).".into());
    }
    let preamp_db = preamp_db.clamp(MIN_GAIN_DB, MAX_GAIN_DB);
    Ok(Imported {
        preamp_db,
        bands,
        skipped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Preamp: -6.2 dB\r\n\
Filter 1: ON PK Fc 31 Hz Gain 6.4 dB Q 1.41\r\n\
Filter 2: ON LSC Fc 105 Hz Gain 4.5 dB Q 0.70\r\n\
Filter 3: ON PK Fc 1500 Hz Gain -2.3 dB Q 2.10\r\n\
Filter 4: OFF PK Fc 3000 Hz Gain 9 dB Q 1\r\n\
Filter 5: ON HSC Fc 10000 Hz Gain -3.0 dB Q 0.70\r\n";

    #[test]
    fn a_typical_autoeq_file_parses() {
        let i = parse_autoeq(SAMPLE).expect("parse");
        assert_eq!(i.preamp_db, -6.2);
        assert_eq!(i.bands.len(), 4, "the OFF filter is ignored");
        assert_eq!(
            i.bands[0],
            Band {
                kind: FilterKind::Peak,
                freq: 31.0,
                gain_db: 6.4,
                q: 1.41
            }
        );
        assert_eq!(i.bands[1].kind, FilterKind::LowShelf);
        assert_eq!(i.bands[3].kind, FilterKind::HighShelf);
        assert_eq!(i.bands[3].freq, 10_000.0);
        assert!(i.skipped.is_empty());
        assert_eq!(i.summary(), "Imported 4 bands, preamp -6.2 dB");
    }

    #[test]
    fn unsupported_filters_are_counted_and_ten_bands_is_the_limit() {
        let mut t = String::from("Preamp: -3 dB\n");
        for n in 1..=12 {
            t.push_str(&format!(
                "Filter {n}: ON PK Fc {} Hz Gain 1 dB Q 1\n",
                100 * n
            ));
        }
        t.push_str("Filter 13: ON LP Fc 20000 Hz\n");
        let i = parse_autoeq(&t).expect("parse");
        assert_eq!(i.bands.len(), MAX_BANDS);
        assert_eq!(i.skipped.len(), 3, "two over the limit and one low-pass");
        assert!(i.summary().contains("3 filters skipped"));
    }

    #[test]
    fn missing_q_values_and_odd_spacing_are_tolerated() {
        let i = parse_autoeq("Filter 1: ON LS Fc 80 Hz Gain 3 dB\nfilter 2:  on  pk  fc  1000  hz  gain  -1.5db  q  4\n")
            .expect("parse");
        assert_eq!(i.preamp_db, 0.0);
        assert_eq!(i.bands[0].q, SHELF_Q);
        assert_eq!(
            i.bands[1],
            Band {
                kind: FilterKind::Peak,
                freq: 1000.0,
                gain_db: -1.5,
                q: 4.0
            }
        );
    }

    #[test]
    fn values_are_clamped_to_what_the_element_accepts() {
        let i = parse_autoeq("Preamp: -90 dB\nFilter 1: ON PK Fc 5 Hz Gain 40 dB Q 99\n")
            .expect("parse");
        assert_eq!(i.preamp_db, MIN_GAIN_DB);
        assert_eq!(
            i.bands[0],
            Band {
                kind: FilterKind::Peak,
                freq: MIN_FREQ,
                gain_db: MAX_GAIN_DB,
                q: MAX_Q
            }
        );
    }

    #[test]
    fn files_that_are_not_presets_are_refused_with_a_reason() {
        assert!(parse_autoeq("").is_err());
        assert!(parse_autoeq("hello world\n").is_err());
        assert!(parse_autoeq("Preamp: -2 dB\n")
            .unwrap_err()
            .contains("no usable filters"));
        assert!(parse_autoeq("Filter 1: OFF PK Fc 100 Hz Gain 1 dB Q 1\n").is_err());
        assert!(parse_autoeq(&"x".repeat(MAX_FILE_BYTES + 1))
            .unwrap_err()
            .contains("too large"));
        // A byte-order mark and a garbage number do not panic.
        assert!(parse_autoeq("\u{feff}Filter 1: ON PK Fc abc Hz Gain 1 dB Q 1\n").is_err());
    }

    #[test]
    fn the_equalizer_is_in_the_path_only_when_it_can_act() {
        let mut s = Settings::default();
        assert!(!s.in_path(false, false), "off by default");
        s.enabled = true;
        assert!(!s.in_path(false, false), "nothing to apply yet");
        s.bands.push(Band {
            kind: FilterKind::Peak,
            freq: 1000.0,
            gain_db: 3.0,
            q: 1.0,
        });
        assert!(s.in_path(false, false));
        assert!(!s.in_path(true, false), "exclusive output stays untouched");
        assert!(
            !s.in_path(false, true),
            "bit-perfect output stays untouched"
        );
        assert!(s.enabled, "the choice itself is kept");
    }

    #[test]
    fn settings_are_clamped_and_amplitude_follows_the_preamp() {
        let s = Settings {
            enabled: true,
            preamp_db: -6.0,
            bands: vec![
                Band {
                    kind: FilterKind::Peak,
                    freq: 1e9,
                    gain_db: f32::NAN,
                    q: 0.0
                };
                14
            ],
        }
        .clamped();
        assert_eq!(s.bands.len(), MAX_BANDS);
        assert_eq!(s.bands[0].freq, MAX_FREQ);
        assert_eq!(s.bands[0].gain_db, 0.0);
        assert_eq!(s.bands[0].q, MIN_Q);
        assert!((s.preamp_amplitude() - 0.5012).abs() < 1e-3);
        let b = Band {
            kind: FilterKind::Peak,
            freq: 1000.0,
            gain_db: 0.0,
            q: 2.0,
        };
        assert_eq!(b.bandwidth_hz(), 500.0);
    }

    #[test]
    fn settings_round_trip_through_json_and_old_files_still_load() {
        let s = Settings {
            enabled: true,
            preamp_db: -2.0,
            bands: vec![Band {
                kind: FilterKind::HighShelf,
                freq: 8000.0,
                gain_db: -2.0,
                q: 0.7,
            }],
        };
        let j = serde_json::to_string(&s).expect("json");
        assert_eq!(serde_json::from_str::<Settings>(&j).expect("back"), s);
        assert_eq!(
            serde_json::from_str::<Settings>("{}").expect("empty"),
            Settings::default()
        );
    }
}
