//! Guest track requests: the host's request book, placement, per-guest caps, duplicate
//! rejection, the fair queue, vote skip and host handoff. Pure: no sockets and no clock of its
//! own, so every rule is unit-tested.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Default pending requests one guest may have at once.
pub const DEFAULT_PENDING_CAP: usize = 3;
/// Most requests the host can hold at once, over all guests.
pub const MAX_PENDING_TOTAL: usize = 30;
/// A track played within this many entries of history counts as a recent duplicate.
pub const RECENT_HISTORY: usize = 20;
/// How long a party survives without its host before the session is offered to a guest.
pub const HANDOFF_GRACE: Duration = Duration::from_secs(60);
/// Default share of guests that must vote for a skip to pass.
pub const DEFAULT_VOTE_PERCENT: u8 = 50;

/// What the host does with a guest's request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestMode {
    /// Requests wait for the host (or a co-host) to approve or deny them.
    #[default]
    Approval,
    /// Requests are placed as soon as they arrive.
    Auto,
    /// Guests cannot request tracks.
    Off,
}

impl RequestMode {
    pub fn from_str_or_default(s: &str) -> Self {
        match s {
            "auto" => Self::Auto,
            "off" => Self::Off,
            _ => Self::Approval,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approval => "approval",
            Self::Auto => "auto",
            Self::Off => "off",
        }
    }
}

/// Where an approved request goes in the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    PlayNext,
    AddToEnd,
    /// Round-robin across guests, so one guest cannot flood the queue.
    #[default]
    Fair,
}

impl Placement {
    pub fn from_str_or_default(s: &str) -> Self {
        match s {
            "next" => Self::PlayNext,
            "end" => Self::AddToEnd,
            _ => Self::Fair,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::PlayNext => "next",
            Self::AddToEnd => "end",
            Self::Fair => "fair",
        }
    }
}

/// What the host needs to know about a requested track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestTrack {
    pub id: u64,
    pub title: String,
    pub artist: String,
    pub quality: String,
    pub explicit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub id: u64,
    pub guest_id: u64,
    pub guest: String,
    pub track: RequestTrack,
}

/// Why a request was turned down; `message` is what the guest sees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reject {
    Off,
    Cap(usize),
    Full,
    AlreadyPending,
    AlreadyQueued,
    RecentlyPlayed,
    Explicit,
}

impl Reject {
    pub fn message(&self) -> String {
        match self {
            Self::Off => "The host is not taking requests.".into(),
            Self::Cap(n) => format!("You already have {n} requests waiting. Wait for the host."),
            Self::Full => "The host has too many requests waiting right now.".into(),
            Self::AlreadyPending => "That track is already waiting for approval.".into(),
            Self::AlreadyQueued => "That track is already in the queue.".into(),
            Self::RecentlyPlayed => "That track was played a moment ago.".into(),
            Self::Explicit => "The host does not allow explicit tracks.".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    pub mode: RequestMode,
    pub cap: usize,
    pub no_explicit: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            mode: RequestMode::Approval,
            cap: DEFAULT_PENDING_CAP,
            no_explicit: false,
        }
    }
}

/// What a new request is checked against.
#[derive(Debug, Clone, Copy)]
pub struct QueueView<'a> {
    pub upcoming: &'a [u64],
    pub recent: &'a [u64],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Submitted {
    /// Waiting for a decision.
    Pending(u64),
    /// Mode is Auto: place it now.
    Approved(Request),
}

#[derive(Debug, Default)]
pub struct RequestBook {
    pub policy: Policy,
    pending: Vec<Request>,
    next_id: u64,
    /// Guests who could not play a track (region, plan), by track.
    unavailable: HashMap<u64, HashSet<u64>>,
}

impl RequestBook {
    pub fn new(policy: Policy) -> Self {
        Self {
            policy,
            ..Self::default()
        }
    }

    pub fn pending(&self) -> &[Request] {
        &self.pending
    }

    pub fn submit(
        &mut self,
        guest_id: u64,
        guest: &str,
        track: RequestTrack,
        view: QueueView<'_>,
    ) -> Result<Submitted, Reject> {
        if self.policy.mode == RequestMode::Off {
            return Err(Reject::Off);
        }
        if self.policy.no_explicit && track.explicit {
            return Err(Reject::Explicit);
        }
        if self.pending.iter().any(|r| r.track.id == track.id) {
            return Err(Reject::AlreadyPending);
        }
        if view.upcoming.contains(&track.id) {
            return Err(Reject::AlreadyQueued);
        }
        if view
            .recent
            .iter()
            .rev()
            .take(RECENT_HISTORY)
            .any(|t| *t == track.id)
        {
            return Err(Reject::RecentlyPlayed);
        }
        let mine = self
            .pending
            .iter()
            .filter(|r| r.guest_id == guest_id)
            .count();
        if self.policy.mode == RequestMode::Approval {
            if mine >= self.policy.cap {
                return Err(Reject::Cap(self.policy.cap));
            }
            if self.pending.len() >= MAX_PENDING_TOTAL {
                return Err(Reject::Full);
            }
        }
        self.next_id += 1;
        let req = Request {
            id: self.next_id,
            guest_id,
            guest: guest.to_string(),
            track,
        };
        if self.policy.mode == RequestMode::Auto {
            return Ok(Submitted::Approved(req));
        }
        let id = req.id;
        self.pending.push(req);
        Ok(Submitted::Pending(id))
    }

    /// Removes and returns a request (approved or denied by the caller).
    pub fn take(&mut self, id: u64) -> Option<Request> {
        let i = self.pending.iter().position(|r| r.id == id)?;
        Some(self.pending.remove(i))
    }

    pub fn take_all(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.pending)
    }

    /// A guest left: their waiting requests go with them.
    pub fn forget_guest(&mut self, guest_id: u64) {
        self.pending.retain(|r| r.guest_id != guest_id);
        for g in self.unavailable.values_mut() {
            g.remove(&guest_id);
        }
    }

    pub fn mark_unavailable(&mut self, track_id: u64, guest_id: u64) {
        self.unavailable
            .entry(track_id)
            .or_default()
            .insert(guest_id);
    }

    /// How many guests said they cannot play `track_id`.
    pub fn unavailable_for(&self, track_id: u64) -> usize {
        self.unavailable.get(&track_id).map_or(0, HashSet::len)
    }
}

/// Where a request goes in the upcoming queue. `owners[i]` is the guest who asked for
/// `upcoming[i]` (None: the host's own track).
///
/// Fair queue: guest requests form a block at the front of the upcoming queue, ordered
/// round-robin: every guest's first request comes before anyone's second.
pub fn insert_index(placement: Placement, owners: &[Option<u64>], guest_id: u64) -> usize {
    match placement {
        Placement::PlayNext => 0,
        Placement::AddToEnd => owners.len(),
        Placement::Fair => {
            let block = owners.iter().take_while(|o| o.is_some()).count();
            let round = owners[..block]
                .iter()
                .filter(|o| **o == Some(guest_id))
                .count();
            let mut seen: HashMap<u64, usize> = HashMap::new();
            let mut at = 0;
            for (i, o) in owners[..block].iter().enumerate() {
                let g = o.unwrap_or_default();
                let r = seen.entry(g).or_default();
                if *r <= round {
                    at = i + 1;
                }
                *r += 1;
            }
            at
        }
    }
}

// ---- vote skip ----

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Vote {
    Passed,
    Waiting { have: usize, need: usize },
}

/// Votes to skip the current track.
#[derive(Debug, Default)]
pub struct VoteSkip {
    /// None: vote skip is off.
    pub percent: Option<u8>,
    voters: HashSet<u64>,
}

impl VoteSkip {
    pub fn new(percent: Option<u8>) -> Self {
        Self {
            percent,
            voters: HashSet::new(),
        }
    }

    pub fn need(percent: u8, guests: usize) -> usize {
        (guests * usize::from(percent.clamp(1, 100)))
            .div_ceil(100)
            .max(1)
    }

    pub fn vote(&mut self, guest_id: u64, guests: usize) -> Option<Vote> {
        let percent = self.percent?;
        self.voters.insert(guest_id);
        let need = Self::need(percent, guests);
        if self.voters.len() >= need {
            self.voters.clear();
            Some(Vote::Passed)
        } else {
            Some(Vote::Waiting {
                have: self.voters.len(),
                need,
            })
        }
    }

    /// New track (or skip): the votes start over.
    pub fn reset(&mut self) {
        self.voters.clear();
    }

    pub fn forget(&mut self, guest_id: u64) {
        self.voters.remove(&guest_id);
    }
}

// ---- host handoff ----

/// Who is offered the session when the host is gone: a co-host first, else the guest who
/// joined earliest. `guests` is in join order.
pub fn successor(guests: &[(u64, String, bool)]) -> Option<u64> {
    guests
        .iter()
        .find(|(_, _, cohost)| *cohost)
        .or_else(|| guests.first())
        .map(|(id, _, _)| *id)
}

/// A guest's view of a host that went quiet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handoff {
    /// Host connected.
    Live,
    /// Lost the host `elapsed` ago: keep trying to reconnect.
    Waiting { elapsed: Duration },
    /// Grace over and this guest is the successor: offer to take over.
    OfferToMe,
    /// Grace over and somebody else is the successor.
    OfferToOther,
    /// Grace over and there is nobody to hand to.
    Over,
}

pub fn handoff_state(
    lost_for: Option<Duration>,
    grace: Duration,
    me: u64,
    successor: Option<u64>,
) -> Handoff {
    match lost_for {
        None => Handoff::Live,
        Some(d) if d < grace => Handoff::Waiting { elapsed: d },
        Some(_) => match successor {
            Some(s) if s == me => Handoff::OfferToMe,
            Some(_) => Handoff::OfferToOther,
            None => Handoff::Over,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: u64) -> RequestTrack {
        RequestTrack {
            id,
            title: format!("t{id}"),
            artist: "a".into(),
            quality: "LOSSLESS".into(),
            explicit: false,
        }
    }

    fn view<'a>(upcoming: &'a [u64], recent: &'a [u64]) -> QueueView<'a> {
        QueueView { upcoming, recent }
    }

    #[test]
    fn fair_queue_is_round_robin_across_guests() {
        // Guest 1 asks three times, then guest 2 and guest 3 once each.
        let mut owners: Vec<Option<u64>> = Vec::new();
        for g in [1, 1, 1, 2, 3, 2] {
            let at = insert_index(Placement::Fair, &owners, g);
            owners.insert(at, Some(g));
        }
        assert_eq!(
            owners,
            vec![Some(1), Some(2), Some(3), Some(1), Some(2), Some(1)]
        );
    }

    #[test]
    fn fair_requests_go_ahead_of_the_hosts_own_tracks_but_not_other_requests() {
        let owners = [Some(1), None, None];
        assert_eq!(insert_index(Placement::Fair, &owners, 2), 1);
        assert_eq!(insert_index(Placement::Fair, &[None, None], 1), 0);
        assert_eq!(insert_index(Placement::Fair, &[], 1), 0);
    }

    #[test]
    fn play_next_and_add_to_end() {
        let owners = [Some(1), None, None];
        assert_eq!(insert_index(Placement::PlayNext, &owners, 2), 0);
        assert_eq!(insert_index(Placement::AddToEnd, &owners, 2), 3);
    }

    #[test]
    fn a_guest_cannot_exceed_the_pending_cap() {
        let mut b = RequestBook::new(Policy::default());
        for i in 1..=3 {
            assert!(matches!(
                b.submit(7, "Ann", track(i), view(&[], &[])),
                Ok(Submitted::Pending(_))
            ));
        }
        assert_eq!(
            b.submit(7, "Ann", track(4), view(&[], &[])),
            Err(Reject::Cap(3))
        );
        // Another guest is unaffected, and a decision frees a slot.
        assert!(b.submit(8, "Bo", track(5), view(&[], &[])).is_ok());
        let first = b.pending()[0].id;
        assert!(b.take(first).is_some());
        assert!(b.submit(7, "Ann", track(4), view(&[], &[])).is_ok());
    }

    #[test]
    fn the_cap_is_configurable() {
        let mut b = RequestBook::new(Policy {
            cap: 1,
            ..Policy::default()
        });
        assert!(b.submit(1, "A", track(1), view(&[], &[])).is_ok());
        assert_eq!(
            b.submit(1, "A", track(2), view(&[], &[])),
            Err(Reject::Cap(1))
        );
    }

    #[test]
    fn duplicates_in_the_queue_history_or_pending_are_rejected_with_a_message() {
        let mut b = RequestBook::new(Policy::default());
        assert_eq!(
            b.submit(1, "A", track(5), view(&[5], &[])),
            Err(Reject::AlreadyQueued)
        );
        assert_eq!(
            b.submit(1, "A", track(6), view(&[], &[6])),
            Err(Reject::RecentlyPlayed)
        );
        assert!(b.submit(1, "A", track(7), view(&[], &[])).is_ok());
        let e = b.submit(2, "B", track(7), view(&[], &[])).unwrap_err();
        assert_eq!(e, Reject::AlreadyPending);
        assert!(e.message().contains("waiting"));
        // Only the recent part of a long history counts.
        let old: Vec<u64> = std::iter::once(9)
            .chain(100..100 + RECENT_HISTORY as u64)
            .collect();
        assert!(b.submit(1, "A", track(9), view(&[], &old)).is_ok());
    }

    #[test]
    fn modes_and_the_explicit_filter() {
        let mut off = RequestBook::new(Policy {
            mode: RequestMode::Off,
            ..Policy::default()
        });
        assert_eq!(
            off.submit(1, "A", track(1), view(&[], &[])),
            Err(Reject::Off)
        );

        let mut auto = RequestBook::new(Policy {
            mode: RequestMode::Auto,
            ..Policy::default()
        });
        match auto.submit(1, "A", track(1), view(&[], &[])) {
            Ok(Submitted::Approved(r)) => assert_eq!(r.track.id, 1),
            other => panic!("{other:?}"),
        }
        assert!(auto.pending().is_empty());

        let mut clean = RequestBook::new(Policy {
            no_explicit: true,
            ..Policy::default()
        });
        let mut t = track(2);
        t.explicit = true;
        assert_eq!(
            clean.submit(1, "A", t, view(&[], &[])),
            Err(Reject::Explicit)
        );
    }

    #[test]
    fn leaving_drops_a_guests_requests_and_unavailability_counts() {
        let mut b = RequestBook::new(Policy::default());
        b.submit(1, "A", track(1), view(&[], &[])).unwrap();
        b.submit(2, "B", track(2), view(&[], &[])).unwrap();
        b.mark_unavailable(2, 1);
        b.mark_unavailable(2, 3);
        b.mark_unavailable(2, 3);
        assert_eq!(b.unavailable_for(2), 2);
        b.forget_guest(1);
        assert_eq!(b.pending().len(), 1);
        assert_eq!(b.unavailable_for(2), 1);
        assert_eq!(b.take_all().len(), 1);
    }

    #[test]
    fn vote_skip_needs_the_hosts_share_of_guests() {
        let mut v = VoteSkip::default();
        assert_eq!(v.vote(1, 4), None, "off by default");
        v.percent = Some(50);
        assert_eq!(v.vote(1, 4), Some(Vote::Waiting { have: 1, need: 2 }));
        assert_eq!(
            v.vote(1, 4),
            Some(Vote::Waiting { have: 1, need: 2 }),
            "one vote each"
        );
        assert_eq!(v.vote(2, 4), Some(Vote::Passed));
        assert_eq!(
            v.vote(3, 3),
            Some(Vote::Waiting { have: 1, need: 2 }),
            "votes reset"
        );
        v.forget(3);
        assert_eq!(VoteSkip::need(50, 1), 1);
        assert_eq!(VoteSkip::need(100, 5), 5);
        assert_eq!(VoteSkip::need(50, 3), 2);
    }

    #[test]
    fn the_successor_is_the_cohost_else_the_earliest_guest() {
        let g = vec![
            (1, "A".into(), false),
            (2, "B".into(), true),
            (3, "C".into(), false),
        ];
        assert_eq!(successor(&g), Some(2));
        assert_eq!(
            successor(&[(1, "A".into(), false), (4, "D".into(), false)]),
            Some(1)
        );
        assert_eq!(successor(&[]), None);
    }

    #[test]
    fn handoff_waits_out_the_grace_period_then_offers_the_session() {
        let s = Some(2);
        let g = HANDOFF_GRACE;
        assert_eq!(handoff_state(None, g, 2, s), Handoff::Live);
        assert!(matches!(
            handoff_state(Some(Duration::from_secs(59)), g, 2, s),
            Handoff::Waiting { .. }
        ));
        assert_eq!(handoff_state(Some(g), g, 2, s), Handoff::OfferToMe);
        assert_eq!(handoff_state(Some(g), g, 3, s), Handoff::OfferToOther);
        assert_eq!(handoff_state(Some(g), g, 3, None), Handoff::Over);
    }
}
