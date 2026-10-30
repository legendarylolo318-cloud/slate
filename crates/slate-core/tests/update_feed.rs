//! The update check against a mock release feed. No real network is touched.

use serde_json::json;
use slate_core::commands::updates::check_feed;
use slate_core::proxy::HostCaps;
use slate_core::proxy_http::ProxiedHttp;
use slate_core::ProxySettings;
use wiremock::matchers::{header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn http() -> ProxiedHttp {
    ProxiedHttp::from_settings(&ProxySettings::default(), &HostCaps::assume_all_present())
}

async fn feed(body: serde_json::Value, status: u16) -> (MockServer, String) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/latest"))
        .and(header_exists("user-agent"))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .expect(1)
        .mount(&server)
        .await;
    let url = format!("{}/latest", server.uri());
    (server, url)
}

#[tokio::test]
async fn a_newer_release_is_reported_with_its_page() {
    let (_server, url) = feed(
        json!({"tag_name": "v0.4.0", "html_url": "https://github.com/legendarylolo318-cloud/slate/releases/tag/v0.4.0"}),
        200,
    )
    .await;
    let info = check_feed(&http(), &url, "0.3.0").await.expect("check");
    assert!(info.available);
    assert_eq!(info.latest, "0.4.0");
    assert!(info.url.ends_with("/releases/tag/v0.4.0"));
}

#[tokio::test]
async fn the_same_or_a_pre_release_is_not_an_update() {
    let (_s1, same) = feed(json!({"tag_name": "v0.3.0"}), 200).await;
    assert!(
        !check_feed(&http(), &same, "0.3.0")
            .await
            .expect("same")
            .available
    );
    let (_s2, pre) = feed(json!({"tag_name": "v0.4.0", "prerelease": true}), 200).await;
    assert!(
        !check_feed(&http(), &pre, "0.3.0")
            .await
            .expect("pre")
            .available
    );
}

#[tokio::test]
async fn a_failing_feed_is_an_error_the_caller_can_ignore() {
    let (_server, url) = feed(json!({"message": "rate limited"}), 403).await;
    assert!(check_feed(&http(), &url, "0.3.0").await.is_err());
}

#[tokio::test]
async fn a_blocked_proxy_means_no_request_at_all() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"tag_name": "v9.0.0"})))
        .expect(0)
        .mount(&server)
        .await;
    let blocked = ProxiedHttp::from_settings(
        &ProxySettings {
            enabled: true,
            host: String::new(),
            port: 8080,
            ..ProxySettings::default()
        },
        &HostCaps::assume_all_present(),
    );
    let err = check_feed(&blocked, &format!("{}/latest", server.uri()), "0.3.0")
        .await
        .expect_err("fail closed");
    assert!(
        matches!(err, slate_core::SlateError::ProxyBlocked { .. }),
        "{err:?}"
    );
}
