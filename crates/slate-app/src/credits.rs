//! The "Credits" panel of a track: producers, writers, engineers and musicians
//! from TIDAL's credits endpoint (one request, cached by the core).

use std::sync::atomic::{AtomicU64, Ordering};

use slate_core::commands::metadata;
use slate_core::queue::Track;
use slate_core::tidal_api::TidalCredit;

use crate::backend::Backend;
use crate::bootstrap::KeyValue;

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

/// Rows for the panel: a heading row (empty value) per group, then one row per
/// role with its people. Roles listed twice are merged; names are not repeated.
pub fn credit_rows(credits: &[TidalCredit]) -> Vec<(String, String)> {
    let mut roles: Vec<(Group, String, Vec<String>)> = Vec::new();
    for credit in credits {
        let role = credit.credit_type.trim();
        if role.is_empty() {
            continue;
        }
        let names = credit
            .contributors
            .iter()
            .map(|c| c.name.trim())
            .filter(|n| !n.is_empty());
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
        for name in names {
            if !roles[slot].2.iter().any(|n| n == name) {
                roles[slot].2.push(name.to_string());
            }
        }
    }
    roles.retain(|(_, _, names)| !names.is_empty());
    roles.sort_by_key(|a| (a.0, a.1.to_lowercase()));
    let mut rows = Vec::new();
    let mut current = None;
    for (group, role, names) in roles {
        if current != Some(group) {
            current = Some(group);
            rows.push((group.heading().to_string(), String::new()));
        }
        rows.push((role, names.join(", ")));
    }
    rows
}

impl Backend {
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
                    let rows = credit_rows(&credits);
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
        rows: Vec<(String, String)>,
    ) {
        self.ui(move |w| {
            // A late answer must not reopen a panel the user has closed.
            if REQUEST.load(Ordering::SeqCst) != request || !(open || w.get_credits_open()) {
                return;
            }
            let model: Vec<KeyValue> = rows
                .into_iter()
                .map(|(key, value)| KeyValue {
                    key: key.into(),
                    value: value.into(),
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

    #[test]
    fn roles_are_grouped_in_a_fixed_order() {
        let rows = credit_rows(&[
            credit("Guitar", &["Ann"]),
            credit("Mastering Engineer", &["Bo"]),
            credit("Lyricist", &["Cy"]),
            credit("Producer", &["Di"]),
            credit("Mixer", &["Ed"]),
        ]);
        let keys: Vec<&str> = rows.iter().map(|(k, _)| k.as_str()).collect();
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
    }

    #[test]
    fn repeated_roles_merge_and_names_are_not_repeated() {
        let rows = credit_rows(&[
            credit("Producer", &["Ann", "Bo"]),
            credit("producer", &["Bo", "Cy"]),
        ]);
        assert_eq!(rows[1], ("Producer".to_string(), "Ann, Bo, Cy".to_string()));
    }

    #[test]
    fn roles_without_people_and_blank_names_are_dropped() {
        assert!(credit_rows(&[credit("Vocals", &[]), credit("Drums", &["  "])]).is_empty());
        assert!(credit_rows(&[]).is_empty());
        assert!(credit_rows(&[credit("", &["X"])]).is_empty());
    }
}
