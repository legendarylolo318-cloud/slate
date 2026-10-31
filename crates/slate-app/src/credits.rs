//! The "Credits" panel of a track: producers, writers, engineers and musicians
//! from TIDAL's credits endpoint (one request, cached by the core).

use std::sync::atomic::{AtomicU64, Ordering};

use slate_core::commands::metadata;
use slate_core::queue::Track;
use slate_core::tidal_api::TidalCredit;

use crate::backend::Backend;
use crate::bootstrap::CreditLine;
use crate::pages::Route;

/// Bumped for every request, so a slow answer for a track the user has moved
/// on from is dropped.
static REQUEST: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Group {
    Producers,
    Writers,
    Engineers,
    Musicians,
}

impl Group {
    fn heading(self) -> &'static str {
        match self {
            Self::Producers => "PRODUCERS",
            Self::Writers => "WRITERS",
            Self::Engineers => "ENGINEERS",
            Self::Musicians => "MUSICIANS",
        }
    }
}

fn group_of(role: &str) -> Group {
    let r = role.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| r.contains(w));
    if has(&["producer", "production"]) {
        Group::Producers
    } else if has(&[
        "writer",
        "composer",
        "lyricist",
        "songwriter",
        "author",
        "arranger",
    ]) {
        Group::Writers
    } else if has(&[
        "engineer",
        "mixer",
        "mixing",
        "mastering",
        "recording",
        "programmer",
        "programming",
    ]) {
        Group::Engineers
    } else {
        Group::Musicians
    }
}

/// One line of the panel: a group heading, or one person in one role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub heading: bool,
    pub role: String,
    pub name: String,
    /// TIDAL artist id of the person, when TIDAL gives one.
    pub artist_id: Option<u64>,
}

/// Lines for the panel: a heading per group, then one line per person per role. Roles listed
/// twice are merged; a person is not repeated within a role, and keeps the first artist id
/// TIDAL gave for them.
pub fn credit_lines(credits: &[TidalCredit]) -> Vec<Line> {
    type Person = (String, Option<u64>);
    let mut roles: Vec<(Group, String, Vec<Person>)> = Vec::new();
    for credit in credits {
        let role = credit.credit_type.trim();
        if role.is_empty() {
            continue;
        }
        let slot = match roles
            .iter()
            .position(|(_, r, _)| r.eq_ignore_ascii_case(role))
        {
            Some(i) => i,
            None => {
                roles.push((group_of(role), role.to_string(), Vec::new()));
                roles.len() - 1
            }
        };
        for c in &credit.contributors {
            let name = c.name.trim();
            if name.is_empty() {
                continue;
            }
            match roles[slot].2.iter_mut().find(|(n, _)| n == name) {
                Some(existing) => {
                    if existing.1.is_none() {
                        existing.1 = c.id;
                    }
                }
                None => roles[slot].2.push((name.to_string(), c.id)),
            }
        }
    }
    roles.retain(|(_, _, people)| !people.is_empty());
    roles.sort_by_key(|a| (a.0, a.1.to_lowercase()));
    let mut lines = Vec::new();
    let mut current = None;
    for (group, role, people) in roles {
        if current != Some(group) {
            current = Some(group);
            lines.push(Line {
                heading: true,
                role: group.heading().to_string(),
                name: String::new(),
                artist_id: None,
            });
        }
        for (i, (name, artist_id)) in people.into_iter().enumerate() {
            lines.push(Line {
                heading: false,
                // The role is written once, beside its first person.
                role: if i == 0 { role.clone() } else { String::new() },
                name,
                artist_id,
            });
        }
    }
    lines
}

/// Where clicking a credited person goes: their artist page when TIDAL gives an id (their
/// own releases are listed there), otherwise a search for the name.
pub fn credit_route(artist_id: &str, name: &str) -> Option<Route> {
    if let Ok(id) = artist_id.parse::<u64>() {
        if id != 0 {
            return Some(Route::Artist(id));
        }
    }
    let name = name.trim();
    (!name.is_empty()).then(|| Route::Search(name.to_string()))
}

impl Backend {
    /// A person in the credits panel was clicked.
    pub fn credit_clicked(&self, artist_id: &str, name: &str) {
        match credit_route(artist_id, name) {
            Some(r) => self.navigate(r),
            None => self.notify("No page for this credit"),
        }
    }

    /// Row action "Credits": opens the panel for `track`.
    pub(crate) fn show_credits(&self, track: &Track) {
        let request = REQUEST.fetch_add(1, Ordering::SeqCst) + 1;
        let title = format!("Credits · {}", track.title());
        let id = track.id;
        self.show_credit_rows(request, true, title.clone(), "Loading…".into(), Vec::new());
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            if REQUEST.load(Ordering::SeqCst) != request {
                return;
            }
            let (status, rows) = match metadata::get_track_credits(&state, id).await {
                Ok(credits) => {
                    let rows = credit_lines(&credits);
                    if rows.is_empty() {
                        ("TIDAL has no credits for this track.".to_string(), rows)
                    } else {
                        (String::new(), rows)
                    }
                }
                Err(e) => (
                    format!("Could not load the credits: {}", e.log_safe()),
                    Vec::new(),
                ),
            };
            this.show_credit_rows(request, false, title, status, rows);
        });
    }

    fn show_credit_rows(
        &self,
        request: u64,
        open: bool,
        title: String,
        status: String,
        rows: Vec<Line>,
    ) {
        self.ui(move |w| {
            // A late answer must not reopen a panel the user has closed.
            if REQUEST.load(Ordering::SeqCst) != request || !(open || w.get_credits_open()) {
                return;
            }
            let model: Vec<CreditLine> = rows
                .into_iter()
                .map(|l| CreditLine {
                    heading: l.heading,
                    role: l.role.into(),
                    name: l.name.into(),
                    artist_id: l
                        .artist_id
                        .map(|i| i.to_string())
                        .unwrap_or_default()
                        .into(),
                })
                .collect();
            w.set_credits_title(title.into());
            w.set_credits_status(status.into());
            w.set_credits_rows(slint::ModelRc::new(slint::VecModel::from(model)));
            w.set_credits_open(true);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slate_core::tidal_api::TidalContributor;

    fn credit(role: &str, names: &[&str]) -> TidalCredit {
        TidalCredit {
            credit_type: role.into(),
            contributors: names
                .iter()
                .map(|n| TidalContributor {
                    name: (*n).into(),
                    id: None,
                })
                .collect(),
        }
    }

    fn people(lines: &[Line]) -> Vec<(String, String)> {
        lines
            .iter()
            .map(|l| (l.role.clone(), l.name.clone()))
            .collect()
    }

    #[test]
    fn roles_are_grouped_in_a_fixed_order() {
        let lines = credit_lines(&[
            credit("Guitar", &["Ann"]),
            credit("Mastering Engineer", &["Bo"]),
            credit("Lyricist", &["Cy"]),
            credit("Producer", &["Di"]),
            credit("Mixer", &["Ed"]),
        ]);
        let keys: Vec<&str> = lines.iter().map(|l| l.role.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "PRODUCERS",
                "Producer",
                "WRITERS",
                "Lyricist",
                "ENGINEERS",
                "Mastering Engineer",
                "Mixer",
                "MUSICIANS",
                "Guitar"
            ]
        );
        assert!(lines[0].heading && !lines[1].heading);
    }

    #[test]
    fn repeated_roles_merge_each_person_gets_a_line_and_ids_are_kept() {
        let mut a = credit("Producer", &["Ann", "Bo"]);
        a.contributors[0].id = None;
        let mut b = credit("producer", &["Ann", "Cy"]);
        b.contributors[0].id = Some(7);
        let lines = credit_lines(&[a, b]);
        assert_eq!(
            people(&lines[1..]),
            vec![
                ("Producer".to_string(), "Ann".to_string()),
                (String::new(), "Bo".to_string()),
                (String::new(), "Cy".to_string()),
            ]
        );
        assert_eq!(
            lines[1].artist_id,
            Some(7),
            "the id TIDAL gave later is kept"
        );
    }

    #[test]
    fn roles_without_people_and_blank_names_are_dropped() {
        assert!(credit_lines(&[credit("Vocals", &[]), credit("Drums", &["  "])]).is_empty());
        assert!(credit_lines(&[]).is_empty());
        assert!(credit_lines(&[credit("", &["X"])]).is_empty());
    }

    #[test]
    fn a_credited_person_opens_their_artist_page_or_a_search() {
        assert!(matches!(credit_route("42", "Ann"), Some(Route::Artist(42))));
        assert!(matches!(credit_route("", " Ann B "), Some(Route::Search(q)) if q == "Ann B"));
        assert!(matches!(credit_route("0", "Ann"), Some(Route::Search(_))));
        assert!(credit_route("", "  ").is_none());
    }
}
