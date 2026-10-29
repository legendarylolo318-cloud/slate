//! Sort order for library lists. Track lists (Loved, playlists) are paged, so
//! TIDAL sorts them (`order` / `orderDirection`); the album, artist and mix
//! grids are fetched whole, so they are sorted here and need no API values
//! beyond the default date-added order.

use crate::pages::CardData;
use crate::uiprefs::{SortPref, UiPrefs};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum List {
    Loved,
    Playlist,
    Albums,
    Artists,
    Mixes,
}

/// `order` is TIDAL's value for track lists, and a local key for grids.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Choice {
    pub order: &'static str,
    pub label: &'static str,
}

const fn c(order: &'static str, label: &'static str) -> Choice {
    Choice { order, label }
}

const LOVED: &[Choice] = &[
    c("DATE", "Date added"),
    c("NAME", "Title"),
    c("ARTIST", "Artist"),
    c("ALBUM", "Album"),
    c("LENGTH", "Duration"),
];
const PLAYLIST: &[Choice] = &[
    c("INDEX", "Playlist order"),
    c("DATE", "Date added"),
    c("NAME", "Title"),
    c("ARTIST", "Artist"),
    c("ALBUM", "Album"),
    c("LENGTH", "Duration"),
];
const ALBUMS: &[Choice] = &[
    c("DATE", "Date added"),
    c("NAME", "Title"),
    c("ARTIST", "Artist"),
];
const NAMED: &[Choice] = &[c("DATE", "Date added"), c("NAME", "Name")];

impl List {
    pub fn key(self) -> &'static str {
        match self {
            Self::Loved => "loved",
            Self::Playlist => "playlist",
            Self::Albums => "albums",
            Self::Artists => "artists",
            Self::Mixes => "mixes",
        }
    }

    pub fn choices(self) -> &'static [Choice] {
        match self {
            Self::Loved => LOVED,
            Self::Playlist => PLAYLIST,
            Self::Albums => ALBUMS,
            Self::Artists | Self::Mixes => NAMED,
        }
    }

    /// What each list showed before sorting existed: newest first, playlists
    /// in their own order.
    fn default_pref(self) -> SortPref {
        match self {
            Self::Playlist => SortPref {
                order: "INDEX".into(),
                desc: false,
            },
            _ => SortPref {
                order: "DATE".into(),
                desc: true,
            },
        }
    }

    pub fn current(self, prefs: &UiPrefs) -> (Choice, bool) {
        let pref = prefs
            .sorts
            .get(self.key())
            .cloned()
            .unwrap_or_else(|| self.default_pref());
        match self.choices().iter().find(|c| c.order == pref.order) {
            Some(choice) => (*choice, pref.desc),
            None => {
                let d = self.default_pref();
                let first = self.choices()[0];
                (
                    self.choices()
                        .iter()
                        .copied()
                        .find(|c| c.order == d.order)
                        .unwrap_or(first),
                    d.desc,
                )
            }
        }
    }

    /// The next order in the list (direction reset to that order's natural one).
    pub fn cycled(self, prefs: &UiPrefs) -> SortPref {
        let (now, _) = self.current(prefs);
        let all = self.choices();
        let i = all.iter().position(|c| *c == now).unwrap_or(0);
        let next = all[(i + 1) % all.len()];
        // Dates read newest first; names and playlist order A→Z.
        SortPref {
            order: next.order.into(),
            desc: next.order == "DATE",
        }
    }

    pub fn flipped(self, prefs: &UiPrefs) -> SortPref {
        let (now, desc) = self.current(prefs);
        SortPref {
            order: now.order.into(),
            desc: !desc,
        }
    }

    pub fn label(self, prefs: &UiPrefs) -> String {
        self.current(prefs).0.label.to_string()
    }

    /// `order` / `orderDirection` for a TIDAL track-list request; `None` keeps
    /// the server's default (playlists in playlist order).
    pub fn api_params(self, prefs: &UiPrefs) -> Option<(String, String)> {
        let (choice, desc) = self.current(prefs);
        if self == Self::Playlist && choice.order == "INDEX" && !desc {
            return None;
        }
        Some((
            choice.order.into(),
            if desc { "DESC" } else { "ASC" }.into(),
        ))
    }
}

/// Sorts grid cards that arrived in TIDAL's default order (newest first).
pub fn sort_cards(cards: &mut [CardData], choice: Choice, desc: bool) {
    let key = |s: &str| s.to_lowercase();
    match choice.order {
        "DATE" => {
            if !desc {
                cards.reverse();
            }
            return;
        }
        "ARTIST" => cards.sort_by(|a, b| {
            key(&a.subtitle)
                .cmp(&key(&b.subtitle))
                .then_with(|| key(&a.title).cmp(&key(&b.title)))
        }),
        _ => cards.sort_by_key(|a| key(&a.title)),
    }
    if desc {
        cards.reverse();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(title: &str, subtitle: &str) -> CardData {
        CardData {
            kind: "album",
            id: title.into(),
            title: title.into(),
            subtitle: subtitle.into(),
            image: None,
        }
    }

    fn titles(cards: &[CardData]) -> Vec<&str> {
        cards.iter().map(|c| c.title.as_str()).collect()
    }

    #[test]
    fn defaults_match_the_unsorted_behaviour() {
        let p = UiPrefs::default();
        assert_eq!(
            List::Loved.api_params(&p),
            Some(("DATE".into(), "DESC".into()))
        );
        assert_eq!(List::Playlist.api_params(&p), None);
        assert_eq!(List::Albums.label(&p), "Date added");
    }

    #[test]
    fn cycling_walks_every_choice_and_wraps() {
        let mut p = UiPrefs::default();
        let mut seen = Vec::new();
        for _ in 0..List::Loved.choices().len() {
            let next = List::Loved.cycled(&p);
            seen.push(next.order.clone());
            p.sorts.insert("loved".into(), next);
        }
        assert_eq!(seen, ["NAME", "ARTIST", "ALBUM", "LENGTH", "DATE"]);
        assert_eq!(
            List::Loved.api_params(&p),
            Some(("DATE".into(), "DESC".into()))
        );
    }

    #[test]
    fn flipping_changes_only_the_direction() {
        let mut p = UiPrefs::default();
        p.sorts
            .insert("playlist".into(), List::Playlist.flipped(&p));
        assert_eq!(
            List::Playlist.api_params(&p),
            Some(("INDEX".into(), "DESC".into()))
        );
    }

    #[test]
    fn an_unknown_saved_order_falls_back_to_the_default() {
        let mut p = UiPrefs::default();
        p.sorts.insert(
            "artists".into(),
            SortPref {
                order: "BOGUS".into(),
                desc: false,
            },
        );
        assert_eq!(List::Artists.current(&p), (NAMED[0], true));
    }

    #[test]
    fn cards_sort_by_title_artist_and_reverse_date() {
        let newest_first = || vec![card("b", "Zed"), card("C", "amy"), card("a", "Zed")];
        let mut v = newest_first();
        sort_cards(&mut v, ALBUMS[1], false);
        assert_eq!(titles(&v), ["a", "b", "C"]);
        sort_cards(&mut v, ALBUMS[1], true);
        assert_eq!(titles(&v), ["C", "b", "a"]);
        let mut v = newest_first();
        sort_cards(&mut v, ALBUMS[2], false);
        assert_eq!(titles(&v), ["C", "a", "b"]);
        let mut v = newest_first();
        sort_cards(&mut v, ALBUMS[0], false);
        assert_eq!(titles(&v), ["a", "C", "b"]);
        let mut v = newest_first();
        sort_cards(&mut v, ALBUMS[0], true);
        assert_eq!(titles(&v), ["b", "C", "a"]);
    }
}
