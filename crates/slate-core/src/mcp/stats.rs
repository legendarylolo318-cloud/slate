//! Listening statistics from the local play log (kept only while the MCP server runs).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub const MAX_PLAYS: usize = 5000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayRec {
    /// Unix seconds when the track started.
    pub t: u64,
    pub id: u64,
    pub title: String,
    pub artist: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Count {
    pub name: String,
    pub plays: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Stats {
    pub plays: u32,
    pub top_artists: Vec<Count>,
    pub top_tracks: Vec<Count>,
}

/// Seconds in a named period ("day", "week", "month", "year", "all").
pub fn period_secs(period: &str) -> Option<u64> {
    match period {
        "day" => Some(86_400),
        "week" => Some(7 * 86_400),
        "month" => Some(30 * 86_400),
        "year" => Some(365 * 86_400),
        "all" => Some(u64::MAX),
        _ => None,
    }
}

fn top(counts: HashMap<String, u32>, limit: usize) -> Vec<Count> {
    let mut v: Vec<Count> = counts
        .into_iter()
        .map(|(name, plays)| Count { name, plays })
        .collect();
    v.sort_by(|a, b| b.plays.cmp(&a.plays).then_with(|| a.name.cmp(&b.name)));
    v.truncate(limit);
    v
}

pub fn stats(plays: &[PlayRec], now: u64, period_secs: u64, limit: usize) -> Stats {
    let since = now.saturating_sub(period_secs);
    let mut artists: HashMap<String, u32> = HashMap::new();
    let mut tracks: HashMap<String, u32> = HashMap::new();
    let mut n = 0;
    for p in plays.iter().filter(|p| p.t >= since) {
        n += 1;
        if !p.artist.is_empty() {
            *artists.entry(p.artist.clone()).or_default() += 1;
        }
        *tracks
            .entry(format!("{} – {}", p.artist, p.title))
            .or_default() += 1;
    }
    Stats {
        plays: n,
        top_artists: top(artists, limit),
        top_tracks: top(tracks, limit),
    }
}

/// Appends a play, keeping at most [`MAX_PLAYS`]. A repeat of the same track within 5 seconds
/// (a duplicate notification) is ignored.
pub fn push(plays: &mut Vec<PlayRec>, rec: PlayRec) {
    if plays
        .last()
        .is_some_and(|l| l.id == rec.id && rec.t.saturating_sub(l.t) < 5)
    {
        return;
    }
    plays.push(rec);
    if plays.len() > MAX_PLAYS {
        let extra = plays.len() - MAX_PLAYS;
        plays.drain(..extra);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(t: u64, id: u64, artist: &str) -> PlayRec {
        PlayRec {
            t,
            id,
            title: format!("T{id}"),
            artist: artist.into(),
        }
    }

    #[test]
    fn counts_top_artists_and_tracks_within_the_period() {
        let plays = vec![
            rec(10, 1, "A"),
            rec(1_000_000, 1, "A"),
            rec(1_000_100, 2, "A"),
            rec(1_000_200, 3, "B"),
        ];
        let s = stats(&plays, 1_000_300, 86_400, 5);
        assert_eq!(s.plays, 3);
        assert_eq!(
            s.top_artists[0],
            Count {
                name: "A".into(),
                plays: 2
            }
        );
        assert_eq!(s.top_tracks.len(), 3);
        let all = stats(&plays, 1_000_300, u64::MAX, 1);
        assert_eq!(all.plays, 4);
        assert_eq!(all.top_artists.len(), 1);
        assert_eq!(all.top_tracks[0].plays, 2);
    }

    #[test]
    fn periods() {
        assert_eq!(period_secs("week"), Some(604_800));
        assert_eq!(period_secs("all"), Some(u64::MAX));
        assert_eq!(period_secs("decade"), None);
    }

    #[test]
    fn the_play_log_is_bounded_and_ignores_double_reports() {
        let mut v = Vec::new();
        push(&mut v, rec(100, 1, "A"));
        push(&mut v, rec(102, 1, "A"));
        assert_eq!(v.len(), 1);
        for i in 0..(MAX_PLAYS as u64 + 10) {
            push(&mut v, rec(1000 + i * 10, i + 10, "A"));
        }
        assert_eq!(v.len(), MAX_PLAYS);
        assert_eq!(stats(&[], 0, 1, 5).plays, 0);
    }
}
