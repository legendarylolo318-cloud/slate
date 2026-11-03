//! "Smarter shuffle": a random order that avoids the same artist, then the same
//! album, twice in a row when it can. One pass over the list when it is shuffled
//! (shuffle turned on, a source or snapshot loaded, tracks appended); nothing runs
//! while playing. Off by default, in which case the plain shuffle is used.

use rand::seq::SliceRandom;
use serde_json::Value;

use crate::queue::Track;

/// How far ahead of the next slot a better neighbour is looked for. Bounds the
/// pass at `n * WINDOW` steps, so a 5000-track queue costs well under a millisecond.
const WINDOW: usize = 64;

fn artist_key(t: &Track) -> &str {
    t.artist_name()
}

fn album_key(t: &Track) -> Option<&Value> {
    t.extra.get("album").and_then(|a| a.get("id"))
}

/// Shuffles `tracks`, then reorders greedily so that, where the list allows it,
/// neither the artist nor the album of the previous track (`prev` for the first
/// slot: the track that is playing) repeats. A track with no artist is never
/// treated as a repeat.
pub fn spread_shuffle(tracks: &mut Vec<Track>, prev: Option<&Track>, rng: &mut impl rand::Rng) {
    tracks.shuffle(rng);
    spread(tracks, prev);
}

/// The reordering step of [`spread_shuffle`] alone (deterministic).
pub fn spread(tracks: &mut Vec<Track>, prev: Option<&Track>) {
    let mut pool: std::collections::VecDeque<Track> = std::mem::take(tracks).into();
    let mut out: Vec<Track> = Vec::with_capacity(pool.len());
    while !pool.is_empty() {
        let last = out.last().or(prev);
        let pick = last.and_then(|l| best_neighbour(&pool, l)).unwrap_or(0);
        if let Some(t) = pool.remove(pick) {
            out.push(t);
        }
    }
    *tracks = out;
}

/// Index (within the window) of the first track that differs from `last` in artist
/// and album, else in artist, else in album. `None` when nothing differs.
fn best_neighbour(pool: &std::collections::VecDeque<Track>, last: &Track) -> Option<usize> {
    let (la, lb) = (artist_key(last), album_key(last));
    let same_artist = |t: &Track| !la.is_empty() && artist_key(t) == la;
    let same_album = |t: &Track| lb.is_some() && album_key(t) == lb;
    let window = pool.iter().take(WINDOW);
    let mut artist_only = None;
    let mut album_only = None;
    for (i, t) in window.enumerate() {
        match (same_artist(t), same_album(t)) {
            (false, false) => return Some(i),
            (false, true) => {
                artist_only.get_or_insert(i);
            }
            (true, false) => {
                album_only.get_or_insert(i);
            }
            (true, true) => {}
        }
    }
    artist_only.or(album_only)
}

/// "Album shuffle": shuffles the albums, not the tracks. Every album's tracks stay together in
/// their album order (disc and track number when all have them, else the order given), so
/// gapless playback and album ReplayGain keep working. A track without an album is a group of
/// one. The album of `prev` (the playing track) goes first so it plays on to its end. With
/// `spread_artists`, consecutive albums by the same artist are avoided when the list allows.
pub fn album_shuffle(
    tracks: &mut Vec<Track>,
    prev: Option<&Track>,
    spread_artists: bool,
    rng: &mut impl rand::Rng,
) {
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut groups: Vec<Vec<Track>> = Vec::new();
    for t in std::mem::take(tracks) {
        match album_key(&t).map(Value::to_string) {
            Some(k) => {
                let i = *index.entry(k).or_insert_with(|| {
                    groups.push(Vec::new());
                    groups.len() - 1
                });
                groups[i].push(t);
            }
            None => groups.push(vec![t]),
        }
    }
    for g in &mut groups {
        let num = |t: &Track, k: &str| t.extra.get(k).and_then(Value::as_u64);
        if g.len() > 1 && g.iter().all(|t| num(t, "trackNumber").is_some()) {
            g.sort_by_key(|t| (num(t, "volumeNumber").unwrap_or(1), num(t, "trackNumber")));
        }
    }
    groups.shuffle(rng);
    let mut out: Vec<Track> = Vec::new();
    let playing_album = prev.and_then(album_key);
    if let Some(pos) = playing_album.and_then(|a| {
        groups
            .iter()
            .position(|g| g.first().and_then(album_key) == Some(a))
    }) {
        out.extend(groups.remove(pos));
    }
    let mut pool: std::collections::VecDeque<Vec<Track>> = groups.into();
    while !pool.is_empty() {
        let pick = if spread_artists {
            let last = out.last().or(prev);
            last.and_then(|l| {
                pool.iter().take(WINDOW).position(|g| {
                    let a = g.first().map_or("", artist_key);
                    a.is_empty() || a != artist_key(l)
                })
            })
            .unwrap_or(0)
        } else {
            0
        };
        if let Some(g) = pool.remove(pick) {
            out.extend(g);
        }
    }
    *tracks = out;
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    fn tr(id: u64, artist: &str, album: u64) -> Track {
        let mut t = Track::new(id);
        t.extra
            .insert("artist".into(), serde_json::json!({ "name": artist }));
        t.extra
            .insert("album".into(), serde_json::json!({ "id": album }));
        t
    }

    fn adjacent_same_artist(v: &[Track]) -> usize {
        v.windows(2)
            .filter(|w| w[0].artist_name() == w[1].artist_name())
            .count()
    }

    #[test]
    fn two_artists_alternate() {
        let mut v: Vec<Track> = (0..6)
            .map(|i| tr(i, if i < 3 { "A" } else { "B" }, i))
            .collect();
        spread(&mut v, None);
        assert_eq!(adjacent_same_artist(&v), 0);
        assert_eq!(v.len(), 6);
    }

    #[test]
    fn the_playing_artist_does_not_come_first() {
        let playing = tr(99, "A", 1);
        let mut v = vec![tr(1, "A", 2), tr(2, "B", 3)];
        spread(&mut v, Some(&playing));
        assert_eq!(v[0].artist_name(), "B");
    }

    #[test]
    fn nothing_is_lost_or_duplicated() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        let mut v: Vec<Track> = (0..200)
            .map(|i| tr(i, &format!("a{}", i % 7), i % 13))
            .collect();
        spread_shuffle(&mut v, None, &mut rng);
        let mut ids: Vec<u64> = v.iter().map(|t| t.id).collect();
        ids.sort_unstable();
        assert_eq!(ids, (0..200).collect::<Vec<_>>());
        assert_eq!(
            adjacent_same_artist(&v),
            0,
            "7 artists in 200 tracks can always be spread"
        );
    }

    #[test]
    fn one_artist_keeps_working() {
        let mut v: Vec<Track> = (0..5).map(|i| tr(i, "A", i)).collect();
        spread(&mut v, None);
        assert_eq!(v.len(), 5);
    }

    #[test]
    fn same_album_is_avoided_when_the_artist_cannot_be() {
        // Everything is by A, two albums: the albums alternate.
        let mut v = vec![tr(1, "A", 1), tr(2, "A", 1), tr(3, "A", 2), tr(4, "A", 2)];
        spread(&mut v, None);
        assert!(
            v.windows(2).all(|w| album_key(&w[0]) != album_key(&w[1])),
            "{:?}",
            v.iter().map(|t| t.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn unknown_artists_are_not_repeats_of_each_other() {
        let mut v = vec![Track::new(1), Track::new(2), tr(3, "A", 1)];
        spread(&mut v, None);
        assert_eq!(v.iter().map(|t| t.id).collect::<Vec<_>>(), vec![1, 2, 3]);
    }

    fn numbered(id: u64, artist: &str, album: u64, n: u64) -> Track {
        let mut t = tr(id, artist, album);
        t.extra.insert("trackNumber".into(), n.into());
        t
    }

    fn album_runs(v: &[Track]) -> Vec<u64> {
        let mut runs = Vec::new();
        for t in v {
            let a = album_key(t).and_then(Value::as_u64).unwrap_or(0);
            if runs.last() != Some(&a) {
                runs.push(a);
            }
        }
        runs
    }

    #[test]
    fn albums_stay_together_in_track_order() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(3);
        for _ in 0..20 {
            let mut v = Vec::new();
            for album in 1..=6u64 {
                // Handed over out of order: sorted by track number inside the album.
                for n in [3u64, 1, 2, 4] {
                    v.push(numbered(album * 10 + n, "A", album, n));
                }
            }
            album_shuffle(&mut v, None, false, &mut rng);
            assert_eq!(v.len(), 24);
            let runs = album_runs(&v);
            assert_eq!(runs.len(), 6, "each album is one run: {runs:?}");
            for chunk in v.chunks(4) {
                let ids: Vec<u64> = chunk.iter().map(|t| t.id % 10).collect();
                assert_eq!(ids, vec![1, 2, 3, 4]);
                assert!(chunk.iter().all(|t| t.id / 10 == chunk[0].id / 10));
            }
        }
    }

    #[test]
    fn without_track_numbers_the_given_order_is_kept() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(1);
        let mut v = vec![tr(1, "A", 1), tr(2, "B", 2), tr(3, "A", 1), tr(4, "B", 2)];
        album_shuffle(&mut v, None, false, &mut rng);
        let ids: Vec<u64> = v.iter().map(|t| t.id).collect();
        assert!(
            ids == vec![1, 3, 2, 4] || ids == vec![2, 4, 1, 3],
            "{ids:?}"
        );
    }

    #[test]
    fn tracks_without_an_album_are_their_own_group_and_nothing_is_lost() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(9);
        let mut v: Vec<Track> = (0..50).map(Track::new).collect();
        v.push(tr(100, "A", 1));
        album_shuffle(&mut v, None, true, &mut rng);
        let mut ids: Vec<u64> = v.iter().map(|t| t.id).collect();
        ids.sort_unstable();
        let mut want: Vec<u64> = (0..50).collect();
        want.push(100);
        assert_eq!(ids, want);
    }

    #[test]
    fn the_playing_album_plays_on_first() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(5);
        let playing = numbered(21, "B", 2, 1);
        for _ in 0..10 {
            let mut v = vec![
                numbered(11, "A", 1, 1),
                numbered(12, "A", 1, 2),
                numbered(22, "B", 2, 2),
                numbered(23, "B", 2, 3),
                numbered(31, "C", 3, 1),
            ];
            album_shuffle(&mut v, Some(&playing), false, &mut rng);
            assert_eq!(v[0].id, 22);
            assert_eq!(v[1].id, 23);
        }
    }

    #[test]
    fn smart_album_shuffle_avoids_one_artist_twice_in_a_row() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(11);
        for _ in 0..20 {
            let mut v = Vec::new();
            for album in 1..=8u64 {
                let artist = if album % 2 == 0 { "A" } else { "B" };
                v.push(numbered(album * 10 + 1, artist, album, 1));
                v.push(numbered(album * 10 + 2, artist, album, 2));
            }
            album_shuffle(&mut v, None, true, &mut rng);
            let firsts: Vec<&str> = v.chunks(2).map(|c| c[0].artist_name()).collect();
            assert!(
                firsts.windows(2).all(|w| w[0] != w[1]),
                "two artists alternate: {firsts:?}"
            );
        }
    }

    #[test]
    fn a_single_artist_still_shuffles() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(2);
        let mut v: Vec<Track> = (1..=4).map(|a| numbered(a, "A", a, 1)).collect();
        album_shuffle(&mut v, None, true, &mut rng);
        assert_eq!(v.len(), 4);
    }
}
