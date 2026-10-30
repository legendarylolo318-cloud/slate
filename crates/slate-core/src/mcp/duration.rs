//! Durations for agents: totals, what is left of the queue, "about N minutes" playlists and
//! the local end time.

/// One candidate for a target-length selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: u64,
    pub secs: u32,
}

pub const TOLERANCE_SECS: u32 = 3 * 60;

pub fn total_secs(items: &[Item]) -> u32 {
    items.iter().map(|i| i.secs).sum()
}

/// Seconds left: what remains of the current track plus every queued track.
pub fn remaining_secs(current_duration: u32, position: u32, upcoming: &[u32]) -> u32 {
    current_duration.saturating_sub(position) + upcoming.iter().sum::<u32>()
}

/// Adds candidates in order until the total is within ±3 minutes of `target_minutes`, skipping
/// a candidate that would overshoot past the window while a later, shorter one could still fit.
/// Returns the chosen items and their total in seconds.
pub fn build_to_target(candidates: &[Item], target_minutes: u32) -> (Vec<Item>, u32) {
    let target = target_minutes.saturating_mul(60);
    let upper = target + TOLERANCE_SECS;
    let lower = target.saturating_sub(TOLERANCE_SECS);
    let mut chosen = Vec::new();
    let mut total = 0u32;
    for c in candidates {
        if total >= lower {
            break;
        }
        if total + c.secs <= upper {
            total += c.secs;
            chosen.push(c.clone());
        }
    }
    (chosen, total)
}

/// Whether more candidates would be needed to reach the window.
pub fn needs_more(total: u32, target_minutes: u32) -> bool {
    total < (target_minutes * 60).saturating_sub(TOLERANCE_SECS)
}

/// "HH:MM" local end time given `remaining` seconds from `now_local_secs` (seconds since local
/// midnight).
pub fn ends_at(now_local_secs: u32, remaining: u32) -> String {
    let t = (u64::from(now_local_secs) + u64::from(remaining)) % 86_400;
    format!("{:02}:{:02}", t / 3600, (t % 3600) / 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(secs: &[u32]) -> Vec<Item> {
        secs.iter()
            .enumerate()
            .map(|(i, s)| Item {
                id: i as u64,
                secs: *s,
            })
            .collect()
    }

    #[test]
    fn totals_and_remaining() {
        assert_eq!(total_secs(&items(&[100, 200])), 300);
        assert_eq!(total_secs(&[]), 0);
        assert_eq!(remaining_secs(240, 100, &[200, 60]), 400);
        assert_eq!(remaining_secs(240, 999, &[]), 0);
    }

    #[test]
    fn target_is_reached_within_three_minutes() {
        let c = items(&[240; 30]);
        let (chosen, total) = build_to_target(&c, 30);
        assert!(total.abs_diff(1800) <= TOLERANCE_SECS, "{total}");
        assert_eq!(chosen.len() as u32 * 240, total);
        // Stops as soon as it is inside the window, not at the exact target.
        assert!(total >= 1800 - TOLERANCE_SECS);
    }

    #[test]
    fn long_tracks_are_skipped_when_they_would_overshoot() {
        let c = items(&[600, 600, 3000, 120, 120]);
        let (chosen, total) = build_to_target(&c, 25);
        // 600+600 = 1200, the 3000 one would overshoot, one 120 s filler reaches the window (25 min ± 3 = 1320 s).
        assert_eq!(total, 1320);
        assert_eq!(
            chosen.iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![0, 1, 3]
        );
        assert!(!needs_more(total, 25));
    }

    #[test]
    fn running_out_of_candidates_reports_it() {
        let (chosen, total) = build_to_target(&items(&[120, 120]), 60);
        assert_eq!((chosen.len(), total), (2, 240));
        assert!(needs_more(total, 60));
        assert_eq!(build_to_target(&[], 10), (vec![], 0));
    }

    #[test]
    fn end_time_wraps_midnight() {
        assert_eq!(ends_at(10 * 3600, 90 * 60), "11:30");
        assert_eq!(ends_at(23 * 3600 + 50 * 60, 20 * 60), "00:10");
        assert_eq!(ends_at(0, 0), "00:00");
    }
}
