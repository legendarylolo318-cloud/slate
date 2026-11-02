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
}
