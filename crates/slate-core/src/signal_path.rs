//! Signal path transparency tracker.
//!
//! Collects what is known at runtime about how audio flows from TIDAL to the
//! DAC: which backend is active, what GStreamer decoded to, what ALSA
//! actually negotiated, and any alterations along the way (resampling,
//! bit-depth promotion, format fallback, software volume, ReplayGain).
//!
//! The tracker is the single source of truth for the frontend
//! "transparency panel". Mutations emit a `signal-path-changed` event so
//! the UI can reactively update without polling.

use serde::Serialize;
use std::sync::Mutex;

/// Snapshot of the runtime signal path. Sent to the frontend as the payload
/// of `signal-path-changed` events and as the return of `get_signal_path`.
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SignalPath {
    /// "Normal" (autoaudiosink → system mixer) or "DirectAlsa" (exclusive ALSA).
    pub backend: Option<String>,

    /// PCM format the GStreamer pipeline decodes to (DirectAlsa only — the
    /// system mixer hides this in Normal mode).
    pub decoded_format: Option<String>,
    pub decoded_rate: Option<u32>,
    pub decoded_channels: Option<u32>,

    /// What ALSA actually negotiated. DirectAlsa only.
    pub output_format: Option<String>,
    pub output_rate: Option<u32>,
    pub output_channels: Option<u32>,
    pub output_device: Option<String>,

    /// Mode flags reflected from settings.
    pub exclusive_mode: bool,
    pub bit_perfect: bool,
    pub volume_normalization: bool,

    /// User-set volume multiplier (1.0 = unity, no software attenuation).
    pub user_volume: f32,
    /// Linear ReplayGain multiplier currently applied (1.0 = none).
    pub norm_gain_factor: f32,

    /// Resampling that occurred (DAC-supported rate fallback in non-bit-perfect mode).
    pub resampled_from: Option<u32>,
    pub resampled_to: Option<u32>,

    /// Bit-depth container promotion (bit-perfect: source widened to nearest
    /// DAC-supported container; pure bit op via zero-padding).
    pub promoted_from: Option<String>,
    pub promoted_to: Option<String>,

    /// Format fallback (non-bit-perfect: DAC didn't accept requested format).
    pub format_fallback_from: Option<String>,
    pub format_fallback_to: Option<String>,

    /// Ground-truth kernel hw_params for the active DAC. Populated by
    /// pipeline_probe; None until first refresh or when no DAC active.
    pub dac: Option<crate::pipeline_probe::DacHwParams>,

    /// OS-mixer info (PulseAudio/PipeWire). None when probe failed or
    /// pactl is unavailable.
    pub os_mixer: Option<crate::pipeline_probe::OsMixerInfo>,
}

pub struct SignalPathTracker {
    state: Mutex<SignalPath>,
    app_handle: crate::AppHandle,
}

impl SignalPathTracker {
    pub fn new(app_handle: crate::AppHandle) -> Self {
        Self {
            state: Mutex::new(SignalPath {
                user_volume: 1.0,
                norm_gain_factor: 1.0,
                ..Default::default()
            }),
            app_handle,
        }
    }

    pub fn snapshot(&self) -> SignalPath {
        self.state.lock().unwrap().clone()
    }

    fn emit(&self, snap: SignalPath) {
        let _ = self.app_handle.emit("signal-path-changed", snap);
    }

    /// Clear per-track signal info but keep mode flags / volume / device.
    /// Called at the start of each `PlayUrl`.
    pub fn reset_for_track(&self) {
        let snap = {
            let mut s = self.state.lock().unwrap();
            s.decoded_format = None;
            s.decoded_rate = None;
            s.decoded_channels = None;
            s.output_format = None;
            s.output_rate = None;
            s.output_channels = None;
            s.resampled_from = None;
            s.resampled_to = None;
            s.promoted_from = None;
            s.promoted_to = None;
            s.format_fallback_from = None;
            s.format_fallback_to = None;
            s.norm_gain_factor = 1.0; // next track may have no RG; don't carry old gain
            s.clone()
        };
        self.emit(snap);
    }

    pub fn set_backend(&self, backend: &str, device: Option<String>) {
        let snap = {
            let mut s = self.state.lock().unwrap();
            s.backend = Some(backend.to_string());
            s.output_device = device;
            s.clone()
        };
        self.emit(snap);
    }

    pub fn set_audio_modes(&self, exclusive: bool, bit_perfect: bool) {
        let snap = {
            let mut s = self.state.lock().unwrap();
            s.exclusive_mode = exclusive;
            s.bit_perfect = bit_perfect;
            s.clone()
        };
        self.emit(snap);
    }

    pub fn set_normalization_enabled(&self, enabled: bool) {
        let snap = {
            let mut s = self.state.lock().unwrap();
            s.volume_normalization = enabled;
            if !enabled {
                // RG is off → factor irrelevant, render as 1.0 (unity).
                s.norm_gain_factor = 1.0;
            }
            s.clone()
        };
        self.emit(snap);
    }

    pub fn set_decoded(&self, fmt: &str, rate: u32, channels: u32) {
        let snap = {
            let mut s = self.state.lock().unwrap();
            s.decoded_format = Some(fmt.to_string());
            s.decoded_rate = Some(rate);
            s.decoded_channels = Some(channels);
            s.clone()
        };
        self.emit(snap);
    }

    pub fn set_output(&self, fmt: &str, rate: u32, channels: u32) {
        let snap = {
            let mut s = self.state.lock().unwrap();
            s.output_format = Some(fmt.to_string());
            s.output_rate = Some(rate);
            s.output_channels = Some(channels);
            s.clone()
        };
        self.emit(snap);
    }

    pub fn set_user_volume(&self, vol: f32) {
        let snap = {
            let mut s = self.state.lock().unwrap();
            if (s.user_volume - vol).abs() < 1e-3 {
                return; // dedup: slider drag fires hundreds of events
            }
            s.user_volume = vol;
            s.clone()
        };
        self.emit(snap);
    }

    pub fn set_norm_gain_factor(&self, factor: f32) {
        let snap = {
            let mut s = self.state.lock().unwrap();
            s.norm_gain_factor = factor;
            s.clone()
        };
        self.emit(snap);
    }

    pub fn record_resample(&self, from: u32, to: u32) {
        let snap = {
            let mut s = self.state.lock().unwrap();
            s.resampled_from = Some(from);
            s.resampled_to = Some(to);
            s.clone()
        };
        self.emit(snap);
    }

    pub fn record_bit_depth_promotion(&self, from: &str, to: &str) {
        let snap = {
            let mut s = self.state.lock().unwrap();
            s.promoted_from = Some(from.to_string());
            s.promoted_to = Some(to.to_string());
            s.clone()
        };
        self.emit(snap);
    }

    pub fn record_format_fallback(&self, from: &str, to: &str) {
        let snap = {
            let mut s = self.state.lock().unwrap();
            s.format_fallback_from = Some(from.to_string());
            s.format_fallback_to = Some(to.to_string());
            s.clone()
        };
        self.emit(snap);
    }

    pub fn clear_resample(&self) {
        let snap = {
            let mut s = self.state.lock().unwrap();
            if s.resampled_from.is_none() && s.resampled_to.is_none() {
                return;
            }
            s.resampled_from = None;
            s.resampled_to = None;
            s.clone()
        };
        self.emit(snap);
    }

    pub fn clear_format_fallback(&self) {
        let snap = {
            let mut s = self.state.lock().unwrap();
            if s.format_fallback_from.is_none() && s.format_fallback_to.is_none() {
                return;
            }
            s.format_fallback_from = None;
            s.format_fallback_to = None;
            s.clone()
        };
        self.emit(snap);
    }

    /// One refresh's probe results, published as one event so the UI never
    /// pairs new caps with a stale mixer or DAC.
    pub fn apply_probe(
        &self,
        decoded: Option<crate::pipeline_probe::PadCaps>,
        output: Option<crate::pipeline_probe::PadCaps>,
        mixer: Option<crate::pipeline_probe::OsMixerInfo>,
        dac: Option<crate::pipeline_probe::DacHwParams>,
    ) {
        let split = |c: Option<crate::pipeline_probe::PadCaps>| match c {
            Some(c) => (Some(c.format), Some(c.rate), Some(c.channels)),
            None => (None, None, None),
        };
        let (dec_fmt, dec_rate, dec_ch) = split(decoded);
        let (out_fmt, out_rate, out_ch) = split(output);
        let snap = {
            let mut s = self.state.lock().unwrap();
            if s.decoded_format == dec_fmt
                && s.decoded_rate == dec_rate
                && s.decoded_channels == dec_ch
                && s.output_format == out_fmt
                && s.output_rate == out_rate
                && s.output_channels == out_ch
                && s.os_mixer == mixer
                && s.dac == dac
            {
                return; // dedup
            }
            s.decoded_format = dec_fmt;
            s.decoded_rate = dec_rate;
            s.decoded_channels = dec_ch;
            s.output_format = out_fmt;
            s.output_rate = out_rate;
            s.output_channels = out_ch;
            s.os_mixer = mixer;
            s.dac = dac;
            s.clone()
        };
        self.emit(snap);
    }
}

// ---------------------------------------------------------------------------
// Verdict
// ---------------------------------------------------------------------------

/// Tolerance for treating a gain as unity.
const UNITY_EPS: f32 = 1e-3;

/// Canonical GStreamer spelling of a PCM format (ALSA, GStreamer and pactl
/// naming all accepted). Ported from the original `toGstreamerFormat`.
pub fn canonical_format(s: &str) -> String {
    let upper = s.to_uppercase().replace('-', "_");
    match upper.as_str() {
        "S16_LE" => "S16LE",
        "S24_LE" => "S24_32LE",
        "S24_3LE" => "S24LE",
        "S32_LE" => "S32LE",
        "FLOAT_LE" => "F32LE",
        "S16_BE" => "S16BE",
        "S24_BE" => "S24_32BE",
        "S24_3BE" => "S24BE",
        "S32_BE" => "S32BE",
        "FLOAT_BE" => "F32BE",
        "FLOAT" | "FLOAT32" | "FLOAT32LE" => "F32LE",
        "FLOAT64" | "FLOAT64LE" => "F64LE",
        _ => return upper,
    }
    .to_string()
}

/// Audio bits per sample, endianness ignored; float counts as 24.
fn audio_bit_depth(fmt: Option<&str>) -> u32 {
    match fmt.map(canonical_format).as_deref() {
        Some("S16LE" | "S16BE") => 16,
        Some("S24LE" | "S24BE" | "S24_32LE" | "S24_32BE" | "F32LE" | "F32BE") => 24,
        Some("S32LE" | "S32BE") => 32,
        _ => 0,
    }
}

fn formats_equivalent(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => canonical_format(a) == canonical_format(b),
        _ => false,
    }
}

/// What the signal path did to the audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verdict {
    pub pristine: bool,
    pub direct_alsa: bool,
    pub user_volume_altered: bool,
    pub normalization_altered: bool,
    pub lossy_format_change: bool,
    pub lossless_promotion: bool,
}

/// "Pristine" means every sample the decoder produced reaches the DAC with all
/// its bits: exclusive direct ALSA, no resampling or format fallback, unity
/// volume and gain, no bit-depth narrowing, and a DAC that matches the pipeline.
pub fn verdict(sp: &SignalPath) -> Verdict {
    let direct = sp.backend.as_deref() == Some("DirectAlsa");
    let user_altered = (sp.user_volume - 1.0).abs() > UNITY_EPS;
    let norm_altered = sp.volume_normalization && (sp.norm_gain_factor - 1.0).abs() > UNITY_EPS;

    let (up_fmt, up_rate) = if direct {
        (sp.output_format.as_deref(), sp.output_rate)
    } else {
        match &sp.os_mixer {
            Some(m) => (Some(m.sink_format.as_str()), m.sink_rate.or(sp.output_rate)),
            None => (sp.output_format.as_deref(), sp.output_rate),
        }
    };
    let dac_matches = match &sp.dac {
        None => true,
        Some(d) if d.state != crate::pipeline_probe::HwParamsState::Active => true,
        Some(d) => formats_equivalent(Some(&d.format), up_fmt) && Some(d.rate) == up_rate,
    };

    let format_changed = sp.decoded_format.is_some()
        && sp.output_format.is_some()
        && !formats_equivalent(sp.decoded_format.as_deref(), sp.output_format.as_deref());
    let lossy = format_changed
        && audio_bit_depth(sp.output_format.as_deref())
            < audio_bit_depth(sp.decoded_format.as_deref());

    let pristine = direct
        && sp.exclusive_mode
        && sp.resampled_from.is_none()
        && sp.format_fallback_from.is_none()
        && !user_altered
        && !norm_altered
        && !lossy
        && dac_matches;

    Verdict {
        pristine,
        direct_alsa: direct,
        user_volume_altered: user_altered,
        normalization_altered: norm_altered,
        lossy_format_change: lossy,
        lossless_promotion: format_changed && !lossy,
    }
}

#[cfg(test)]
mod verdict_tests {
    use super::*;

    fn pristine_path() -> SignalPath {
        SignalPath {
            backend: Some("DirectAlsa".into()),
            decoded_format: Some("S24_32LE".into()),
            output_format: Some("S24_LE".into()),
            exclusive_mode: true,
            user_volume: 1.0,
            norm_gain_factor: 1.0,
            ..Default::default()
        }
    }

    #[test]
    fn canonical_names_match_across_conventions() {
        assert_eq!(canonical_format("S24_LE"), "S24_32LE");
        assert_eq!(canonical_format("s32-le"), "S32LE");
        assert_eq!(canonical_format("float32le"), "F32LE");
        assert_eq!(canonical_format("S16LE"), "S16LE");
    }

    #[test]
    fn exclusive_untouched_path_is_pristine() {
        let v = verdict(&pristine_path());
        assert!(v.pristine && v.direct_alsa);
        assert!(!v.lossless_promotion, "same format under another name");
    }

    #[test]
    fn each_alteration_breaks_pristine() {
        let mut sp = pristine_path();
        sp.user_volume = 0.5;
        assert!(!verdict(&sp).pristine);
        let mut sp = pristine_path();
        sp.exclusive_mode = false;
        assert!(!verdict(&sp).pristine);
        let mut sp = pristine_path();
        sp.resampled_from = Some(96_000);
        assert!(!verdict(&sp).pristine);
        let mut sp = pristine_path();
        sp.backend = Some("Normal".into());
        assert!(!verdict(&sp).pristine);
        let mut sp = pristine_path();
        sp.volume_normalization = true;
        sp.norm_gain_factor = 0.8;
        assert!(verdict(&sp).normalization_altered && !verdict(&sp).pristine);
    }

    #[test]
    fn narrowing_is_lossy_but_widening_is_a_lossless_promotion() {
        let mut sp = pristine_path();
        sp.decoded_format = Some("S32LE".into());
        sp.output_format = Some("S16LE".into());
        let v = verdict(&sp);
        assert!(v.lossy_format_change && !v.pristine);
        let mut sp = pristine_path();
        sp.decoded_format = Some("S16LE".into());
        sp.output_format = Some("S32LE".into());
        let v = verdict(&sp);
        assert!(v.lossless_promotion && v.pristine);
    }
}
