//! Human-readable description of how a track is being streamed.

use slate_core::tidal_api::StreamInfo;

/// e.g. `FLAC · 24-bit / 96 kHz · MAX`.
pub fn describe(info: &StreamInfo) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(c) = info.codec.as_deref().filter(|c| !c.is_empty()) {
        parts.push(codec_name(c));
    }
    match (info.bit_depth, info.sample_rate) {
        (Some(b), Some(r)) => parts.push(format!("{b}-bit / {} kHz", khz(r))),
        (None, Some(r)) => parts.push(format!("{} kHz", khz(r))),
        _ => {}
    }
    if let Some(tier) = info
        .audio_quality
        .as_deref()
        .map(tier_name)
        .filter(|t| !t.is_empty())
    {
        parts.push(tier);
    }
    if info.audio_mode.as_deref() == Some("DOLBY_ATMOS") {
        parts.push("ATMOS".into());
    }
    parts.join(" · ")
}

fn khz(hz: u32) -> String {
    if hz.is_multiple_of(1000) {
        format!("{}", hz / 1000)
    } else {
        format!("{:.1}", f64::from(hz) / 1000.0)
    }
}

fn codec_name(c: &str) -> String {
    match c.to_ascii_lowercase().as_str() {
        "flac" => "FLAC".into(),
        "mp4a.40.2" | "aac" | "aaclc" => "AAC".into(),
        "mp4a.40.5" => "HE-AAC".into(),
        "eac3" | "ec-3" | "eac3_joc" => "E-AC-3".into(),
        other => other.to_uppercase(),
    }
}

fn tier_name(q: &str) -> String {
    match q {
        "HI_RES_LOSSLESS" => "MAX",
        "HI_RES" => "HI-RES",
        "LOSSLESS" => "LOSSLESS",
        "HIGH" => "HIGH",
        "LOW" => "LOW",
        _ => "",
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(codec: &str, bits: Option<u32>, rate: Option<u32>, quality: &str) -> StreamInfo {
        StreamInfo {
            url: String::new(),
            codec: Some(codec.into()),
            bit_depth: bits,
            sample_rate: rate,
            audio_quality: Some(quality.into()),
            audio_mode: None,
            asset_presentation: None,
            manifest: None,
            manifest_mime_type: None,
            ..StreamInfo::default()
        }
    }

    #[test]
    fn describes_hi_res_and_lossy_streams() {
        assert_eq!(
            describe(&info("flac", Some(24), Some(96_000), "HI_RES_LOSSLESS")),
            "FLAC · 24-bit / 96 kHz · MAX"
        );
        assert_eq!(
            describe(&info("flac", Some(16), Some(44_100), "LOSSLESS")),
            "FLAC · 16-bit / 44.1 kHz · LOSSLESS"
        );
        assert_eq!(
            describe(&info("mp4a.40.2", None, None, "HIGH")),
            "AAC · HIGH"
        );
    }

    #[test]
    fn unknown_tiers_are_left_out() {
        assert_eq!(describe(&info("flac", None, None, "WEIRD")), "FLAC");
    }
}
