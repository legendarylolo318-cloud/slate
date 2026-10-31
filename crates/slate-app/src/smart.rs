//! Smart playlists in the app: the sidebar section, the rule editor, evaluating a list when
//! it is opened (against Loved Tracks plus any playlists its rules name), and the snapshot
//! to a real TIDAL playlist.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use slate_core::commands::library as lib_cmds;
use slate_core::list_filter::fold;
use slate_core::smart::{self, Cand, Field, Rule, SmartPlaylist, Store, FIELDS, SORTS};
use slate_core::tidal_api::TidalTrack;
use slint::{Image, ModelRc, SharedString, VecModel};

use crate::backend::Backend;
use crate::bootstrap::{PlaylistEntry, SmartRuleRow};
use crate::library::SourceKind;
use crate::models::PAGE_SIZE;

/// The list being edited and whether it is new.
static DRAFT: Mutex<Option<(SmartPlaylist, bool)>> = Mutex::new(None);

fn store_path() -> Option<PathBuf> {
    slate_core::config_dir_for_env().map(|d| d.join("smart_playlists.json"))
}

fn load_store() -> Store {
    store_path().map(|p| Store::load(&p)).unwrap_or_default()
}

fn save_store(s: &Store) -> Result<(), String> {
    let p = store_path().ok_or("No config directory")?;
    s.save(&p)
        .map_err(|e| format!("Could not save smart playlists: {e}"))
}

pub(crate) fn today() -> i64 {
    i64::try_from(slate_core::now_secs() / 86_400).unwrap_or(0)
}

/// The rules' view of one TIDAL track.
fn cand_of(t: &TidalTrack, loved_added: Option<&Option<String>>, playlists: &[String]) -> Cand {
    let badges = crate::library::to_track(t)
        .map(|x| crate::quality::track_badges(&x))
        .unwrap_or_default();
    let mut artists: Vec<String> = t.artists.iter().flatten().map(|a| a.name.clone()).collect();
    if artists.is_empty() {
        artists.extend(t.artist.iter().map(|a| a.name.clone()));
    }
    let title = match t.version.as_deref() {
        Some(v) if !v.is_empty() => format!("{} ({v})", t.title),
        _ => t.title.clone(),
    };
    let date = t
        .album
        .as_ref()
        .and_then(|a| a.release_date.as_deref())
        .or(t.stream_start_date.as_deref());
    Cand {
        title,
        artists,
        album: t
            .album
            .as_ref()
            .map(|a| a.title.clone())
            .unwrap_or_default(),
        tier: badges.tier,
        atmos: badges.atmos,
        explicit: t.explicit == Some(true),
        duration_secs: t.duration,
        year: date.and_then(smart::year_of),
        added_day: loved_added
            .and_then(|d| d.as_deref())
            .and_then(smart::day_number),
        loved: loved_added.is_some(),
        playlists: playlists.to_vec(),
    }
}

/// A track with: `Some(date added)` if it is in Loved Tracks, and the playlists holding it.
type Seen = (TidalTrack, Option<Option<String>>, Vec<String>);

/// Library data → candidates, deduplicated by track id, in first-seen order.
fn build_universe(
    loved: Vec<TidalTrack>,
    playlists: Vec<(String, Vec<TidalTrack>)>,
) -> (Vec<TidalTrack>, Vec<Cand>) {
    let mut order: Vec<u64> = Vec::new();
    let mut by_id: HashMap<u64, Seen> = HashMap::new();
    for t in loved {
        let added = t.date_added.clone();
        let id = t.id;
        order.push(id);
        by_id.insert(id, (t, Some(added), Vec::new()));
    }
    for (title, tracks) in playlists {
        for t in tracks {
            let id = t.id;
            let e = by_id.entry(id).or_insert_with(|| {
                order.push(id);
                (t, None, Vec::new())
            });
            if !e.2.contains(&title) {
                e.2.push(title.clone());
            }
        }
    }
    let mut tracks = Vec::with_capacity(order.len());
    let mut cands = Vec::with_capacity(order.len());
    for id in order {
        if let Some((t, added, pls)) = by_id.remove(&id) {
            cands.push(cand_of(&t, added.as_ref(), &pls));
            tracks.push(t);
        }
    }
    (tracks, cands)
}

/// Sidebar entry for a smart playlist.
fn entry(l: &SmartPlaylist) -> PlaylistEntry {
    PlaylistEntry {
        uuid: format!("smart:{}", l.id).into(),
        title: l.name.clone().into(),
        subtitle: "Smart playlist".into(),
        folder: false,
        cover: Image::default(),
    }
}

impl Backend {
    /// Fills the sidebar's Smart section.
    pub(crate) fn load_smart_sidebar(&self) {
        let lists = load_store().lists;
        self.ui(move |w| {
            let entries: Vec<PlaylistEntry> = lists.iter().map(entry).collect();
            w.set_smart_lists(ModelRc::new(VecModel::from(entries)));
        });
    }

    /// Evaluates smart playlist `id` against the library. `Err` is shown to the user.
    pub(crate) async fn evaluate_smart(
        &self,
        id: &str,
    ) -> Result<(SmartPlaylist, Vec<TidalTrack>), String> {
        let list = load_store()
            .get(id)
            .cloned()
            .ok_or("That smart playlist no longer exists.")?;
        let state = self.state().ok_or("The core is still starting.")?;
        // Loved Tracks, page by page through the cached fetcher.
        let mut loved = Vec::new();
        for page in 0.. {
            let (items, total) = self.fetch_page(&SourceKind::Loved, page).await?;
            let got = items.len();
            loved.extend(items);
            if got == 0 || loved.len() >= total {
                break;
            }
        }
        let mut playlists = Vec::new();
        let known = self
            .lib
            .playlist_titles
            .lock()
            .map(|t| t.clone())
            .unwrap_or_default();
        for wanted in list.referenced_playlists() {
            let Some((uuid, title)) = known
                .iter()
                .find(|(_, t)| fold(t) == fold(&wanted))
                .cloned()
            else {
                return Err(format!("No playlist named “{wanted}”."));
            };
            let tracks = lib_cmds::get_playlist_tracks(&state, self.app.clone(), uuid)
                .await
                .map_err(|e| format!("Could not read “{title}”: {}", e.log_safe()))?;
            playlists.push((title, tracks));
        }
        let (tracks, cands) = build_universe(loved, playlists);
        let hits = smart::evaluate(&list, &cands, today());
        let picked = hits
            .into_iter()
            .filter_map(|i| tracks.get(i).cloned())
            .collect();
        Ok((list, picked))
    }

    // ---- editor ----

    pub(crate) fn smart_action(&self, id: &str, action: &str) {
        match action {
            "edit" => {
                if let Some(l) = load_store().get(id).cloned() {
                    self.open_smart_editor(l, false);
                }
            }
            "delete" => {
                let mut s = load_store();
                if s.delete(id) {
                    if let Err(e) = save_store(&s) {
                        self.ui(move |w| w.set_error(e.into()));
                    }
                }
                self.load_smart_sidebar();
                if self.lib.active_kind_pub() == Some(SourceKind::Smart(id.to_string())) {
                    self.navigate(crate::pages::Route::Home);
                }
            }
            _ => {}
        }
    }

    pub fn new_smart(&self) {
        let store = load_store();
        let id = store.new_id(slate_core::now_secs());
        self.open_smart_editor(SmartPlaylist::new(&id, "My smart playlist"), true);
    }

    fn open_smart_editor(&self, l: SmartPlaylist, new: bool) {
        if let Ok(mut d) = DRAFT.lock() {
            *d = Some((l, new));
        }
        self.sync_smart_editor("");
    }

    fn sync_smart_editor(&self, error: &str) {
        let Some((l, _)) = DRAFT.lock().ok().and_then(|d| d.clone()) else {
            return;
        };
        let error = error.to_string();
        self.ui(move |w| {
            let rows: Vec<SmartRuleRow> = l
                .rules
                .iter()
                .map(|r| SmartRuleRow {
                    field: r.field.label().into(),
                    op: r.op.label(r.field).into(),
                    value: r.value.clone().into(),
                    hint: r.field.hint().into(),
                })
                .collect();
            w.set_smart_name(l.name.clone().into());
            w.set_smart_all(l.match_all);
            w.set_smart_rules(ModelRc::new(VecModel::from(rows)));
            w.set_smart_sort(l.sort.label().into());
            w.set_smart_desc(l.descending);
            w.set_smart_limit(
                if l.limit == 0 {
                    String::new()
                } else {
                    l.limit.to_string()
                }
                .into(),
            );
            w.set_smart_error(SharedString::from(error));
            w.set_smart_open(true);
        });
    }

    fn edit_smart(&self, f: impl FnOnce(&mut SmartPlaylist)) {
        if let Ok(mut d) = DRAFT.lock() {
            if let Some((l, _)) = d.as_mut() {
                f(l);
            }
        }
        self.sync_smart_editor("");
    }

    pub fn smart_edit(&self, what: &str, index: i32, text: &str) {
        let i = usize::try_from(index).unwrap_or(usize::MAX);
        match what {
            "name" => {
                let text = text.to_string();
                if let Ok(mut d) = DRAFT.lock() {
                    if let Some((l, _)) = d.as_mut() {
                        l.name = text;
                    }
                }
            }
            "limit" => {
                let n = text.trim().parse::<u32>().unwrap_or(0);
                if let Ok(mut d) = DRAFT.lock() {
                    if let Some((l, _)) = d.as_mut() {
                        l.limit = n;
                    }
                }
            }
            "value" => {
                let text = text.to_string();
                if let Ok(mut d) = DRAFT.lock() {
                    if let Some((l, _)) = d.as_mut() {
                        if let Some(r) = l.rules.get_mut(i) {
                            r.value = text;
                        }
                    }
                }
            }
            "match" => self.edit_smart(|l| l.match_all = !l.match_all),
            "desc" => self.edit_smart(|l| l.descending = !l.descending),
            "sort" => self.edit_smart(|l| {
                let at = SORTS.iter().position(|s| *s == l.sort).unwrap_or(0);
                l.sort = SORTS[(at + 1) % SORTS.len()];
            }),
            "add" => self.edit_smart(|l| {
                if l.rules.len() < smart::MAX_RULES {
                    l.rules.push(Rule::new(Field::Artist));
                }
            }),
            "remove" => self.edit_smart(|l| {
                if i < l.rules.len() {
                    l.rules.remove(i);
                }
            }),
            "field" => self.edit_smart(|l| {
                if let Some(r) = l.rules.get_mut(i) {
                    let at = FIELDS.iter().position(|f| *f == r.field).unwrap_or(0);
                    *r = Rule::new(FIELDS[(at + 1) % FIELDS.len()]);
                }
            }),
            "op" => self.edit_smart(|l| {
                if let Some(r) = l.rules.get_mut(i) {
                    let ops = r.field.ops();
                    let at = ops.iter().position(|o| *o == r.op).unwrap_or(0);
                    r.op = ops[(at + 1) % ops.len()];
                }
            }),
            "cancel" => {
                if let Ok(mut d) = DRAFT.lock() {
                    *d = None;
                }
                self.ui(|w| w.set_smart_open(false));
            }
            "save" => self.save_smart(),
            _ => {}
        }
    }

    fn save_smart(&self) {
        let Some((mut l, _new)) = DRAFT.lock().ok().and_then(|d| d.clone()) else {
            return;
        };
        l.name = l.name.trim().chars().take(60).collect();
        let mut store = load_store();
        if let Err(e) = store.upsert(l.clone()).and_then(|()| save_store(&store)) {
            self.sync_smart_editor(&e);
            return;
        }
        if let Ok(mut d) = DRAFT.lock() {
            *d = None;
        }
        self.ui(|w| w.set_smart_open(false));
        self.load_smart_sidebar();
        self.navigate(crate::pages::Route::Smart(l.id));
    }

    /// "Save as TIDAL playlist" on a smart playlist page: an UNLISTED snapshot.
    pub fn save_smart_snapshot(&self) {
        let Some(SourceKind::Smart(_)) = self.lib.active_kind_pub() else {
            return;
        };
        let name = self
            .lib
            .active_for_export()
            .map(|(_, t)| t)
            .unwrap_or_else(|| "Smart playlist".into());
        let ids: Vec<u64> = self
            .lib
            .static_tracks
            .lock()
            .map(|s| s.iter().map(|t| t.id).collect())
            .unwrap_or_default();
        if ids.is_empty() {
            self.notify("Nothing to save");
            return;
        }
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            let Some(job) = this.begin_bulk() else { return };
            this.ui(|w| w.set_notice("Saving the playlist…".into()));
            let title: String = name.chars().take(100).collect();
            let msg = match lib_cmds::create_playlist_with_tracks_until(
                &state,
                title.clone(),
                "UNLISTED",
                &ids,
                &|| job.cancelled(),
            )
            .await
            {
                Ok((_, n)) if job.cancelled() => {
                    format!("Stopped: saved “{title}” with {n} tracks")
                }
                Ok((_, n)) => format!("Saved “{title}” as a TIDAL playlist with {n} tracks"),
                Err(e) => format!("Could not save the playlist: {}", e.log_safe()),
            };
            drop(job);
            this.load_library();
            this.notify(&msg);
        });
    }
}

/// `PAGE_SIZE` is re-exported for the page header count.
#[allow(dead_code)]
const _: usize = PAGE_SIZE;

#[cfg(test)]
mod tests {
    use super::*;
    use slate_core::smart::Op;

    fn t(id: u64, title: &str, added: Option<&str>) -> TidalTrack {
        serde_json::from_value(serde_json::json!({
            "id": id, "title": title, "duration": 200,
            "dateAdded": added,
            "audioQuality": "LOSSLESS",
            "artists": [{"id": 1, "name": "Ann"}],
            "album": {"id": 5, "title": "Alb", "releaseDate": "1999-04-01"},
        }))
        .expect("track")
    }

    #[test]
    fn universe_merges_loved_and_playlists_by_track() {
        let loved = vec![
            t(1, "One", Some("2024-02-29T10:00:00.000+0000")),
            t(2, "Two", None),
        ];
        let pls = vec![(
            "Gym".to_string(),
            vec![t(2, "Two", None), t(3, "Three", None)],
        )];
        let (tracks, cands) = build_universe(loved, pls);
        assert_eq!(tracks.iter().map(|t| t.id).collect::<Vec<_>>(), [1, 2, 3]);
        assert!(cands[0].loved && cands[1].loved && !cands[2].loved);
        assert_eq!(cands[0].added_day, Some(19_782));
        assert_eq!(cands[1].playlists, ["Gym"]);
        assert_eq!(cands[2].playlists, ["Gym"]);
        assert_eq!(cands[0].year, Some(1999));
        assert_eq!(cands[0].artists, ["Ann"]);
    }

    #[test]
    fn rules_run_over_a_built_universe() {
        let loved = vec![
            t(1, "One", Some("2024-02-29")),
            t(2, "Two", Some("2020-01-01")),
        ];
        let (_, cands) = build_universe(loved, vec![]);
        let mut l = SmartPlaylist::new("a", "A");
        l.rules = vec![
            Rule {
                field: Field::Added,
                op: Op::Lt,
                value: "60".into(),
            },
            Rule {
                field: Field::Quality,
                op: Op::Is,
                value: "lossless".into(),
            },
        ];
        let today = smart::day_number("2024-03-15").expect("date");
        assert_eq!(smart::evaluate(&l, &cands, today), vec![0]);
    }
}
