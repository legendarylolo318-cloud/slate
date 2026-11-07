//! Queue panel extras: save the queue as a TIDAL playlist, and named queue snapshots kept on
//! disk (save, restore by replacing or appending, rename, delete). The snapshot list is read
//! from disk only when the dialog opens.

use slate_core::commands::library as lib_cmds;
use slate_core::queue::Track;
use slate_core::queue_snapshots::{self as snaps, Restore, Store};
use slint::{ModelRc, VecModel};

use crate::backend::Backend;
use crate::bootstrap::Choice;

fn store() -> Option<Store> {
    slate_core::config_dir_for_env().map(|d| Store::new(d.join("queue-snapshots")))
}

fn default_name() -> String {
    format!("Queue {}", snaps::ymd(slate_core::now_secs()))
}

impl Backend {
    /// What is playing plus everything upcoming.
    async fn whole_queue(&self) -> Vec<Track> {
        let Some(ctl) = self.controller() else {
            return Vec::new();
        };
        ctl.with_queue(|q| {
            q.current
                .iter()
                .filter(|t| !t.is_video())
                .chain(q.upcoming().filter(|t| !t.is_video()))
                .map(|t| Track::clone(t))
                .collect()
        })
        .await
    }

    /// The queue panel's "⋯" menu.
    pub fn queue_action(&self, action: &str) {
        match action {
            "save-playlist" => self.save_queue_as_playlist(),
            "snapshots" => self.open_snapshots(),
            "fill-custom" => self.ui(|w| {
                w.set_dialog_text("".into());
                w.set_dialog_target("".into());
                w.set_dialog_kind("fill".into());
                w.set_dialog_open(true);
            }),
            a => {
                if let Some(min) = a.strip_prefix("fill-").and_then(|m| m.parse().ok()) {
                    self.fill_queue(min);
                }
            }
        }
    }

    /// "Fill queue to N minutes" (one undo step).
    pub(crate) fn fill_queue(&self, minutes: u32) {
        use slate_core::controller::FillOutcome;
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(ctl) = this.controller() else { return };
            let msg = match ctl.fill_queue(minutes as f32 * 60.0).await {
                FillOutcome::Added {
                    tracks,
                    total_secs,
                    from_source,
                } => {
                    this.refresh_queue_info().await;
                    this.save().await;
                    format!(
                        "Added {tracks} {} from the {}; the queue is now {}",
                        if tracks == 1 { "track" } else { "tracks" },
                        if from_source { "source" } else { "radio" },
                        crate::textutil::minutes_text(total_secs)
                    )
                }
                FillOutcome::AlreadyLongEnough => {
                    format!("The queue already holds {minutes} minutes or more")
                }
                FillOutcome::NothingToAdd => "No more tracks to fill the queue with".to_string(),
            };
            this.notify(&msg);
        });
    }

    /// The custom-minutes dialog.
    pub(crate) fn fill_custom(&self, text: &str) {
        match slate_core::queue_fill::parse_minutes(text) {
            Some(m) => self.fill_queue(m),
            None => self.notify("Enter a number of minutes from 1 to 600"),
        }
    }

    /// "Save queue as playlist": an UNLISTED playlist, like the 1.2 tools make.
    fn save_queue_as_playlist(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            let ids: Vec<u64> = this.whole_queue().await.iter().map(|t| t.id).collect();
            if ids.is_empty() {
                this.notify("The queue is empty");
                return;
            }
            let Some(state) = this.state() else { return };
            let Some(job) = this.begin_bulk() else { return };
            this.ui(|w| w.set_notice("Saving the queue as a playlist…".into()));
            let title = default_name();
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

    /// Reads the snapshot headers and opens the dialog.
    fn open_snapshots(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            let list =
                tokio::task::spawn_blocking(|| store().map(|s| s.list()).unwrap_or_default())
                    .await
                    .unwrap_or_default();
            let rows: Vec<Choice> = list
                .into_iter()
                .map(|m| Choice {
                    title: m.label().into(),
                    id: m.id.into(),
                })
                .collect();
            this.ui(move |w| {
                w.set_dialog_choices(ModelRc::new(VecModel::from(rows)));
                w.set_dialog_text("".into());
                w.set_dialog_target("".into());
                w.set_dialog_kind("snapshots".into());
                w.set_dialog_open(true);
            });
        });
    }

    /// Buttons in the snapshot dialog: save | replace | append | rename | delete.
    pub fn snapshot_action(&self, kind: &str, id: &str, text: &str) {
        let (kind, id, text) = (kind.to_string(), id.to_string(), text.trim().to_string());
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(ctl) = this.controller() else { return };
            let result: Result<String, String> = match kind.as_str() {
                "save" => {
                    let tracks = this.whole_queue().await;
                    let name = if text.is_empty() {
                        default_name()
                    } else {
                        text
                    };
                    let now = slate_core::now_secs();
                    let total = tracks.len();
                    tokio::task::spawn_blocking(move || {
                        store()
                            .ok_or_else(|| "No place to keep snapshots.".to_string())?
                            .save(&name, &tracks, now)
                            .map(|m| {
                                if m.count < total {
                                    format!(
                                        "Saved “{}” (the first {} of {total} tracks)",
                                        m.name, m.count
                                    )
                                } else {
                                    format!("Saved “{}” ({} tracks)", m.name, m.count)
                                }
                            })
                    })
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()))
                }
                "replace" | "append" => {
                    let mode = if kind == "replace" {
                        Restore::Replace
                    } else {
                        Restore::Append
                    };
                    let loaded = tokio::task::spawn_blocking(move || {
                        store()
                            .ok_or_else(|| "No place to keep snapshots.".to_string())?
                            .load(&id)
                    })
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()));
                    match loaded {
                        Ok(tracks) => {
                            let n = tracks.len();
                            match mode {
                                Restore::Replace => ctl.replace_upcoming(tracks).await,
                                Restore::Append => ctl.append_upcoming(tracks).await,
                            }
                            this.refresh_queue_info().await;
                            this.save().await;
                            Ok(format!(
                                "{} {n} tracks {} the queue",
                                if mode == Restore::Replace {
                                    "Restored"
                                } else {
                                    "Added"
                                },
                                if mode == Restore::Replace { "as" } else { "to" }
                            ))
                        }
                        Err(e) => Err(e),
                    }
                }
                "rename" => tokio::task::spawn_blocking(move || {
                    store()
                        .ok_or_else(|| "No place to keep snapshots.".to_string())?
                        .rename(&id, &text)
                        .map(|()| "Renamed".to_string())
                })
                .await
                .unwrap_or_else(|e| Err(e.to_string())),
                "delete" => tokio::task::spawn_blocking(move || {
                    store()
                        .ok_or_else(|| "No place to keep snapshots.".to_string())?
                        .delete(&id)
                        .map(|()| "Snapshot deleted".to_string())
                })
                .await
                .unwrap_or_else(|e| Err(e.to_string())),
                _ => return,
            };
            match result {
                Ok(m) => this.notify(&m),
                Err(e) => this.ui(move |w| w.set_error(e.into())),
            }
            // Saving, renaming and deleting keep the dialog; the list is read again.
            if matches!(kind.as_str(), "save" | "rename" | "delete") {
                this.open_snapshots();
            } else {
                this.ui(|w| w.set_dialog_open(false));
            }
        });
    }
}
