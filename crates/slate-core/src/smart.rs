//! Smart playlists: saved rules evaluated locally against library data when opened. Pure
//! logic plus the on-disk list; fetching the library data is the app's job.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::list_filter::fold;

pub const MAX_RULES: usize = 12;
pub const MAX_LISTS: usize = 50;
pub const MAX_LIMIT: u32 = 5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Field {
    Loved,
    Artist,
    Album,
    Quality,
    Explicit,
    Duration,
    Year,
    Added,
    Playlist,
}

pub const FIELDS: [Field; 9] = [
    Field::Loved,
    Field::Artist,
    Field::Album,
    Field::Quality,
    Field::Explicit,
    Field::Duration,
    Field::Year,
    Field::Added,
    Field::Playlist,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    Is,
    IsNot,
    Contains,
    Gt,
    Lt,
}

impl Field {
    pub fn label(self) -> &'static str {
        match self {
            Self::Loved => "Loved",
            Self::Artist => "Artist",
            Self::Album => "Album",
            Self::Quality => "Quality",
            Self::Explicit => "Explicit",
            Self::Duration => "Duration (minutes)",
            Self::Year => "Release year",
            Self::Added => "Added to Loved (days ago)",
            Self::Playlist => "In playlist",
        }
    }

    pub fn ops(self) -> &'static [Op] {
        match self {
            Self::Loved | Self::Explicit => &[Op::Is],
            Self::Artist | Self::Album | Self::Playlist => &[Op::Contains, Op::Is, Op::IsNot],
            Self::Quality => &[Op::Is, Op::IsNot],
            Self::Duration | Self::Added => &[Op::Lt, Op::Gt],
            Self::Year => &[Op::Is, Op::Gt, Op::Lt],
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Loved | Self::Explicit => "yes or no",
            Self::Quality => "lossless, hi-res or atmos",
            Self::Duration => "minutes, e.g. 4.5",
            Self::Year => "e.g. 1999",
            Self::Added => "days, e.g. 30",
            Self::Playlist => "playlist name",
            Self::Artist | Self::Album => "text",
        }
    }
}

impl Op {
    pub fn label(self, field: Field) -> &'static str {
        match (self, field) {
            (Self::Is, _) => "is",
            (Self::IsNot, _) => "is not",
            (Self::Contains, _) => "contains",
            (Self::Gt, Field::Year) => "after",
            (Self::Lt, Field::Year) => "before",
            (Self::Gt, Field::Duration) => "longer than",
            (Self::Lt, Field::Duration) => "shorter than",
            (Self::Gt, _) => "more than",
            (Self::Lt, _) => "less than",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub field: Field,
    pub op: Op,
    pub value: String,
}

impl Rule {
    pub fn new(field: Field) -> Self {
        Self {
            field,
            op: field.ops()[0],
            value: String::new(),
        }
    }

    /// Why the rule cannot be used, in words for the user.
    pub fn validate(&self) -> Result<(), String> {
        let v = self.value.trim();
        let ok = match self.field {
            Field::Loved | Field::Explicit => yes_no(v).is_some(),
            Field::Quality => quality_of(v).is_some(),
            Field::Duration => v.parse::<f64>().is_ok_and(|n| n >= 0.0 && n.is_finite()),
            Field::Year => v.parse::<i32>().is_ok_and(|y| (1900..=2100).contains(&y)),
            Field::Added => v.parse::<u32>().is_ok(),
            Field::Artist | Field::Album | Field::Playlist => !v.is_empty(),
        };
        if !self.field.ops().contains(&self.op) {
            return Err(format!(
                "“{}” does not work with {}.",
                self.op.label(self.field),
                self.field.label()
            ));
        }
        if ok {
            Ok(())
        } else {
            Err(format!(
                "{}: enter {}.",
                self.field.label(),
                self.field.hint()
            ))
        }
    }
}

fn yes_no(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "yes" | "y" | "true" => Some(true),
        "no" | "n" | "false" => Some(false),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Lossless,
    HiRes,
    Atmos,
}

fn quality_of(v: &str) -> Option<Level> {
    match v.to_ascii_lowercase().replace(['-', ' ', '_'], "").as_str() {
        "lossless" => Some(Level::Lossless),
        "hires" | "max" => Some(Level::HiRes),
        "atmos" | "dolbyatmos" => Some(Level::Atmos),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortKey {
    Added,
    Title,
    Artist,
    Album,
    Duration,
    Year,
}

pub const SORTS: [SortKey; 6] = [
    SortKey::Added,
    SortKey::Title,
    SortKey::Artist,
    SortKey::Album,
    SortKey::Duration,
    SortKey::Year,
];

impl SortKey {
    pub fn label(self) -> &'static str {
        match self {
            Self::Added => "Date added to Loved",
            Self::Title => "Title",
            Self::Artist => "Artist",
            Self::Album => "Album",
            Self::Duration => "Duration",
            Self::Year => "Release year",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmartPlaylist {
    pub id: String,
    pub name: String,
    /// true: every rule must match (AND); false: any (OR).
    pub match_all: bool,
    pub rules: Vec<Rule>,
    pub sort: SortKey,
    pub descending: bool,
    /// 0: no limit.
    pub limit: u32,
}

impl SmartPlaylist {
    pub fn new(id: &str, name: &str) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            match_all: true,
            rules: vec![Rule::new(Field::Loved)],
            sort: SortKey::Added,
            descending: true,
            limit: 0,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Give the smart playlist a name.".into());
        }
        if self.rules.len() > MAX_RULES {
            return Err(format!("Use at most {MAX_RULES} rules."));
        }
        if self.limit > MAX_LIMIT {
            return Err(format!("The limit can be at most {MAX_LIMIT}."));
        }
        self.rules.iter().try_for_each(Rule::validate)
    }

    /// Titles of the playlists the rules refer to (their tracks must be loaded).
    pub fn referenced_playlists(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for r in self.rules.iter().filter(|r| r.field == Field::Playlist) {
            let t = r.value.trim().to_string();
            if !t.is_empty() && !out.iter().any(|o| fold(o) == fold(&t)) {
                out.push(t);
            }
        }
        out
    }
}

/// One track as the rules see it.
#[derive(Debug, Clone, Default)]
pub struct Cand {
    pub title: String,
    pub artists: Vec<String>,
    pub album: String,
    /// 0 none, 1 Lossless, 2 Hi-Res.
    pub tier: i32,
    pub atmos: bool,
    pub explicit: bool,
    pub duration_secs: u32,
    pub year: Option<i32>,
    /// Day number (days since 1970-01-01) the track was added to Loved.
    pub added_day: Option<i64>,
    pub loved: bool,
    /// Titles of the loaded playlists that contain it.
    pub playlists: Vec<String>,
}

fn text_op(op: Op, haystacks: &[String], needle: &str) -> bool {
    let n = fold(needle.trim());
    let hit = |f: fn(&str, &str) -> bool| haystacks.iter().any(|h| f(&fold(h), &n));
    match op {
        Op::Contains => hit(|h, n| h.contains(n)),
        Op::Is => hit(|h, n| h == n),
        Op::IsNot => !hit(|h, n| h == n),
        _ => false,
    }
}

fn num_op(op: Op, have: f64, want: f64) -> bool {
    match op {
        Op::Is => (have - want).abs() < 1e-9,
        Op::Gt => have > want,
        Op::Lt => have < want,
        _ => false,
    }
}

pub fn rule_matches(r: &Rule, c: &Cand, today: i64) -> bool {
    let v = r.value.trim();
    match r.field {
        Field::Loved => yes_no(v).is_some_and(|w| c.loved == w),
        Field::Explicit => yes_no(v).is_some_and(|w| c.explicit == w),
        Field::Artist => text_op(r.op, &c.artists, v),
        Field::Album => text_op(r.op, std::slice::from_ref(&c.album), v),
        Field::Playlist => text_op(r.op, &c.playlists, v),
        Field::Quality => {
            let Some(level) = quality_of(v) else {
                return false;
            };
            let has = match level {
                Level::Lossless => c.tier >= 1,
                Level::HiRes => c.tier >= 2,
                Level::Atmos => c.atmos,
            };
            match r.op {
                Op::Is => has,
                Op::IsNot => !has,
                _ => false,
            }
        }
        Field::Duration => v
            .parse::<f64>()
            .is_ok_and(|m| num_op(r.op, f64::from(c.duration_secs) / 60.0, m)),
        Field::Year => match (v.parse::<f64>(), c.year) {
            (Ok(y), Some(have)) => num_op(r.op, f64::from(have), y),
            _ => false,
        },
        Field::Added => match (v.parse::<f64>(), c.added_day) {
            (Ok(days), Some(day)) => num_op(r.op, (today - day) as f64, days),
            _ => false,
        },
    }
}

/// Indices into `cands` of the matches, sorted and limited. `today` is a day number.
pub fn evaluate(list: &SmartPlaylist, cands: &[Cand], today: i64) -> Vec<usize> {
    let mut hits: Vec<usize> = cands
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            if list.rules.is_empty() {
                return true;
            }
            let mut it = list.rules.iter().map(|r| rule_matches(r, c, today));
            if list.match_all {
                it.all(|m| m)
            } else {
                it.any(|m| m)
            }
        })
        .map(|(i, _)| i)
        .collect();
    let key = |i: &usize| -> (i64, String) {
        let c = &cands[*i];
        match list.sort {
            SortKey::Added => (c.added_day.unwrap_or(i64::MIN), String::new()),
            SortKey::Title => (0, fold(&c.title)),
            SortKey::Artist => (0, c.artists.first().map(|a| fold(a)).unwrap_or_default()),
            SortKey::Album => (0, fold(&c.album)),
            SortKey::Duration => (i64::from(c.duration_secs), String::new()),
            SortKey::Year => (i64::from(c.year.unwrap_or(0)), String::new()),
        }
    };
    // Stable, so ties keep library order.
    hits.sort_by_key(key);
    if list.descending {
        hits.reverse();
    }
    if list.limit > 0 {
        hits.truncate(list.limit as usize);
    }
    hits
}

// ---- dates ----

/// Days since 1970-01-01 of a `YYYY-MM-DD…` string.
pub fn day_number(date: &str) -> Option<i64> {
    let y: i64 = date.get(0..4)?.parse().ok()?;
    let m: i64 = date.get(5..7)?.parse().ok()?;
    let d: i64 = date.get(8..10)?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || date.get(4..5)? != "-" {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

pub fn year_of(date: &str) -> Option<i32> {
    date.get(0..4)?.parse().ok()
}

// ---- storage ----

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub lists: Vec<SmartPlaylist>,
}

impl Store {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(tmp, path)
    }

    pub fn get(&self, id: &str) -> Option<&SmartPlaylist> {
        self.lists.iter().find(|l| l.id == id)
    }

    pub fn upsert(&mut self, list: SmartPlaylist) -> Result<(), String> {
        list.validate()?;
        if let Some(slot) = self.lists.iter_mut().find(|l| l.id == list.id) {
            *slot = list;
            return Ok(());
        }
        if self.lists.len() >= MAX_LISTS {
            return Err("Too many smart playlists; delete one first.".into());
        }
        self.lists.push(list);
        Ok(())
    }

    pub fn delete(&mut self, id: &str) -> bool {
        let n = self.lists.len();
        self.lists.retain(|l| l.id != id);
        self.lists.len() != n
    }

    /// An id not in use.
    pub fn new_id(&self, seed: u64) -> String {
        (seed..)
            .map(|n| format!("sp{n:x}"))
            .find(|id| self.get(id).is_none())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(title: &str, artist: &str, album: &str) -> Cand {
        Cand {
            title: title.into(),
            artists: vec![artist.into()],
            album: album.into(),
            duration_secs: 240,
            year: Some(2000),
            loved: true,
            ..Cand::default()
        }
    }

    fn rule(field: Field, op: Op, value: &str) -> Rule {
        Rule {
            field,
            op,
            value: value.into(),
        }
    }

    fn list(rules: Vec<Rule>, all: bool) -> SmartPlaylist {
        SmartPlaylist {
            match_all: all,
            rules,
            ..SmartPlaylist::new("a", "A")
        }
    }

    #[test]
    fn day_numbers_match_the_calendar() {
        assert_eq!(day_number("1970-01-01"), Some(0));
        assert_eq!(day_number("2000-03-01"), Some(11_017));
        assert_eq!(day_number("2024-02-29T10:00:00.000+0000"), Some(19_782));
        assert_eq!(day_number("nope"), None);
        assert_eq!(day_number("2024-13-01"), None);
        assert_eq!(year_of("1999-05-05"), Some(1999));
    }

    #[test]
    fn and_or_semantics() {
        let cs = [cand("A", "Björk", "X"), cand("B", "Beyoncé", "Y")];
        let both = list(
            vec![
                rule(Field::Artist, Op::Contains, "bjork"),
                rule(Field::Album, Op::Is, "y"),
            ],
            true,
        );
        assert!(evaluate(&both, &cs, 0).is_empty());
        let either = SmartPlaylist {
            match_all: false,
            ..both
        };
        assert_eq!(evaluate(&either, &cs, 0).len(), 2);
        assert_eq!(evaluate(&list(vec![], true), &cs, 0).len(), 2);
    }

    #[test]
    fn each_field_matches() {
        let mut c = cand("T", "Ann", "Alb");
        c.tier = 2;
        c.atmos = false;
        c.explicit = true;
        c.added_day = Some(100);
        c.playlists = vec!["Road Trip".into()];
        let t = |r: Rule| rule_matches(&r, &c, 110);
        assert!(t(rule(Field::Loved, Op::Is, "yes")));
        assert!(!t(rule(Field::Loved, Op::Is, "no")));
        assert!(t(rule(Field::Quality, Op::Is, "Lossless")));
        assert!(t(rule(Field::Quality, Op::Is, "hi-res")));
        assert!(!t(rule(Field::Quality, Op::Is, "atmos")));
        assert!(t(rule(Field::Quality, Op::IsNot, "atmos")));
        assert!(t(rule(Field::Explicit, Op::Is, "yes")));
        assert!(t(rule(Field::Duration, Op::Gt, "3.5")));
        assert!(!t(rule(Field::Duration, Op::Lt, "4")));
        assert!(t(rule(Field::Year, Op::Is, "2000")));
        assert!(t(rule(Field::Year, Op::Lt, "2001")));
        assert!(t(rule(Field::Added, Op::Lt, "30")));
        assert!(!t(rule(Field::Added, Op::Gt, "30")));
        assert!(t(rule(Field::Playlist, Op::Is, "road trip")));
        assert!(t(rule(Field::Playlist, Op::Contains, "road")));
        assert!(!t(rule(Field::Playlist, Op::IsNot, "road trip")));
        // No date known: date rules never match.
        c.added_day = None;
        assert!(!rule_matches(&rule(Field::Added, Op::Lt, "30"), &c, 110));
    }

    #[test]
    fn sorting_descending_ties_and_limit() {
        let mut a = cand("b", "x", "");
        a.added_day = Some(1);
        let mut b = cand("a", "x", "");
        b.added_day = Some(3);
        let mut c = cand("c", "x", "");
        c.added_day = Some(2);
        let cs = [a, b, c];
        let mut l = list(vec![], true);
        assert_eq!(evaluate(&l, &cs, 0), vec![1, 2, 0]); // newest first
        l.sort = SortKey::Title;
        l.descending = false;
        assert_eq!(evaluate(&l, &cs, 0), vec![1, 0, 2]);
        l.limit = 2;
        assert_eq!(evaluate(&l, &cs, 0), vec![1, 0]);
    }

    #[test]
    fn validation_reports_the_problem() {
        assert!(rule(Field::Year, Op::Is, "abc").validate().is_err());
        assert!(rule(Field::Year, Op::Is, "1999").validate().is_ok());
        assert!(rule(Field::Loved, Op::Contains, "yes").validate().is_err());
        assert!(rule(Field::Quality, Op::Is, "vinyl").validate().is_err());
        assert!(rule(Field::Artist, Op::Contains, "  ").validate().is_err());
        let mut l = SmartPlaylist::new("a", " ");
        assert!(l.validate().is_err());
        l.name = "ok".into();
        l.rules = vec![rule(Field::Loved, Op::Is, "yes")];
        assert!(l.validate().is_ok());
        l.limit = MAX_LIMIT + 1;
        assert!(l.validate().is_err());
    }

    #[test]
    fn referenced_playlists_are_unique() {
        let l = list(
            vec![
                rule(Field::Playlist, Op::Is, "Road Trip"),
                rule(Field::Playlist, Op::Is, "road trip"),
                rule(Field::Playlist, Op::Is, "Gym"),
                rule(Field::Loved, Op::Is, "yes"),
            ],
            false,
        );
        assert_eq!(l.referenced_playlists(), vec!["Road Trip", "Gym"]);
    }

    #[test]
    fn store_crud_and_disk() {
        let mut s = Store::default();
        let id = s.new_id(1);
        let mut l = SmartPlaylist::new(&id, "Mine");
        l.rules = vec![rule(Field::Loved, Op::Is, "yes")];
        s.upsert(l.clone()).expect("valid");
        assert_ne!(s.new_id(1), id);
        l.name = "Renamed".into();
        s.upsert(l.clone()).expect("replace");
        assert_eq!(s.lists.len(), 1);
        assert!(s.upsert(SmartPlaylist::new("zz", "")).is_err());
        let dir = std::env::temp_dir().join(format!("slate-sp-{}", std::process::id()));
        let path = dir.join("smart.json");
        s.save(&path).expect("save");
        assert_eq!(Store::load(&path), s);
        assert!(s.delete(&id));
        assert!(!s.delete(&id));
        std::fs::write(&path, "garbage").expect("write");
        assert_eq!(Store::load(&path), Store::default());
        let _ = std::fs::remove_dir_all(dir);
    }
}
