//! Listening party in the app: start / end as host, join / leave as guest, keeping a guest's
//! playback in step with the host, and the Settings → Party page. Nothing here exists until a
//! party is started or joined; ending one drops the network endpoint and every task with it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use slate_core::commands::metadata;
use slate_core::party::net::{Event, Guest, Host};
use slate_core::party::proto::{self, Correction, HostMsg};
use slate_core::queue::Track;
use slint::{ComponentHandle, Image, ModelRc, SharedString, VecModel};
use tokio::sync::mpsc;

use crate::backend::Backend;
use crate::bootstrap::{AppWindow, SettingsState};

/// One atomic read on the playback paths; true only while hosting.
static HOSTING: AtomicBool = AtomicBool::new(false);

enum Role {
    Host(Host),
    Guest(Arc<Guest>),
}

static ROLE: Mutex<Option<Role>> = Mutex::new(None);
/// Suggestions waiting for the host's decision: (guest, track id, title).
static SUGGESTIONS: Mutex<Vec<(String, u64, String)>> = Mutex::new(Vec::new());

pub fn hosting() -> bool {
    HOSTING.load(Ordering::Relaxed)
}

fn relay_pref() -> Option<String> {
    let r = crate::uiprefs::UiPrefs::load().party_relay;
    (!r.trim().is_empty()).then_some(r)
}

fn strings(v: Vec<String>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        v.into_iter().map(SharedString::from).collect::<Vec<_>>(),
    ))
}

impl Backend {
    fn party_status(&self, text: impl Into<String>) {
        let text = text.into();
        self.ui(move |w| w.global::<SettingsState>().set_party_status(text.into()));
    }

    fn party_show(&self, role: &'static str, link: String, guests: Vec<String>) {
        let qr = if link.is_empty() {
            None
        } else {
            crate::images::qr(&link)
        };
        self.ui(move |w| {
            let s = w.global::<SettingsState>();
            s.set_party_role(role.into());
            s.set_party_link(link.into());
            s.set_party_qr(qr.map(Image::from_rgba8).unwrap_or_default());
            s.set_party_guests(strings(guests));
        });
    }

    fn party_mirror(&self, active: bool, role: &str, guests: usize) {
        let Some(state) = self.state() else { return };
        if !state.mcp_active.load(Ordering::Relaxed) {
            return;
        }
        let v = serde_json::json!({ "active": active, "role": role, "guest_count": guests });
        self.rt.spawn(async move {
            state.mcp_state.write().await.party = Some(v);
        });
    }

    // ---- host ----

    pub fn party_start(&self, allow_suggest: bool) {
        if ROLE.lock().is_ok_and(|r| r.is_some()) {
            self.party_status("A party is already running.");
            return;
        }
        self.party_status("Starting the party…");
        let (tx, rx) = mpsc::unbounded_channel();
        let this = self.clone();
        self.rt.spawn(async move {
            match Host::start(relay_pref().as_deref(), "Host", allow_suggest, tx).await {
                Ok(host) => {
                    let link = host.link();
                    if let Ok(mut r) = ROLE.lock() {
                        *r = Some(Role::Host(host));
                    }
                    HOSTING.store(true, Ordering::Relaxed);
                    this.party_show("host", link, Vec::new());
                    this.party_status("Party started. Share the link or QR code.");
                    this.party_mirror(true, "host", 0);
                    this.host_sync_now();
                    this.pump_host_events(rx);
                }
                Err(e) => this.party_status(e),
            }
        });
    }

    fn pump_host_events(&self, mut rx: mpsc::UnboundedReceiver<Event>) {
        let this = self.clone();
        self.rt.spawn(async move {
            while let Some(e) = rx.recv().await {
                match e {
                    Event::GuestJoined(n) => this.party_status(format!("{n} joined")),
                    Event::GuestLeft(n) => this.party_status(format!("{n} left")),
                    Event::Suggestion { guest, track_id } => {
                        this.host_got_suggestion(guest, track_id)
                    }
                    Event::Closed(_) | Event::Host(_) => {}
                }
                let names = ROLE
                    .lock()
                    .ok()
                    .and_then(|r| match r.as_ref() {
                        Some(Role::Host(h)) => Some(h.guest_names()),
                        _ => None,
                    })
                    .unwrap_or_default();
                let (link, n) = (this.current_party_link(), names.len());
                this.party_show("host", link, names);
                this.party_mirror(true, "host", n);
            }
        });
    }

    fn current_party_link(&self) -> String {
        ROLE.lock()
            .ok()
            .and_then(|r| match r.as_ref() {
                Some(Role::Host(h)) => Some(h.link()),
                _ => None,
            })
            .unwrap_or_default()
    }

    fn host_got_suggestion(&self, guest: String, track_id: u64) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            let title = match metadata::get_track(&state, track_id).await {
                Ok(v) => serde_json::from_value::<Track>(v)
                    .map(|t| format!("{} – {}", t.artist_name(), t.title()))
                    .unwrap_or_else(|_| format!("track {track_id}")),
                Err(_) => return,
            };
            if let Ok(mut s) = SUGGESTIONS.lock() {
                if s.len() >= proto::MAX_PENDING_SUGGESTIONS {
                    return;
                }
                s.push((guest, track_id, title));
            }
            this.show_party_suggestions();
        });
    }

    fn show_party_suggestions(&self) {
        let lines: Vec<String> = SUGGESTIONS
            .lock()
            .map(|s| {
                s.iter()
                    .map(|(g, _, t)| format!("{g} suggests {t}"))
                    .collect()
            })
            .unwrap_or_default();
        self.ui(move |w| {
            w.global::<SettingsState>()
                .set_party_suggestions(strings(lines))
        });
    }

    /// Approve (add to the queue) or deny suggestion `index`.
    pub fn party_decide(&self, index: usize, approve: bool) {
        let picked = SUGGESTIONS
            .lock()
            .ok()
            .and_then(|mut s| (index < s.len()).then(|| s.remove(index)));
        if let Some((_, id, _)) = picked.filter(|_| approve) {
            self.queue_track_ids(vec![id]);
        }
        self.show_party_suggestions();
    }

    pub fn party_kick(&self, name: &str) {
        if let Ok(r) = ROLE.lock() {
            if let Some(Role::Host(h)) = r.as_ref() {
                h.kick(name);
            }
        }
    }

    // ---- hooks from playback (host) ----

    fn host_do(&self, f: impl FnOnce(&Host)) {
        if let Ok(r) = ROLE.lock() {
            if let Some(Role::Host(h)) = r.as_ref() {
                f(h);
            }
        }
    }

    /// The current track and play state, sent to guests (at party start).
    fn host_sync_now(&self) {
        let pos = f32::from_bits(self.shared.position.load(Ordering::Relaxed)).max(0.0);
        let playing = self.shared.playing.load(Ordering::Relaxed);
        if let Some(id) = self.current_track_id() {
            self.host_do(|h| h.track_changed(id, (pos * 1000.0) as u64, playing));
        }
    }

    pub(crate) fn party_track_changed(&self, id: u64) {
        if hosting() {
            self.host_do(|h| h.track_changed(id, 0, true));
        }
    }

    pub(crate) fn party_state_changed(&self, position_secs: f32, playing: bool) {
        if hosting() {
            self.host_do(|h| h.state_changed((position_secs.max(0.0) * 1000.0) as u64, playing));
        }
    }

    pub(crate) fn party_queue_changed(&self, upcoming: &[u64]) {
        if hosting() {
            self.host_do(|h| h.queue_changed(upcoming));
        }
    }

    // ---- guest ----

    pub fn party_join(&self, ticket: &str) {
        if ROLE.lock().is_ok_and(|r| r.is_some()) {
            self.party_status("Leave the current party first.");
            return;
        }
        let Some(ticket) = proto::parse_ticket(ticket) else {
            self.party_status("That is not a Slate party link.");
            return;
        };
        self.party_status("Joining…");
        let (tx, rx) = mpsc::unbounded_channel();
        let this = self.clone();
        self.rt.spawn(async move {
            match Guest::join(&ticket, "Guest", relay_pref().as_deref(), tx).await {
                Ok((guest, welcome)) => {
                    let guest = Arc::new(guest);
                    if let Ok(mut r) = ROLE.lock() {
                        *r = Some(Role::Guest(Arc::clone(&guest)));
                    }
                    let (host, guests) = match &welcome {
                        HostMsg::Welcome { host, guests, .. } => (host.clone(), guests.clone()),
                        _ => (String::new(), Vec::new()),
                    };
                    this.party_show("guest", String::new(), guests);
                    this.party_status(format!(
                        "Joined {host}'s party. Everyone plays from their own account."
                    ));
                    this.party_mirror(true, "guest", 0);
                    if let HostMsg::Welcome { state, .. } = welcome {
                        if let Some(id) = state.track {
                            let now = guest.host_now_ms();
                            let pos = proto::host_position_now(
                                state.position_ms,
                                state.playing,
                                state.at_ms,
                                now,
                            );
                            this.guest_play(id, pos, state.playing);
                        }
                    }
                    this.pump_guest_events(rx, guest);
                }
                Err(e) => this.party_status(e),
            }
        });
    }

    fn pump_guest_events(&self, mut rx: mpsc::UnboundedReceiver<Event>, guest: Arc<Guest>) {
        let this = self.clone();
        self.rt.spawn(async move {
            while let Some(e) = rx.recv().await {
                match e {
                    Event::Host(m) => this.guest_apply(&guest, m),
                    Event::Closed(reason) => {
                        if let Ok(mut r) = ROLE.lock() {
                            *r = None;
                        }
                        this.party_show("", String::new(), Vec::new());
                        this.party_status(reason);
                        this.party_mirror(false, "", 0);
                        return;
                    }
                    _ => {}
                }
            }
        });
    }

    fn guest_apply(&self, guest: &Guest, m: HostMsg) {
        let now = guest.host_now_ms();
        match m {
            HostMsg::Track {
                id,
                position_ms,
                playing,
                at_ms,
            } => {
                let pos = proto::host_position_now(position_ms, playing, at_ms, now);
                self.guest_play(id, pos, playing);
            }
            HostMsg::State {
                position_ms,
                playing,
                at_ms,
            }
            | HostMsg::Heartbeat {
                position_ms,
                playing,
                at_ms,
            } => {
                let target = proto::host_position_now(position_ms, playing, at_ms, now);
                self.guest_follow(target, playing);
            }
            HostMsg::Queue { next } => self.guest_prefetch(next),
            HostMsg::Guests { guests } => {
                let link = String::new();
                let n = guests.len();
                self.party_show("guest", link, guests);
                self.party_mirror(true, "guest", n);
            }
            _ => {}
        }
    }

    /// Plays the host's track from its position; skips with a notice if it is not available here.
    fn guest_play(&self, id: u64, position_ms: u64, playing: bool) {
        let this = self.clone();
        self.rt.spawn(async move {
            let (Some(state), Some(ctl)) = (this.state(), this.controller()) else {
                return;
            };
            let track = match metadata::get_track(&state, id).await {
                Ok(v) => serde_json::from_value::<Track>(v).ok(),
                Err(_) => None,
            };
            let Some(track) = track else {
                this.notify("A track in the party is not available to you; skipping it.");
                return;
            };
            let outcome = ctl
                .play_from_source(track.clone(), vec![track], None, false)
                .await;
            let failed = matches!(outcome, slate_core::controller::Outcome::Failed(_));
            this.apply(outcome).await;
            if failed {
                this.notify(
                    "That track is not available in your region; skipped until the next one.",
                );
                return;
            }
            if position_ms > 1500 {
                this.seek_to_secs(position_ms as f32 / 1000.0);
            }
            if !playing {
                this.guest_set_playing(false);
            }
        });
    }

    fn guest_set_playing(&self, want: bool) {
        if self.shared.playing.load(Ordering::Relaxed) != want {
            self.toggle_play();
        }
    }

    /// Pause / resume / seek to match the host; drift under 750 ms is left alone.
    fn guest_follow(&self, target_ms: u64, playing: bool) {
        self.guest_set_playing(playing);
        let local =
            (f32::from_bits(self.shared.position.load(Ordering::Relaxed)).max(0.0) * 1000.0) as u64;
        if let Correction::SeekTo(to) = proto::correction(local, target_ms) {
            self.seek_to_secs(to as f32 / 1000.0);
        }
    }

    /// The next few tracks, loaded into the guest's queue so the player prefetches them.
    fn guest_prefetch(&self, next: Vec<u64>) {
        self.queue_track_ids_replace(next);
    }

    pub fn party_end(&self) {
        let role = ROLE.lock().ok().and_then(|mut r| r.take());
        HOSTING.store(false, Ordering::Relaxed);
        if let Ok(mut s) = SUGGESTIONS.lock() {
            s.clear();
        }
        self.show_party_suggestions();
        self.party_show("", String::new(), Vec::new());
        self.party_mirror(false, "", 0);
        let this = self.clone();
        self.rt.spawn(async move {
            match role {
                Some(Role::Host(h)) => h.end().await,
                Some(Role::Guest(g)) => {
                    // The reader task holds a clone; leaving closes the endpoint for both.
                    if let Ok(g) = Arc::try_unwrap(g) {
                        g.leave().await;
                    }
                }
                None => {}
            }
            this.party_status("Left the party.");
        });
    }

    /// A `slate://party/…` link: fill in the join field and open the Party page.
    pub fn party_link_opened(&self, ticket: String) {
        self.ui(move |w| {
            w.global::<SettingsState>().set_party_ticket(ticket.into());
            w.global::<SettingsState>().set_tab(7);
        });
        self.navigate(crate::pages::Route::Settings);
        self.party_status("Press Join to enter the party.");
    }
}

/// Settings → Party fields. UI thread.
pub fn fill(w: &AppWindow) {
    let prefs = crate::uiprefs::UiPrefs::load();
    w.global::<SettingsState>()
        .set_party_relay(prefs.party_relay.into());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_hosting_by_default() {
        assert!(!hosting());
        assert!(ROLE.lock().expect("lock").is_none());
        assert!(SUGGESTIONS.lock().expect("lock").is_empty());
    }
}
