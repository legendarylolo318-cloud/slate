//! "Other versions" of a track or album, found by searching the primary artist plus the
//! title with version noise removed (see `crate::versions`).

use serde_json::{json, Value};

use crate::error::SlateError;
use crate::tidal_api::{TidalAlbumDetail, TidalTrack};
use crate::versions::{self, Candidate};
use crate::AppState;

fn year_of(date: Option<&str>) -> Option<i32> {
    date.and_then(|d| d.get(..4)).and_then(|y| y.parse().ok())
}

fn artist_of(t: &TidalTrack) -> String {
    t.artists
        .as_ref()
        .map(|a| {
            a.iter()
                .map(|x| x.name.clone())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|s| !s.is_empty())
        .or_else(|| t.artist.as_ref().map(|a| a.name.clone()))
        .unwrap_or_default()
}

fn quality_tier(quality: Option<&str>) -> i32 {
    match quality {
        Some("HI_RES_LOSSLESS" | "HI_RES") => 2,
        Some("LOSSLESS") => 1,
        _ => 0,
    }
}

fn track_candidate(t: &TidalTrack) -> Candidate {
    let atmos = t.title.to_lowercase().contains("atmos")
        || t.version
            .as_deref()
            .is_some_and(|v| v.to_lowercase().contains("atmos"));
    Candidate {
        id: t.id,
        title: match t.version.as_deref() {
            Some(v) if !v.is_empty() => format!("{} ({v})", t.title),
            _ => t.title.clone(),
        },
        version: t.version.clone(),
        artist: artist_of(t),
        year: year_of(t.album.as_ref().and_then(|a| a.release_date.as_deref())),
        explicit: t.explicit == Some(true),
        atmos,
        quality: quality_tier(t.audio_quality.as_deref()),
        duration_secs: t.duration,
    }
}

fn describe(c: &Candidate) -> Value {
    json!({
        "id": c.id,
        "title": c.title,
        "label": versions::kind_label(&c.title, c.version.as_deref(), c.explicit, c.atmos),
        "year": c.year,
        "quality": match c.quality { 2 => "Hi-Res", 1 => "Lossless", _ => "" },
        "atmos": c.atmos,
        "explicit": c.explicit,
        "duration_seconds": c.duration_secs,
        "artist": c.artist,
    })
}

/// Other versions of `track_id` (not the track itself), as JSON rows.
pub async fn track_versions(state: &AppState, track_id: u64) -> Result<Vec<Value>, SlateError> {
    let mut client = state.tidal_client.lock().await;
    let v = client.get_track(track_id).await?;
    let track: TidalTrack = serde_json::from_value(v)
        .map_err(|e| SlateError::Parse(format!("track {track_id}: {e}")))?;
    let primary = track
        .artist
        .as_ref()
        .map(|a| a.name.clone())
        .unwrap_or_else(|| artist_of(&track));
    let q = format!("{primary} {}", versions::base_title(&track.title));
    let hits = client.search(&q, 50).await?;
    let cands = hits.tracks.iter().map(track_candidate).collect();
    Ok(versions::others(&track.title, &primary, track_id, cands)
        .iter()
        .map(describe)
        .collect())
}

fn album_candidate(a: &TidalAlbumDetail) -> Candidate {
    Candidate {
        id: a.id,
        title: match a.version.as_deref() {
            Some(v) if !v.is_empty() => format!("{} ({v})", a.title),
            _ => a.title.clone(),
        },
        version: a.version.clone(),
        artist: a
            .artist
            .as_ref()
            .map(|x| x.name.clone())
            .unwrap_or_default(),
        year: year_of(a.release_date.as_deref()),
        explicit: a.explicit == Some(true),
        atmos: a
            .audio_modes
            .as_ref()
            .is_some_and(|m| m.iter().any(|x| x == "DOLBY_ATMOS")),
        quality: quality_tier(a.audio_quality.as_deref()),
        duration_secs: a.duration.unwrap_or(0),
    }
}

/// Other versions of an album (deluxe, remaster, clean/explicit…), as JSON rows.
pub async fn album_versions(state: &AppState, album_id: u64) -> Result<Vec<Value>, SlateError> {
    let mut client = state.tidal_client.lock().await;
    let album = client.get_album_detail(album_id).await?;
    let primary = album
        .artist
        .as_ref()
        .map(|a| a.name.clone())
        .unwrap_or_default();
    let q = format!("{primary} {}", versions::base_title(&album.title));
    let hits = client.search(&q, 50).await?;
    let cands = hits.albums.iter().map(album_candidate).collect();
    Ok(
        versions::other_albums(&album.title, &primary, album_id, cands)
            .iter()
            .map(describe)
            .collect(),
    )
}
