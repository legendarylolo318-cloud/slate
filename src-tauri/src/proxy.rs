//! Single source of truth for whether and how SONE proxies a request.
//!
//! Nothing outside this module constructs a proxy URI. `Direct` means the
//! system's own configuration applies; a `PlanError` blocks every capability.

use std::net::Ipv6Addr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Creds {
    pub user: String,
    pub pass: String,
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
    if host
        .contains(|c: char| matches!(c, '@' | '/' | '?' | '#' | '\\') || c.is_whitespace())
    {
        return Err(PlanError::BadHost(raw.to_string()));
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
        assert!(matches!(
            plan(&s, &HostCaps::assume_all_present()),
            Ok(ProxyPlan::Socks5 { .. })
        ));
    }
}
