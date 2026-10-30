//! Human-readable description of how a track is being streamed.

use serde_json::Value;
use slate_core::queue::Track;
use slate_core::tidal_api::{MediaMetadata, StreamInfo};

/// What the badges next to a track or album say, from the quality data TIDAL
/// already sent with it (no request of its own).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Badges {
    /// 0 none, 1 Lossless, 2 Hi-Res.
    pub tier: i32,
    pub atmos: bool,
}

fn badges_of(audio_quality: Option<&str>, tags: &[&str], modes: &[&str]) -> Badges {
    let has_tag = |t: &str| tags.iter().any(|x| x.eq_ignore_ascii_case(t));
    // `HI_RES` (and the `MQA` tag) is TIDAL's old MQA tier, which now streams
    // as 16-bit FLAC: Lossless, not Hi-Res.
    let tier = if has_tag("HIRES_LOSSLESS") || audio_quality == Some("HI_RES_LOSSLESS") {
        2
    } else if has_tag("LOSSLESS")
        || has_tag("MQA")
        || matches!(audio_quality, Some("LOSSLESS" | "HI_RES"))
    {
        1
    } else {
        0
    };
    let atmos =
        has_tag("DOLBY_ATMOS") || modes.iter().any(|m| m.eq_ignore_ascii_case("DOLBY_ATMOS"));
    Badges { tier, atmos }
}

fn strings(v: Option<&Value>) -> Vec<&str> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// Badges for a track row or the player bar. Videos have none.
pub fn track_badges(t: &Track) -> Badges {
    if t.is_video() {
        return Badges::default();
    }
    badges_of(
        t.extra.get("audioQuality").and_then(Value::as_str),
        &strings(t.extra.get("mediaMetadata").and_then(|m| m.get("tags"))),
        &strings(t.extra.get("audioModes")),
    )
}

/// Badges for an album header.
pub fn album_badges(
    audio_quality: Option<&str>,
    media_metadata: Option<&MediaMetadata>,
    audio_modes: Option<&[String]>,
) -> Badges {
    let tags: Vec<&str> = media_metadata
        .map(|m| m.tags.iter().map(String::as_str).collect())
        .unwrap_or_default();
    let modes: Vec<&str> = audio_modes
        .map(|m| m.iter().map(String::as_str).collect())
        .unwrap_or_default();
    badges_of(audio_quality, &tags, &modes)
}

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

    fn track_with(v: serde_json::Value) -> Track {
        let mut t = Track::new(1);
        if let serde_json::Value::Object(m) = v {
            t.extra = m;
        }
        t
    }

    #[test]
    fn hi_res_wins_over_lossless_and_atmos_is_separate() {
        let b = track_badges(&track_with(serde_json::json!({
            "audioQuality": "HI_RES_LOSSLESS",
            "mediaMetadata": {"tags": ["LOSSLESS", "HIRES_LOSSLESS", "DOLBY_ATMOS"]}
        })));
        assert_eq!(
            b,
            Badges {
                tier: 2,
                atmos: true
            }
        );
    }

    #[test]
    fn lossless_and_lossy_tracks() {
        let lossless = track_badges(&track_with(serde_json::json!({
            "audioQuality": "LOSSLESS", "mediaMetadata": {"tags": ["LOSSLESS"]}
        })));
        assert_eq!(
            lossless,
            Badges {
                tier: 1,
                atmos: false
            }
        );
        for q in ["HIGH", "LOW"] {
            assert_eq!(
                track_badges(&track_with(serde_json::json!({"audioQuality": q}))),
                Badges::default()
            );
        }
        assert_eq!(
            track_badges(&track_with(serde_json::json!({}))),
            Badges::default()
        );
    }

    #[test]
    fn the_old_mqa_tier_is_lossless_not_hi_res() {
        let mqa = track_badges(&track_with(serde_json::json!({
            "audioQuality": "HI_RES", "mediaMetadata": {"tags": ["LOSSLESS", "MQA"]}
        })));
        assert_eq!(mqa.tier, 1);
        let tag_only = track_badges(&track_with(serde_json::json!({
            "audioQuality": "HIGH", "mediaMetadata": {"tags": ["MQA"]}
        })));
        assert_eq!(tag_only.tier, 1);
    }

    #[test]
    fn atmos_can_come_from_the_audio_modes() {
        let b = track_badges(&track_with(serde_json::json!({
            "audioQuality": "LOSSLESS", "audioModes": ["STEREO", "DOLBY_ATMOS"]
        })));
        assert!(b.atmos);
    }

    #[test]
    fn videos_and_albums() {
        let video =
            track_with(serde_json::json!({"itemType": "video", "audioQuality": "LOSSLESS"}));
        assert_eq!(track_badges(&video), Badges::default());
        let meta = MediaMetadata {
            tags: vec!["HIRES_LOSSLESS".into()],
        };
        assert_eq!(album_badges(Some("LOSSLESS"), Some(&meta), None).tier, 2);
        assert_eq!(album_badges(None, None, None), Badges::default());
        assert!(album_badges(None, None, Some(&["DOLBY_ATMOS".to_string()])).atmos);
    }
}
