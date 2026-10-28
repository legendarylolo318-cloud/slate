//! Signal Path panel: turns the core's measurements and verdict into text rows.

use slate_core::commands::utility;
use slate_core::signal_path::{canonical_format, verdict, SignalPath};
use slint::{ModelRc, VecModel};

use crate::backend::Backend;
use crate::bootstrap::KeyValue;

fn rate(hz: Option<u32>) -> String {
    match hz {
        Some(h) if h >= 1000 => format!("{} kHz", f64::from(h) / 1000.0),
        Some(h) => format!("{h} Hz"),
        None => "—".into(),
    }
}

fn fmt(f: &Option<String>) -> String {
    f.as_deref().map_or("—".into(), canonical_format)
}

fn db(factor: f32) -> String {
    if (factor - 1.0).abs() < 1e-3 || factor <= 0.0 {
        "0.0 dB".into()
    } else {
        format!("{:+.1} dB", 20.0 * factor.log10())
    }
}

/// (headline, rows) for a signal path.
pub fn describe(sp: &SignalPath) -> (String, Vec<(String, String)>) {
    let v = verdict(sp);
    let headline = if sp.backend.is_none() {
        "Pipeline starting… play something first.".to_string()
    } else if v.pristine {
        if v.lossless_promotion {
            format!(
                "{} → {} — lossless promotion, every source bit preserved",
                fmt(&sp.decoded_format),
                fmt(&sp.output_format)
            )
        } else {
            "Source PCM reaches your DAC untouched".into()
        }
    } else if let (Some(a), Some(b)) = (sp.resampled_from, sp.resampled_to) {
        format!("Resampled {} → {}", rate(Some(a)), rate(Some(b)))
    } else if let (Some(a), Some(b)) = (&sp.format_fallback_from, &sp.format_fallback_to) {
        format!("DAC refused {a} — fell back to {b}")
    } else if v.lossy_format_change {
        format!(
            "Bit depth reduced {} → {}",
            fmt(&sp.decoded_format),
            fmt(&sp.output_format)
        )
    } else if v.normalization_altered && v.user_volume_altered {
        "Volume and ReplayGain scaling applied".into()
    } else if v.normalization_altered {
        "ReplayGain scaling applied".into()
    } else if v.user_volume_altered {
        "Software volume applied".into()
    } else if !v.direct_alsa {
        "Playing through the system mixer — not exclusive".into()
    } else {
        "Pipeline pass-through".into()
    };

    let mut rows: Vec<(String, String)> = vec![(
        "Backend".into(),
        sp.backend.clone().unwrap_or_else(|| "—".into()),
    )];
    rows.push((
        "Decoded".into(),
        format!(
            "{} · {} · {} ch",
            fmt(&sp.decoded_format),
            rate(sp.decoded_rate),
            sp.decoded_channels.unwrap_or(0)
        ),
    ));
    rows.push((
        "Output".into(),
        format!(
            "{} · {} · {} ch",
            fmt(&sp.output_format),
            rate(sp.output_rate),
            sp.output_channels.unwrap_or(0)
        ),
    ));
    if let Some(d) = &sp.output_device {
        rows.push(("Device".into(), d.clone()));
    }
    if let Some(d) = &sp.dac {
        rows.push((
            "DAC".into(),
            format!(
                "{} · {} · {:?}",
                canonical_format(&d.format),
                rate(Some(d.rate)),
                d.state
            ),
        ));
        rows.push(("DAC card".into(), d.card_name.clone()));
    }
    rows.push(("Volume".into(), format!("{:.0}%", sp.user_volume * 100.0)));
    rows.push((
        "ReplayGain".into(),
        if sp.volume_normalization {
            db(sp.norm_gain_factor)
        } else {
            "off".into()
        },
    ));
    if let Some(m) = &sp.os_mixer {
        rows.push(("OS mixer".into(), format!("{} · {}", m.server, m.sink_name)));
        rows.push((
            "Mixer format".into(),
            format!("{} · {}", m.sink_format, rate(m.sink_rate)),
        ));
        rows.push((
            "Mixer volume".into(),
            format!(
                "{:.0}%{}",
                m.sink_volume * 100.0,
                if m.sink_muted { " (muted)" } else { "" }
            ),
        ));
    }
    if let (Some(a), Some(b)) = (&sp.promoted_from, &sp.promoted_to) {
        rows.push(("Promoted".into(), format!("{a} → {b}")));
    }
    (headline, rows)
}

impl Backend {
    pub fn open_signal_path(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            let st = state.clone();
            let sp = tokio::task::spawn_blocking(move || utility::refresh_signal_path(&st))
                .await
                .unwrap_or_else(|_| utility::get_signal_path(&state));
            let (headline, rows) = describe(&sp);
            let pristine = verdict(&sp).pristine;
            this.ui(move |w| {
                let model: Vec<KeyValue> = rows
                    .into_iter()
                    .map(|(k, v)| KeyValue {
                        key: k.into(),
                        value: v.into(),
                    })
                    .collect();
                w.set_sp_rows(ModelRc::new(VecModel::from(model)));
                w.set_sp_headline(headline.into());
                w.set_sp_pristine(pristine);
                w.set_sp_open(true);
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starting_state_has_a_clear_headline() {
        let (h, rows) = describe(&SignalPath::default());
        assert!(h.starts_with("Pipeline starting"));
        assert_eq!(rows[0].0, "Backend");
    }

    #[test]
    fn resampling_is_named_in_the_headline() {
        let sp = SignalPath {
            backend: Some("DirectAlsa".into()),
            exclusive_mode: true,
            resampled_from: Some(44_100),
            resampled_to: Some(48_000),
            user_volume: 1.0,
            norm_gain_factor: 1.0,
            ..Default::default()
        };
        assert_eq!(describe(&sp).0, "Resampled 44.1 kHz → 48 kHz");
    }

    #[test]
    fn gain_in_db() {
        assert_eq!(db(1.0), "0.0 dB");
        assert_eq!(db(0.5), "-6.0 dB");
    }
}
