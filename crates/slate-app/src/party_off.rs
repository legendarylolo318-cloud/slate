//! Built without the `party` feature: the Party page explains that, and every playback hook
//! is empty.

use slint::ComponentHandle;

use crate::backend::Backend;
use crate::bootstrap::{AppWindow, SettingsState};

const OFF: &str = "This build of Slate has no listening party.";

pub fn hosting() -> bool {
    false
}

impl Backend {
    fn party_off(&self) {
        self.ui(|w| w.global::<SettingsState>().set_party_status(OFF.into()));
    }

    pub fn party_start(&self, _allow_suggest: bool) {
        self.party_off();
    }

    pub fn party_join(&self, _ticket: &str) {
        self.party_off();
    }

    pub fn party_end(&self) {}

    pub fn party_kick(&self, _name: &str) {}

    pub fn party_decide(&self, _index: usize, _approve: bool) {}

    pub(crate) fn party_track_changed(&self, _id: u64) {}

    pub(crate) fn party_state_changed(&self, _position_secs: f32, _playing: bool) {}

    pub(crate) fn party_queue_changed(&self, _upcoming: &[u64]) {}

    pub fn party_link_opened(&self, _ticket: String) {
        self.party_off();
    }
}

/// Settings → Party fields. UI thread.
pub fn fill(w: &AppWindow) {
    w.global::<SettingsState>().set_party_status(OFF.into());
}
