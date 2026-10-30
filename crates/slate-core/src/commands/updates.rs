use crate::error::SlateError;
use crate::AppState;
use semver::Version;
use serde::Serialize;

/// Slate's own release feed. Only the latest published, non-pre-release
/// release is ever looked at, and nothing is downloaded from it.
pub const GITHUB_LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/legendarylolo318-cloud/slate/releases/latest";

/// The only place an update notice may send the browser.
const RELEASE_PAGE_PREFIX: &str = "https://github.com/legendarylolo318-cloud/slate/";
const RELEASES_PAGE: &str = "https://github.com/legendarylolo318-cloud/slate/releases";

/// Whether `url` is somewhere on Slate's own GitHub page; the only links an
/// update notice may open.
pub fn is_release_url(url: &str) -> bool {
    url.starts_with(RELEASE_PAGE_PREFIX) || url == RELEASES_PAGE
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub available: bool,
    pub current: String,
    pub latest: String,
    pub url: String,
}

/// Parse a GitHub release tag (e.g. "v0.19.0" or "0.19.0") into a semver Version.
fn parse_release_tag(tag: &str) -> Option<Version> {
    Version::parse(tag.trim().trim_start_matches('v')).ok()
}

/// True only when both versions parse, `latest_tag` is strictly newer than
/// `current`, and it is a final release (a `-rc.1` style tag never counts).
pub fn is_update_available(current: &str, latest_tag: &str) -> bool {
    match (Version::parse(current), parse_release_tag(latest_tag)) {
        (Ok(cur), Some(latest)) => latest.pre.is_empty() && latest > cur,
        _ => false,
    }
}

/// Reads a `releases/latest` body. Drafts and pre-releases are ignored, and
/// the link is only passed on when it points into Slate's own repository.
fn evaluate_release(current: &str, body: &serde_json::Value) -> UpdateInfo {
    let flag = |k: &str| body.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
    let tag = body
        .get("tag_name")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let url = body
        .get("html_url")
        .and_then(|v| v.as_str())
        .filter(|u| is_release_url(u))
        .unwrap_or(RELEASES_PAGE)
        .to_string();
    UpdateInfo {
        available: !flag("prerelease") && !flag("draft") && is_update_available(current, tag),
        current: current.to_string(),
        latest: tag.trim().trim_start_matches('v').to_string(),
        url,
    }
}

/// Check GitHub Releases for a newer version. Network/parse failures surface as
/// `SlateError`; the caller treats any failure as "no update" (silent).
///
/// GitHub is egress like any other: this goes through the shared cell, so a
/// blocked proxy means no request rather than a direct one.
pub async fn check_for_update(state: &AppState) -> Result<UpdateInfo, SlateError> {
    check_feed(
        &state.proxied_http,
        GITHUB_LATEST_RELEASE_URL,
        env!("CARGO_PKG_VERSION"),
    )
    .await
}

/// `check_for_update` against any feed URL and version (tests point it at a mock).
pub async fn check_feed(
    http: &crate::proxy_http::ProxiedHttp,
    feed: &str,
    current: &str,
) -> Result<UpdateInfo, SlateError> {
    let client = http
        .client()
        .map_err(|e| SlateError::ProxyBlocked { reason: e.cause })?;

    let body: serde_json::Value = client
        .get(feed)
        // GitHub rejects requests without a User-Agent.
        .header(reqwest::header::USER_AGENT, "Slate-update-checker")
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        // The cell's client carries a 30s timeout; the update check gives up
        // after 10s.
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    Ok(evaluate_release(current, &body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tag_with_v_prefix() {
        assert_eq!(parse_release_tag("v0.19.0"), Version::parse("0.19.0").ok());
    }

    #[test]
    fn parses_tag_without_prefix() {
        assert_eq!(parse_release_tag("0.19.0"), Version::parse("0.19.0").ok());
    }

    #[test]
    fn rejects_garbage_tag() {
        assert!(parse_release_tag("nightly").is_none());
    }

    #[test]
    fn newer_release_is_available() {
        assert!(is_update_available("0.18.1", "v0.19.0"));
    }

    #[test]
    fn same_version_is_not_available() {
        assert!(!is_update_available("0.18.1", "v0.18.1"));
    }

    #[test]
    fn older_release_is_not_available() {
        assert!(!is_update_available("0.19.0", "v0.18.1"));
    }

    #[test]
    fn patch_bump_is_available() {
        assert!(is_update_available("0.18.1", "0.18.2"));
    }

    #[test]
    fn unparseable_inputs_are_not_available() {
        assert!(!is_update_available("0.18.1", "nightly"));
        assert!(!is_update_available("not-semver", "0.19.0"));
    }

    #[test]
    fn pre_release_tags_and_flags_never_count() {
        assert!(!is_update_available("0.3.0", "v0.4.0-rc.1"));
        let body = |v: serde_json::Value| evaluate_release("0.3.0", &v);
        assert!(!body(serde_json::json!({"tag_name": "v0.4.0", "prerelease": true})).available);
        assert!(!body(serde_json::json!({"tag_name": "v0.4.0", "draft": true})).available);
        assert!(body(serde_json::json!({"tag_name": "v0.4.0"})).available);
    }

    #[test]
    fn versions_compare_as_semver_not_text() {
        assert!(is_update_available("0.9.0", "v0.10.0"));
        assert!(is_update_available("0.10.0", "v1.0.0"));
        assert!(!is_update_available("1.0.0", "v0.10.0"));
        assert!(!is_update_available("1.0.0", "v1.1.0-beta.2"));
        assert!(!is_update_available("0.10.0", "v0.9.9"));
    }

    #[test]
    fn only_links_into_slates_repository_are_passed_on() {
        let ok = evaluate_release(
            "0.3.0",
            &serde_json::json!({"tag_name": "v0.4.0", "html_url": "https://github.com/legendarylolo318-cloud/slate/releases/tag/v0.4.0"}),
        );
        assert_eq!(
            ok.url,
            "https://github.com/legendarylolo318-cloud/slate/releases/tag/v0.4.0"
        );
        for bad in [
            "https://evil.example/releases",
            "https://github.com/legendarylolo318-cloud/slate-evil/x",
            "file:///etc/passwd",
        ] {
            let info = evaluate_release(
                "0.3.0",
                &serde_json::json!({"tag_name": "v0.4.0", "html_url": bad}),
            );
            assert_eq!(info.url, RELEASES_PAGE);
        }
        assert_eq!(info_latest("v0.4.0"), "0.4.0");
    }

    fn info_latest(tag: &str) -> String {
        evaluate_release("0.3.0", &serde_json::json!({ "tag_name": tag })).latest
    }
}
