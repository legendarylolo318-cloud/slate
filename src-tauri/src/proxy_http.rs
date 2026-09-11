//! The one HTTP client SONE's reqwest consumers share.
//!
//! Holding a `Result` rather than a `Client` is deliberate: there is no way to
//! obtain a client when the plan is blocked, so no consumer can fall back to a
//! direct one. reqwest auto-detects the system proxy, so a client "without a
//! proxy" would egress.

use crate::proxy::{BlockReason, Capability, HostCaps, ProxyPlan, Route};
use std::sync::{Arc, Mutex, RwLock};
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
    cell: Arc<RwLock<Result<reqwest::Client, BlockReason>>>,
    /// Held across build-and-store so two concurrent `replace` calls cannot
    /// interleave and leave the older plan's client in the cell. `build_client`
    /// resolves names, so the window between build and store is long enough to
    /// lose that race in practice.
    updating: Arc<Mutex<()>>,
}

impl ProxiedHttp {
    fn wrap(state: Result<reqwest::Client, BlockReason>) -> Self {
        Self {
            cell: Arc::new(RwLock::new(state)),
            updating: Arc::new(Mutex::new(())),
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

    /// Blocked plans return `Err`; there is no proxy-less fallback.
    pub fn client(&self) -> Result<reqwest::Client, BlockReason> {
        match self.cell.read() {
            Ok(g) => g.clone(),
            // A panic elsewhere must not downgrade egress: read through the
            // poison rather than substituting a fresh, unproxied client.
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Swap in the client for a new plan. Blocking: `build_client` may resolve
    /// the proxy host, so call it off the async runtime (`spawn_blocking`).
    pub fn replace(&self, p: &ProxyPlan, env: &HostCaps) {
        let _serialized = self
            .updating
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let next = build_client(p, env);
        match self.cell.write() {
            Ok(mut g) => *g = next,
            Err(poisoned) => *poisoned.into_inner() = next,
        }
    }

    /// Block the cell outright, for settings that do not form a plan at all.
    pub fn block(&self, cause: impl Into<String>) {
        let _serialized = self
            .updating
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let next = Err(BlockReason {
            cause: cause.into(),
        });
        match self.cell.write() {
            Ok(mut g) => *g = next,
            Err(poisoned) => *poisoned.into_inner() = next,
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

    #[test]
    fn concurrent_replaces_cannot_leave_the_older_plan_in_the_cell() {
        // `build_client` is slow (it may resolve a name) and `replace` is called
        // from a Tauri command, so two settings saves can overlap. Without the
        // update lock, the slower build can store *after* the faster one and
        // resurrect the plan the user just replaced.
        let caps = HostCaps::assume_all_present();
        let h = ProxiedHttp::from_plan(&ProxyPlan::Direct, &caps);

        // The slow one blocks on DNS; the fast one is a literal address.
        let slow = plan(&enabled_socks("no-such-host.invalid", 3128), &caps).unwrap();
        let fast = plan(&enabled("127.0.0.1", 3128), &caps).unwrap();

        for _ in 0..8 {
            let a = h.clone();
            let b = h.clone();
            let (s, f) = (slow.clone(), fast.clone());
            let t = std::thread::spawn(move || a.replace(&s, &caps));
            b.replace(&f, &caps);
            t.join().unwrap();

            // Whichever finished last wins, but the winner must be one of the
            // two plans in full — never a torn state, and never a client that
            // lost its proxy.
            match h.client() {
                Ok(c) => assert!(
                    format!("{c:?}").contains("All(http://127.0.0.1:3128)"),
                    "a surviving client must still carry its proxy: {c:?}"
                ),
                Err(e) => assert!(e.cause.contains("no-such-host.invalid")),
            }
        }

        // The last word belongs to whoever wrote last: a final uncontended
        // replace must be observable through every clone.
        h.replace(&fast, &caps);
        let c = h.clone().client().unwrap();
        assert!(format!("{c:?}").contains("All(http://127.0.0.1:3128)"));
    }
}
