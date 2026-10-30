//! "Suggested tracks" under a playlist you own. Nothing is requested when the page opens: the
//! section loads when the list is scrolled to its end (or its button is pressed), and all of
//! it is dropped when the page closes.

use std::sync::Mutex;

use slate_core::commands::library as lib_cmds;
use slate_core::queue::Track;
use slate_core::tidal_api::TidalTrack;
use slint::{Image, ModelRc, VecModel};

use crate::backend::Backend;
use crate::bootstrap::TrackRow;
use crate::library::{row_of, to_track};

/// Settings choices for "Suggested tracks under playlists".
pub const COUNTS: [usize; 4] = [0, 5, 10, 20];
pub const DEFAULT_INDEX: usize = 2;

pub fn count_for(index: usize) -> usize {
    COUNTS.get(index).copied().unwrap_or(COUNTS[DEFAULT_INDEX])
}

#[derive(Default)]
struct State {
    playlist: String,
    shown: Vec<TidalTrack>,
    next_offset: u32,
    exhausted: bool,
    loading: bool,
}

/// Only exists while an eligible playlist page is open.
static STATE: Mutex<Option<State>> = Mutex::new(None);

fn rows_of(shown: &[TidalTrack]) -> Vec<slate_core::queue::Track> {
    shown.iter().filter_map(to_track).collect()
}

impl Backend {
    /// A page opened or closed: drop everything from the previous one.
    pub(crate) fn suggest_reset(&self) {
        if let Ok(mut s) = STATE.lock() {
            *s = None;
        }
        self.ui(|w| {
            w.set_suggest_visible(false);
            w.set_suggest_status("".into());
            w.set_suggestions(ModelRc::new(VecModel::from(Vec::<TrackRow>::new())));
        });
    }

    /// The playlist page is open; offer the section if it is the user's own and not switched off.
    pub(crate) fn suggest_page_opened(&self, uuid: &str, owned: bool) {
        let index = crate::uiprefs::UiPrefs::load().suggest_index;
        let open = crate::library::SourceKind::Playlist(uuid.to_string());
        if self.lib.active_kind_pub().as_ref() != Some(&open) {
            return;
        }
        if !owned || count_for(index) == 0 {
            return;
        }
        if let Ok(mut s) = STATE.lock() {
            *s = Some(State {
                playlist: uuid.to_string(),
                ..State::default()
            });
        }
        self.ui(|w| {
            w.set_suggest_status("Scroll down or press the button for suggested tracks.".into());
            w.set_suggest_visible(true);
        });
    }

    fn suggest_count(&self) -> usize {
        count_for(crate::uiprefs::UiPrefs::load().suggest_index)
    }

    /// `replace`: a new batch instead of more. `top_up`: only enough to fill the section again.
    pub fn suggest_load(&self, mode: &str) {
        let (playlist, offset, want, existing) = {
            let Ok(mut guard) = STATE.lock() else { return };
            let Some(st) = guard.as_mut() else { return };
            if st.loading || (st.exhausted && mode == "more") {
                return;
            }
            let count = self.suggest_count();
            let (offset, want) = match mode {
                "refresh" => (if st.exhausted { 0 } else { st.next_offset }, count),
                "more" => (st.next_offset, count),
                "top-up" => (st.next_offset, count.saturating_sub(st.shown.len())),
                // First load: only when nothing has been fetched yet.
                _ if st.next_offset == 0 && st.shown.is_empty() => (0, count),
                _ => return,
            };
            if want == 0 {
                return;
            }
            st.loading = true;
            (st.playlist.clone(), offset, want, st.shown.clone())
        };
        let (mode, this) = (mode.to_string(), self.clone());
        self.ui(|w| w.set_suggest_status("Loading suggestions…".into()));
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            let result =
                lib_cmds::playlist_suggestions(&state, this.app.clone(), &playlist, offset, want)
                    .await;
            let status = {
                let Ok(mut guard) = STATE.lock() else { return };
                let Some(st) = guard.as_mut().filter(|s| s.playlist == playlist) else {
                    return; // the page closed meanwhile
                };
                st.loading = false;
                match result {
                    Ok(batch) => {
                        st.next_offset = batch.next_offset;
                        st.exhausted = batch.exhausted;
                        let keep = if mode == "refresh" {
                            Vec::new()
                        } else {
                            existing
                        };
                        let mut all = keep;
                        all.extend(batch.tracks);
                        st.shown = all;
                        if st.shown.is_empty() {
                            "No suggestions right now.".to_string()
                        } else {
                            String::new()
                        }
                    }
                    Err(e) => format!("Could not load suggestions: {}", e.log_safe()),
                }
            };
            this.show_suggestions(status);
        });
    }

    fn show_suggestions(&self, status: String) {
        let tracks = STATE
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|s| rows_of(&s.shown)))
            .unwrap_or_default();
        let rows: Vec<crate::models::RowData> = tracks
            .iter()
            .map(|t| {
                let mut r = row_of(t);
                r.favorite = self.is_favorite_track(t.id);
                r
            })
            .collect();
        self.ui(move |w| {
            let model: Vec<TrackRow> = rows
                .into_iter()
                .map(|r| TrackRow {
                    id: r.id.into(),
                    title: r.title.into(),
                    artist: r.artist.into(),
                    album: r.album.into(),
                    duration: r.duration.into(),
                    explicit: r.explicit,
                    loaded: true,
                    favorite: r.favorite,
                    quality: r.quality,
                    atmos: r.atmos,
                    cover: Image::default(),
                })
                .collect();
            w.set_suggestions(ModelRc::new(VecModel::from(model)));
            w.set_suggest_status(status.into());
        });
    }

    pub(crate) fn suggested_track(&self, index: usize) -> Option<Track> {
        let g = STATE.lock().ok()?;
        to_track(g.as_ref()?.shown.get(index)?)
    }

    /// "+" on a suggestion: add it to the playlist, drop the row, top the section up.
    pub fn suggest_add(&self, index: usize) {
        let Some(track) = self.suggested_track(index) else {
            return;
        };
        let Some(playlist) = STATE
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|s| s.playlist.clone()))
        else {
            return;
        };
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            match lib_cmds::add_track_to_playlist(&state, playlist.clone(), track.id).await {
                Ok(()) => {
                    if let Ok(mut g) = STATE.lock() {
                        if let Some(st) = g.as_mut().filter(|s| s.playlist == playlist) {
                            if index < st.shown.len() {
                                st.shown.remove(index);
                            }
                        }
                    }
                    this.show_suggestions(String::new());
                    this.append_row_to_page(&playlist, track);
                    this.suggest_load("top-up");
                }
                Err(e) => {
                    let msg = format!("Could not add the track: {}", e.log_safe());
                    this.ui(move |w| w.set_error(msg.into()));
                }
            }
        });
    }

    /// Shows a just-added track at the end of the open playlist (no reload, no scroll jump).
    fn append_row_to_page(&self, playlist: &str, track: Track) {
        use crate::library::SourceKind;
        use std::sync::atomic::Ordering;
        if self.lib.active_kind_pub() != Some(SourceKind::Playlist(playlist.to_string())) {
            return;
        }
        if self.lib.view.lock().is_ok_and(|v| v.is_some()) {
            return; // a filter is showing; the list refreshes when it is cleared
        }
        let index = self.lib.total.fetch_add(1, Ordering::Relaxed);
        let mut row = row_of(&track);
        row.favorite = self.is_favorite_track(track.id);
        if let Ok(mut cache) = self.lib.tracks.lock() {
            cache.insert(index, track);
        }
        self.ui(move |w| {
            crate::models::with_tracks(&w, |m| {
                m.set_total(index + 1);
                m.fill(index, vec![row]);
            });
        });
    }

    pub fn suggest_action(&self, index: usize, action: &str) {
        if let Some(t) = self.suggested_track(index) {
            self.track_action(t, None, action);
        }
    }

    pub fn suggest_play(&self, index: usize) {
        let Some(track) = self.suggested_track(index) else {
            return;
        };
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(ctl) = this.controller() else { return };
            let outcome = ctl
                .play_from_source(track.clone(), vec![track], None, false)
                .await;
            this.apply(outcome).await;
        });
    }

    /// Settings → General.
    pub(crate) fn set_suggest_index(&self, index: usize) -> Result<(), String> {
        let mut prefs = crate::uiprefs::UiPrefs::load();
        prefs.suggest_index = index.min(COUNTS.len() - 1);
        prefs.save().map_err(|e| e.to_string())?;
        // Off hides the section at once; turning it on shows after the page is reopened.
        if count_for(prefs.suggest_index) == 0 {
            self.suggest_reset();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_map_from_the_setting() {
        assert_eq!(count_for(0), 0);
        assert_eq!(count_for(1), 5);
        assert_eq!(count_for(2), 10);
        assert_eq!(count_for(3), 20);
        assert_eq!(count_for(99), 10);
        assert_eq!(count_for(DEFAULT_INDEX), 10);
    }

    #[test]
    fn nothing_exists_until_a_page_opens() {
        // No state, no rows: the section's data is created only for an eligible page.
        let g = STATE.lock().expect("lock");
        assert!(g.is_none() || g.as_ref().is_some_and(|s| s.shown.is_empty()));
    }
}
