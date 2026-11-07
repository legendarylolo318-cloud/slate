//! "Fill queue to N minutes": the choice of which tracks to add. Pure; the controller
//! gathers the candidates and applies the result as one undoable edit.

use std::collections::HashSet;

use crate::queue::TrackRef;

/// Presets offered in the queue menu, in minutes.
pub const PRESETS_MIN: [u32; 5] = [15, 30, 45, 60, 90];
/// Custom targets are 1 minute to 10 hours.
pub const MAX_MINUTES: u32 = 600;

/// Minutes typed by the user; `None` when it is not a whole number in range.
pub fn parse_minutes(text: &str) -> Option<u32> {
    let n: u32 = text
        .trim()
        .trim_end_matches("min")
        .trim_end_matches('m')
        .trim()
        .parse()
        .ok()?;
    (1..=MAX_MINUTES).contains(&n).then_some(n)
}

/// The candidates to add so that `have_secs` of queue grows towards `target_secs`.
/// Candidates already in `seen` (or twice in the list) are passed over. Adding stops at
/// the first track that would exceed the target by more than half its own length, and
/// at once when the target is reached.
pub fn select(
    have_secs: f32,
    seen: &HashSet<u64>,
    candidates: impl IntoIterator<Item = TrackRef>,
    target_secs: f32,
) -> Vec<TrackRef> {
    let mut total = have_secs;
    let mut used: HashSet<u64> = HashSet::new();
    let mut picked = Vec::new();
    for t in candidates {
        if total >= target_secs {
            break;
        }
        if seen.contains(&t.id) || !used.insert(t.id) {
            continue;
        }
        let d = t.duration_secs().max(0.0);
        // total + d <= target + d / 2
        if total + d / 2.0 > target_secs {
            break;
        }
        total += d;
        picked.push(t);
    }
    picked
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::Track;
    use std::sync::Arc;

    fn t(id: u64, secs: u32) -> TrackRef {
        Arc::new(
            Track::from_serialized(&serde_json::json!({ "id": id, "duration": secs }))
                .expect("track"),
        )
    }

    fn ids(v: &[TrackRef]) -> Vec<u64> {
        v.iter().map(|t| t.id).collect()
    }

    #[test]
    fn minutes_are_parsed_leniently_and_bounded() {
        assert_eq!(parse_minutes("45"), Some(45));
        assert_eq!(parse_minutes(" 20 min "), Some(20));
        assert_eq!(parse_minutes("0"), None);
        assert_eq!(parse_minutes("601"), None);
        assert_eq!(parse_minutes("1.5"), None);
        assert_eq!(parse_minutes("-3"), None);
        assert_eq!(parse_minutes(""), None);
    }

    #[test]
    fn it_fills_up_to_the_target() {
        let c: Vec<_> = (1..=10).map(|i| t(i, 240)).collect();
        // 15 minutes from empty: 4 x 4 min = 16 min; the fourth passes (1 min over < 2 min).
        let got = select(0.0, &HashSet::new(), c, 15.0 * 60.0);
        assert_eq!(ids(&got), vec![1, 2, 3, 4]);
    }

    #[test]
    fn it_stops_at_the_first_track_that_overshoots_by_more_than_half_its_length() {
        // 10 min have; target 15; a 20-minute track would end 15 minutes over: stop,
        // even though the short one behind it would fit.
        let c = vec![t(1, 1200), t(2, 120)];
        assert!(select(600.0, &HashSet::new(), c, 900.0).is_empty());
        // A 10-minute track lands exactly 5 over = half its length: allowed.
        let c = vec![t(3, 600), t(4, 60)];
        assert_eq!(ids(&select(600.0, &HashSet::new(), c, 900.0)), vec![3]);
    }

    #[test]
    fn nothing_is_added_when_the_queue_is_long_enough_and_known_tracks_are_skipped() {
        let c = vec![t(1, 200), t(2, 200)];
        assert!(select(900.0, &HashSet::new(), c.clone(), 900.0).is_empty());
        let seen: HashSet<u64> = [1].into_iter().collect();
        let got = select(0.0, &seen, vec![t(1, 200), t(2, 200), t(2, 200)], 400.0);
        assert_eq!(
            ids(&got),
            vec![2],
            "known and repeated tracks are passed over"
        );
    }
}
