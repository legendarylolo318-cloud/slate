//! Multi-select in lists: plain click, Ctrl-click toggles, Shift-click selects a range from
//! the last anchor, select-all, clear. Indices only; what a row is lives with the list.

use std::collections::BTreeSet;

/// Which modifier was held for a click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modifier {
    None,
    Ctrl,
    Shift,
    /// Ctrl and Shift: the range is added to what is selected.
    CtrlShift,
}

impl Modifier {
    pub fn from_keys(ctrl: bool, shift: bool) -> Self {
        match (ctrl, shift) {
            (false, false) => Self::None,
            (true, false) => Self::Ctrl,
            (false, true) => Self::Shift,
            (true, true) => Self::CtrlShift,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Selection {
    anchor: Option<usize>,
    set: BTreeSet<usize>,
}

impl Selection {
    /// Empty; `const` so a selection can live in a `static`.
    pub const fn new() -> Self {
        Self {
            anchor: None,
            set: BTreeSet::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    pub fn len(&self) -> usize {
        self.set.len()
    }

    pub fn contains(&self, i: usize) -> bool {
        self.set.contains(&i)
    }

    /// Selected indices, ascending.
    pub fn indices(&self) -> Vec<usize> {
        self.set.iter().copied().collect()
    }

    pub fn clear(&mut self) {
        self.anchor = None;
        self.set.clear();
    }

    /// A click on row `i` of a list of `len` rows. Returns whether the click was a
    /// selecting one; a plain click is not (the caller plays the row) and drops the
    /// selection.
    pub fn click(&mut self, i: usize, modifier: Modifier, len: usize) -> bool {
        if i >= len {
            return false;
        }
        match modifier {
            Modifier::None => {
                self.clear();
                false
            }
            Modifier::Ctrl => {
                if !self.set.remove(&i) {
                    self.set.insert(i);
                }
                self.anchor = Some(i);
                true
            }
            Modifier::Shift | Modifier::CtrlShift => {
                let from = self.anchor.filter(|a| *a < len).unwrap_or(i);
                let (lo, hi) = (from.min(i), from.max(i));
                if modifier == Modifier::Shift {
                    self.set.clear();
                }
                self.set.extend(lo..=hi);
                // The anchor stays, so the range can be re-aimed with another Shift-click.
                self.anchor.get_or_insert(from);
                true
            }
        }
    }

    pub fn select_all(&mut self, len: usize) {
        self.set = (0..len).collect();
        self.anchor = if len > 0 { Some(0) } else { None };
    }

    /// Drops indices at or beyond `len` (the list got shorter).
    pub fn prune(&mut self, len: usize) {
        self.set.retain(|i| *i < len);
        if self.anchor.is_some_and(|a| a >= len) {
            self.anchor = None;
        }
    }

    /// After removing the rows in `removed` (ascending, all selected or not): the
    /// remaining selected rows shift up and the removed ones leave the selection.
    pub fn after_removal(&mut self, removed: &[usize]) {
        let shift = |i: usize| i - removed.iter().filter(|r| **r < i).count();
        self.set = self
            .set
            .iter()
            .filter(|i| !removed.contains(i))
            .map(|i| shift(*i))
            .collect();
        self.anchor = self.anchor.filter(|a| !removed.contains(a)).map(shift);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(items: &[usize]) -> Selection {
        let mut s = Selection::default();
        for i in items {
            s.click(*i, Modifier::Ctrl, 100);
        }
        s
    }

    #[test]
    fn ctrl_click_toggles() {
        let mut s = Selection::default();
        assert!(s.click(3, Modifier::Ctrl, 10));
        assert!(s.click(5, Modifier::Ctrl, 10));
        assert_eq!(s.indices(), vec![3, 5]);
        assert!(s.click(3, Modifier::Ctrl, 10));
        assert_eq!(s.indices(), vec![5]);
    }

    #[test]
    fn shift_click_selects_a_range_from_the_anchor_in_either_direction() {
        let mut s = Selection::default();
        s.click(4, Modifier::Ctrl, 20);
        s.click(7, Modifier::Shift, 20);
        assert_eq!(s.indices(), vec![4, 5, 6, 7]);
        // Re-aiming replaces the range, the anchor stays.
        s.click(2, Modifier::Shift, 20);
        assert_eq!(s.indices(), vec![2, 3, 4]);
    }

    #[test]
    fn shift_with_no_anchor_selects_just_that_row() {
        let mut s = Selection::default();
        s.click(6, Modifier::Shift, 20);
        assert_eq!(s.indices(), vec![6]);
    }

    #[test]
    fn ctrl_shift_adds_a_range() {
        let mut s = sel(&[0]);
        s.click(10, Modifier::Ctrl, 20);
        s.click(12, Modifier::CtrlShift, 20);
        assert_eq!(s.indices(), vec![0, 10, 11, 12]);
    }

    #[test]
    fn a_plain_click_clears_and_is_not_a_selecting_click() {
        let mut s = sel(&[1, 2]);
        assert!(!s.click(5, Modifier::None, 10));
        assert!(s.is_empty());
    }

    #[test]
    fn clicks_past_the_end_are_ignored() {
        let mut s = Selection::default();
        assert!(!s.click(10, Modifier::Ctrl, 10));
        assert!(s.is_empty());
    }

    #[test]
    fn select_all_and_clear() {
        let mut s = Selection::default();
        s.select_all(4);
        assert_eq!(s.indices(), vec![0, 1, 2, 3]);
        s.clear();
        assert!(s.is_empty());
        s.select_all(0);
        assert!(s.is_empty());
    }

    #[test]
    fn pruning_drops_rows_past_a_shorter_list() {
        let mut s = sel(&[1, 8, 9]);
        s.prune(9);
        assert_eq!(s.indices(), vec![1, 8]);
    }

    #[test]
    fn removal_shifts_the_rest_up() {
        let mut s = sel(&[2, 5, 7]);
        s.after_removal(&[2, 6]);
        assert_eq!(s.indices(), vec![4, 5]);
    }

    #[test]
    fn modifier_from_keys() {
        assert_eq!(Modifier::from_keys(true, true), Modifier::CtrlShift);
        assert_eq!(Modifier::from_keys(false, false), Modifier::None);
    }
}
