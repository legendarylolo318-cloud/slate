//! Multi-select in track lists: Ctrl-click toggles, Shift-click selects a range, Ctrl+A
//! selects all, Esc clears. The selection lives in the list model (`models::LazyTracks`);
//! this turns the clicks into it and the menu's bulk actions into queue, playlist and
//! party calls.

use slate_core::commands::library as lib_cmds;
use slate_core::playlist_tools;
use slate_core::queue::Track;
use slate_core::selection::Modifier;

use std::sync::Mutex;

use slate_core::selection::Selection;
use slint::{Model, VecModel};

use crate::backend::Backend;
use crate::library::SourceKind;
use crate::models::with_tracks;

/// The queue panel's selection (rows as shown: "added by hand" first, then the context).
static QUEUE_SEL: Mutex<Selection> = Mutex::new(Selection::new());

/// Applies `f` to the queue selection and shows the result on the rows. UI thread.
pub(crate) fn refresh_queue_selection(
    w: &crate::bootstrap::AppWindow,
    f: impl FnOnce(&mut Selection, usize),
) {
    let rows = w.get_queue_rows();
    let len = rows.row_count();
    let Ok(mut sel) = QUEUE_SEL.lock() else {
        return;
    };
    f(&mut sel, len);
    if let Some(list) = rows
        .as_any()
        .downcast_ref::<VecModel<crate::bootstrap::QueueRow>>()
    {
        for i in 0..len {
            if let Some(mut row) = list.row_data(i) {
                let on = sel.contains(i);
                if row.selected != on {
                    row.selected = on;
                    list.set_row_data(i, row);
                }
            }
        }
    }
    w.set_queue_selection(i32::try_from(sel.len()).unwrap_or(i32::MAX));
}

/// Most tracks sent to the party in one go: the host takes three requests a minute from a
/// guest, so more would only be turned down.
const SUGGEST_AT_ONCE: usize = 3;

impl Backend {
    /// A Ctrl / Shift click on shown row `row`.
    pub fn select_row(&self, row: i32, ctrl: bool, shift: bool) {
        let Ok(row) = usize::try_from(row) else {
            return;
        };
        self.ui(move |w| {
            with_tracks(&w, |m| {
                m.select_click(row, Modifier::from_keys(ctrl, shift))
            });
        });
    }

    pub fn select_all(&self) {
        self.ui(|w| {
            with_tracks(&w, |m| m.select_all());
        });
    }

    pub fn select_clear(&self) {
        self.ui(|w| {
            with_tracks(&w, |m| m.select_clear());
        });
    }

    /// Ctrl / Shift click on a queue row.
    pub fn queue_select(&self, row: i32, ctrl: bool, shift: bool) {
        let Ok(row) = usize::try_from(row) else {
            return;
        };
        self.ui(move |w| {
            crate::selection_ui::refresh_queue_selection(&w, |s, len| {
                s.click(row, Modifier::from_keys(ctrl, shift), len);
            });
        });
    }

    /// The selected upcoming tracks, with their position in the queue.
    pub(crate) async fn queue_selected_tracks(&self) -> Vec<(usize, Track)> {
        let Some(ctl) = self.controller() else {
            return Vec::new();
        };
        let picked = QUEUE_SEL.lock().map(|s| s.indices()).unwrap_or_default();
        ctl.with_queue(|q| {
            let upcoming: Vec<&Track> = q.upcoming().map(|t| &**t).collect();
            picked
                .into_iter()
                .filter_map(|i| upcoming.get(i).map(|t| (i, (*t).clone())))
                .collect()
        })
        .await
    }

    /// A bulk action from the queue panel's menu on the selection.
    pub fn queue_selection_action(&self, action: &str) {
        if action == "sel-clear" {
            self.ui(|w| crate::selection_ui::refresh_queue_selection(&w, |s, _| s.clear()));
            return;
        }
        let (this, action) = (self.clone(), action.to_string());
        self.rt.spawn(async move {
            let picked = this.queue_selected_tracks().await;
            let Some(ctl) = this.controller() else { return };
            let n = picked.len();
            if n == 0 {
                return;
            }
            match action.as_str() {
                "sel-remove" | "sel-next" => {
                    // Last position first, so the earlier ones keep their place.
                    for (i, _) in picked.iter().rev() {
                        ctl.remove_from_queue(*i).await;
                    }
                    if action == "sel-next" {
                        for (_, t) in picked.into_iter().rev() {
                            ctl.play_next_in_queue(t, None).await;
                        }
                    }
                    this.ui(|w| crate::selection_ui::refresh_queue_selection(&w, |s, _| s.clear()));
                    this.refresh_queue_info().await;
                    this.save().await;
                    this.notify(&if action == "sel-remove" {
                        format!("Removed {n} tracks from the queue")
                    } else {
                        format!("{n} tracks will play next")
                    });
                }
                "sel-tags" => {
                    let tracks: Vec<Track> = picked.into_iter().map(|(_, t)| t).collect();
                    this.tags_open_dialog(&tracks);
                }
                "sel-suggest" => {
                    for (_, t) in picked.iter().take(SUGGEST_AT_ONCE) {
                        this.party_suggest(t.id);
                    }
                    if n > SUGGEST_AT_ONCE {
                        this.notify(&format!(
                            "Sent {SUGGEST_AT_ONCE} of {n}: the host takes {SUGGEST_AT_ONCE} requests a minute"
                        ));
                    }
                }
                _ => {}
            }
        });
    }

    /// The selected rows as source indices, ascending.
    async fn selected_sources(&self) -> Vec<usize> {
        self.read_ui(|w| with_tracks(&w, |m| m.selected_sources()))
            .await
            .unwrap_or_default()
    }

    /// The selected tracks with their source index, loading the pages that are not in yet.
    pub(crate) async fn selected_tracks(&self) -> Vec<(usize, Track)> {
        let sources = self.selected_sources().await;
        let missing = {
            let cache = self
                .lib
                .tracks
                .lock()
                .map(|c| c.clone())
                .unwrap_or_default();
            sources.iter().any(|i| !cache.contains_key(i))
        };
        if missing && self.ensure_all().await.is_none() {
            return Vec::new();
        }
        let cache = self
            .lib
            .tracks
            .lock()
            .map(|c| c.clone())
            .unwrap_or_default();
        sources
            .into_iter()
            .filter_map(|i| cache.get(&i).cloned().map(|t| (i, t)))
            .collect()
    }

    /// A bulk action from the multi-selection menu.
    pub(crate) fn selection_action(&self, action: &str) {
        if action == "sel-clear" {
            self.select_clear();
            return;
        }
        let (this, action) = (self.clone(), action.to_string());
        self.rt.spawn(async move {
            let picked = this.selected_tracks().await;
            if picked.is_empty() {
                return;
            }
            let n = picked.len();
            match action.as_str() {
                "sel-queue" | "sel-next" => {
                    let Some(ctl) = this.controller() else { return };
                    let tracks: Vec<Track> = picked.into_iter().map(|(_, t)| t).collect();
                    if action == "sel-queue" {
                        for t in tracks {
                            ctl.add_to_queue(t, None).await;
                        }
                    } else {
                        // Last first, so the first selected track plays first.
                        for t in tracks.into_iter().rev() {
                            ctl.play_next_in_queue(t, None).await;
                        }
                    }
                    this.refresh_queue_info().await;
                    this.save().await;
                    this.notify(&if action == "sel-queue" {
                        format!("Added {n} tracks to the queue")
                    } else {
                        format!("{n} tracks will play next")
                    });
                    this.select_clear();
                }
                "sel-suggest" => {
                    for (_, t) in picked.iter().take(SUGGEST_AT_ONCE) {
                        this.party_suggest(t.id);
                    }
                    if n > SUGGEST_AT_ONCE {
                        this.notify(&format!(
                            "Sent {SUGGEST_AT_ONCE} of {n}: the host takes {SUGGEST_AT_ONCE} requests a minute"
                        ));
                    }
                }
                "sel-remove" => {
                    let rows: Vec<usize> = picked.into_iter().map(|(i, _)| i).collect();
                    this.remove_rows_from_playlist(rows).await;
                }
                _ => {}
            }
        });
    }

    /// Removes the rows (source indices) from the open playlist, last first so the
    /// positions of the rest stay valid.
    async fn remove_rows_from_playlist(&self, mut rows: Vec<usize>) {
        let Some(SourceKind::Playlist(uuid)) = self.lib.active_kind_pub() else {
            self.notify("Open one of your playlists to remove tracks from it.");
            return;
        };
        let (Some(state), Some(job)) = (self.state(), self.begin_bulk()) else {
            return;
        };
        rows.sort_unstable_by(|a, b| b.cmp(a));
        let total = rows.len();
        let mut removed = 0usize;
        for (n, index) in rows.into_iter().enumerate() {
            if !job.step(n).await {
                break;
            }
            self.ui({
                let text = format!("Removing tracks… {n} of {total}");
                move |w| w.set_notice(text.into())
            });
            let idx = u32::try_from(index).unwrap_or(u32::MAX);
            match lib_cmds::remove_track_from_playlist(&state, uuid.clone(), idx).await {
                Ok(()) => removed += 1,
                Err(e) => {
                    let msg = format!(
                        "Removed {removed} of {total} tracks, then failed: {}",
                        e.log_safe()
                    );
                    self.ui(move |w| {
                        w.set_notice("".into());
                        w.set_error(msg.into());
                    });
                    break;
                }
            }
        }
        drop(job);
        self.ui(|w| w.set_notice("".into()));
        self.notify(&format!("Removed {removed} of {total} tracks"));
        self.navigate_refresh(crate::pages::Route::Playlist(uuid));
    }

    /// "Add N tracks to playlist": tracks already in it are left out and counted.
    pub(crate) fn add_selection_to_playlist(&self, uuid: String, queue: bool) {
        let this = self.clone();
        self.rt.spawn(async move {
            let picked = if queue {
                this.queue_selected_tracks().await
            } else {
                this.selected_tracks().await
            };
            let Some(state) = this.state() else { return };
            if picked.is_empty() {
                return;
            }
            let existing =
                match lib_cmds::get_playlist_tracks(&state, this.app.clone(), uuid.clone()).await {
                    Ok(t) => playlist_tools::keys_of(&t),
                    Err(e) => {
                        let msg = format!("Could not read the playlist: {}", e.log_safe());
                        this.ui(move |w| w.set_error(msg.into()));
                        return;
                    }
                };
            let (fresh, skipped) =
                split_new(picked.into_iter().map(|(_, t)| t).collect(), &existing);
            let ids: Vec<u64> = fresh.iter().map(|t| t.id).collect();
            let Some(job) = this.begin_bulk() else { return };
            let added =
                lib_cmds::add_tracks_paced_until(&state, &uuid, &ids, &|| job.cancelled()).await;
            drop(job);
            let msg = match added {
                Ok(n) => added_message(n, skipped),
                Err(e) => format!("Could not add the tracks: {}", e.log_safe()),
            };
            this.notify(&msg);
            this.load_library();
            this.select_clear();
        });
    }
}

/// Splits `tracks` into those not in the playlist (`existing`) and how many were.
pub(crate) fn split_new(
    tracks: Vec<Track>,
    existing: &std::collections::HashSet<playlist_tools::TrackKey>,
) -> (Vec<Track>, usize) {
    let total = tracks.len();
    let fresh: Vec<Track> = tracks
        .into_iter()
        .filter(|t| !crate::dnd::already_in(t, existing))
        .collect();
    let skipped = total - fresh.len();
    (fresh, skipped)
}

pub(crate) fn added_message(added: usize, skipped: usize) -> String {
    match (added, skipped) {
        (0, 0) => "Nothing to add".to_string(),
        (0, s) => format!("Nothing added: {s} already in the playlist"),
        (n, 0) => format!("Added {n} tracks to the playlist"),
        (n, s) => format!("Added {n} tracks; {s} already in the playlist were left out"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use playlist_tools::TrackKey;

    fn t(id: u64) -> Track {
        Track::new(id)
    }

    #[test]
    fn tracks_already_in_the_playlist_are_left_out_and_counted() {
        let existing: std::collections::HashSet<TrackKey> =
            [TrackKey::Id(2), TrackKey::Id(4)].into_iter().collect();
        let (fresh, skipped) = split_new(vec![t(1), t(2), t(3), t(4)], &existing);
        assert_eq!(fresh.iter().map(|x| x.id).collect::<Vec<_>>(), vec![1, 3]);
        assert_eq!(skipped, 2);
    }

    #[test]
    fn the_message_says_what_was_skipped() {
        assert_eq!(added_message(3, 0), "Added 3 tracks to the playlist");
        assert!(added_message(2, 1).contains("1 already in the playlist"));
        assert!(added_message(0, 2).starts_with("Nothing added"));
        assert_eq!(added_message(0, 0), "Nothing to add");
    }
}
