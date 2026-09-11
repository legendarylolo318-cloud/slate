//! The one HTTP client SONE's reqwest consumers share.
//!
//! Holding a `Result` rather than a `Client` is deliberate: there is no way to
//! obtain a client when the plan is blocked, so no consumer can fall back to a
//! direct one. reqwest auto-detects the system proxy, so a client "without a
//! proxy" would egress.

use crate::proxy::{BlockReason, Capability, HostCaps, ProxyPlan, Route};
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
pub struct ProxiedHttp(Arc<RwLock<Result<reqwest::Client, BlockReason>>>);

impl ProxiedHttp {
    pub fn from_plan(p: &ProxyPlan, env: &HostCaps) -> Self {
        Self(Arc::new(RwLock::new(build_client(p, env))))
    }

    /// Blocked plans return `Err`; there is no proxy-less fallback.
    pub fn client(&self) -> Result<reqwest::Client, BlockReason> {
        match self.0.read() {
            Ok(g) => g.clone(),
            // A panic elsewhere must not downgrade egress: read through the
            // poison rather than substituting a fresh, unproxied client.
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    pub fn replace(&self, p: &ProxyPlan, env: &HostCaps) {
        let next = build_client(p, env);
        match self.0.write() {
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
    fn credentials_build_a_client_without_entering_the_uri() {
        let caps = HostCaps::assume_all_present();
        let mut s = enabled("127.0.0.1", 3128);
        s.username = Some("u".into());
        s.password = Some("hunter2".into());
        let p = plan(&s, &caps).unwrap();
        // `Route::Via` hands the credentials over separately; only
        // `Proxy::basic_auth` ever reunites them with the endpoint.
        assert!(!format!("{p:?}").contains("hunter2"));
        assert!(ProxiedHttp::from_plan(&p, &caps).client().is_ok());
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
}
