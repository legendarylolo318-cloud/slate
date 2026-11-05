//! The party set: the tracks that played while this instance hosted a party, kept in memory
//! only (same lifetime as the party plus the time until the next one starts) so the host can
//! save them as a TIDAL playlist. A guest never records anything.

use std::sync::Mutex;

use slate_core::commands::library as lib_cmds;
use slate_core::queue::Track;
use slint::ComponentHandle;

use crate::backend::Backend;
use crate::bootstrap::SettingsState;

/// Most tracks kept; a longer party stops recording.
pub const MAX_SET: usize = 500;
/// A track counts as played once this much of it was heard (or half of a shorter one), so
/// tracks skipped through on the way to another are not in the set.
pub const HEARD_SECS: f32 = 30.0;

/// Whether `secs` of a track `duration` long counts as having played it.
pub fn heard(secs: f32, duration: f32) -> bool {
    secs >= HEARD_SECS || (duration > 0.0 && secs >= duration / 2.0)
}

#[derive(Default)]
pub struct PartySet {
    ids: Vec<u64>,
    /// The track playing now (id, duration), decided when it ends.
    playing: Option<(u64, f32)>,
}

impl PartySet {
    pub const fn new() -> Self {
        Self {
            ids: Vec::new(),
            playing: None,
        }
    }

    /// The playing track changes to `next` (`None`: nothing that belongs in the set) after
    /// `secs` of the previous one were heard. Returns whether the set grew.
    pub fn moved_on(&mut self, next: Option<(u64, f32)>, secs: f32) -> bool {
        let grew = match self.playing.take() {
            Some((id, dur)) if heard(secs, dur) => self.record(id),
            _ => false,
        };
        self.playing = next;
        grew
    }

    /// Notes a track that started playing. A repeat of a track already in the set, and
    /// anything past the cap, is ignored. Returns whether the set grew.
    pub fn record(&mut self, id: u64) -> bool {
        if self.ids.len() >= MAX_SET || self.ids.contains(&id) {
            return false;
        }
        self.ids.push(id);
        true
    }

    pub fn ids(&self) -> &[u64] {
        &self.ids
    }

    pub fn clear(&mut self) {
        self.ids = Vec::new();
        self.playing = None;
    }
}

static SET: Mutex<PartySet> = Mutex::new(PartySet::new());

fn set() -> std::sync::MutexGuard<'static, PartySet> {
    SET.lock().unwrap_or_else(|e| e.into_inner())
}

/// "Party – 2023-11-14".
pub fn default_name(secs: u64) -> String {
    format!("Party – {}", slate_core::queue_snapshots::ymd(secs))
}

/// Sign-out: nothing of the last party is kept.
pub fn reset() {
    set().clear();
}

impl Backend {
    /// Whether `track` can be in the set: no videos, nothing the content filter hides.
    fn party_set_wants(&self, track: &Track) -> Option<(u64, f32)> {
        if track.is_video() {
            return None;
        }
        if let Some(state) = self.state() {
            let (explicit, ai) = slate_core::commands::utility::get_content_filter(&state);
            if slate_core::controller::ContentPrefs::hiding(explicit, ai).blocks(track) {
                return None;
            }
        }
        Some((track.id, track.duration_secs()))
    }

    /// Host only: `next` started (`None`: the party ends) after `heard` seconds of the track
    /// before it. The previous track joins the set if it was heard long enough.
    pub(crate) fn party_set_moved_on(&self, next: Option<&Track>, heard: f32) {
        let next = next.and_then(|t| self.party_set_wants(t));
        if set().moved_on(next, heard) {
            self.party_set_count_shown();
        }
    }

    /// A party starts with `current` already playing: it is the first candidate.
    pub(crate) fn party_set_begin(&self, current: Option<&Track>) {
        let next = current.and_then(|t| self.party_set_wants(t));
        let mut s = set();
        s.clear();
        s.playing = next;
    }

    pub(crate) fn party_set_count_shown(&self) {
        let n = set().ids().len() as i32;
        self.ui(move |w| w.global::<SettingsState>().set_party_set_count(n));
    }

    /// Opens the name dialog for the set (nothing happens when the set is empty).
    pub fn party_offer_save_set(&self) {
        let n = set().ids().len();
        if n == 0 {
            self.notify("No tracks were played in the party");
            return;
        }
        let name = default_name(slate_core::now_secs());
        self.ui(move |w| {
            w.set_dialog_text(name.into());
            w.set_dialog_target("".into());
            w.set_dialog_kind("party-set".into());
            w.set_dialog_open(true);
        });
    }

    /// Creates the playlist from the set.
    pub fn party_save_set(&self, name: &str) {
        let ids: Vec<u64> = set().ids().to_vec();
        if ids.is_empty() {
            self.notify("No tracks were played in the party");
            return;
        }
        let title = if name.trim().is_empty() {
            default_name(slate_core::now_secs())
        } else {
            name.trim().to_string()
        };
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            let Some(job) = this.begin_bulk() else { return };
            this.ui(|w| w.set_notice("Saving the party set…".into()));
            let msg = match lib_cmds::create_playlist_with_tracks_until(
                &state,
                title.clone(),
                "UNLISTED",
                &ids,
                &|| job.cancelled(),
            )
            .await
            {
                Ok((p, n)) => {
                    this.note_created(&p.uuid, &p.title);
                    slate_core::playlist_tools::created_message(
                        &title,
                        n,
                        slate_core::playlist_tools::unique_ids(&ids).len(),
                        job.cancelled(),
                    )
                }
                Err(e) => format!("Could not save the playlist: {}", e.log_safe()),
            };
            drop(job);
            this.load_library();
            this.notify(&msg);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_in_order_without_repeats() {
        let mut s = PartySet::new();
        assert!(s.record(3));
        assert!(s.record(1));
        assert!(!s.record(3));
        assert_eq!(s.ids(), &[3, 1]);
    }

    #[test]
    fn skipped_tracks_are_not_in_the_set() {
        let mut s = PartySet::new();
        s.moved_on(Some((1, 200.0)), 0.0);
        assert!(!s.moved_on(Some((2, 200.0)), 3.0), "1 skipped after 3 s");
        assert!(s.moved_on(Some((3, 40.0)), 31.0), "2 heard for 31 s");
        assert!(s.moved_on(None, 20.0), "3: half of a 40 s track");
        assert_eq!(s.ids(), &[2, 3]);
        // A seek does not change the track, so it decides nothing; a filtered or video
        // track (`None`) is never recorded however long it plays.
        s.moved_on(None, 300.0);
        assert_eq!(s.ids(), &[2, 3]);
    }

    #[test]
    fn stops_at_the_cap() {
        let mut s = PartySet::new();
        for id in 0..(MAX_SET as u64 + 50) {
            s.record(id);
        }
        assert_eq!(s.ids().len(), MAX_SET);
        assert!(!s.record(10_000));
    }

    #[test]
    fn clear_empties_and_allows_recording_again() {
        let mut s = PartySet::new();
        s.record(7);
        s.clear();
        assert!(s.ids().is_empty());
        assert!(s.record(7));
    }

    #[test]
    fn default_name_has_the_date() {
        assert_eq!(default_name(1_700_000_000), "Party – 2023-11-14");
    }
}
