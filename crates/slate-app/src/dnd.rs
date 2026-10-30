//! Drag and drop: tracks onto a sidebar playlist, reordering an owned playlist or the queue,
//! playlists into and out of sidebar folders. Payloads are small text values (see `dnd.slint`).

use slate_core::commands::library as lib_cmds;
use slate_core::playlist_tools::TrackKey;
use slate_core::queue::Track;
use slint::{ComponentHandle, DataTransfer, SharedString};

use crate::backend::Backend;
use crate::bootstrap::{AppWindow, Dnd};
use crate::library::SourceKind;
use crate::pages::Route;

/// A payload as Rust sees it.
#[derive(Debug, PartialEq, Eq)]
pub enum Payload {
    Row(usize),
    Suggestion(usize),
    Queue(usize),
    Playlist(String),
}

pub fn parse_payload(text: &str) -> Option<Payload> {
    let (kind, rest) = text.split_once(':')?;
    match kind {
        "row" => rest.parse().ok().map(Payload::Row),
        "sugg" => rest.parse().ok().map(Payload::Suggestion),
        "queue" => rest.parse().ok().map(Payload::Queue),
        "pl" if !rest.is_empty() => Some(Payload::Playlist(rest.to_string())),
        _ => None,
    }
}

/// A drop target as Rust sees it.
#[derive(Debug, PartialEq, Eq)]
pub enum Target {
    /// A playlist in the sidebar (uuid).
    Playlist(String),
    /// A folder in the sidebar (`folder:ID`).
    Folder(String),
    /// "PLAYLISTS": out of any folder.
    Root,
    Row(usize),
    QueueRow(usize),
}

pub fn parse_target(text: &str) -> Option<Target> {
    if text == "root" {
        return Some(Target::Root);
    }
    if let Some(i) = text.strip_prefix("row:") {
        return i.parse().ok().map(Target::Row);
    }
    if let Some(i) = text.strip_prefix("queue:") {
        return i.parse().ok().map(Target::QueueRow);
    }
    if text.starts_with("folder:") {
        return Some(Target::Folder(text.to_string()));
    }
    (!text.is_empty()).then(|| Target::Playlist(text.to_string()))
}

/// Whether `track` is already in a playlist whose keys are `existing`.
pub fn already_in(track: &Track, existing: &std::collections::HashSet<TrackKey>) -> bool {
    let isrc = track
        .extra
        .get("isrc")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let key = match isrc {
        Some(i) => TrackKey::Isrc(i.to_ascii_uppercase()),
        None => TrackKey::Id(track.id),
    };
    existing.contains(&key)
}

/// Installs the callbacks that make and read payloads. UI thread, once.
pub fn install(window: &AppWindow, backend: &Backend) {
    let d = window.global::<Dnd>();
    d.on_make(DataTransfer::from);
    d.on_kind(|data| {
        let text = data.plain_text().unwrap_or_default();
        SharedString::from(text.split(':').next().unwrap_or(""))
    });
    let b = backend.clone();
    d.on_dropped(move |data, target| {
        if let Ok(text) = data.plain_text() {
            b.dnd_dropped(&text, &target);
        }
    });
}

impl Backend {
    pub(crate) fn dnd_dropped(&self, payload: &str, target: &str) {
        let (Some(p), Some(t)) = (parse_payload(payload), parse_target(target)) else {
            return;
        };
        match (p, t) {
            (Payload::Row(i), Target::Playlist(uuid)) => {
                if let Some(track) = self.cached_track(i) {
                    self.drop_track_on_playlist(track, uuid);
                }
            }
            (Payload::Suggestion(i), Target::Playlist(uuid)) => {
                if let Some(track) = self.suggested_track(i) {
                    self.drop_track_on_playlist(track, uuid);
                }
            }
            (Payload::Row(from), Target::Row(to)) => self.reorder_playlist_rows(from, to),
            (Payload::Queue(from), Target::QueueRow(to)) => self.queue_move(from, to),
            (Payload::Playlist(uuid), Target::Folder(folder)) => {
                self.move_playlist_to(&uuid, folder.trim_start_matches("folder:"));
            }
            (Payload::Playlist(uuid), Target::Root) => self.move_playlist_to(&uuid, "root"),
            _ => {}
        }
    }

    /// Adds `track` to the playlist, asking first if it is already there.
    fn drop_track_on_playlist(&self, track: Track, uuid: String) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            let existing =
                match lib_cmds::get_playlist_tracks(&state, this.app.clone(), uuid.clone()).await {
                    Ok(t) => slate_core::playlist_tools::keys_of(&t),
                    Err(e) => {
                        let msg = format!("Could not read the playlist: {}", e.log_safe());
                        this.ui(move |w| w.set_error(msg.into()));
                        return;
                    }
                };
            if already_in(&track, &existing) {
                let (id, name) = (track.id.to_string(), track.title().to_string());
                this.ui(move |w| {
                    w.set_dialog_kind("dup".into());
                    w.set_dialog_target(format!("{uuid}|{id}").into());
                    w.set_dialog_text(format!("“{name}” is already in this playlist.").into());
                    w.set_dialog_open(true);
                });
                return;
            }
            this.add_dropped(&uuid, track.id).await;
        });
    }

    /// Adds one track to a playlist; shared by the drop and by "Add anyway".
    pub(crate) async fn add_dropped(&self, uuid: &str, track_id: u64) {
        let Some(state) = self.state() else { return };
        match lib_cmds::add_track_to_playlist(&state, uuid.to_string(), track_id).await {
            Ok(()) => {
                self.notify("Added to playlist");
                self.load_library();
            }
            Err(e) => {
                let msg = format!("Could not add the track: {}", e.log_safe());
                self.ui(move |w| w.set_error(msg.into()));
            }
        }
    }

    /// A row dropped on another row of the open, owned playlist (its own order).
    fn reorder_playlist_rows(&self, from: usize, to: usize) {
        let Some(SourceKind::Playlist(uuid)) = self.lib.active_kind_pub() else {
            return;
        };
        if self.lib.view.lock().is_ok_and(|v| v.is_some()) {
            self.notify("Clear the filter to reorder");
            return;
        }
        if from == to {
            return;
        }
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            match lib_cmds::move_playlist_item(&state, uuid.clone(), from as u32, to as u32).await {
                Ok(()) => this.navigate_refresh(Route::Playlist(uuid)),
                Err(e) => {
                    let msg = format!("Could not move the track: {}", e.log_safe());
                    this.ui(move |w| w.set_error(msg.into()));
                }
            }
        });
    }

    fn move_playlist_to(&self, uuid: &str, folder: &str) {
        let (uuid, folder) = (uuid.to_string(), folder.to_string());
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            let dest = if folder == "root" {
                "root".to_string()
            } else {
                folder
            };
            match lib_cmds::move_playlist_to_folder(&state, dest, format!("trn:playlist:{uuid}"))
                .await
            {
                Ok(()) => {
                    this.notify("Moved");
                    this.load_library();
                }
                Err(e) => {
                    let msg = format!("Could not move the playlist: {}", e.log_safe());
                    this.ui(move |w| w.set_error(msg.into()));
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payloads_parse() {
        assert_eq!(parse_payload("row:12"), Some(Payload::Row(12)));
        assert_eq!(parse_payload("sugg:0"), Some(Payload::Suggestion(0)));
        assert_eq!(parse_payload("queue:3"), Some(Payload::Queue(3)));
        assert_eq!(
            parse_payload("pl:abc-def"),
            Some(Payload::Playlist("abc-def".into()))
        );
        for bad in ["", "row", "row:x", "pl:", "other:1", "row:-1"] {
            assert_eq!(parse_payload(bad), None, "{bad}");
        }
    }

    #[test]
    fn targets_parse() {
        assert_eq!(parse_target("root"), Some(Target::Root));
        assert_eq!(parse_target("row:4"), Some(Target::Row(4)));
        assert_eq!(parse_target("queue:1"), Some(Target::QueueRow(1)));
        assert_eq!(
            parse_target("folder:f1"),
            Some(Target::Folder("folder:f1".into()))
        );
        assert_eq!(
            parse_target("uuid-1"),
            Some(Target::Playlist("uuid-1".into()))
        );
        assert_eq!(parse_target(""), None);
    }

    #[test]
    fn duplicates_are_found_by_isrc_then_id() {
        let mut t = Track::new(7);
        t.extra.insert("isrc".into(), "usabc1234567".into());
        let by_isrc = slate_core::playlist_tools::keys_of(&[]);
        assert!(!already_in(&t, &by_isrc));
        let mut with = std::collections::HashSet::new();
        with.insert(TrackKey::Isrc("USABC1234567".into()));
        assert!(already_in(&t, &with));
        let plain = Track::new(9);
        let mut ids = std::collections::HashSet::new();
        ids.insert(TrackKey::Id(9));
        assert!(already_in(&plain, &ids));
        assert!(!already_in(&Track::new(10), &ids));
    }
}
