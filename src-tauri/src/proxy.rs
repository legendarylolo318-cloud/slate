//! Single source of truth for whether and how SONE proxies a request.
//!
//! Nothing outside this module constructs a proxy URI. `Direct` means the
//! system's own configuration applies; a `PlanError` blocks every capability.

use std::net::Ipv6Addr;
use std::path::{Path, PathBuf};

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

/// Every variable libcurl, libproxy or reqwest may read to find a proxy, in
/// both spellings. `curlhttpsrc` reads `no_proxy` at construction and there is
/// no property to override it, so an ambient value must be gone before any
/// element exists.
///
/// Both cases of all four, because the three readers disagree and the union is
/// what has to go:
///
/// - libcurl (so `curlhttpsrc`) honours lowercase `http_proxy` only — an
///   uppercase one would be attacker-controlled through the `Proxy:` request
///   header under CGI — but reads either case of `https_proxy`, `all_proxy`
///   and `no_proxy`.
/// - reqwest prefers the uppercase spelling of `HTTP_PROXY`, `HTTPS_PROXY` and
///   `ALL_PROXY` and falls back to lowercase, and reads `NO_PROXY` then
///   `no_proxy`.
/// - libproxy (so gio, libsoup and WebKit) reads both cases of the per-scheme
///   names and of `no_proxy`, and never reads `all_proxy` at all — that one is
///   libcurl's and reqwest's.
///
/// So no single reader wants all eight, and scrubbing one spelling leaves the
/// other live for at least one of them. Per-scheme names SONE never speaks
/// (`ftp_proxy`, `rsync_proxy`) are deliberately absent: no transport in this
/// process reads them, and removing a variable that is not ours to remove is
/// its own surprise.
///
/// Not covered, and not coverable here: libproxy 0.4.x also honours
/// `_PX_DEBUG_PACURL`, which points the whole gio/libsoup/WebKit path at a PAC
/// file regardless of everything above. It is an obscure debug hook rather
/// than a configuration mechanism, and removing it would be mutating something
/// that is plainly not ours; noted so nobody concludes this list is airtight.
pub const PROXY_ENV_VARS: [&str; 8] = [
    "http_proxy",
    "HTTP_PROXY",
    "https_proxy",
    "HTTPS_PROXY",
    "all_proxy",
    "ALL_PROXY",
    "no_proxy",
    "NO_PROXY",
];

/// A plaintext companion to the encrypted settings, holding only what `main.rs`
/// needs before `AppState` (and therefore the decryption key) exists.
pub fn sidecar_path(config_dir: &Path) -> PathBuf {
    config_dir.join("proxy.mode")
}

/// Mirror the two non-secret fields to the sidecar. Host, port and credentials
/// stay in the encrypted file: `main.rs` decides only *whether* it is proxying,
/// never *where to*, so nothing else belongs in a plaintext file.
///
/// Best effort by design. A sidecar that cannot be written leaves the next
/// launch believing SONE is not proxying, which is the same state as today and
/// degrades to a leak of the ambient configuration, not to a broken app —
/// whereas failing the save would strand the user on the one screen that can
/// undo a bad proxy.
pub fn write_sidecar(config_dir: &Path, s: &crate::ProxySettings) {
    let kind = match s.proxy_type {
        crate::ProxyType::Http => "http",
        crate::ProxyType::Socks5 => "socks5",
    };
    let body = format!("{}\n{}\n", if s.enabled { "on" } else { "off" }, kind);
    if let Err(e) = write_sidecar_file(&sidecar_path(config_dir), &body) {
        log::warn!("[proxy] could not write mode sidecar: {e}");
    }
}

/// Owner-only, 0600. The contents are not secret, but they do disclose *that*
/// this user proxies and *with what* — beside a settings file that is
/// encrypted precisely so a reader of the config directory learns neither.
///
/// The mode is set twice on purpose: `OpenOptions::mode` applies only when the
/// file is created, so a sidecar written before this change — or by an older
/// build under a loose umask — would keep its old permissions forever without
/// the explicit `set_permissions`.
fn write_sidecar_file(path: &Path, body: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(body.as_bytes())?;
    f.set_permissions(std::fs::Permissions::from_mode(0o600))
}

/// `None` whenever the file is absent, truncated, or carries a proxy type this
/// build does not know. The caller treats every `None` as "not proxying", so a
/// partial or unrecognised read must never come back as a half-answer — and
/// the type is validated rather than passed through because it is a value a
/// later task is expected to act on.
pub fn read_sidecar(config_dir: &Path) -> Option<(bool, String)> {
    let body = std::fs::read_to_string(sidecar_path(config_dir)).ok()?;
    let mut lines = body.lines();
    let enabled = lines.next()?.trim() == "on";
    let kind = match lines.next()?.trim() {
        k @ ("http" | "socks5") => k.to_string(),
        _ => return None,
    };
    Some((enabled, kind))
}

/// The proxy environment as it stood before `main.rs` removed it, captured so
/// the `Direct` path can hand it back.
///
/// Empty whenever no scrub happened, which is both the common case and the
/// safe default: consumers then behave exactly as they did before this
/// existed, letting their own auto-detection read a untouched environment.
static SCRUBBED_ENV: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();

/// Called once from `main.rs`, immediately before the variables are removed.
/// Later calls are ignored: the first capture is the only true one, since by
/// then the environment no longer holds what it is recording.
pub fn remember_scrubbed_env(vars: Vec<(String, String)>) {
    let _ = SCRUBBED_ENV.set(vars);
}

pub fn scrubbed_env() -> &'static [(String, String)] {
    SCRUBBED_ENV.get().map(Vec::as_slice).unwrap_or(&[])
}

/// What the system's own configuration said, resolved from captured variables.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemProxyEnv {
    pub http: Option<String>,
    pub https: Option<String>,
    pub no_proxy: Option<String>,
}

impl SystemProxyEnv {
    pub fn is_empty(&self) -> bool {
        self.http.is_none() && self.https.is_none() && self.no_proxy.is_none()
    }
}

/// Resolve captured variables exactly as **reqwest 0.11.27** would have, so
/// restoring them reproduces the routing the user already had.
///
/// This is a deliberate mirror of one release — `reqwest-0.11.27/src/proxy.rs`,
/// `get_from_environment` and `NoProxy::from_env` — and not an attempt at a
/// good rule:
///
/// - Per scheme, the uppercase spelling is tried first and the lowercase one
///   is the fallback, where "tried" means set, non-empty after trimming, and
///   parseable as a proxy URI. An unparseable `HTTP_PROXY` therefore falls
///   through to `http_proxy` rather than winning and yielding nothing.
/// - `ALL_PROXY` (then `all_proxy`) is applied **last and overwrites both
///   schemes**, because 0.11.27 runs that block after the per-scheme ones and
///   `insert_proxy` is an unconditional `HashMap::insert`.
/// - `NO_PROXY` wins over `no_proxy` on *presence*, not on emptiness: 0.11.27
///   takes `env::var("NO_PROXY").or_else(|_| env::var("no_proxy"))`, so an
///   exported-but-empty `NO_PROXY` suppresses the lowercase one entirely.
///
/// The `ALL_PROXY` rule is the one worth defending, because it is the one a
/// reviewer will want to "fix": per-scheme-wins is saner and is what reqwest
/// 0.12+ does. It is still wrong here. With `ALL_PROXY=http://all:1` and
/// `https_proxy=http://s:2` this user's traffic *was* going to `all:1`; a
/// better precedence would silently send it somewhere their own configuration
/// never chose. Restoration must be faithful, not improved. `source_guards.rs`
/// pins the `reqwest = "0.11"` requirement so a major bump fails the suite
/// instead of diverging quietly.
///
/// One documented non-mirror: 0.11.27 ignores `HTTP_PROXY` when `REQUEST_METHOD`
/// is set, because under CGI that variable is attacker-controlled. SONE is a
/// desktop application, never a CGI process, and `REQUEST_METHOD` is not among
/// the variables `main.rs` captures, so there is nothing to reproduce.
///
/// `usable` is injected rather than called directly because deciding whether a
/// string parses as a proxy URI means building a `reqwest::Proxy`, and those
/// are confined to `proxy_http.rs`. Pure otherwise, because the precedence is
/// the whole of the risk and is not observable from outside the process.
pub fn system_proxy_from_env(
    vars: &[(String, String)],
    usable: impl Fn(&str) -> bool,
) -> SystemProxyEnv {
    let present = |name: &str| {
        vars.iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.to_string())
    };
    // `insert_proxy`: empty or whitespace is rejected before parsing, and an
    // unparseable value is rejected too — both leave the lowercase fallback to
    // be tried.
    let get = |name: &str| present(name).filter(|v| !v.trim().is_empty() && usable(v));

    let mut resolved = SystemProxyEnv {
        http: get("HTTP_PROXY").or_else(|| get("http_proxy")),
        https: get("HTTPS_PROXY").or_else(|| get("https_proxy")),
        // Presence, not usability: `from_env` reads the variable and hands
        // whatever it finds to `from_string`, which rejects an empty list on
        // its own.
        no_proxy: present("NO_PROXY").or_else(|| present("no_proxy")),
    };

    // Last, and overwriting. See the note above before changing this.
    if let Some(all) = get("ALL_PROXY").or_else(|| get("all_proxy")) {
        resolved.http = Some(all.clone());
        resolved.https = Some(all);
    }
    resolved
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

    /// The sidecar exists so `main.rs` can decide whether to scrub before
    /// `AppState` — and therefore the decryption key — exists. Everything it
    /// does not strictly need stays in the encrypted file.
    #[test]
    fn sidecar_round_trips_only_enabled_and_type() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = settings("secret-proxy.internal", 3128);
        s.username = Some("bob".into());
        s.password = Some("hunter2".into());
        s.proxy_type = ProxyType::Socks5;

        write_sidecar(dir.path(), &s);
        let raw = std::fs::read_to_string(sidecar_path(dir.path())).unwrap();

        // Nothing secret may leave the encrypted settings file.
        assert!(!raw.contains("secret-proxy.internal"), "{raw}");
        assert!(!raw.contains("bob"), "{raw}");
        assert!(!raw.contains("hunter2"), "{raw}");
        assert!(!raw.contains("3128"), "{raw}");

        assert_eq!(read_sidecar(dir.path()), Some((true, "socks5".to_string())));
    }

    /// Disabled must round-trip as `false`, not merely as "absent". A sidecar
    /// that only ever recorded the enabled case would leave a stale `on` on
    /// disk after the user turns the proxy off, and the next launch would scrub
    /// a host whose own configuration is now the only thing routing it.
    #[test]
    fn turning_the_proxy_off_is_recorded_as_off() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = settings("127.0.0.1", 8080);

        write_sidecar(dir.path(), &s);
        assert_eq!(read_sidecar(dir.path()), Some((true, "http".to_string())));

        s.enabled = false;
        write_sidecar(dir.path(), &s);
        assert_eq!(read_sidecar(dir.path()), Some((false, "http".to_string())));
    }

    /// A first launch, and the common case: no sidecar means nothing is known,
    /// which must read as "not proxying" rather than as a default.
    #[test]
    fn missing_sidecar_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_sidecar(dir.path()), None);
    }

    /// A truncated or hand-edited file must not read as a half-answer. Anything
    /// the writer would not have produced is no answer at all.
    #[test]
    fn a_truncated_sidecar_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(sidecar_path(dir.path()), "on\n").unwrap();
        assert_eq!(read_sidecar(dir.path()), None);
    }

    /// A type this build does not know is not a half-answer to be passed
    /// along: a later task is expected to act on that string.
    #[test]
    fn an_unknown_proxy_type_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(sidecar_path(dir.path()), "off\nbanana\n").unwrap();
        assert_eq!(read_sidecar(dir.path()), None);
    }

    /// Not secret, but it discloses that this user proxies and with what,
    /// beside a settings file encrypted so that a reader of the config
    /// directory learns neither.
    #[test]
    fn the_sidecar_is_owner_only_even_when_it_already_existed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = sidecar_path(dir.path());

        // A sidecar left by an older build under a loose umask.
        std::fs::write(&path, "on\nhttp\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_sidecar(dir.path(), &settings("127.0.0.1", 8080));
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "sidecar is {mode:o}, not 0600");
    }

    /// Stands in for reqwest's `into_proxy_scheme`, which is private and lives
    /// behind a `reqwest::Proxy` this module is not allowed to build. Coarse on
    /// purpose — these tests are about precedence, not about URI parsing — but
    /// it agrees with the real one on every value used below: reqwest rejects
    /// an `ftp://` proxy outright, while a bare word is *accepted* as
    /// `http://<word>`, which is why no test here uses one. `proxy_http.rs`
    /// exercises the real predicate end to end.
    fn parses(uri: &str) -> bool {
        uri.starts_with("http://") || uri.starts_with("https://")
    }

    fn resolve(vars: &[(&str, &str)]) -> SystemProxyEnv {
        let owned: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        system_proxy_from_env(&owned, parses)
    }

    /// Nothing was scrubbed, so nothing is restored and every consumer keeps
    /// reading the untouched environment itself. The common case.
    #[test]
    fn an_unscrubbed_environment_resolves_to_nothing() {
        assert!(resolve(&[]).is_empty());
        assert_eq!(resolve(&[]), SystemProxyEnv::default());
    }

    /// reqwest 0.11.27's own precedence, reproduced: restoring has to give the
    /// user back what they had, not a policy of our own invention.
    #[test]
    fn uppercase_wins_over_lowercase_for_every_name() {
        let e = resolve(&[
            ("http_proxy", "http://lower:1"),
            ("HTTP_PROXY", "http://upper:1"),
            ("https_proxy", "http://lower:2"),
            ("HTTPS_PROXY", "http://upper:2"),
            ("no_proxy", "lower.example"),
            ("NO_PROXY", "upper.example"),
        ]);
        assert_eq!(e.http.as_deref(), Some("http://upper:1"));
        assert_eq!(e.https.as_deref(), Some("http://upper:2"));
        assert_eq!(e.no_proxy.as_deref(), Some("upper.example"));
    }

    #[test]
    fn lowercase_is_used_when_it_is_the_only_spelling() {
        let e = resolve(&[
            ("http_proxy", "http://lower:1"),
            ("no_proxy", "lower.example"),
        ]);
        assert_eq!(e.http.as_deref(), Some("http://lower:1"));
        assert_eq!(e.no_proxy.as_deref(), Some("lower.example"));
        assert_eq!(e.https, None);
    }

    /// An unparseable uppercase value is not a win, it is a miss: 0.11.27's
    /// `insert_proxy` returns false for anything it cannot turn into a proxy
    /// scheme, so the lowercase spelling is still tried. Getting this wrong
    /// leaves the user with no http proxy at all where they had a working one.
    #[test]
    fn an_unparseable_uppercase_value_falls_through_to_the_lowercase_one() {
        let e = resolve(&[
            ("HTTP_PROXY", "ftp://nope:1"),
            ("http_proxy", "http://ok:1"),
            ("HTTPS_PROXY", "gopher://nope:2"),
            ("https_proxy", "http://ok:2"),
        ]);
        assert_eq!(e.http.as_deref(), Some("http://ok:1"));
        assert_eq!(e.https.as_deref(), Some("http://ok:2"));
    }

    #[test]
    fn an_unparseable_value_with_no_fallback_is_no_proxy() {
        assert!(resolve(&[("HTTP_PROXY", "ftp://nope:1")]).is_empty());
    }

    /// The deliberate mirror of 0.11.27, and the assertion most likely to be
    /// "corrected" by a future reader: `ALL_PROXY` is applied last and
    /// **overwrites** the per-scheme names. reqwest 0.12+ reversed this and the
    /// reversal is saner — but restoring has to reproduce where this user's
    /// traffic was actually going, which with `all_proxy` set was `all:1`.
    #[test]
    fn all_proxy_is_applied_last_and_overwrites_the_per_scheme_names() {
        let e = resolve(&[("ALL_PROXY", "http://all:1")]);
        assert_eq!(e.http.as_deref(), Some("http://all:1"));
        assert_eq!(e.https.as_deref(), Some("http://all:1"));

        let e = resolve(&[("all_proxy", "http://all:1"), ("https_proxy", "http://s:2")]);
        assert_eq!(e.http.as_deref(), Some("http://all:1"));
        assert_eq!(
            e.https.as_deref(),
            Some("http://all:1"),
            "0.11.27 overwrites https_proxy with all_proxy; a per-scheme-wins \
             rule here would route this user somewhere their own configuration \
             never chose"
        );
    }

    /// And uppercase-first applies to the catch-all too, with the lowercase
    /// spelling tried only when the uppercase one is unusable.
    #[test]
    fn the_uppercase_catch_all_wins_and_an_unusable_one_falls_through() {
        let e = resolve(&[("ALL_PROXY", "http://upper:1"), ("all_proxy", "http://lower:1")]);
        assert_eq!(e.http.as_deref(), Some("http://upper:1"));

        let e = resolve(&[("ALL_PROXY", "ftp://nope:1"), ("all_proxy", "http://lower:1")]);
        assert_eq!(e.http.as_deref(), Some("http://lower:1"));
    }

    /// An exported-but-empty variable is how a shell profile disables one.
    /// Treating it as a proxy endpoint would be worse than ignoring it.
    #[test]
    fn empty_and_whitespace_values_are_not_proxies() {
        assert!(resolve(&[("HTTP_PROXY", ""), ("https_proxy", "   ")]).is_empty());
    }

    /// `no_proxy` is chosen on presence, not on content: `NoProxy::from_env`
    /// reads `NO_PROXY` and only falls back when that variable is *unset*, so
    /// an exported-but-empty one suppresses the lowercase spelling. The list
    /// itself is then rejected downstream by `from_string`.
    #[test]
    fn an_exported_but_empty_no_proxy_suppresses_the_lowercase_one() {
        let e = resolve(&[("NO_PROXY", ""), ("no_proxy", "lower.example")]);
        assert_eq!(e.no_proxy.as_deref(), Some(""));
    }

    /// Whatever `main.rs` captured must survive the round trip unchanged; the
    /// default is empty, so a process that never scrubbed restores nothing.
    #[test]
    fn the_capture_defaults_to_empty_and_resolves_to_nothing() {
        assert!(system_proxy_from_env(scrubbed_env(), parses).is_empty());
    }

    /// Both spellings, because the consumers disagree about which they read:
    /// libcurl takes lowercase `http_proxy` only but uppercase for the rest,
    /// while reqwest and libproxy read either. Scrubbing one case leaves the
    /// other live.
    #[test]
    fn env_var_list_covers_both_cases_of_all_four_names() {
        for name in ["http_proxy", "https_proxy", "all_proxy", "no_proxy"] {
            assert!(PROXY_ENV_VARS.contains(&name), "missing {name}");
            let upper = name.to_uppercase();
            assert!(PROXY_ENV_VARS.contains(&upper.as_str()), "missing {upper}");
        }
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
