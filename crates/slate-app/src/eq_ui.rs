//! Settings → Playback → Equalizer: the band editor, AutoEq import and the glue to the core.
//! Nothing here runs until the equalizer is used; the page reads the saved settings when it
//! opens.

use slate_core::commands::utility;
use slate_core::eq::{self, Band, FilterKind, Settings};
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::backend::Backend;
use crate::bootstrap::{AppWindow, EqBand, SettingsState};

fn num(v: f32) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn kind_index(k: FilterKind) -> i32 {
    match k {
        FilterKind::Peak => 0,
        FilterKind::LowShelf => 1,
        FilterKind::HighShelf => 2,
    }
}

fn kind_of(i: i32) -> FilterKind {
    match i {
        1 => FilterKind::LowShelf,
        2 => FilterKind::HighShelf,
        _ => FilterKind::Peak,
    }
}

/// The rows the page shows for `s`.
pub fn rows(s: &Settings) -> Vec<(i32, String, String, String)> {
    s.bands
        .iter()
        .map(|b| (kind_index(b.kind), num(b.freq), num(b.gain_db), num(b.q)))
        .collect()
}

/// Settings page fields. UI thread.
pub fn fill(w: &AppWindow, s: &Settings, blocked: bool) {
    let g = w.global::<SettingsState>();
    g.set_eq_enabled(s.enabled);
    g.set_eq_blocked(blocked);
    g.set_eq_preamp(num(s.preamp_db).into());
    let model: Vec<EqBand> = rows(s)
        .into_iter()
        .map(|(kind, freq, gain, q)| EqBand {
            kind,
            freq: freq.into(),
            gain: gain.into(),
            q: q.into(),
        })
        .collect();
    g.set_eq_bands(ModelRc::new(VecModel::from(model)));
}

/// Applies one edit to the saved settings. Returns the message for the page.
fn edit(s: &mut Settings, key: &str, value: &str) -> Result<(), String> {
    let (what, idx) = match key.split_once(':') {
        Some((w, i)) => (w, i.parse::<usize>().ok()),
        None => (key, None),
    };
    let parse = |v: &str| -> Result<f32, String> {
        v.trim()
            .replace(',', ".")
            .parse::<f32>()
            .ok()
            .filter(|n| n.is_finite())
            .ok_or_else(|| "That is not a number.".to_string())
    };
    match what {
        "eq-preamp" => s.preamp_db = parse(value)?,
        "eq-freq" | "eq-gain" | "eq-q" => {
            let b = idx
                .and_then(|i| s.bands.get_mut(i))
                .ok_or("No such band.")?;
            let v = parse(value)?;
            match what {
                "eq-freq" => b.freq = v,
                "eq-gain" => b.gain_db = v,
                _ => b.q = v,
            }
        }
        _ => return Err("Unknown equalizer setting.".into()),
    }
    *s = std::mem::take(s).clamped();
    Ok(())
}

impl Backend {
    /// Saves `s`, applies it to the player and refreshes the page.
    fn eq_apply(&self, s: Settings, status: &str) {
        let Some(state) = self.state() else { return };
        let blocked = utility::get_exclusive_mode(&state) || utility::get_bit_perfect(&state);
        if let Err(e) = utility::set_eq(&state, s.clone()) {
            self.settings_status(format!("Could not change the equalizer: {}", e.log_safe()));
            return;
        }
        self.device_note_eq(&s);
        let status = status.to_string();
        self.ui(move |w| {
            fill(&w, &s, blocked);
            w.global::<SettingsState>().set_eq_status(status.into());
        });
    }

    /// The on/off switch.
    pub fn eq_set_enabled(&self, on: bool) {
        let Some(state) = self.state() else { return };
        let mut s = utility::get_eq(&state);
        s.enabled = on;
        let note =
            if on && (utility::get_exclusive_mode(&state) || utility::get_bit_perfect(&state)) {
                eq::BLOCKED_NOTE
            } else {
                ""
            };
        self.eq_apply(s, note);
    }

    /// A text field (`eq-preamp`, `eq-freq:3`, …).
    pub fn eq_edited(&self, key: &str, value: &str) {
        let Some(state) = self.state() else { return };
        let mut s = utility::get_eq(&state);
        match edit(&mut s, key, value) {
            Ok(()) => self.eq_apply(s, ""),
            Err(e) => {
                // Put the saved value back in the field.
                let blocked =
                    utility::get_exclusive_mode(&state) || utility::get_bit_perfect(&state);
                self.ui(move |w| {
                    fill(&w, &s, blocked);
                    w.global::<SettingsState>().set_eq_status(e.into());
                });
            }
        }
    }

    /// Buttons: `eq-import`, `eq-add`, `eq-reset`, `eq-remove:N`, `eq-kind:N:K`.
    pub fn eq_action(&self, name: &str) {
        let Some(state) = self.state() else { return };
        let mut s = utility::get_eq(&state);
        match name {
            "eq-import" => return self.eq_import_dialog(),
            "eq-add" => {
                if s.bands.len() >= eq::MAX_BANDS {
                    return;
                }
                s.bands.push(Band {
                    kind: FilterKind::Peak,
                    freq: 1000.0,
                    gain_db: 0.0,
                    q: 1.0,
                });
            }
            "eq-reset" => {
                s.bands.clear();
                s.preamp_db = 0.0;
            }
            n if n.starts_with("eq-remove:") => {
                if let Ok(i) = n["eq-remove:".len()..].parse::<usize>() {
                    if i < s.bands.len() {
                        s.bands.remove(i);
                    }
                }
            }
            n if n.starts_with("eq-kind:") => {
                let mut it = n["eq-kind:".len()..].split(':');
                let (i, k) = (
                    it.next().and_then(|v| v.parse::<usize>().ok()),
                    it.next().and_then(|v| v.parse::<i32>().ok()),
                );
                if let (Some(i), Some(k)) = (i, k) {
                    if let Some(b) = s.bands.get_mut(i) {
                        b.kind = kind_of(k);
                    }
                }
            }
            _ => return,
        }
        self.eq_apply(s, "");
    }

    fn eq_import_dialog(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(file) = rfd::AsyncFileDialog::new()
                .set_title("Import AutoEq ParametricEQ.txt")
                .add_filter("AutoEq preset", &["txt"])
                .pick_file()
                .await
            else {
                return;
            };
            this.eq_import_path(file.path().to_path_buf()).await;
        });
    }

    async fn eq_import_path(&self, path: std::path::PathBuf) {
        use tokio::io::AsyncReadExt;
        let read = async {
            let f = tokio::fs::File::open(&path)
                .await
                .map_err(|e| format!("Could not read that file: {e}"))?;
            let mut buf = Vec::new();
            f.take(eq::MAX_FILE_BYTES as u64 + 1)
                .read_to_end(&mut buf)
                .await
                .map_err(|e| format!("Could not read that file: {e}"))?;
            String::from_utf8(buf).map_err(|_| "That is not a text file.".to_string())
        };
        let imported = match read.await.and_then(|t| eq::parse_autoeq(&t)) {
            Ok(i) => i,
            Err(e) => {
                self.ui(move |w| w.global::<SettingsState>().set_eq_status(e.into()));
                return;
            }
        };
        let Some(state) = self.state() else { return };
        let mut s = utility::get_eq(&state);
        s.preamp_db = imported.preamp_db;
        s.bands = imported.bands.clone();
        s.enabled = true;
        self.eq_apply(s, &imported.summary());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_band() -> Settings {
        Settings {
            enabled: true,
            preamp_db: 0.0,
            bands: vec![Band {
                kind: FilterKind::Peak,
                freq: 1000.0,
                gain_db: 2.0,
                q: 1.0,
            }],
        }
    }

    #[test]
    fn numbers_are_shown_without_trailing_zeros() {
        assert_eq!(num(1000.0), "1000");
        assert_eq!(num(-6.2), "-6.2");
        assert_eq!(num(0.707), "0.71");
        assert_eq!(num(0.0), "0");
    }

    #[test]
    fn edits_are_parsed_clamped_and_bad_input_is_refused() {
        let mut s = with_band();
        edit(&mut s, "eq-gain:0", "-3,5").expect("comma decimal");
        assert_eq!(s.bands[0].gain_db, -3.5);
        edit(&mut s, "eq-freq:0", "99999").expect("edit");
        assert_eq!(s.bands[0].freq, eq::MAX_FREQ);
        edit(&mut s, "eq-preamp", "-40").expect("edit");
        assert_eq!(s.preamp_db, eq::MIN_GAIN_DB);
        let before = s.clone();
        assert!(edit(&mut s, "eq-gain:0", "loud").is_err());
        assert!(edit(&mut s, "eq-gain:7", "1").is_err());
        assert!(edit(&mut s, "eq-q:0", "NaN").is_err());
        assert_eq!(s, before, "a refused edit changes nothing");
    }

    #[test]
    fn rows_and_kinds_round_trip() {
        let r = rows(&with_band());
        assert_eq!(r, vec![(0, "1000".into(), "2".into(), "1".into())]);
        for k in [
            FilterKind::Peak,
            FilterKind::LowShelf,
            FilterKind::HighShelf,
        ] {
            assert_eq!(kind_of(kind_index(k)), k);
        }
    }
}
