//! Settings search: an index of every setting's label and hint, read from `settings.slint`
//! itself (so it can never go stale), and a case- and accent-insensitive search over it.

use slate_core::list_filter::fold;

const SOURCE: &str = include_str!("../ui/settings.slint");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub tab: usize,
    pub label: String,
    pub hint: String,
}

/// The names in the tab bar, in order.
pub fn tab_names(src: &str) -> Vec<String> {
    let Some(i) = src.find("property <[string]> tabs: [") else {
        return Vec::new();
    };
    let rest = &src[i + "property <[string]> tabs: [".len()..];
    let end = rest.find(']').unwrap_or(0);
    rest[..end]
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn literal_after<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let i = line.find(key)?;
    let rest = line[i + key.len()..].trim_start().strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(&rest[..end])
}

/// Every `Row { label: "…"; hint: "…"; }` with the tab it sits in.
pub fn index(src: &str) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    let mut tab = 0usize;
    for line in src.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("if SettingsState.tab == ") {
            if let Some(n) = rest
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .and_then(|n| n.parse().ok())
            {
                tab = n;
            }
            continue;
        }
        // Only rows (a label on its own line, not a Button's `label:` inside braces).
        if t.starts_with("label: \"") {
            if let Some(l) = literal_after(t, "label:") {
                out.push(Entry {
                    tab,
                    label: l.to_string(),
                    hint: String::new(),
                });
            }
        } else if t.starts_with("hint: \"") {
            if let (Some(h), Some(last)) = (literal_after(t, "hint:"), out.last_mut()) {
                if last.hint.is_empty() {
                    last.hint = h.to_string();
                }
            }
        } else if t.starts_with("label: ") && t.contains("hint:") {
            // `label: …; hint: "…";` on one line is not used; ignore.
        }
    }
    out
}

/// Entries whose label or hint contains every word of `query`, labels first.
pub fn search<'a>(entries: &'a [Entry], query: &str) -> Vec<&'a Entry> {
    let q = fold(query);
    let words: Vec<&str> = q.split_whitespace().collect();
    if words.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<(bool, &Entry)> = entries
        .iter()
        .filter_map(|e| {
            let label = fold(&e.label);
            let all = format!("{label} {}", fold(&e.hint));
            words
                .iter()
                .all(|w| all.contains(w))
                .then(|| (words.iter().all(|w| label.contains(w)), e))
        })
        .collect();
    hits.sort_by_key(|(in_label, _)| std::cmp::Reverse(*in_label));
    hits.into_iter().map(|(_, e)| e).collect()
}

/// "Playback › Crossfade".
pub fn describe(tabs: &[String], e: &Entry) -> String {
    format!(
        "{} › {}",
        tabs.get(e.tab).map_or("", String::as_str),
        e.label
    )
}

/// Runs a search and shows the result list. UI thread (it is a callback of the window).
pub fn apply(w: &slint::Weak<crate::bootstrap::AppWindow>, query: &str) {
    use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
    static CACHE: std::sync::OnceLock<(Vec<String>, Vec<Entry>)> = std::sync::OnceLock::new();
    let (tabs, entries) = CACHE.get_or_init(|| (tab_names(SOURCE), index(SOURCE)));
    let Some(w) = w.upgrade() else { return };
    let hits = search(entries, query);
    let g = w.global::<crate::bootstrap::SettingsState>();
    g.set_search_results(ModelRc::new(VecModel::from(
        hits.iter()
            .map(|e| SharedString::from(describe(tabs, e)))
            .collect::<Vec<_>>(),
    )));
    g.set_search_tabs(ModelRc::new(VecModel::from(
        hits.iter().map(|e| e.tab as i32).collect::<Vec<_>>(),
    )));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tab_bar_is_read_from_the_page() {
        let tabs = tab_names(SOURCE);
        assert!(tabs.len() >= 9);
        assert_eq!(tabs[0], "General");
        assert!(tabs.contains(&"Party".to_string()));
    }

    #[test]
    fn the_index_covers_the_real_settings() {
        let tabs = tab_names(SOURCE);
        let idx = index(SOURCE);
        assert!(idx.len() > 40, "{} entries", idx.len());
        let crossfade = idx
            .iter()
            .find(|e| e.label == "Crossfade")
            .expect("crossfade row");
        assert_eq!(tabs[crossfade.tab], "Playback");
        let party = idx
            .iter()
            .find(|e| e.label == "Listening party")
            .expect("party row");
        assert_eq!(tabs[party.tab], "Party");
        assert!(idx.iter().all(|e| e.tab < tabs.len()));
    }

    #[test]
    fn search_finds_by_label_and_hint_without_accents_or_case() {
        let idx = index(SOURCE);
        let tabs = tab_names(SOURCE);
        let r = search(&idx, "CROSSFADE");
        assert!(!r.is_empty());
        assert_eq!(describe(&tabs, r[0]), "Playback › Crossfade");
        assert!(search(&idx, "replaygain")
            .iter()
            .any(|e| e.label.contains("normal") || e.hint.contains("ReplayGain")));
        assert!(search(&idx, "zzzzqqq").is_empty());
        assert!(search(&idx, "   ").is_empty());
        // Label matches come before hint-only matches.
        let r = search(&idx, "gapless");
        assert!(fold(&r[0].label).contains("gapless"));
    }
}
