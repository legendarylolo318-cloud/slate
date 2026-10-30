//! Pure playlist helpers: duplicate detection (and, later, comparison) by ISRC with a
//! fallback to the TIDAL track id.

use std::collections::HashMap;

use crate::tidal_api::TidalTrack;

/// What makes two playlist entries "the same track".
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TrackKey {
    Isrc(String),
    Id(u64),
}

pub fn track_key(t: &TidalTrack) -> TrackKey {
    match t.isrc.as_deref().map(str::trim) {
        Some(isrc) if !isrc.is_empty() => TrackKey::Isrc(isrc.to_ascii_uppercase()),
        _ => TrackKey::Id(t.id),
    }
}

/// A later occurrence of a track that already appeared earlier in the playlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Duplicate {
    /// Position of this copy in the playlist (what the remove API takes).
    pub index: u32,
    pub track_id: u64,
    /// Position of the first occurrence, which is kept.
    pub kept_index: u32,
}

/// Every entry after the first occurrence of its key, in playlist order.
pub fn find_duplicates(tracks: &[TidalTrack]) -> Vec<Duplicate> {
    let mut first: HashMap<TrackKey, u32> = HashMap::with_capacity(tracks.len());
    let mut out = Vec::new();
    for (i, t) in tracks.iter().enumerate() {
        let index = i as u32;
        match first.get(&track_key(t)) {
            Some(&kept_index) => out.push(Duplicate {
                index,
                track_id: t.id,
                kept_index,
            }),
            None => {
                first.insert(track_key(t), index);
            }
        }
    }
    out
}

/// Indices to remove, highest first, so earlier removals never shift later ones.
pub fn removal_order(dups: &[Duplicate]) -> Vec<u32> {
    let mut v: Vec<u32> = dups.iter().map(|d| d.index).collect();
    v.sort_unstable_by(|a, b| b.cmp(a));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: u64, isrc: Option<&str>) -> TidalTrack {
        let mut v = serde_json::json!({ "id": id, "title": "t", "duration": 1 });
        if let Some(i) = isrc {
            v["isrc"] = serde_json::json!(i);
        }
        serde_json::from_value(v).expect("track json")
    }

    #[test]
    fn same_isrc_different_id_is_a_duplicate() {
        let t = [track(1, Some("USABC")), track(2, Some("usabc"))];
        let d = find_duplicates(&t);
        assert_eq!(
            d,
            vec![Duplicate {
                index: 1,
                track_id: 2,
                kept_index: 0
            }]
        );
    }

    #[test]
    fn falls_back_to_track_id_without_isrc() {
        let t = [track(5, None), track(6, None), track(5, Some(" "))];
        let d = find_duplicates(&t);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].index, 2);
        assert_eq!(d[0].kept_index, 0);
    }

    #[test]
    fn keeps_first_of_three_and_removes_highest_index_first() {
        let t = [
            track(1, Some("A")),
            track(2, Some("B")),
            track(3, Some("A")),
            track(4, Some("A")),
        ];
        let d = find_duplicates(&t);
        assert_eq!(d.iter().map(|x| x.kept_index).collect::<Vec<_>>(), [0, 0]);
        assert_eq!(removal_order(&d), vec![3, 2]);
    }

    #[test]
    fn no_duplicates_is_empty() {
        assert!(find_duplicates(&[track(1, None), track(2, None)]).is_empty());
        assert!(find_duplicates(&[]).is_empty());
    }
}
