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

/// Keys of the tracks already in a playlist.
pub fn keys_of(tracks: &[TidalTrack]) -> std::collections::HashSet<TrackKey> {
    tracks.iter().map(track_key).collect()
}

/// Suggestions that are not in the playlist already (by ISRC, else id) and not repeated among
/// themselves, order kept.
pub fn filter_suggestions(
    candidates: Vec<TidalTrack>,
    existing: &std::collections::HashSet<TrackKey>,
) -> Vec<TidalTrack> {
    let mut seen = existing.clone();
    candidates
        .into_iter()
        .filter(|t| seen.insert(track_key(t)))
        .collect()
}

/// Result of comparing two playlists; indices point into the lists that were compared.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Comparison {
    /// Tracks of A that B lacks (first occurrence each, A's order).
    pub only_a: Vec<usize>,
    /// Tracks of B that A lacks.
    pub only_b: Vec<usize>,
    /// Tracks of A that B also has (indices into A).
    pub both: Vec<usize>,
}

fn first_occurrences(t: &[TidalTrack]) -> Vec<(usize, TrackKey)> {
    let mut seen = std::collections::HashSet::new();
    t.iter()
        .enumerate()
        .map(|(i, x)| (i, track_key(x)))
        .filter(|(_, k)| seen.insert(k.clone()))
        .collect()
}

pub fn compare(a: &[TidalTrack], b: &[TidalTrack]) -> Comparison {
    let in_b: std::collections::HashSet<TrackKey> = b.iter().map(track_key).collect();
    let in_a: std::collections::HashSet<TrackKey> = a.iter().map(track_key).collect();
    let mut c = Comparison::default();
    for (i, k) in first_occurrences(a) {
        if in_b.contains(&k) {
            c.both.push(i);
        } else {
            c.only_a.push(i);
        }
    }
    for (i, k) in first_occurrences(b) {
        if !in_a.contains(&k) {
            c.only_b.push(i);
        }
    }
    c
}

/// Track ids at `indices`, in that order.
pub fn ids_at(tracks: &[TidalTrack], indices: &[usize]) -> Vec<u64> {
    indices
        .iter()
        .filter_map(|i| tracks.get(*i).map(|t| t.id))
        .collect()
}

/// A's tracks followed by B's that A lacks, without repeats.
pub fn merged_ids(a: &[TidalTrack], b: &[TidalTrack]) -> Vec<u64> {
    let mut seen = std::collections::HashSet::new();
    a.iter()
        .chain(b.iter())
        .filter(|t| seen.insert(track_key(t)))
        .map(|t| t.id)
        .collect()
}

/// The single-item moves `(from, to)` that turn the order `current` into `target`, applied in
/// sequence. Ids missing from either side are ignored.
pub fn moves_to_order(current: &[u64], target: &[u64]) -> Vec<(u32, u32)> {
    let mut cur: Vec<u64> = current
        .iter()
        .copied()
        .filter(|i| target.contains(i))
        .collect();
    let want: Vec<u64> = target.iter().copied().filter(|i| cur.contains(i)).collect();
    let mut moves = Vec::new();
    for (i, id) in want.iter().enumerate() {
        if cur.get(i) == Some(id) {
            continue;
        }
        if let Some(j) = cur.iter().position(|x| x == id) {
            let item = cur.remove(j);
            cur.insert(i, item);
            moves.push((j as u32, i as u32));
        }
    }
    moves
}

/// `ids` without repeats, order kept.
pub fn unique_ids(ids: &[u64]) -> Vec<u64> {
    let mut seen = std::collections::HashSet::new();
    ids.iter().copied().filter(|i| seen.insert(*i)).collect()
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

    #[test]
    fn suggestions_skip_what_the_playlist_has_and_repeats() {
        let have = keys_of(&[track(1, Some("A")), track(2, None)]);
        let cands = vec![
            track(10, Some("a")), // same ISRC as track 1
            track(2, None),       // same id as track 2
            track(11, Some("B")),
            track(12, Some("B")), // repeat of the previous suggestion
            track(13, None),
        ];
        let ids: Vec<u64> = filter_suggestions(cands, &have)
            .iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(ids, vec![11, 13]);
        assert!(filter_suggestions(vec![], &have).is_empty());
    }

    #[test]
    fn compare_splits_into_a_only_b_only_and_both() {
        let a = [
            track(1, Some("X")),
            track(2, Some("Y")),
            track(3, None),
            track(4, Some("X")),
        ];
        let b = [
            track(10, Some("Y")),
            track(3, None),
            track(11, Some("Z")),
            track(12, Some("Z")),
        ];
        let c = compare(&a, &b);
        assert_eq!(c.only_a, vec![0]); // X, once (the repeat at 3 collapses)
        assert_eq!(c.both, vec![1, 2]);
        assert_eq!(c.only_b, vec![2]); // Z, once
        assert_eq!(ids_at(&b, &c.only_b), vec![11]);
    }

    #[test]
    fn merge_is_a_then_new_from_b_without_repeats() {
        let a = [track(1, Some("X")), track(2, Some("Y"))];
        let b = [
            track(9, Some("Y")),
            track(3, Some("Z")),
            track(3, Some("Z")),
        ];
        assert_eq!(merged_ids(&a, &b), vec![1, 2, 3]);
        assert!(merged_ids(&[], &[]).is_empty());
        assert_eq!(unique_ids(&[3, 1, 3, 2, 1]), vec![3, 1, 2]);
    }

    #[test]
    fn comparing_with_an_empty_list() {
        let a = [track(1, None)];
        assert_eq!(compare(&a, &[]).only_a, vec![0]);
        assert_eq!(compare(&[], &a).only_b, vec![0]);
        assert_eq!(compare(&a, &a).both, vec![0]);
    }

    #[test]
    fn moves_restore_an_order() {
        let apply = |mut v: Vec<u64>, moves: &[(u32, u32)]| {
            for (f, t) in moves {
                let x = v.remove(*f as usize);
                v.insert(*t as usize, x);
            }
            v
        };
        let target = vec![1, 2, 3, 4, 5];
        for cur in [
            vec![5, 4, 3, 2, 1],
            vec![2, 1, 3, 4, 5],
            vec![1, 2, 3, 4, 5],
            vec![3, 1, 5, 2, 4],
        ] {
            let m = moves_to_order(&cur, &target);
            assert_eq!(apply(cur.clone(), &m), target, "{cur:?}");
        }
        assert!(moves_to_order(&target, &target).is_empty());
        // Ids that vanished are ignored.
        assert_eq!(moves_to_order(&[2, 1], &[1, 2, 9]), vec![(1, 0)]);
    }
}
