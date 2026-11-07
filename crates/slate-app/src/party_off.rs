//! Built without the `party` feature: the Party page explains that, and every playback hook
//! is empty.

use slint::ComponentHandle;

use crate::backend::Backend;
use crate::bootstrap::{AppWindow, SettingsState};

const OFF: &str = "This build of Slate has no listening party.";

pub fn is_guest() -> bool {
    false
}

pub fn hosting() -> bool {
    false
}

impl Backend {
    fn party_off(&self) {
        self.ui(|w| w.global::<SettingsState>().set_party_status(OFF.into()));
    }

    pub fn party_start(&self) {
        self.party_off();
    }

    pub fn party_join(&self, _ticket: &str) {
        self.party_off();
    }

    pub fn party_end(&self) {}

    pub fn party_kick(&self, _name: &str) {}

    pub fn party_cohost(&self, _name: &str, _on: bool) {}

    pub fn party_decide_id(&self, _id: &str, _approve: bool) {}

    pub fn party_decide_all(&self, _approve: bool) {}

    pub(crate) fn party_mcp_decide(&self, _request_id: u64, _approve: bool) {}

    pub fn party_takeover(&self) {}

    pub fn party_decline(&self) {}

    pub fn party_suggest(&self, _track_id: u64) {}

    pub fn party_vote(&self) {}

    pub fn party_react(&self, _index: i32) {}

    pub fn party_chat_send(&self, _text: String) {}

    pub fn party_chat_mute(&self, _name: &str, _muted: bool) {}

    pub fn party_pref_int(&self, _key: &str, _value: i32) {}

    pub fn party_pref_bool(&self, _key: &str, _on: bool) {}

    pub(crate) fn party_track_changed(&self, _track: &slate_core::queue::Track, _heard: f32) {}

    pub fn party_offer_save_set(&self) {}

    pub fn party_save_set(&self, _name: &str) {}

    pub(crate) fn party_state_changed(&self, _position_secs: f32, _playing: bool) {}

    pub(crate) async fn party_order_queue(&self, _ctl: &slate_core::controller::Controller) {}

    pub(crate) fn party_queue_changed(&self, _upcoming: &[u64]) {}

    pub fn party_link_opened(&self, _ticket: String) {
        self.party_off();
    }
}

/// Settings → Party fields. UI thread.
pub fn fill(w: &AppWindow) {
    w.global::<SettingsState>().set_party_status(OFF.into());
}
