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

#[derive(Default)]
pub struct PartySet {
    ids: Vec<u64>,
}

impl PartySet {
    pub const fn new() -> Self {
        Self { ids: Vec::new() }
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

/// A new party starts with an empty set.
pub fn reset() {
    set().clear();
}

impl Backend {
    /// Host only: `track` just started. Videos and tracks the content filter hides are not
    /// part of the set.
    pub(crate) fn party_set_record(&self, track: &Track) {
        if track.is_video() {
            return;
        }
        if let Some(state) = self.state() {
            let (explicit, ai) = slate_core::commands::utility::get_content_filter(&state);
            if slate_core::controller::ContentPrefs::hiding(explicit, ai).blocks(track) {
                return;
            }
        }
        if set().record(track.id) {
            self.party_set_count_shown();
        }
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
