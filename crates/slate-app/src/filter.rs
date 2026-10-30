//! Ctrl+F on a track page: text filter and quality chips over the loaded rows. Not-yet
//! loaded pages are fetched first (the same pager playback uses). The result is a list of
//! source indices handed to `LazyTracks::set_view`.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use slate_core::list_filter::{fold, text_matches, QualityFilter};
use slate_core::queue::Track;

use crate::backend::Backend;
use crate::library::{row_of, SourceKind};
use crate::models::with_tracks;

/// Latest request wins when the user keeps typing while pages load.
static LATEST: AtomicU64 = AtomicU64::new(0);

fn artists_text(t: &Track) -> String {
    let names: Vec<&str> = t
        .extra
        .get("artists")
        .and_then(|a| a.as_array())
        .into_iter()
        .flatten()
        .filter_map(|a| a.get("name")?.as_str())
        .collect();
    if names.is_empty() {
        t.artist_name().to_string()
    } else {
        names.join(" ")
    }
}

/// Source indices of the tracks that pass the text and quality filters, in order.
pub fn view_for(cache: &BTreeMap<usize, Track>, query: &str, quality: QualityFilter) -> Vec<usize> {
    let q = fold(query);
    cache
        .iter()
        .filter(|(_, t)| {
            let b = crate::quality::track_badges(t);
            if !quality.accepts(b.tier, b.atmos) {
                return false;
            }
            let album = t
                .extra
                .get("album")
                .and_then(|a| a.get("title"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            text_matches(&q, t.title(), &artists_text(t), album)
        })
        .map(|(i, _)| *i)
        .collect()
}

/// Key for remembering the chip per page type.
fn page_key(kind: &SourceKind) -> &'static str {
    match kind {
        SourceKind::Loved => "loved",
        SourceKind::Playlist(_) => "playlist",
        SourceKind::Album(_) => "album",
        SourceKind::Mix(_) => "mix",
        SourceKind::Smart(_) => "smart",
    }
}

impl Backend {
    /// The quality chip remembered for the page type that is opening.
    pub(crate) fn saved_quality_filter(&self, kind: &SourceKind) -> i32 {
        crate::uiprefs::UiPrefs::load()
            .quality_filters
            .get(page_key(kind))
            .copied()
            .unwrap_or(0)
    }

    /// The filter box or a chip changed. `quality`: 0 All, 1 Lossless, 2 Hi-Res, 3 Atmos.
    pub fn page_filter_changed(&self, text: &str, quality: i32) {
        let Some(kind) = self.lib.active_kind_pub() else {
            return;
        };
        let mut prefs = crate::uiprefs::UiPrefs::load();
        let key = page_key(&kind).to_string();
        if prefs.quality_filters.get(&key).copied().unwrap_or(0) != quality {
            if quality == 0 {
                prefs.quality_filters.remove(&key);
            } else {
                prefs.quality_filters.insert(key, quality);
            }
            let _ = prefs.save();
        }
        let ticket = LATEST.fetch_add(1, Ordering::SeqCst) + 1;
        let generation = self.lib.generation_now();
        let (text, qf) = (text.to_string(), QualityFilter::from_index(quality));
        let this = self.clone();
        self.rt.spawn(async move {
            if text.trim().is_empty() && qf == QualityFilter::All {
                if let Ok(mut v) = this.lib.view.lock() {
                    *v = None;
                }
                this.ui(|w| {
                    with_tracks(&w, |m| m.set_view(None));
                    w.set_filter_status("".into());
                });
                return;
            }
            let total = this.lib.total_now();
            let loaded = this.lib.tracks.lock().map_or(0, |c| c.len());
            if loaded < total {
                let text = format!("Loading {} more tracks to search…", total - loaded);
                this.ui(move |w| w.set_filter_status(text.into()));
                if this.ensure_all().await.is_none() {
                    this.ui(|w| w.set_filter_status("".into()));
                    return;
                }
                this.fill_all_rows(generation);
            }
            if this.lib.generation_now() != generation || LATEST.load(Ordering::SeqCst) != ticket {
                return;
            }
            let view = this
                .lib
                .tracks
                .lock()
                .map(|c| view_for(&c, &text, qf))
                .unwrap_or_default();
            let status = format!("{} of {total} tracks", view.len());
            if let Ok(mut v) = this.lib.view.lock() {
                *v = Some(view.clone());
            }
            this.ui(move |w| {
                with_tracks(&w, |m| m.set_view(Some(view)));
                w.set_filter_status(status.into());
            });
        });
    }

    /// Gives the model the rows of pages that were fetched for filtering.
    fn fill_all_rows(&self, generation: u64) {
        let Ok(cache) = self.lib.tracks.lock() else {
            return;
        };
        let rows: Vec<(usize, _)> = cache
            .iter()
            .map(|(i, t)| {
                let mut r = row_of(t);
                r.favorite = self.is_favorite_track(t.id);
                (*i, r)
            })
            .collect();
        drop(cache);
        // One `fill` per contiguous run.
        let mut runs: Vec<(usize, Vec<_>)> = Vec::new();
        for (i, r) in rows {
            match runs.last_mut() {
                Some((start, v)) if *start + v.len() == i => v.push(r),
                _ => runs.push((i, vec![r])),
            }
        }
        let lib = std::sync::Arc::clone(&self.lib);
        self.ui(move |w| {
            if lib.generation_now() != generation {
                return;
            }
            with_tracks(&w, |m| {
                for (start, v) in runs {
                    m.fill(start, v);
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn track(id: u64, title: &str, artist: &str, album: &str, quality: &str) -> (usize, Track) {
        let mut t = Track::new(id);
        if let serde_json::Value::Object(m) = json!({
            "title": title,
            "artist": {"name": artist},
            "artists": [{"name": artist}],
            "album": {"title": album},
            "audioQuality": quality,
        }) {
            t.extra = m;
        }
        (id as usize, t)
    }

    fn cache() -> BTreeMap<usize, Track> {
        [
            track(1, "Jóga", "Björk", "Homogenic", "LOSSLESS"),
            track(2, "Hunter", "Björk", "Homogenic", "HIGH"),
            track(
                3,
                "Halo",
                "Beyoncé",
                "I Am... Sasha Fierce",
                "HI_RES_LOSSLESS",
            ),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn text_matches_title_artist_and_album_without_accents() {
        let c = cache();
        assert_eq!(view_for(&c, "joga", QualityFilter::All), vec![1]);
        assert_eq!(view_for(&c, "BJORK", QualityFilter::All), vec![1, 2]);
        assert_eq!(view_for(&c, "sasha", QualityFilter::All), vec![3]);
        assert_eq!(view_for(&c, "beyonce halo", QualityFilter::All), vec![3]);
        assert!(view_for(&c, "zzz", QualityFilter::All).is_empty());
    }

    #[test]
    fn chips_combine_with_text() {
        let c = cache();
        assert_eq!(view_for(&c, "", QualityFilter::Lossless), vec![1, 3]);
        assert_eq!(view_for(&c, "", QualityFilter::HiRes), vec![3]);
        assert_eq!(view_for(&c, "bjork", QualityFilter::Lossless), vec![1]);
        assert!(view_for(&c, "", QualityFilter::Atmos).is_empty());
    }

    #[test]
    fn indices_keep_their_source_position() {
        let mut c = BTreeMap::new();
        c.insert(5, track(1, "A", "X", "Z", "LOSSLESS").1);
        c.insert(250, track(2, "A", "Y", "Z", "LOSSLESS").1);
        assert_eq!(view_for(&c, "a", QualityFilter::All), vec![5, 250]);
    }
}
