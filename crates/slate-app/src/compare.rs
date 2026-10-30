//! Sidebar → right-click a playlist → "Compare with…": only in A / only in B / in both, with
//! actions to build new playlists, add the missing tracks to an owned one, or merge.

use std::sync::Mutex;

use slate_core::commands::{library as lib_cmds, metadata};
use slate_core::playlist_tools::{compare, ids_at, merged_ids, Comparison};
use slate_core::tidal_api::TidalTrack;
use slint::{ModelRc, SharedString, VecModel};

use crate::backend::Backend;

const MAX_LINES: usize = 500;

struct Side {
    uuid: String,
    title: String,
    owned: bool,
    tracks: Vec<TidalTrack>,
}

struct State {
    a: Side,
    b: Side,
    cmp: Comparison,
    tab: usize,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn line(t: &TidalTrack) -> String {
    let artist = t
        .artists
        .as_ref()
        .and_then(|a| a.first())
        .map(|a| a.name.as_str())
        .or_else(|| t.artist.as_ref().map(|a| a.name.as_str()))
        .unwrap_or("");
    format!("{artist} – {}", t.title)
}

/// (tab labels, lines of the shown tab).
fn view(s: &State) -> (Vec<String>, Vec<String>) {
    let labels = vec![
        format!("Only in A ({})", s.cmp.only_a.len()),
        format!("Only in B ({})", s.cmp.only_b.len()),
        format!("In both ({})", s.cmp.both.len()),
    ];
    let (idx, src) = match s.tab {
        0 => (&s.cmp.only_a, &s.a.tracks),
        1 => (&s.cmp.only_b, &s.b.tracks),
        _ => (&s.cmp.both, &s.a.tracks),
    };
    let mut lines: Vec<String> = idx
        .iter()
        .take(MAX_LINES)
        .filter_map(|i| src.get(*i).map(line))
        .collect();
    if idx.len() > MAX_LINES {
        lines.push(format!("…and {} more", idx.len() - MAX_LINES));
    }
    (labels, lines)
}

impl Backend {
    /// The sidebar dialog's "Compare with…" choice: `a` and `b` are playlist uuids.
    pub(crate) async fn run_compare(&self, a: &str, b: &str) {
        if a == b {
            self.notify("Pick a different playlist");
            return;
        }
        let Some(state) = self.state() else { return };
        let user = self.user_id().await;
        self.ui(|w| w.set_notice("Reading both playlists…".into()));
        let mut sides = Vec::new();
        for uuid in [a, b] {
            let details = metadata::get_playlist_details(&state, uuid.to_string()).await;
            let (title, owned) = match &details {
                Ok(d) => (
                    d["title"].as_str().unwrap_or("Playlist").to_string(),
                    user.is_some() && d["creator"]["id"].as_u64() == user,
                ),
                Err(_) => ("Playlist".to_string(), false),
            };
            match lib_cmds::get_playlist_tracks(&state, self.app.clone(), uuid.to_string()).await {
                Ok(tracks) => sides.push(Side {
                    uuid: uuid.to_string(),
                    title,
                    owned,
                    tracks,
                }),
                Err(e) => {
                    let msg = format!("Could not read the playlist: {}", e.log_safe());
                    self.ui(move |w| {
                        w.set_notice("".into());
                        w.set_error(msg.into());
                    });
                    return;
                }
            }
        }
        let (Some(second), Some(first)) = (sides.pop(), sides.pop()) else {
            return;
        };
        let cmp = compare(&first.tracks, &second.tracks);
        let st = State {
            a: first,
            b: second,
            cmp,
            tab: 0,
        };
        self.show_compare(&st);
        if let Ok(mut s) = STATE.lock() {
            *s = Some(st);
        }
    }

    fn show_compare(&self, s: &State) {
        let (labels, lines) = view(s);
        let title = format!("A: {}   ·   B: {}", s.a.title, s.b.title);
        let (tab, can_a, can_b) = (s.tab as i32, s.a.owned, s.b.owned);
        self.ui(move |w| {
            let strings = |v: Vec<String>| {
                ModelRc::new(VecModel::from(
                    v.into_iter().map(SharedString::from).collect::<Vec<_>>(),
                ))
            };
            w.set_notice("".into());
            w.set_cmp_title(title.into());
            w.set_cmp_labels(strings(labels));
            w.set_cmp_lines(strings(lines));
            w.set_cmp_tab(tab);
            w.set_cmp_can_add_a(can_a);
            w.set_cmp_can_add_b(can_b);
            w.set_cmp_open(true);
        });
    }

    pub fn compare_tab(&self, tab: i32) {
        let Ok(mut guard) = STATE.lock() else { return };
        if let Some(s) = guard.as_mut() {
            s.tab = usize::try_from(tab).unwrap_or(0).min(2);
            self.show_compare(s);
        }
    }

    pub fn compare_close(&self) {
        if let Ok(mut s) = STATE.lock() {
            *s = None;
        }
    }

    /// Buttons of the compare dialog.
    pub fn compare_action(&self, action: &str) {
        // Take what is needed so no lock is held across network calls.
        let plan = {
            let Ok(guard) = STATE.lock() else { return };
            let Some(s) = guard.as_ref() else { return };
            let tab_ids = match s.tab {
                0 => ids_at(&s.a.tracks, &s.cmp.only_a),
                1 => ids_at(&s.b.tracks, &s.cmp.only_b),
                _ => ids_at(&s.a.tracks, &s.cmp.both),
            };
            match action {
                "tab-new" => Plan::New(format!("{} / {}", s.a.title, s.b.title), tab_ids),
                "merge-new" => Plan::New(
                    format!("{} + {}", s.a.title, s.b.title),
                    merged_ids(&s.a.tracks, &s.b.tracks),
                ),
                // Tracks B has and A lacks go into A.
                "add-to-a" if s.a.owned => {
                    Plan::Add(s.a.uuid.clone(), ids_at(&s.b.tracks, &s.cmp.only_b))
                }
                "add-to-b" if s.b.owned => {
                    Plan::Add(s.b.uuid.clone(), ids_at(&s.a.tracks, &s.cmp.only_a))
                }
                _ => return,
            }
        };
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            if let Ok(mut s) = STATE.lock() {
                *s = None;
            }
            this.ui(|w| w.set_cmp_open(false));
            let msg = match plan {
                Plan::New(_, ids) | Plan::Add(_, ids) if ids.is_empty() => {
                    "Nothing to add".to_string()
                }
                Plan::New(name, ids) => {
                    let name: String = name.chars().take(100).collect();
                    this.ui(|w| w.set_notice("Creating the playlist…".into()));
                    match lib_cmds::create_playlist_with_tracks(
                        &state,
                        name.clone(),
                        "UNLISTED",
                        &ids,
                    )
                    .await
                    {
                        Ok((_, added)) => format!("Created “{name}” with {added} tracks"),
                        Err(e) => format!("Could not create the playlist: {}", e.log_safe()),
                    }
                }
                Plan::Add(uuid, ids) => {
                    this.ui(|w| w.set_notice("Adding tracks…".into()));
                    match lib_cmds::add_tracks_paced(&state, &uuid, &ids).await {
                        Ok(n) => format!("Added {n} tracks"),
                        Err(e) => format!("Could not add the tracks: {}", e.log_safe()),
                    }
                }
            };
            this.load_library();
            this.notify(&msg);
        });
    }
}

enum Plan {
    New(String, Vec<u64>),
    Add(String, Vec<u64>),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(id: u64, isrc: &str) -> TidalTrack {
        serde_json::from_value(serde_json::json!({
            "id": id, "title": format!("T{id}"), "duration": 1, "isrc": isrc,
            "artist": {"id": 1, "name": "Ann"},
        }))
        .expect("track")
    }

    fn state() -> State {
        let a = vec![t(1, "X"), t(2, "Y")];
        let b = vec![t(9, "Y"), t(3, "Z")];
        let cmp = compare(&a, &b);
        let side = |tracks| Side {
            uuid: "u".into(),
            title: "P".into(),
            owned: true,
            tracks,
        };
        State {
            a: side(a),
            b: side(b),
            cmp,
            tab: 0,
        }
    }

    #[test]
    fn tabs_show_counts_and_the_right_lines() {
        let mut s = state();
        let (labels, lines) = view(&s);
        assert_eq!(labels, ["Only in A (1)", "Only in B (1)", "In both (1)"]);
        assert_eq!(lines, ["Ann – T1"]);
        s.tab = 1;
        assert_eq!(view(&s).1, ["Ann – T3"]);
        s.tab = 2;
        assert_eq!(view(&s).1, ["Ann – T2"]);
    }

    #[test]
    fn long_tabs_are_cut_with_a_summary() {
        let mut s = state();
        s.a.tracks = (0..MAX_LINES as u64 + 5)
            .map(|i| t(i + 100, &format!("I{i}")))
            .collect();
        s.cmp = compare(&s.a.tracks, &[]);
        let lines = view(&s).1;
        assert_eq!(lines.len(), MAX_LINES + 1);
        assert_eq!(lines.last().map(String::as_str), Some("…and 5 more"));
    }
}
