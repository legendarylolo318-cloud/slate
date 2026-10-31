//! Listening party in the app: start / end as host, join / leave as guest, keeping a guest's
//! playback in step with the host, and the Settings → Party page. Nothing here exists until a
//! party is started or joined; ending one drops the network endpoint and every task with it.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use slate_core::commands::{metadata, versions};
use slate_core::party::net::{Event, Guest, Host};
use slate_core::party::proto::{self, Correction, HostMsg, ReqView};
use slate_core::party::requests::{
    self, Handoff, Placement, Policy, RequestMode, RequestTrack, Submitted, HANDOFF_GRACE,
};
use slate_core::queue::Track;
use slint::{
    ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel,
};
use tokio::sync::mpsc;

use crate::backend::Backend;
use crate::bootstrap::{AppWindow, PartyGuest, PartyRequest, PartyState, SettingsState};
use crate::uiprefs::UiPrefs;

/// One atomic read on the playback paths; true only while hosting.
static HOSTING: AtomicBool = AtomicBool::new(false);

enum Role {
    Host(Host),
    Guest(Arc<Guest>),
}

static ROLE: Mutex<Option<Role>> = Mutex::new(None);
/// Host: who asked for each track that is in the queue because of a request.
static OWNERS: Mutex<Vec<(u64, u64, String)>> = Mutex::new(Vec::new());
/// Host: the tracks behind waiting requests, so approving needs no second lookup.
static PENDING: Mutex<Vec<Track>> = Mutex::new(Vec::new());
/// Guest: what we know about the party we are in (also while reconnecting).
static GUEST: Mutex<Option<GuestCtx>> = Mutex::new(None);
/// A party is being started or joined (the network setup takes seconds); a second click waits.
static CONNECTING: AtomicBool = AtomicBool::new(false);
/// Bumped by `party_end`, so a start or join still in flight ends itself when it completes.
static SESSION: AtomicU64 = AtomicU64::new(0);
/// Guest: bumped per host track / queue message, so an older lookup that finishes late does
/// not overwrite a newer one.
static PLAY_GEN: AtomicU64 = AtomicU64::new(0);
static PREFETCH_GEN: AtomicU64 = AtomicU64::new(0);
/// Guest: tracks already reported as unavailable to this host (each is sent once).
static REPORTED: Mutex<Vec<u64>> = Mutex::new(Vec::new());
/// The QR code of the current invite link; it changes only after a kick.
static QR: Mutex<Option<(String, SharedPixelBuffer<Rgba8Pixel>)>> = Mutex::new(None);
/// Most request owners remembered (oldest are forgotten first).
const MAX_OWNERS: usize = 500;

#[derive(Clone, Default)]
struct GuestCtx {
    ticket: String,
    you: String,
    cohost: bool,
    requests_on: bool,
    /// Join order, with co-host flags.
    guests: Vec<(String, bool)>,
}

pub fn hosting() -> bool {
    HOSTING.load(Ordering::Relaxed)
}

fn relay_pref() -> Option<String> {
    let r = UiPrefs::load().party_relay;
    (!r.trim().is_empty()).then_some(r)
}

/// Pending-request caps offered in Settings → Party.
const CAPS: [usize; 5] = [1, 2, 3, 5, 10];
/// Vote-skip shares: Off, 25 %, 50 %, 75 %.
const VOTES: [u8; 4] = [0, 25, 50, 75];

fn policy_of(p: &UiPrefs) -> Policy {
    Policy {
        mode: RequestMode::from_str_or_default(&p.party_mode),
        cap: if p.party_cap == 0 {
            requests::DEFAULT_PENDING_CAP
        } else {
            p.party_cap
        },
        no_explicit: p.party_no_explicit,
    }
}

fn vote_of(p: &UiPrefs) -> Option<u8> {
    (p.party_vote > 0).then_some(p.party_vote)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn track_label(t: &Track) -> String {
    format!("{} – {}", t.artist_name(), t.title())
}

fn request_track(t: &Track) -> RequestTrack {
    RequestTrack {
        id: t.id,
        title: t.title().to_string(),
        artist: t.artist_name().to_string(),
        quality: t
            .extra
            .get("audioQuality")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        explicit: t.extra.get("explicit").and_then(|v| v.as_bool()) == Some(true),
    }
}

fn quality_label(q: &str) -> String {
    match q {
        "HI_RES_LOSSLESS" | "HIRES_LOSSLESS" => "Hi-Res".into(),
        "LOSSLESS" | "HI_RES" => "Lossless".into(),
        "" => String::new(),
        other => other.to_lowercase(),
    }
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

    fn party_show(&self, role: &'static str, link: String, guests: Vec<(String, bool)>) {
        let qr = if link.is_empty() {
            *lock(&QR) = None;
            None
        } else {
            let mut cache = lock(&QR);
            match cache.as_ref() {
                Some((l, img)) if *l == link => Some(img.clone()),
                _ => {
                    let img = crate::images::qr(&link);
                    *cache = img.clone().map(|i| (link.clone(), i));
                    img
                }
            }
        };
        let can_suggest = role == "guest" && lock(&GUEST).as_ref().is_some_and(|g| g.requests_on);
        let cohost = role == "guest" && lock(&GUEST).as_ref().is_some_and(|g| g.cohost);
        self.ui(move |w| {
            let s = w.global::<SettingsState>();
            s.set_party_role(role.into());
            s.set_party_link(link.into());
            s.set_party_qr(qr.map(Image::from_rgba8).unwrap_or_default());
            s.set_party_cohost(cohost);
            let rows: Vec<PartyGuest> = guests
                .into_iter()
                .map(|(name, cohost)| PartyGuest {
                    name: name.into(),
                    cohost,
                })
                .collect();
            s.set_party_guests(ModelRc::new(VecModel::from(rows)));
            let p = w.global::<PartyState>();
            p.set_role(role.into());
            p.set_can_suggest(can_suggest);
            if role.is_empty() {
                s.set_party_requests(ModelRc::default());
                s.set_party_upcoming(ModelRc::default());
                s.set_party_offer(false);
            }
        });
    }

    fn party_mirror(&self, active: bool, role: &str, guests: usize) {
        let Some(state) = self.state() else { return };
        if !state.mcp_active.load(Ordering::Relaxed) {
            return;
        }
        let requests: Vec<serde_json::Value> = with_host(|h| {
            h.pending()
                .into_iter()
                .map(|(r, unavailable)| {
                    serde_json::json!({
                        "request_id": r.id,
                        "guest": r.guest,
                        "track_id": r.track.id,
                        "title": r.track.title,
                        "artist": r.track.artist,
                        "quality": quality_label(&r.track.quality),
                        "unavailable_for": unavailable,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
        let v = serde_json::json!({
            "active": active, "role": role, "guest_count": guests, "requests": requests,
        });
        self.rt.spawn(async move {
            state.mcp_state.write().await.party = Some(v);
        });
    }

    // ---- host ----

    pub fn party_start(&self) {
        if lock(&ROLE).is_some() {
            self.party_status("A party is already running.");
            return;
        }
        if CONNECTING.swap(true, Ordering::AcqRel) {
            return;
        }
        self.party_status("Starting the party…");
        let prefs = UiPrefs::load();
        let (policy, vote) = (policy_of(&prefs), vote_of(&prefs));
        let (tx, rx) = mpsc::unbounded_channel();
        let session = SESSION.load(Ordering::Acquire);
        let this = self.clone();
        self.rt.spawn(async move {
            let started = Host::start(relay_pref().as_deref(), "Host", policy, vote, tx).await;
            CONNECTING.store(false, Ordering::Release);
            match started {
                // Ended (or left) while the endpoint was coming up.
                Ok(host) if SESSION.load(Ordering::Acquire) != session => host.end().await,
                Ok(host) => {
                    let link = host.link();
                    *lock(&ROLE) = Some(Role::Host(host));
                    HOSTING.store(true, Ordering::Relaxed);
                    this.party_show("host", link, Vec::new());
                    this.party_status("Party started. Share the link or QR code.");
                    this.party_mirror(true, "host", 0);
                    this.host_sync_now();
                    this.party_publish_queue_soon();
                    this.pump_host_events(rx);
                }
                Err(e) => this.party_status(e),
            }
        });
    }

    fn pump_host_events(&self, mut rx: mpsc::UnboundedReceiver<Event>) {
        let this = self.clone();
        self.rt.spawn(async move {
            enum Kind {
                Roster,
                Requests,
                Other,
            }
            while let Some(e) = rx.recv().await {
                let e_kind = match &e {
                    Event::GuestJoined(_) | Event::GuestLeft(_) => Kind::Roster,
                    Event::RequestsChanged => Kind::Requests,
                    _ => Kind::Other,
                };
                match e {
                    Event::GuestJoined(n) => this.party_status(format!("{n} joined")),
                    Event::GuestLeft(n) => this.party_status(format!("{n} left")),
                    Event::Suggestion {
                        guest_id,
                        guest,
                        track_id,
                    } => this.host_got_request(guest_id, guest, track_id),
                    Event::CohostDecide {
                        request_id,
                        approve,
                        ..
                    } => match request_id {
                        Some(id) => this.host_decide(id, approve),
                        None => this.party_decide_all(approve),
                    },
                    Event::CohostSkip(_) | Event::VoteSkipPassed => this.next(),
                    Event::CohostMove { from, to, .. } => {
                        this.queue_move(from as usize, to as usize)
                    }
                    Event::RequestsChanged => {}
                    Event::Closed(_) | Event::Lost(_) | Event::Host(_) => {}
                }
                // Only what the event changed is rebuilt: the guest list on joins and leaves,
                // the request tray when requests changed.
                let roster = matches!(e_kind, Kind::Roster);
                if roster {
                    let guests = with_host(Host::guests_detail).unwrap_or_default();
                    this.party_show("host", this.current_party_link(), guests);
                }
                if !matches!(e_kind, Kind::Other) {
                    this.show_requests();
                    let n = with_host(|h| h.guest_names().len()).unwrap_or(0);
                    this.party_mirror(true, "host", n);
                }
            }
        });
    }

    fn current_party_link(&self) -> String {
        with_host(Host::link).unwrap_or_default()
    }

    /// A guest asked for a track: look it up, then let the request book decide.
    fn host_got_request(&self, guest_id: u64, guest: String, track_id: u64) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            let track = match metadata::get_track(&state, track_id).await {
                Ok(v) => serde_json::from_value::<Track>(v).ok(),
                Err(_) => None,
            };
            let Some(track) = track.filter(|t| !t.is_video()) else {
                with_host(|h| h.reject(guest_id, track_id, "That track could not be found."));
                return;
            };
            let rt = request_track(&track);
            let label = track_label(&track);
            let result = with_host(|h| h.submit(guest_id, rt));
            match result {
                Some(Ok(Submitted::Pending(_))) => {
                    lock(&PENDING).push(track);
                    this.notify(&format!("{guest} requested {label}"));
                }
                Some(Ok(Submitted::Approved(req))) => {
                    this.place_request(req.guest_id, req.guest, track, true);
                }
                _ => {}
            }
            this.show_requests();
        });
    }

    /// The tray under Settings → Party (host, and co-hosts see their own copy).
    fn show_requests(&self) {
        let rows: Vec<PartyRequest> = with_host(|h| {
            let pending = h.pending();
            // Requests that went away without a decision (the guest left) drop their track.
            lock(&PENDING).retain(|t| pending.iter().any(|(r, _)| r.track.id == t.id));
            pending
                .into_iter()
                .map(|(r, unavailable)| PartyRequest {
                    id: r.id.to_string().into(),
                    guest: r.guest.into(),
                    track: format!("{} – {}", r.track.artist, r.track.title).into(),
                    quality: quality_label(&r.track.quality).into(),
                    unavailable: unavailable as i32,
                })
                .collect()
        })
        .unwrap_or_default();
        self.ui(move |w| {
            w.global::<SettingsState>()
                .set_party_requests(ModelRc::new(VecModel::from(rows)))
        });
    }

    /// Approve or deny the request with this id (the host's tray, a co-host, or an agent).
    fn host_decide(&self, id: u64, approve: bool) {
        let Some(req) = with_host(|h| h.decide(id, approve)).flatten() else {
            return;
        };
        let track = {
            let mut p = lock(&PENDING);
            p.iter()
                .position(|t| t.id == req.track.id)
                .map(|i| p.remove(i))
        };
        if let (true, Some(track)) = (approve, track) {
            self.place_request(req.guest_id, req.guest, track, false);
        }
        self.show_requests();
    }

    /// Tray buttons (host) and co-host buttons (guest). `id` is the request id.
    pub fn party_decide_id(&self, id: &str, approve: bool) {
        let Ok(id) = id.parse::<u64>() else { return };
        if hosting() {
            self.host_decide(id, approve);
        } else if let Some(g) = guest_handle() {
            self.rt.spawn(async move {
                let _ = g.decide(id, approve).await;
            });
        }
    }

    pub fn party_decide_all(&self, approve: bool) {
        if hosting() {
            let all = with_host(|h| h.decide_all(approve)).unwrap_or_default();
            for req in all {
                let track = {
                    let mut p = lock(&PENDING);
                    p.iter()
                        .position(|t| t.id == req.track.id)
                        .map(|i| p.remove(i))
                };
                if let (true, Some(track)) = (approve, track) {
                    self.place_request(req.guest_id, req.guest, track, false);
                }
            }
            self.show_requests();
        } else if let Some(g) = guest_handle() {
            self.rt.spawn(async move {
                let _ = g.decide_all(approve).await;
            });
        }
    }

    /// An agent's `approve_request` / `deny_request`.
    pub(crate) fn party_mcp_decide(&self, request_id: u64, approve: bool) {
        self.host_decide(request_id, approve);
    }

    /// Puts an approved request in the queue where the host's placement choice says.
    fn place_request(&self, guest_id: u64, guest: String, track: Track, auto: bool) {
        let placement = Placement::from_str_or_default(&UiPrefs::load().party_placement);
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(ctl) = this.controller() else { return };
            let id = track.id;
            let own = lock(&OWNERS).clone();
            let owners: Vec<Option<u64>> = ctl
                .with_queue(|q| {
                    q.manual
                        .iter()
                        .map(|t| own.iter().find(|o| o.0 == t.id).map(|o| o.1))
                        .collect()
                })
                .await;
            let at = requests::insert_index(placement, &owners, guest_id);
            {
                let mut own = lock(&OWNERS);
                own.push((id, guest_id, guest.clone()));
                let extra = own.len().saturating_sub(MAX_OWNERS);
                own.drain(..extra);
            }
            ctl.insert_in_queue(at, track.clone()).await;
            this.refresh_queue_info().await;
            this.save().await;
            let how = if auto { "Added" } else { "Approved" };
            this.notify(&format!("{how}: {} (from {guest})", track_label(&track)));
        });
    }

    /// The host's request settings changed (Settings → Party).
    fn host_policy_changed(&self) {
        let prefs = UiPrefs::load();
        with_host(|h| {
            h.set_policy(policy_of(&prefs));
            h.set_vote_percent(vote_of(&prefs));
        });
    }

    pub fn party_kick(&self, name: &str) {
        with_host(|h| h.kick(name));
    }

    pub fn party_cohost(&self, name: &str, on: bool) {
        with_host(|h| h.set_cohost(name, on));
        let guests = with_host(Host::guests_detail).unwrap_or_default();
        self.party_show("host", self.current_party_link(), guests);
    }

    /// Settings → Party choices that are stored. `value` is the index of the option picked.
    pub fn party_pref_int(&self, key: &str, value: i32) {
        let i = usize::try_from(value).unwrap_or(0);
        let mut p = UiPrefs::load();
        match key {
            "party-mode" => {
                p.party_mode = ["approval", "auto", "off"]
                    .get(i)
                    .copied()
                    .unwrap_or("approval")
                    .into()
            }
            "party-placement" => {
                p.party_placement = ["next", "end", "fair"]
                    .get(i)
                    .copied()
                    .unwrap_or("fair")
                    .into()
            }
            "party-cap" => p.party_cap = CAPS.get(i).copied().unwrap_or(3),
            "party-vote" => p.party_vote = VOTES.get(i).copied().unwrap_or(0),
            _ => return,
        }
        if let Err(e) = p.save() {
            self.party_status(format!("Could not save: {e}"));
        }
        self.host_policy_changed();
    }

    pub fn party_pref_bool(&self, key: &str, on: bool) {
        if key == "party-no-explicit" {
            let mut p = UiPrefs::load();
            p.party_no_explicit = on;
            let _ = p.save();
            self.host_policy_changed();
        }
    }

    // ---- hooks from playback (host) ----

    /// The current track and play state, sent to guests (at party start).
    fn host_sync_now(&self) {
        let pos = f32::from_bits(self.shared.position.load(Ordering::Relaxed)).max(0.0);
        let playing = self.shared.playing.load(Ordering::Relaxed);
        if let Some(id) = self.current_track_id() {
            with_host(|h| h.track_changed(id, (pos * 1000.0) as u64, playing));
        }
    }

    /// Sends the queue to guests (a new party, or after a takeover).
    fn party_publish_queue_soon(&self) {
        let this = self.clone();
        self.rt.spawn(async move { this.publish_queue().await });
    }

    pub(crate) fn party_track_changed(&self, id: u64) {
        if hosting() {
            lock(&OWNERS).retain(|o| o.0 != id);
            with_host(|h| h.track_changed(id, 0, true));
        }
    }

    pub(crate) fn party_state_changed(&self, position_secs: f32, playing: bool) {
        if hosting() {
            with_host(|h| h.state_changed((position_secs.max(0.0) * 1000.0) as u64, playing));
        }
    }

    pub(crate) fn party_queue_changed(&self, upcoming: &[u64]) {
        if hosting() {
            let by: Vec<String> = {
                let own = lock(&OWNERS);
                upcoming
                    .iter()
                    .take(proto::PREFETCH_TRACKS)
                    .map(|id| {
                        own.iter()
                            .find(|o| o.0 == *id)
                            .map(|o| o.2.clone())
                            .unwrap_or_default()
                    })
                    .collect()
            };
            with_host(|h| h.queue_changed(upcoming, &by));
        }
    }

    // ---- guest ----

    pub fn party_join(&self, ticket: &str) {
        if lock(&ROLE).is_some() {
            self.party_status("Leave the current party first.");
            return;
        }
        let Some(ticket) = proto::parse_ticket(ticket) else {
            self.party_status("That is not a Slate party link.");
            return;
        };
        if CONNECTING.swap(true, Ordering::AcqRel) {
            return;
        }
        self.party_status("Joining…");
        let name = UiPrefs::load().party_name;
        self.guest_connect(ticket, name);
    }

    fn guest_connect(&self, ticket: String, name: String) {
        let (tx, rx) = mpsc::unbounded_channel();
        let session = SESSION.load(Ordering::Acquire);
        let this = self.clone();
        self.rt.spawn(async move {
            let joined = Guest::join(&ticket, &name, relay_pref().as_deref(), tx).await;
            CONNECTING.store(false, Ordering::Release);
            match joined {
                Ok((guest, _)) if SESSION.load(Ordering::Acquire) != session => guest.leave().await,
                Ok((guest, welcome)) => this.guest_joined(guest, welcome, ticket, name, rx),
                Err(e) => this.party_status(e),
            }
        });
    }

    fn guest_joined(
        &self,
        guest: Guest,
        welcome: HostMsg,
        ticket: String,
        name: String,
        rx: mpsc::UnboundedReceiver<Event>,
    ) {
        let guest = Arc::new(guest);
        let HostMsg::Welcome {
            host,
            guests,
            state,
            allow_suggest,
            vote_skip,
            ..
        } = welcome
        else {
            return;
        };
        *lock(&GUEST) = Some(GuestCtx {
            ticket,
            you: guest.you.clone(),
            cohost: false,
            requests_on: allow_suggest,
            guests: guests.iter().map(|g| (g.clone(), false)).collect(),
        });
        let _ = name;
        lock(&REPORTED).clear();
        *lock(&ROLE) = Some(Role::Guest(Arc::clone(&guest)));
        self.party_show(
            "guest",
            String::new(),
            guests.iter().map(|g| (g.clone(), false)).collect(),
        );
        self.party_status(format!(
            "Joined {host}'s party. Everyone plays from their own account."
        ));
        self.ui(move |w| {
            w.global::<SettingsState>()
                .set_party_vote_available(vote_skip)
        });
        self.party_mirror(true, "guest", 0);
        // A late joiner starts with the host's current track and position.
        if let Some(id) = state.track {
            let pos = proto::host_position_now(
                state.position_ms,
                state.playing,
                state.at_ms,
                guest.host_now_ms(),
            );
            self.guest_play(id, pos, state.playing);
        }
        self.pump_guest_events(rx, guest);
    }

    fn pump_guest_events(&self, mut rx: mpsc::UnboundedReceiver<Event>, guest: Arc<Guest>) {
        let this = self.clone();
        self.rt.spawn(async move {
            while let Some(e) = rx.recv().await {
                // After leaving (or once another party took its place) nothing from this
                // connection is applied any more.
                let current = lock(&ROLE)
                    .as_ref()
                    .is_some_and(|r| matches!(r, Role::Guest(g) if Arc::ptr_eq(g, &guest)));
                if !current {
                    return;
                }
                match e {
                    Event::Host(m) => this.guest_apply(&guest, m),
                    Event::Closed(reason) => {
                        *lock(&ROLE) = None;
                        *lock(&GUEST) = None;
                        this.party_show("", String::new(), Vec::new());
                        this.party_status(reason);
                        this.party_mirror(false, "", 0);
                        return;
                    }
                    Event::Lost(reason) => {
                        *lock(&ROLE) = None;
                        // The dead connection's endpoint is closed; reconnecting makes a new one.
                        guest.leave().await;
                        this.guest_lost(reason);
                        return;
                    }
                    _ => {}
                }
            }
        });
    }

    /// The host went quiet: keep the party for a grace period and try to get back in.
    fn guest_lost(&self, reason: String) {
        let Some(ctx) = lock(&GUEST).clone() else {
            return;
        };
        self.party_status(format!(
            "{reason} Trying to reconnect for {} seconds…",
            HANDOFF_GRACE.as_secs()
        ));
        let lost = Instant::now();
        let this = self.clone();
        self.rt.spawn(async move {
            this.guest_grace(ctx, lost).await;
        });
    }

    /// Retries every few seconds until the grace period is over, then decides what happens to
    /// the party (see `requests::handoff_state`).
    async fn guest_grace(&self, ctx: GuestCtx, lost: Instant) {
        LOST_SINCE.with_lock(|l| *l = Some(lost));
        loop {
            // "Leave" while waiting clears the context.
            if lock(&GUEST).is_none() {
                LOST_SINCE.with_lock(|l| *l = None);
                return;
            }
            let order: Vec<(u64, String, bool)> = ctx
                .guests
                .iter()
                .enumerate()
                .map(|(i, (n, c))| (i as u64, n.clone(), *c))
                .collect();
            let me = order
                .iter()
                .find(|(_, n, _)| *n == ctx.you)
                .map_or(u64::MAX, |g| g.0);
            match requests::handoff_state(
                Some(lost.elapsed()),
                HANDOFF_GRACE,
                me,
                requests::successor(&order),
            ) {
                Handoff::Waiting { .. } => {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    let name = UiPrefs::load().party_name;
                    let (tx, rx) = mpsc::unbounded_channel();
                    let session = SESSION.load(Ordering::Acquire);
                    if let Ok((guest, welcome)) =
                        Guest::join(&ctx.ticket, &name, relay_pref().as_deref(), tx).await
                    {
                        LOST_SINCE.with_lock(|l| *l = None);
                        if SESSION.load(Ordering::Acquire) != session || lock(&GUEST).is_none() {
                            guest.leave().await;
                            return;
                        }
                        self.guest_joined(guest, welcome, ctx.ticket.clone(), name, rx);
                        return;
                    }
                }
                Handoff::OfferToMe => {
                    LOST_SINCE.with_lock(|l| *l = None);
                    self.party_status("The host is gone. You are next in line to host.");
                    self.ui(|w| w.global::<SettingsState>().set_party_offer(true));
                    return;
                }
                Handoff::OfferToOther => {
                    LOST_SINCE.with_lock(|l| *l = None);
                    *lock(&GUEST) = None;
                    self.party_show("", String::new(), Vec::new());
                    let next = order
                        .iter()
                        .find(|(i, _, _)| Some(*i) == requests::successor(&order))
                        .map(|g| g.1.clone())
                        .unwrap_or_default();
                    self.party_status(format!(
                        "The host is gone. {next} can take the party over; ask them for the new link."
                    ));
                    return;
                }
                Handoff::Over | Handoff::Live => {
                    LOST_SINCE.with_lock(|l| *l = None);
                    *lock(&GUEST) = None;
                    self.party_show("", String::new(), Vec::new());
                    self.party_status("The host left and the party ended.");
                    return;
                }
            }
        }
    }

    /// "Take over" after the host left: this Slate becomes the host.
    pub fn party_takeover(&self) {
        *lock(&GUEST) = None;
        self.ui(|w| w.global::<SettingsState>().set_party_offer(false));
        self.party_start();
    }

    pub fn party_decline(&self) {
        *lock(&GUEST) = None;
        self.party_show("", String::new(), Vec::new());
        self.party_status("Left the party.");
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
            HostMsg::Queue { next, by } => self.guest_prefetch(next, by),
            HostMsg::Guests { guests, cohosts } => {
                let rows: Vec<(String, bool)> = guests
                    .iter()
                    .map(|g| (g.clone(), cohosts.contains(g)))
                    .collect();
                if let Some(c) = lock(&GUEST).as_mut() {
                    c.guests = rows.clone();
                }
                let n = rows.len();
                self.party_show("guest", String::new(), rows);
                self.party_mirror(true, "guest", n);
            }
            HostMsg::Role { cohost } => {
                if let Some(c) = lock(&GUEST).as_mut() {
                    c.cohost = cohost;
                }
                self.ui(move |w| w.global::<SettingsState>().set_party_cohost(cohost));
                self.notify(if cohost {
                    "The host made you a co-host: you can approve requests and skip."
                } else {
                    "You are no longer a co-host."
                });
            }
            HostMsg::Requests { pending, .. } => self.show_cohost_requests(pending),
            HostMsg::RequestResult { reason, .. } => self.notify(&reason),
            HostMsg::Notice { text } => self.notify(&text),
            _ => {}
        }
    }

    fn show_cohost_requests(&self, pending: Vec<ReqView>) {
        let rows: Vec<PartyRequest> = pending
            .into_iter()
            .map(|r| PartyRequest {
                id: r.id.to_string().into(),
                guest: r.guest.into(),
                track: format!("{} – {}", r.artist, r.title).into(),
                quality: quality_label(&r.quality).into(),
                unavailable: r.unavailable as i32,
            })
            .collect();
        self.ui(move |w| {
            w.global::<SettingsState>()
                .set_party_requests(ModelRc::new(VecModel::from(rows)))
        });
    }

    /// Sends a track to the host (right-click menu, ＋ on a search result).
    pub fn party_suggest(&self, track_id: u64) {
        let Some(g) = guest_handle() else {
            self.notify(if hosting() {
                "You are the host: add it to your own queue."
            } else {
                "Join a party first."
            });
            return;
        };
        if !lock(&GUEST).as_ref().is_some_and(|c| c.requests_on) {
            self.notify("The host is not taking requests.");
            return;
        }
        let this = self.clone();
        self.rt.spawn(async move {
            match g.suggest(track_id).await {
                Ok(()) => this.notify("Sent to the host"),
                Err(_) => this.notify("Could not reach the host."),
            }
        });
    }

    pub fn party_vote(&self) {
        if let Some(g) = guest_handle() {
            self.rt.spawn(async move {
                let _ = g.vote_skip().await;
            });
        }
    }

    /// Plays the host's track from its position. A track this account cannot stream is
    /// replaced by another version of the song when there is one, otherwise skipped locally
    /// (the party carries on; the host is told how many guests could not play it).
    fn guest_play(&self, id: u64, position_ms: u64, playing: bool) {
        let gen = PLAY_GEN.fetch_add(1, Ordering::AcqRel) + 1;
        let this = self.clone();
        self.rt.spawn(async move {
            let (Some(state), Some(ctl)) = (this.state(), this.controller()) else {
                return;
            };
            let track = match metadata::get_track(&state, id).await {
                Ok(v) => serde_json::from_value::<Track>(v).ok(),
                Err(_) => None,
            };
            // The host moved on while this track was looked up.
            if PLAY_GEN.load(Ordering::Acquire) != gen {
                return;
            }
            let mut played = match &track {
                Some(t) => this.guest_try(&ctl, t.clone()).await,
                None => false,
            };
            if !played {
                report_unavailable(id).await;
                if PLAY_GEN.load(Ordering::Acquire) != gen {
                    return;
                }
                if let Some(alt) = this.guest_alternative(&state, id).await {
                    if this.guest_try(&ctl, alt.clone()).await {
                        this.notify(&format!(
                            "Playing another version of {} for you",
                            alt.title()
                        ));
                        played = true;
                    }
                }
            }
            if !played {
                this.notify("A track in the party is not available to you; skipping it.");
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

    async fn guest_try(&self, ctl: &slate_core::controller::Controller, track: Track) -> bool {
        let outcome = ctl
            .play_from_source(track.clone(), vec![track], None, false)
            .await;
        let failed = matches!(outcome, slate_core::controller::Outcome::Failed(_));
        self.apply(outcome).await;
        !failed
    }

    /// The first other version of the song this account can load.
    async fn guest_alternative(&self, state: &slate_core::AppState, id: u64) -> Option<Track> {
        let items = versions::track_versions(state, id).await.ok()?;
        for it in items.iter().take(3) {
            let alt = it.get("id").and_then(serde_json::Value::as_u64)?;
            if let Ok(v) = metadata::get_track(state, alt).await {
                if let Ok(t) = serde_json::from_value::<Track>(v) {
                    return Some(t);
                }
            }
        }
        None
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

    /// The next few tracks, loaded into the guest's queue so the player prefetches them. Tracks
    /// this account cannot stream are left out of the local queue and reported to the host.
    fn guest_prefetch(&self, next: Vec<u64>, by: Vec<String>) {
        let gen = PREFETCH_GEN.fetch_add(1, Ordering::AcqRel) + 1;
        let this = self.clone();
        self.rt.spawn(async move {
            let (Some(state), Some(ctl)) = (this.state(), this.controller()) else {
                return;
            };
            let now = slate_core::now_secs() as i64;
            let (mut tracks, mut lines) = (Vec::new(), Vec::new());
            for (i, id) in next.iter().enumerate() {
                let t = match metadata::get_track(&state, *id).await {
                    Ok(v) => serde_json::from_value::<Track>(v).ok(),
                    Err(e) if e.is_rate_limited() => break,
                    Err(_) => None,
                };
                let Some(t) = t else { continue };
                let who = by.get(i).filter(|n| !n.is_empty());
                lines.push(match who {
                    Some(n) => format!("{}  ·  suggested by {n}", track_label(&t)),
                    None => track_label(&t),
                });
                if t.is_unavailable(now) {
                    report_unavailable(*id).await;
                } else {
                    tracks.push(t);
                }
            }
            // A newer queue arrived while these were looked up: it fills the queue instead.
            if PREFETCH_GEN.load(Ordering::Acquire) != gen {
                return;
            }
            ctl.clear_next_up().await;
            for t in tracks {
                ctl.add_to_queue(t, None).await;
            }
            this.refresh_queue_info().await;
            this.ui(move |w| {
                w.global::<SettingsState>()
                    .set_party_upcoming(strings(lines))
            });
        });
    }

    pub fn party_end(&self) {
        SESSION.fetch_add(1, Ordering::AcqRel);
        let role = lock(&ROLE).take();
        *lock(&GUEST) = None;
        lock(&REPORTED).clear();
        HOSTING.store(false, Ordering::Relaxed);
        lock(&OWNERS).clear();
        lock(&PENDING).clear();
        self.party_show("", String::new(), Vec::new());
        self.party_mirror(false, "", 0);
        let this = self.clone();
        self.rt.spawn(async move {
            match role {
                Some(Role::Host(h)) => h.end().await,
                // The event pump holds another reference; leaving closes the connection for
                // both, and the pump ends when the reader stops.
                Some(Role::Guest(g)) => g.leave().await,
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

/// Runs `f` on the host, if this Slate is hosting.
fn with_host<R>(f: impl FnOnce(&Host) -> R) -> Option<R> {
    match lock(&ROLE).as_ref() {
        Some(Role::Host(h)) => Some(f(h)),
        _ => None,
    }
}

/// Tells the host (once per track) that this guest cannot play `id`.
async fn report_unavailable(id: u64) {
    {
        let mut r = lock(&REPORTED);
        if r.contains(&id) {
            return;
        }
        if r.len() >= requests::MAX_UNAVAILABLE_TRACKS {
            r.remove(0);
        }
        r.push(id);
    }
    if let Some(g) = guest_handle() {
        let _ = g.unavailable(id).await;
    }
}

fn guest_handle() -> Option<Arc<Guest>> {
    match lock(&ROLE).as_ref() {
        Some(Role::Guest(g)) => Some(Arc::clone(g)),
        _ => None,
    }
}

/// When the host was lost, while a guest is waiting for it to come back.
struct LostSince(Mutex<Option<Instant>>);

impl LostSince {
    fn with_lock<R>(&self, f: impl FnOnce(&mut Option<Instant>) -> R) -> R {
        f(&mut lock(&self.0))
    }
}

static LOST_SINCE: LostSince = LostSince(Mutex::new(None));

/// Settings → Party fields. UI thread.
pub fn fill(w: &AppWindow) {
    let prefs = UiPrefs::load();
    let s = w.global::<SettingsState>();
    s.set_party_relay(prefs.party_relay.clone().into());
    s.set_party_name(prefs.party_name.clone().into());
    s.set_party_mode(
        ["approval", "auto", "off"]
            .iter()
            .position(|m| *m == prefs.party_mode)
            .unwrap_or(0) as i32,
    );
    s.set_party_placement(
        ["next", "end", "fair"]
            .iter()
            .position(|m| *m == prefs.party_placement)
            .unwrap_or(2) as i32,
    );
    s.set_party_cap_index(
        CAPS.iter()
            .position(|c| *c == policy_of(&prefs).cap)
            .unwrap_or(2) as i32,
    );
    s.set_party_no_explicit(prefs.party_no_explicit);
    s.set_party_vote_index(
        VOTES
            .iter()
            .position(|v| *v == prefs.party_vote)
            .unwrap_or(0) as i32,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_hosting_by_default() {
        assert!(!hosting());
        assert!(lock(&ROLE).is_none());
        assert!(lock(&OWNERS).is_empty() && lock(&PENDING).is_empty());
    }

    #[test]
    fn stored_party_choices_map_to_the_core_types() {
        let mut p = UiPrefs::default();
        assert_eq!(
            policy_of(&p),
            Policy::default(),
            "defaults: approval, cap 3"
        );
        assert_eq!(vote_of(&p), None);
        p.party_mode = "auto".into();
        p.party_cap = 5;
        p.party_no_explicit = true;
        p.party_vote = 50;
        let policy = policy_of(&p);
        assert_eq!(policy.mode, RequestMode::Auto);
        assert_eq!((policy.cap, policy.no_explicit), (5, true));
        assert_eq!(vote_of(&p), Some(50));
        assert_eq!(CAPS[2], requests::DEFAULT_PENDING_CAP);
    }

    #[test]
    fn quality_labels_are_short() {
        assert_eq!(quality_label("HI_RES_LOSSLESS"), "Hi-Res");
        assert_eq!(quality_label("LOSSLESS"), "Lossless");
        assert_eq!(quality_label(""), "");
    }
}
