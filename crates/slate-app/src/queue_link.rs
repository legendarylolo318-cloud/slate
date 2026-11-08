//! Share the queue as a `slate://queue/…` link, and open one. Reading a link is bounded and
//! pure (`slate_core::queue_link`); this file asks what to do with it, then loads the tracks
//! it can (4 requests at a time, like every other page) and skips the ones that are
//! unavailable or hidden by the content filters, with a count.

use std::sync::Mutex;

use futures_util::stream::{self, StreamExt};
use slate_core::commands::metadata;
use slate_core::queue::Track;
use slate_core::queue_link as link;

use crate::backend::Backend;
use crate::textutil::clean_line;

/// The link waiting for "Replace / Add / Cancel". One at a time: a newer link replaces it.
static PENDING: Mutex<Option<(link::Decoded, u64)>> = Mutex::new(None);

/// Sign-out: a link waiting for its answer is dropped.
pub fn forget() {
    *pending() = None;
}

fn pending() -> std::sync::MutexGuard<'static, Option<(link::Decoded, u64)>> {
    PENDING.lock().unwrap_or_else(|e| e.into_inner())
}

/// What the dialog says about a link.
pub fn describe(d: &link::Decoded) -> String {
    let n = d.ids.len();
    let tracks = format!("{n} {}", if n == 1 { "track" } else { "tracks" });
    match &d.name {
        Some(name) => format!("“{name}”: {tracks}. Replace your queue with it, or add it after what is queued?"),
        None => format!("A shared queue of {tracks}. Replace your queue with it, or add it after what is queued?"),
    }
}

/// The notice after copying.
pub fn copied_message(e: &link::Encoded) -> String {
    if e.was_cut() {
        format!(
            "Copied a link to the first {} of {} tracks; a queue link holds at most {}",
            e.included,
            e.total,
            link::MAX_TRACKS
        )
    } else {
        format!(
            "Copied a link to {} {}",
            e.included,
            if e.included == 1 { "track" } else { "tracks" }
        )
    }
}

/// The notice after loading.
pub fn loaded_message(added: usize, skipped: usize, replaced: bool) -> String {
    let what = format!("{added} {}", if added == 1 { "track" } else { "tracks" });
    let head = if replaced {
        format!("Queue replaced with {what}")
    } else {
        format!("Added {what} to the queue")
    };
    match skipped {
        0 => head,
        n => format!(
            "{head}; skipped {n} that {} unavailable or blocked",
            if n == 1 { "is" } else { "are" }
        ),
    }
}

impl Backend {
    /// Queue "⋯" → Copy queue link.
    pub(crate) fn copy_queue_link(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            let ids: Vec<u64> = this.whole_queue().await.iter().map(|t| t.id).collect();
            if ids.is_empty() {
                this.notify("The queue is empty");
                return;
            }
            let e = link::encode(None, &ids, clean_line);
            let msg = match crate::clipboard::set_text(e.link.clone()) {
                Ok(()) => copied_message(&e),
                Err(_) => "Could not reach the clipboard".to_string(),
            };
            this.notify(&msg);
        });
    }

    /// A `slate://queue/…` link arrived (typed, clicked, or forwarded by a second launch).
    pub(crate) fn queue_link_opened(&self, raw: &str) {
        match link::decode(raw, clean_line) {
            Err(sentence) => self.notify(&sentence),
            Ok(d) => {
                let text = describe(&d);
                *pending() = Some((d, self.account_epoch()));
                self.ui(move |w| {
                    w.set_dialog_text(text.into());
                    w.set_dialog_target("".into());
                    w.set_dialog_kind("queue-link".into());
                    w.set_dialog_open(true);
                });
            }
        }
    }

    /// The dialog's Replace / Add button.
    pub(crate) fn queue_link_accepted(&self, replace: bool) {
        let Some((d, epoch)) = pending().take() else {
            return;
        };
        if !self.same_account(epoch) {
            return;
        }
        let this = self.clone();
        self.rt.spawn(async move {
            let (Some(state), Some(ctl)) = (this.state(), this.controller()) else {
                return;
            };
            this.notify("Loading the shared queue…");
            let fetched: Vec<Option<Track>> = stream::iter(d.ids.iter().copied())
                .map(|id| {
                    let state = state.clone();
                    async move {
                        let v = metadata::get_track(&state, id).await.ok()?;
                        serde_json::from_value::<Track>(v).ok()
                    }
                })
                .buffered(4)
                .collect()
                .await;
            // Signed out while the tracks loaded: the queue is not touched.
            if !this.same_account(epoch) {
                return;
            }
            let missing = fetched.iter().filter(|t| t.is_none()).count();
            let (tracks, blocked) = ctl.playable(fetched.into_iter().flatten().collect());
            let skipped = missing + blocked;
            let added = tracks.len();
            if tracks.is_empty() {
                this.notify(&format!(
                    "None of the {skipped} tracks in that queue link can be played here"
                ));
                return;
            }
            if replace {
                let outcome = ctl.play_all(tracks, None, false, false).await;
                this.apply(outcome).await;
            } else {
                // "Add it after what is queued", as the dialog says: after the rest of
                // the playing album or playlist too, not in "next up" ahead of it.
                ctl.append_after_all(tracks).await;
            }
            this.refresh_queue_info().await;
            this.save().await;
            this.notify(&loaded_message(added, skipped, replace));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dialog_names_the_queue_and_counts_tracks() {
        let named = link::Decoded {
            name: Some("Road trip".into()),
            ids: vec![1, 2],
        };
        assert!(describe(&named).starts_with("“Road trip”: 2 tracks."));
        let one = link::Decoded {
            name: None,
            ids: vec![1],
        };
        assert!(describe(&one).starts_with("A shared queue of 1 track."));
    }

    #[test]
    fn copy_says_when_the_queue_was_cut() {
        let cut = link::Encoded {
            link: String::new(),
            included: 500,
            total: 620,
        };
        assert!(copied_message(&cut).contains("first 500 of 620"));
        let all = link::Encoded {
            link: String::new(),
            included: 3,
            total: 3,
        };
        assert_eq!(copied_message(&all), "Copied a link to 3 tracks");
    }

    #[test]
    fn loading_counts_what_was_skipped() {
        assert_eq!(loaded_message(10, 0, true), "Queue replaced with 10 tracks");
        assert_eq!(
            loaded_message(9, 1, false),
            "Added 9 tracks to the queue; skipped 1 that is unavailable or blocked"
        );
        assert!(loaded_message(5, 3, false).contains("3 that are"));
    }
}
