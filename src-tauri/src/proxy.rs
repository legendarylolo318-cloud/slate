//! Single source of truth for whether and how SONE proxies a request.
//!
//! Nothing outside this module constructs a proxy URI. `Direct` means the
//! system's own configuration applies; a `PlanError` blocks every capability.

use std::net::Ipv6Addr;

#[derive(Clone, PartialEq, Eq)]
pub struct Creds {
    pub user: String,
    pub pass: String,
}

// Hand-written so a stray `{plan:?}` log never prints the proxy password.
impl std::fmt::Debug for Creds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Creds")
            .field("user", &self.user)
            .field("pass", &"***")
            .finish()
    }
}

/// Host facts that cannot be read without `gst::init()`, injected so `plan()`
/// stays pure and unit-testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostCaps {
    pub has_dashdemux: bool,
    pub has_curlhttpsrc: bool,
    pub gst_version: (u32, u32, u32),
}

impl HostCaps {
    /// Stage 1 default. Stage 3 replaces this with a real probe run after
    /// `gst::init()` on the audio thread.
    pub fn assume_all_present() -> Self {
        Self {
            has_dashdemux: true,
            has_curlhttpsrc: true,
            gst_version: (1, 26, 10),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyPlan {
    Direct,
    Http {
        host: String,
        port: u16,
        creds: Option<Creds>,
    },
    Socks5 {
        host: String,
        port: u16,
        creds: Option<Creds>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    PortZero,
    BadHost(String),
    NonAsciiHost,
    BracketedHost,
    EmbeddedPort,
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PortZero => write!(f, "proxy port must not be 0"),
            Self::BadHost(h) => write!(f, "invalid proxy host: {h}"),
            Self::NonAsciiHost => write!(f, "proxy host must be ASCII"),
            Self::BracketedHost => {
                write!(f, "enter an IPv6 address without brackets")
            }
            Self::EmbeddedPort => {
                write!(f, "enter the host without a port; use the port field")
            }
        }
    }
}

/// Reject anything that could change the meaning of a URI we build by
/// concatenation, or that would reach a parser known to abort on it.
fn validate_host(raw: &str) -> Result<String, PlanError> {
    let host = raw.trim();
    if host.is_empty() {
        return Err(PlanError::BadHost(raw.to_string()));
    }
    if !host.is_ascii() {
        return Err(PlanError::NonAsciiHost);
    }
    if host.starts_with('[') || host.ends_with(']') {
        return Err(PlanError::BracketedHost);
    }
    // A bare IPv6 literal is the only legitimate reason for a colon here.
    if host.contains(':') {
        if host.parse::<Ipv6Addr>().is_ok() {
            return Ok(host.to_string());
        }
        // Scope ids (`fe80::1%eth0`) land here too: Ipv6Addr rejects them, and a
        // scoped address is meaningless for a proxy endpoint.
        if host.matches(':').count() == 1 {
            return Err(PlanError::EmbeddedPort);
        }
        return Err(PlanError::BadHost(raw.to_string()));
    }
    // Allowlist, not denylist: this string is later concatenated verbatim into
    // a URI and handed to a GStreamer element property with no `Url` parsing
    // in between, so anything that isn't plainly a hostname character is
    // rejected outright. This is what actually closes NUL bytes, C0/DEL
    // control characters, `%`-encoding, and stray URI delimiters — a denylist
    // of "known-bad" characters always misses one.
    if !host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        return Err(PlanError::BadHost(raw.to_string()));
    }
    Ok(host.to_string())
}

fn creds_of(s: &crate::ProxySettings) -> Option<Creds> {
    let user = s.username.as_deref().unwrap_or("").trim();
    if user.is_empty() {
        return None;
    }
    Some(Creds {
        user: user.to_string(),
        // souphttpsrc only authenticates when BOTH properties are set, so an
        // absent password becomes empty rather than no credentials at all.
        pass: s.password.clone().unwrap_or_default(),
    })
}

pub fn plan(s: &crate::ProxySettings, _env: &HostCaps) -> Result<ProxyPlan, PlanError> {
    if !s.enabled {
        return Ok(ProxyPlan::Direct);
    }
    if s.port == 0 {
        return Err(PlanError::PortZero);
    }
    let host = validate_host(&s.host)?;
    let creds = creds_of(s);
    Ok(match s.proxy_type {
        crate::ProxyType::Http => ProxyPlan::Http {
            host,
            port: s.port,
            creds,
        },
        crate::ProxyType::Socks5 => ProxyPlan::Socks5 {
            host,
            port: s.port,
            creds,
        },
    })
}

/// Which consumer is asking. The spelling of a SOCKS5 URI and the element
/// requirements differ per consumer, so this is not cosmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    /// reqwest: API, auth, scrobbling, play reports, artwork, update check.
    Api,
    /// GStreamer progressive HTTP (lossy) — may use souphttpsrc.
    Lossy,
    /// GStreamer DASH segments (lossless/hi-res).
    Dash,
    /// The shared WebKit network session.
    Webview,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// Proceed with the system's own configuration.
    NoProxy,
    Via {
        uri: String,
        creds: Option<Creds>,
    },
}

/// Why a capability cannot be served. Carries a cause because failures are
/// discovered in places `plan()` cannot see, such as resolving the proxy host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockReason {
    pub cause: String,
}

impl BlockReason {
    fn new(cause: impl Into<String>) -> Self {
        Self {
            cause: cause.into(),
        }
    }
}

impl std::fmt::Display for BlockReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.cause)
    }
}

/// Minimum GStreamer with a working `curlhttpsrc` progressive seek.
const CURL_SEEK_FIXED: (u32, u32, u32) = (1, 26, 10);

fn authority(host: &str, port: u16) -> String {
    if host.parse::<Ipv6Addr>().is_ok() {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

impl ProxyPlan {
    pub fn route(&self, c: Capability, env: &HostCaps) -> Result<Route, BlockReason> {
        let (host, port, creds, socks) = match self {
            ProxyPlan::Direct => return Ok(Route::NoProxy),
            ProxyPlan::Http { host, port, creds } => (host, *port, creds, false),
            ProxyPlan::Socks5 { host, port, creds } => (host, *port, creds, true),
        };

        if matches!(c, Capability::Lossy | Capability::Dash) {
            // souphttpsrc cannot authenticate over a CONNECT tunnel, so credentials
            // force curlhttpsrc; and it cannot do authenticated SOCKS5 at all.
            if !socks && creds.is_some() && !env.has_curlhttpsrc {
                return Err(BlockReason::new(
                    "audio cannot be proxied with credentials: the curl source plugin is missing",
                ));
            }
            if socks && creds.is_some() && !env.has_curlhttpsrc {
                return Err(BlockReason::new(
                    "authenticated SOCKS5 audio requires the curl source plugin",
                ));
            }
        }

        if c == Capability::Dash && !env.has_dashdemux {
            return Err(BlockReason::new(
                "high-resolution audio cannot be proxied: the legacy adaptive demuxer is missing",
            ));
        }

        if c == Capability::Lossy && creds.is_some() && env.gst_version < CURL_SEEK_FIXED {
            let (a, b, d) = env.gst_version;
            let (x, y, z) = CURL_SEEK_FIXED;
            return Err(BlockReason::new(format!(
                "authenticated proxies need GStreamer {x}.{y}.{z} or newer for seeking (found {a}.{b}.{d})"
            )));
        }

        // The spelling keys on which library resolves the name, not on the
        // capability: gio has a socks5 impl and none for socks5h, while libcurl
        // and reqwest resolve locally unless told socks5h.
        let scheme = match (socks, c) {
            (false, _) => "http",
            // reqwest: socks5h is what defers resolution to the proxy.
            (true, Capability::Api) => "socks5h",
            // WebKit resolves via gio, which implements socks5 only — and gio's socks5
            // already sends the hostname, so it carries socks5h semantics.
            (true, Capability::Webview) => "socks5",
            // Audio flips element on credentials: curl source (libcurl, needs socks5h)
            // when authenticating, soup source (gio, needs socks5) otherwise.
            (true, Capability::Lossy) | (true, Capability::Dash) => {
                if creds.is_some() {
                    debug_assert!(
                        env.has_curlhttpsrc,
                        "authenticated audio reached the scheme match without the curl source; \
                         the guard above must block this",
                    );
                    // `debug_assert!` compiles out under `--release`, and the
                    // degraded behaviour there is fail-open: socks5h handed to a
                    // gio-backed source that implements only socks5 is a wrong
                    // scheme, not a block. Fail closed in every profile.
                    if !env.has_curlhttpsrc {
                        return Err(BlockReason::new(
                            "authenticated SOCKS5 audio requires the curl source plugin",
                        ));
                    }
                    "socks5h"
                } else {
                    "socks5"
                }
            }
        };

        Ok(Route::Via {
            uri: format!("{scheme}://{}", authority(host, port)),
            creds: creds.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ProxySettings, ProxyType};

    fn settings(host: &str, port: u16) -> ProxySettings {
        ProxySettings {
            enabled: true,
            proxy_type: ProxyType::Http,
            host: host.to_string(),
            port,
            username: None,
            password: None,
        }
    }

    #[test]
    fn disabled_is_direct() {
        let mut s = settings("127.0.0.1", 8080);
        s.enabled = false;
        assert!(matches!(
            plan(&s, &HostCaps::assume_all_present()),
            Ok(ProxyPlan::Direct)
        ));
    }

    #[test]
    fn port_zero_is_an_error_not_direct() {
        // Regression: today this silently resolves to Direct, which is fail-open.
        assert!(matches!(
            plan(&settings("127.0.0.1", 0), &HostCaps::assume_all_present()),
            Err(PlanError::PortZero)
        ));
    }

    #[test]
    fn plain_host_is_accepted() {
        let p = plan(&settings("proxy.example", 3128), &HostCaps::assume_all_present()).unwrap();
        assert!(matches!(p, ProxyPlan::Http { ref host, port: 3128, .. } if host == "proxy.example"));
    }

    #[test]
    fn host_is_trimmed() {
        let p = plan(&settings("  proxy.example  ", 3128), &HostCaps::assume_all_present()).unwrap();
        assert!(matches!(p, ProxyPlan::Http { ref host, .. } if host == "proxy.example"));
    }

    #[test]
    fn bare_ipv6_is_accepted_and_stored_unbracketed() {
        let p = plan(&settings("2001:db8::1", 8080), &HostCaps::assume_all_present()).unwrap();
        assert!(matches!(p, ProxyPlan::Http { ref host, .. } if host == "2001:db8::1"));
    }

    #[test]
    fn already_bracketed_ipv6_is_rejected() {
        // Double-bracketing core-dumps souphttpsrc, so it must never reach an element.
        assert!(matches!(
            plan(&settings("[::1]", 8080), &HostCaps::assume_all_present()),
            Err(PlanError::BracketedHost)
        ));
    }

    #[test]
    fn host_with_embedded_port_is_rejected() {
        assert!(matches!(
            plan(&settings("1.2.3.4:9999", 8080), &HostCaps::assume_all_present()),
            Err(PlanError::EmbeddedPort)
        ));
    }

    #[test]
    fn ipv6_scope_id_is_rejected() {
        assert!(matches!(
            plan(&settings("fe80::1%eth0", 8080), &HostCaps::assume_all_present()),
            Err(PlanError::BadHost(_))
        ));
    }

    #[test]
    fn non_ascii_host_is_rejected() {
        assert!(matches!(
            plan(&settings("пример.рф", 8080), &HostCaps::assume_all_present()),
            Err(PlanError::NonAsciiHost)
        ));
    }

    #[test]
    fn url_ish_and_delimiter_hosts_are_rejected() {
        for bad in ["http://proxy.example", "user@proxy", "pro?xy", "pro#xy", "", "   "] {
            assert!(
                plan(&settings(bad, 8080), &HostCaps::assume_all_present()).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn credentials_are_captured_with_empty_password_preserved() {
        let mut s = settings("proxy.example", 3128);
        s.username = Some("bob".into());
        s.password = Some(String::new());
        let p = plan(&s, &HostCaps::assume_all_present()).unwrap();
        match p {
            ProxyPlan::Http { creds: Some(c), .. } => {
                assert_eq!(c.user, "bob");
                assert_eq!(c.pass, "");
            }
            other => panic!("expected creds, got {other:?}"),
        }
    }

    #[test]
    fn username_without_password_still_yields_creds() {
        // souphttpsrc needs BOTH properties set; an absent password becomes empty,
        // never a missing credential pair.
        let mut s = settings("proxy.example", 3128);
        s.username = Some("bob".into());
        s.password = None;
        assert!(matches!(
            plan(&s, &HostCaps::assume_all_present()),
            Ok(ProxyPlan::Http { creds: Some(_), .. })
        ));
    }

    #[test]
    fn password_without_username_is_no_credentials() {
        let mut s = settings("proxy.example", 3128);
        s.username = None;
        s.password = Some("hunter2".into());
        assert!(matches!(
            plan(&s, &HostCaps::assume_all_present()),
            Ok(ProxyPlan::Http { creds: None, .. })
        ));
    }

    #[test]
    fn socks5_type_is_preserved() {
        let mut s = settings("proxy.example", 1080);
        s.proxy_type = ProxyType::Socks5;
        s.username = Some("bob".into());
        s.password = Some("hunter2".into());
        let p = plan(&s, &HostCaps::assume_all_present()).unwrap();
        match p {
            ProxyPlan::Socks5 { host, port, creds } => {
                assert_eq!(host, "proxy.example");
                assert_eq!(port, 1080);
                let c = creds.expect("expected creds to survive on the Socks5 arm");
                assert_eq!(c.user, "bob");
                assert_eq!(c.pass, "hunter2");
            }
            other => panic!("expected Socks5, got {other:?}"),
        }
    }

    #[test]
    fn nul_byte_host_is_rejected() {
        // ToGlibPtr for str only checks interior NULs under debug_assertions;
        // in release the host is silently truncated at the NUL, so the
        // element ends up contacting a different host than was validated.
        assert!(matches!(
            plan(
                &settings("evil.com\0.good.proxy", 8080),
                &HostCaps::assume_all_present()
            ),
            Err(PlanError::BadHost(_))
        ));
    }

    #[test]
    fn control_character_host_is_rejected() {
        for bad in ["pro\x07xy", "a\x1bb.com", "a\x7fb.com"] {
            assert!(
                matches!(
                    plan(&settings(bad, 8080), &HostCaps::assume_all_present()),
                    Err(PlanError::BadHost(_))
                ),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn percent_encoded_host_is_rejected() {
        // Unfiltered `%` lets a downstream percent-decoder reach a different
        // host than the one validated here.
        for bad in ["good.proxy%00.evil.com", "pro%40evil.com", "evil%2ecom"] {
            assert!(
                matches!(
                    plan(&settings(bad, 8080), &HostCaps::assume_all_present()),
                    Err(PlanError::BadHost(_))
                ),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn delimiter_hosts_are_rejected() {
        for bad in [
            "a,b.com", "a;b.com", "a|b.com", "a<b>.com", "a\"b.com", "a`b.com", "a*b.com",
            "a{b}.com", "a[b", "a]b.com",
        ] {
            assert!(
                plan(&settings(bad, 8080), &HostCaps::assume_all_present()).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn creds_debug_redacts_password() {
        let c = Creds {
            user: "bob".into(),
            pass: "hunter2".into(),
        };
        let debug = format!("{c:?}");
        assert!(debug.contains("bob"));
        assert!(!debug.contains("hunter2"));
    }

    fn http_plan(port: u16) -> ProxyPlan {
        plan(&settings("proxy.example", port), &HostCaps::assume_all_present()).unwrap()
    }

    fn socks_plan(with_creds: bool) -> ProxyPlan {
        let mut s = settings("proxy.example", 1080);
        s.proxy_type = ProxyType::Socks5;
        if with_creds {
            s.username = Some("bob".into());
            s.password = Some("hunter2".into());
        }
        plan(&s, &HostCaps::assume_all_present()).unwrap()
    }

    fn authed_plan(port: u16) -> ProxyPlan {
        let mut s = settings("proxy.example", port);
        s.username = Some("bob".into());
        s.password = Some("hunter2".into());
        plan(&s, &HostCaps::assume_all_present()).unwrap()
    }

    fn uri_of(r: &Route) -> &str {
        match r {
            Route::Via { uri, .. } => uri,
            Route::NoProxy => panic!("expected a proxied route"),
        }
    }

    #[test]
    fn direct_routes_to_noproxy_for_every_capability() {
        let caps = HostCaps::assume_all_present();
        for c in [Capability::Api, Capability::Lossy, Capability::Dash, Capability::Webview] {
            assert!(matches!(
                ProxyPlan::Direct.route(c, &caps),
                Ok(Route::NoProxy)
            ));
        }
    }

    #[test]
    fn port_80_is_preserved_for_every_capability() {
        // Regression: round-tripping through url::Url drops a default port and
        // libcurl then silently dials 1080.
        let caps = HostCaps::assume_all_present();
        let p = http_plan(80);
        for c in [Capability::Api, Capability::Lossy, Capability::Dash, Capability::Webview] {
            let r = p.route(c, &caps).unwrap();
            assert_eq!(uri_of(&r), "http://proxy.example:80", "capability {c:?}");
        }
    }

    #[test]
    fn every_port_round_trips_exactly() {
        let caps = HostCaps::assume_all_present();
        for port in [1u16, 80, 443, 1080, 8080, 65535] {
            let r = http_plan(port).route(Capability::Api, &caps).unwrap();
            assert_eq!(uri_of(&r), format!("http://proxy.example:{port}"));
        }
    }

    #[test]
    fn ipv6_is_bracketed_exactly_once_in_the_uri() {
        let caps = HostCaps::assume_all_present();
        let p = plan(&settings("2001:db8::1", 8080), &caps).unwrap();
        let r = p.route(Capability::Lossy, &caps).unwrap();
        assert_eq!(uri_of(&r), "http://[2001:db8::1]:8080");
    }

    #[test]
    fn socks5_spellings_without_credentials_cover_every_capability() {
        // The spelling keys on the resolving library, never on the capability
        // alone. Unauthenticated audio goes through the soup source (gio), and
        // gio implements socks5 only — its socks5 already sends the hostname.
        // reqwest resolves locally unless told socks5h.
        let caps = HostCaps::assume_all_present();
        let p = socks_plan(false);
        for (c, want) in [
            (Capability::Api, "socks5h://proxy.example:1080"),
            (Capability::Webview, "socks5://proxy.example:1080"),
            (Capability::Lossy, "socks5://proxy.example:1080"),
            (Capability::Dash, "socks5://proxy.example:1080"),
        ] {
            assert_eq!(uri_of(&p.route(c, &caps).unwrap()), want, "capability {c:?}");
        }
    }

    #[test]
    fn socks5_spellings_with_credentials_cover_every_capability() {
        // Credentials flip both audio capabilities onto the curl source, which
        // is libcurl and so resolves locally unless told socks5h. Webview still
        // goes through gio and must stay socks5, or it reaches NO GProxy IMPL.
        let caps = HostCaps::assume_all_present();
        let p = socks_plan(true);
        for (c, want) in [
            (Capability::Api, "socks5h://proxy.example:1080"),
            (Capability::Webview, "socks5://proxy.example:1080"),
            (Capability::Lossy, "socks5h://proxy.example:1080"),
            (Capability::Dash, "socks5h://proxy.example:1080"),
        ] {
            assert_eq!(uri_of(&p.route(c, &caps).unwrap()), want, "capability {c:?}");
        }
    }

    #[test]
    fn http_plans_are_http_for_every_capability_regardless_of_credentials() {
        let caps = HostCaps::assume_all_present();
        for p in [http_plan(3128), authed_plan(3128)] {
            for c in [
                Capability::Api,
                Capability::Webview,
                Capability::Lossy,
                Capability::Dash,
            ] {
                assert_eq!(
                    uri_of(&p.route(c, &caps).unwrap()),
                    "http://proxy.example:3128",
                    "capability {c:?}"
                );
            }
        }
    }

    #[test]
    fn no_route_uri_ever_contains_credentials() {
        let caps = HostCaps::assume_all_present();
        let p = authed_plan(3128);
        for c in [Capability::Api, Capability::Lossy, Capability::Dash, Capability::Webview] {
            let r = p.route(c, &caps).unwrap();
            let uri = uri_of(&r);
            assert!(!uri.contains('@'), "{uri}");
            assert!(!uri.contains("bob"), "{uri}");
            assert!(!uri.contains("hunter2"), "{uri}");
            assert!(matches!(r, Route::Via { creds: Some(_), .. }));
        }
    }

    fn block_cause(p: &ProxyPlan, c: Capability, env: &HostCaps) -> String {
        match p.route(c, env) {
            Err(b) => b.cause,
            Ok(r) => panic!("expected {c:?} to be blocked, got {r:?}"),
        }
    }

    #[test]
    fn socks5_with_credentials_needs_curlhttpsrc_for_audio() {
        let mut caps = HostCaps::assume_all_present();
        caps.has_curlhttpsrc = false;
        let p = socks_plan(true);
        for c in [Capability::Lossy, Capability::Dash] {
            let cause = block_cause(&p, c, &caps);
            assert!(cause.contains("SOCKS5"), "capability {c:?}: {cause}");
        }
        // The API transport is unaffected by a missing GStreamer plugin.
        assert!(p.route(Capability::Api, &caps).is_ok());
    }

    #[test]
    fn authenticated_socks5_audio_fails_closed_even_if_the_earlier_guard_is_bypassed() {
        // Pins the release profile, where the `debug_assert!` in the scheme match
        // is compiled out. Without the guard beside it this state would yield a
        // socks5h URI for a gio-backed source instead of a block — a wrong scheme,
        // which is a DNS leak rather than a clean failure.
        let mut caps = HostCaps::assume_all_present();
        caps.has_curlhttpsrc = false;
        let p = socks_plan(true);
        for c in [Capability::Lossy, Capability::Dash] {
            assert_eq!(
                block_cause(&p, c, &caps),
                "authenticated SOCKS5 audio requires the curl source plugin",
                "capability {c:?} must fail closed"
            );
        }
    }

    #[test]
    fn http_with_credentials_needs_curlhttpsrc_for_audio() {
        // Mirror of the SOCKS5 case: both branches must stay reachable and must
        // not share a message, or the SOCKS5 wording silently stops firing.
        let mut caps = HostCaps::assume_all_present();
        caps.has_curlhttpsrc = false;
        let p = authed_plan(3128);
        for c in [Capability::Lossy, Capability::Dash] {
            let cause = block_cause(&p, c, &caps);
            assert!(!cause.contains("SOCKS5"), "capability {c:?}: {cause}");
            assert!(cause.contains("curl source plugin"), "capability {c:?}: {cause}");
        }
        assert!(p.route(Capability::Api, &caps).is_ok());
    }

    #[test]
    fn socks5_without_credentials_works_without_curlhttpsrc() {
        let mut caps = HostCaps::assume_all_present();
        caps.has_curlhttpsrc = false;
        assert!(socks_plan(false).route(Capability::Lossy, &caps).is_ok());
    }

    #[test]
    fn missing_legacy_demuxer_blocks_dash_but_not_lossy() {
        let mut caps = HostCaps::assume_all_present();
        caps.has_dashdemux = false;
        let p = http_plan(3128);
        let cause = block_cause(&p, Capability::Dash, &caps);
        assert!(cause.contains("adaptive demuxer"), "{cause}");
        assert!(p.route(Capability::Lossy, &caps).is_ok());
        assert!(p.route(Capability::Api, &caps).is_ok());
    }

    // Load-bearing jointly with `curl_seek_version_boundary_is_exact`: the
    // substring assertion below is also satisfied by a constant of (1, 26, 101),
    // which only the boundary test rejects. Deleting either weakens the pair.
    #[test]
    fn old_gstreamer_blocks_lossy_only_when_credentials_are_present() {
        // curlhttpsrc progressive seek is broken below 1.26.10 and it is the only
        // element that can authenticate over a CONNECT tunnel.
        let mut caps = HostCaps::assume_all_present();
        caps.gst_version = (1, 24, 2);

        let with_creds = authed_plan(3128);
        let cause = block_cause(&with_creds, Capability::Lossy, &caps);
        assert!(cause.contains("1.26.10"), "{cause}");
        assert!(cause.contains("1.24.2"), "{cause}");
        assert!(with_creds.route(Capability::Dash, &caps).is_ok());

        let without = http_plan(3128);
        assert!(without.route(Capability::Lossy, &caps).is_ok());
    }

    // The other half of that pair: this is what catches a (1, 26, 101) constant
    // that the substring assertions above would happily accept.
    #[test]
    fn curl_seek_version_boundary_is_exact() {
        // Only the far-away (1,24,2) was covered, so mutating CURL_SEEK_FIXED to
        // (1,26,0) left the suite green. Pin both sides of the real boundary.
        let p = authed_plan(3128);

        let mut just_below = HostCaps::assume_all_present();
        just_below.gst_version = (1, 26, 9);
        let cause = block_cause(&p, Capability::Lossy, &just_below);
        assert!(cause.contains("1.26.9"), "{cause}");

        let mut exactly_fixed = HostCaps::assume_all_present();
        exactly_fixed.gst_version = (1, 26, 10);
        assert!(p.route(Capability::Lossy, &exactly_fixed).is_ok());
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use crate::{ProxySettings, ProxyType};
    use proptest::prelude::*;

    /// Mixes shapes that each reach a different branch of `validate_host`. An
    /// arbitrary-Unicode host alone is almost never accepted, which leaves the
    /// accept path and the bracketing in `authority()` untested.
    fn any_host() -> impl Strategy<Value = String> {
        prop_oneof![
            // Reaches the accept path.
            4 => "[a-z][a-z0-9._-]{0,20}",
            // Reaches the IPv6 accept branch, and the single-bracketing in `authority`.
            3 => any::<std::net::Ipv6Addr>().prop_map(|a| a.to_string()),
            // Reaches `BracketedHost`: the input that double-brackets a URI and
            // core-dumps souphttpsrc if it is ever let through.
            3 => any::<std::net::Ipv6Addr>().prop_map(|a| format!("[{a}]")),
            // Reaches `EmbeddedPort`.
            3 => (any::<std::net::Ipv4Addr>(), any::<u16>()).prop_map(|(a, p)| format!("{a}:{p}")),
            // Broad adversarial coverage: brackets, colons, percent signs,
            // delimiters, non-ASCII and whitespace. Weighted to roughly a third
            // of generated hosts — both historical crashes came from here, and
            // starving this arm to steer cases at the structured shapes is what
            // would let the next one through.
            7 => "[\\PC]{0,40}",
        ]
    }

    fn any_settings() -> impl Strategy<Value = ProxySettings> {
        (
            any::<bool>(),
            any::<bool>(),
            any_host(),
            any::<u16>(),
            proptest::option::of("[\\PC]{0,20}"),
            proptest::option::of("[\\PC]{0,20}"),
        )
            .prop_map(|(enabled, socks, host, port, username, password)| ProxySettings {
                enabled,
                proxy_type: if socks { ProxyType::Socks5 } else { ProxyType::Http },
                host,
                port,
                username,
                password,
            })
    }

    proptest! {
        #[test]
        fn plan_never_panics_and_routes_are_credential_free(s in any_settings()) {
            let caps = HostCaps::assume_all_present();
            if let Ok(p) = plan(&s, &caps) {
                for c in [Capability::Api, Capability::Lossy, Capability::Dash, Capability::Webview] {
                    if let Ok(Route::Via { uri, .. }) = p.route(c, &caps) {
                        prop_assert!(!uri.contains('@'), "uri leaked a delimiter: {uri}");
                        // A one- or two-character credential collides with a port
                        // digit or a `:`/`/` delimiter by coincidence, not by
                        // leaking, so only a credential long enough to be
                        // unambiguous proves anything. The `@` check above stays
                        // unconditional: it cannot false-positive. A non-ASCII
                        // credential needs no length floor at all: the host is
                        // ASCII-only by `validate_host`, so it cannot collide.
                        if let Some(u) = s.username.as_deref() {
                            let u = u.trim();
                            if !u.is_ascii() || u.len() >= 4 {
                                prop_assert!(!uri.contains(u), "uri leaked the username: {uri}");
                            }
                        }
                        if let Some(pw) = s.password.as_deref() {
                            if !pw.is_ascii() || pw.len() >= 4 {
                                prop_assert!(!uri.contains(pw), "uri leaked the password: {uri}");
                            }
                        }
                    }
                }
            }
        }

        #[test]
        fn accepted_uris_are_single_bracketed_and_end_in_the_given_port(s in any_settings()) {
            let caps = HostCaps::assume_all_present();
            if let Ok(p @ (ProxyPlan::Http { .. } | ProxyPlan::Socks5 { .. })) = plan(&s, &caps) {
                if let Ok(Route::Via { uri, .. }) = p.route(Capability::Api, &caps) {
                    prop_assert!(!uri.contains("[["), "double bracketed: {uri}");
                    prop_assert!(!uri.contains("]]"), "double bracketed: {uri}");
                    prop_assert!(
                        uri.ends_with(&format!(":{}", s.port)),
                        "port not preserved: {uri}"
                    );
                }
            }
        }

        #[test]
        fn enabled_settings_never_silently_become_direct(s in any_settings()) {
            // The original fail-open: `enabled` with unusable input resolved to Direct.
            let caps = HostCaps::assume_all_present();
            if s.enabled {
                prop_assert!(!matches!(plan(&s, &caps), Ok(ProxyPlan::Direct)));
            }
        }
    }
}
