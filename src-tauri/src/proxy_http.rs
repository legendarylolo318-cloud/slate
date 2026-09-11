//! The one HTTP client SONE's reqwest consumers share.
//!
//! Holding a `Result` rather than a `Client` is deliberate: there is no way to
//! obtain a client when the plan is blocked, so no consumer can fall back to a
//! direct one. reqwest auto-detects the system proxy, so a client "without a
//! proxy" would egress.

use crate::proxy::{BlockReason, Capability, HostCaps, ProxyPlan, Route};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

pub fn build_client(p: &ProxyPlan, env: &HostCaps) -> Result<reqwest::Client, BlockReason> {
    let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(30));

    match p.route(Capability::Api, env)? {
        // Direct means the system's own configuration applies: leave reqwest's
        // auto-detection alone.
        Route::NoProxy => {}
        Route::Via { uri, creds } => {
            // Credentials never travel in the URI: `Route::Via` keeps them
            // apart and `basic_auth` is the only thing that reunites them.
            let mut obj = reqwest::Proxy::all(&uri).map_err(|e| BlockReason {
                cause: format!("proxy unusable ({uri}): {e}"),
            })?;
            if let Some(c) = creds {
                obj = obj.basic_auth(&c.user, &c.pass);
            }
            builder = builder.proxy(obj);
        }
    }

    builder.build().map_err(|e| BlockReason {
        cause: format!("could not build HTTP client: {e}"),
    })
}

#[derive(Clone)]
pub struct ProxiedHttp {
    /// The generation that produced the current state, beside the state itself.
    cell: Arc<RwLock<(u64, Result<reqwest::Client, BlockReason>)>>,
    /// Dispenses a generation to each writer BEFORE it starts building, so
    /// "newest" means the newest caller rather than whichever build happened to
    /// finish last. A slow build must never resurrect the settings it replaced.
    next: Arc<AtomicU64>,
}

impl ProxiedHttp {
    fn wrap(state: Result<reqwest::Client, BlockReason>) -> Self {
        Self {
            cell: Arc::new(RwLock::new((0, state))),
            next: Arc::new(AtomicU64::new(1)),
        }
    }

    pub fn from_plan(p: &ProxyPlan, env: &HostCaps) -> Self {
        Self::wrap(build_client(p, env))
    }

    /// A cell that can never hand out a client. Used where the settings do not
    /// even form a plan: the alternative — treating an unplannable proxy as
    /// `Direct` — is the silent downgrade this module exists to prevent.
    pub fn blocked(cause: impl Into<String>) -> Self {
        Self::wrap(Err(BlockReason {
            cause: cause.into(),
        }))
    }

    /// The cell for a settings value. The single place that decides what an
    /// unplannable proxy means, so neither startup nor a settings save can
    /// drift back to `Direct`.
    pub fn from_settings(s: &crate::ProxySettings, env: &HostCaps) -> Self {
        match crate::proxy::plan(s, env) {
            Ok(p) => Self::from_plan(&p, env),
            Err(e) => {
                log::error!("proxy settings unusable, blocking all HTTP: {e}");
                Self::blocked(e.to_string())
            }
        }
    }

    /// Blocked plans return `Err`; there is no proxy-less fallback.
    pub fn client(&self) -> Result<reqwest::Client, BlockReason> {
        match self.cell.read() {
            Ok(g) => g.1.clone(),
            // A panic elsewhere must not downgrade egress: read through the
            // poison rather than substituting a fresh, unproxied client.
            Err(poisoned) => poisoned.into_inner().1.clone(),
        }
    }

    /// Swap in the client for a new plan. Blocking: `build_client` may resolve
    /// the proxy host, so call it off the async runtime (`spawn_blocking`).
    pub fn replace(&self, p: &ProxyPlan, env: &HostCaps) {
        let generation = self.claim();
        // Deliberately outside every lock: this can block on `getaddrinfo`, and
        // a reader or a concurrent `block` must never wait on that.
        let built = build_client(p, env);
        self.store(generation, built);
    }

    /// Block the cell outright, for settings that do not form a plan at all.
    pub fn block(&self, cause: impl Into<String>) {
        let generation = self.claim();
        self.store(
            generation,
            Err(BlockReason {
                cause: cause.into(),
            }),
        );
    }

    /// Apply a settings value to the live cell. Blocking, for the same reason
    /// as `replace`.
    pub fn apply(&self, s: &crate::ProxySettings, env: &HostCaps) {
        match crate::proxy::plan(s, env) {
            Ok(p) => self.replace(&p, env),
            Err(e) => {
                log::error!("proxy settings unusable, blocking all HTTP: {e}");
                self.block(e.to_string())
            }
        }
    }

    /// Claim a generation. Must happen before the build, never after.
    fn claim(&self) -> u64 {
        self.next.fetch_add(1, Ordering::SeqCst)
    }

    fn store(&self, generation: u64, built: Result<reqwest::Client, BlockReason>) {
        let mut g = match self.cell.write() {
            Ok(g) => g,
            // A panic elsewhere must not strand the cell on stale settings.
            Err(poisoned) => poisoned.into_inner(),
        };
        // The whole point. Without this comparison a slow build started under
        // the OLD settings lands last and wins, so the cell disagrees with what
        // the user saved — including the enable -> disable -> enable case, where
        // the loser's client carries no proxy at all.
        if generation >= g.0 {
            *g = (generation, built);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::{plan, HostCaps, ProxyPlan};
    use crate::{ProxySettings, ProxyType};

    fn enabled(host: &str, port: u16) -> ProxySettings {
        ProxySettings {
            enabled: true,
            proxy_type: ProxyType::Http,
            host: host.to_string(),
            port,
            username: None,
            password: None,
        }
    }

    /// `Capability::Api` spells SOCKS5 as `socks5h`, and that is the scheme
    /// reqwest resolves eagerly. See `unresolvable_proxy_host_*` below.
    fn enabled_socks(host: &str, port: u16) -> ProxySettings {
        ProxySettings {
            proxy_type: ProxyType::Socks5,
            ..enabled(host, port)
        }
    }

    #[test]
    fn direct_plan_yields_a_usable_client() {
        let caps = HostCaps::assume_all_present();
        let h = ProxiedHttp::from_plan(&ProxyPlan::Direct, &caps);
        assert!(h.client().is_ok());
    }

    #[test]
    fn valid_proxy_plan_yields_a_usable_client() {
        let caps = HostCaps::assume_all_present();
        let p = plan(&enabled("127.0.0.1", 3128), &caps).unwrap();
        assert!(ProxiedHttp::from_plan(&p, &caps).client().is_ok());
    }

    #[test]
    fn credentials_travel_beside_the_uri_not_inside_it() {
        let caps = HostCaps::assume_all_present();
        let mut s = enabled("127.0.0.1", 3128);
        s.username = Some("bob".into());
        s.password = Some("hunter2".into());
        let p = plan(&s, &caps).unwrap();

        // This is the exact string `build_client` hands to `reqwest::Proxy::all`.
        // Asserting the whole URI, not just the absence of a password, is what
        // makes this fail if credentials ever get folded into it.
        let route = p.route(Capability::Api, &caps).unwrap();
        let Route::Via { uri, creds } = route else {
            panic!("an enabled proxy must not route as NoProxy");
        };
        assert_eq!(uri, "http://127.0.0.1:3128");
        assert!(creds.is_some(), "credentials must arrive beside the uri");

        // Only `Proxy::basic_auth` reunites them with that endpoint.
        assert!(build_client(&p, &caps).is_ok());
    }

    #[test]
    fn unresolvable_proxy_host_blocks_with_a_cause_instead_of_a_direct_client() {
        // reqwest resolves a socks5/socks5h proxy host when the client is
        // built, so this failure is invisible to plan() and must surface as a
        // BlockReason, never a proxy-less client. `.invalid` is reserved by
        // RFC 6761 and can never resolve, so the outcome does not depend on
        // which resolver (or none) the machine has.
        let caps = HostCaps::assume_all_present();
        let p = plan(&enabled_socks("no-such-host.invalid", 3128), &caps).unwrap();
        let err = ProxiedHttp::from_plan(&p, &caps).client().unwrap_err();
        assert!(!err.cause.is_empty());
        assert!(err.cause.contains("socks5h://no-such-host.invalid:3128"));
        // Name the failure, not just its existence: without reqwest's `socks`
        // feature `Proxy::all` still errors here, with "unknown proxy scheme",
        // and this test would stay green while real SOCKS5 support was gone.
        assert!(
            err.cause.contains("failed to lookup address"),
            "expected an eager resolution failure, got: {}",
            err.cause
        );
    }

    #[test]
    fn an_unresolvable_http_proxy_still_yields_a_fully_proxied_client() {
        // Asymmetry worth pinning: reqwest defers the name lookup for an
        // `http://` proxy, so the build succeeds. That is still fail-closed —
        // the client intercepts every request, so it fails at the proxy rather
        // than reaching the network directly.
        let caps = HostCaps::assume_all_present();
        let p = plan(&enabled("no-such-host.invalid", 3128), &caps).unwrap();
        let c = ProxiedHttp::from_plan(&p, &caps).client().unwrap();
        assert!(format!("{c:?}").contains("All(http://no-such-host.invalid:3128)"));
    }

    #[test]
    fn replace_swaps_the_cell_without_reconstructing_consumers() {
        let caps = HostCaps::assume_all_present();
        let h = ProxiedHttp::from_plan(&ProxyPlan::Direct, &caps);
        let clone = h.clone();
        let p = plan(&enabled_socks("no-such-host.invalid", 3128), &caps).unwrap();
        h.replace(&p, &caps);
        // The clone observes the new state: there is one cell, not two clients.
        assert!(clone.client().is_err());
    }

    #[test]
    fn a_poisoned_cell_neither_blocks_egress_nor_drops_the_proxy() {
        let caps = HostCaps::assume_all_present();
        let p = plan(&enabled("127.0.0.1", 3128), &caps).unwrap();
        let h = ProxiedHttp::from_plan(&p, &caps);

        // A std lock is only poisoned by a real panic, so stage one. The hook is
        // silenced across the join and restored before any assertion below, so a
        // genuine failure still prints.
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let poisoner = h.clone();
        let outcome = std::thread::spawn(move || {
            let _held = poisoner.cell.write().unwrap();
            panic!("deliberate: poisons the cell while the write lock is held");
        })
        .join();
        std::panic::set_hook(prev);

        assert!(
            outcome.is_err(),
            "the staged panic must actually have happened"
        );
        assert!(h.cell.read().is_err(), "the cell must now be poisoned");

        // Reading through the poison: an unrelated panic must not block egress,
        // and must not hand back a client that lost its proxy.
        let c = h.client().expect("a poisoned cell must not block egress");
        assert!(format!("{c:?}").contains("All(http://127.0.0.1:3128)"));

        // Writing through the poison: `replace` still lands, and a clone sees it.
        let observer = h.clone();
        let blocked = plan(&enabled_socks("no-such-host.invalid", 3128), &caps).unwrap();
        h.replace(&blocked, &caps);
        assert!(
            observer.client().is_err(),
            "replace must land through the poison, not be dropped"
        );
    }

    #[test]
    fn an_explicitly_blocked_cell_never_hands_out_a_client() {
        // Settings that do not form a plan at all must land here, not on
        // `ProxyPlan::Direct`: "we could not understand your proxy" must never
        // resolve to "so we went around it".
        let h = ProxiedHttp::blocked("invalid proxy host: ho st");
        let err = h.client().unwrap_err();
        assert_eq!(err.cause, "invalid proxy host: ho st");

        // And the same for a cell that starts usable and is then blocked.
        let caps = HostCaps::assume_all_present();
        let live = ProxiedHttp::from_plan(&ProxyPlan::Direct, &caps);
        let observer = live.clone();
        assert!(live.client().is_ok());
        live.block("proxy port must not be 0");
        assert_eq!(
            observer.client().unwrap_err().cause,
            "proxy port must not be 0",
            "block must land in the shared cell, not a private copy"
        );
    }

    /// The falsifying test for the generation guard. Note the direction: the
    /// SLOW build is the one that yields a real client (~17ms to stand up a
    /// connector), and the FAST one is the blocked plan (~0.3ms, because
    /// `Proxy::all` rejects `socks5h://…invalid` before any connector is built).
    /// So the stale winner under a broken implementation is a *usable* client
    /// built from settings the user already replaced.
    #[test]
    fn a_slow_older_replace_never_overwrites_the_newer_settings() {
        let caps = HostCaps::assume_all_present();

        for _ in 0..16 {
            let h = ProxiedHttp::from_plan(&ProxyPlan::Direct, &caps);
            let older = plan(&enabled("127.0.0.1", 3128), &caps).unwrap();
            let newer = plan(&enabled_socks("no-such-host.invalid", 3128), &caps).unwrap();

            let before = h.next.load(Ordering::SeqCst);
            let slow = h.clone();
            let t = std::thread::spawn(move || slow.replace(&older, &caps));

            // Pin the generation order without pinning the completion order:
            // spin only until the older caller has claimed its generation. It
            // is then ~17ms from storing, while this thread is ~0.3ms from it.
            while h.next.load(Ordering::SeqCst) == before {
                std::hint::spin_loop();
            }
            h.replace(&newer, &caps);
            t.join().unwrap();

            let err = h.client().unwrap_err();
            assert!(
                err.cause.contains("no-such-host.invalid"),
                "the newest settings must win even though their predecessor's \
                 build finished last; cell holds a client from the old ones"
            );
        }
    }

    /// The same rule with the race removed, so it cannot pass by luck — and in
    /// the direction that actually fails open. `next` and `store` are exactly
    /// what two interleaved `replace` calls use; only the interleaving is
    /// pinned. enable -> disable -> enable: if the `Direct` build lands last,
    /// the cell goes unproxied while the saved settings say enabled.
    #[test]
    fn the_newest_caller_wins_no_matter_which_build_finished_last() {
        let caps = HostCaps::assume_all_present();
        let h = ProxiedHttp::from_plan(&ProxyPlan::Direct, &caps);
        let proxied = plan(&enabled("127.0.0.1", 3128), &caps).unwrap();

        // Two callers claim in order; their builds complete in the opposite one.
        let disable = h.claim();
        let reenable = h.claim();
        h.store(reenable, build_client(&proxied, &caps));
        h.store(disable, build_client(&ProxyPlan::Direct, &caps));

        let c = h
            .client()
            .expect("a usable plan must still yield a usable client");
        assert!(
            format!("{c:?}").contains("All(http://127.0.0.1:3128)"),
            "a late `Direct` build must not strip the proxy the user re-enabled: {c:?}"
        );

        // And the reverse: a late proxied build must not resurrect a proxy the
        // user has since turned off.
        let enable = h.claim();
        let disable = h.claim();
        h.store(disable, build_client(&ProxyPlan::Direct, &caps));
        h.store(enable, build_client(&proxied, &caps));
        let c = h.client().unwrap();
        assert!(
            !format!("{c:?}").contains("All(http://127.0.0.1:3128)"),
            "a late proxied build must not outlive the settings that asked for it"
        );
    }

    /// `block` must not be able to wait on another writer's `getaddrinfo`: it
    /// runs inline on the Tauri runtime. Nothing is held across the build, so
    /// this completes while a slow `replace` is still resolving.
    #[test]
    fn block_does_not_queue_behind_a_slow_replace() {
        let caps = HostCaps::assume_all_present();
        let h = ProxiedHttp::from_plan(&ProxyPlan::Direct, &caps);
        let p = plan(&enabled("127.0.0.1", 3128), &caps).unwrap();

        let before = h.next.load(Ordering::SeqCst);
        let slow = h.clone();
        let t = std::thread::spawn(move || slow.replace(&p, &caps));
        while h.next.load(Ordering::SeqCst) == before {
            std::hint::spin_loop();
        }

        let t0 = std::time::Instant::now();
        h.block("proxy port must not be 0");
        let waited = t0.elapsed();
        t.join().unwrap();

        assert!(
            waited < std::time::Duration::from_millis(100),
            "block waited {waited:?} — it is holding, or queueing behind, the build lock"
        );
    }

    #[test]
    fn settings_that_do_not_form_a_plan_block_the_cell_instead_of_going_direct() {
        // This is the case `AppState::new` hits on a corrupt saved proxy. A
        // `ProxyPlan::Direct` fallback here would mean every request silently
        // leaves the machine unproxied.
        let caps = HostCaps::assume_all_present();
        for bad in [
            enabled("127.0.0.1", 0),
            enabled("proxy.local:3128", 3128),
            enabled("ho st", 3128),
            enabled("[::1]", 3128),
            enabled("", 3128),
            enabled("пример.рф", 3128),
        ] {
            assert!(
                crate::proxy::plan(&bad, &caps).is_err(),
                "{bad:?} was expected to be unplannable"
            );
            let err = match ProxiedHttp::from_settings(&bad, &caps).client() {
                Err(e) => e,
                Ok(c) => panic!("{bad:?} must block, but yielded a client: {c:?}"),
            };
            assert!(!err.cause.is_empty(), "a block must carry its cause");
        }

        // The legitimate direct case must survive that strictness.
        let mut off = enabled("127.0.0.1", 3128);
        off.enabled = false;
        assert!(
            ProxiedHttp::from_settings(&off, &caps).client().is_ok(),
            "a disabled proxy is Direct, not a block"
        );
    }

    #[test]
    fn applying_unplannable_settings_blocks_a_live_cell() {
        // The `set_proxy_settings` path: a cell that is serving traffic must go
        // blocked, not fall back to direct, when the new settings do not plan.
        let caps = HostCaps::assume_all_present();
        let h = ProxiedHttp::from_plan(&ProxyPlan::Direct, &caps);
        let observer = h.clone();
        assert!(h.client().is_ok());

        h.apply(&enabled("127.0.0.1", 0), &caps);
        let err = observer
            .client()
            .expect_err("unplannable settings must block the shared cell");
        assert!(err.cause.contains("port"), "got: {}", err.cause);

        // And a subsequent good save recovers it, through the same entry point.
        h.apply(&enabled("127.0.0.1", 3128), &caps);
        let c = observer.client().expect("a valid save must unblock the cell");
        assert!(format!("{c:?}").contains("All(http://127.0.0.1:3128)"));
    }
}
