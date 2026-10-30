//! TIDAL client behaviour against a mock server: device login, token refresh,
//! error classification and the rate-limit gate. No real network is touched.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use slate_core::proxy::HostCaps;
use slate_core::proxy_http::ProxiedHttp;
use slate_core::tidal_api::{
    as_background, ApiUrls, AuthTokens, ClientCell, TidalClient, MAX_IN_FLIGHT,
};
use slate_core::{ProxySettings, SlateError};
use wiremock::matchers::{body_string_contains, header, method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer) -> TidalClient {
    let http =
        ProxiedHttp::from_settings(&ProxySettings::default(), &HostCaps::assume_all_present());
    let base = server.uri();
    let mut c = TidalClient::new(http).with_urls(ApiUrls {
        auth: format!("{base}/auth"),
        api: format!("{base}/v1"),
        api_v2: format!("{base}/v2"),
        openapi: format!("{base}/openapi"),
    });
    c.set_credentials("client-id", "client-secret");
    c
}

fn tokens(access: &str) -> AuthTokens {
    AuthTokens {
        access_token: access.into(),
        refresh_token: "refresh-1".into(),
        expires_in: 3600,
        token_type: "Bearer".into(),
        user_id: Some(42),
    }
}

#[tokio::test]
async fn device_auth_sends_credentials_and_parses_the_code() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/auth/device_authorization"))
        .and(body_string_contains("client_id=client-id"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "deviceCode": "dev-123",
            "userCode": "ABCDE",
            "verificationUri": "link.tidal.com",
            "verificationUriComplete": "link.tidal.com/ABCDE",
            "expiresIn": 300,
            "interval": 2
        })))
        .expect(1)
        .mount(&server)
        .await;

    let resp = client(&server)
        .start_device_auth()
        .await
        .expect("device auth");
    assert_eq!(resp.user_code, "ABCDE");
    assert_eq!(resp.device_code, "dev-123");
    assert_eq!(resp.interval, 2);
}

#[tokio::test]
async fn device_auth_without_a_client_id_never_hits_the_network() {
    let server = MockServer::start().await;
    let mut c = client(&server);
    c.set_credentials("", "");
    assert!(matches!(
        c.start_device_auth().await,
        Err(SlateError::NotConfigured(_))
    ));
    assert!(server
        .received_requests()
        .await
        .unwrap_or_default()
        .is_empty());
}

#[tokio::test]
async fn polling_reports_pending_then_tokens() {
    let server = MockServer::start().await;
    // First answer: still waiting. Later answers: approved.
    Mock::given(method("POST"))
        .and(path("/auth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "status": 400, "error": "authorization_pending"
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/auth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "tok", "refresh_token": "ref", "expires_in": 3600,
            "token_type": "Bearer", "user_id": 7
        })))
        .mount(&server)
        .await;

    let mut c = client(&server);
    assert!(c
        .poll_device_token("dev-123")
        .await
        .expect("pending")
        .is_none());
    let t = c
        .poll_device_token("dev-123")
        .await
        .expect("ok")
        .expect("tokens");
    assert_eq!(t.access_token, "tok");
    assert_eq!(t.user_id, Some(7));
    assert_eq!(
        c.tokens.as_ref().map(|t| t.refresh_token.as_str()),
        Some("ref")
    );
}

#[tokio::test]
async fn polling_surfaces_real_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/auth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "expired_token"})))
        .mount(&server)
        .await;
    let err = client(&server)
        .poll_device_token("d")
        .await
        .expect_err("error");
    assert!(matches!(err, SlateError::Api { status: 400, .. }));
}

#[tokio::test]
async fn an_expired_token_is_refreshed_persisted_and_the_request_retried() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/tracks/1"))
        .and(header("authorization", "Bearer old"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"status": 401})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/auth/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=refresh-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "new", "expires_in": 3600, "token_type": "Bearer"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/tracks/1"))
        .and(header("authorization", "Bearer new"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 1, "title": "Song"})))
        .expect(1)
        .mount(&server)
        .await;

    let persisted: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = Arc::clone(&persisted);
    let mut c = client(&server);
    c.set_tokens(Some(tokens("old")));
    c.set_token_persist(Arc::new(move |t: &AuthTokens| {
        if let Ok(mut g) = sink.lock() {
            g.push(t.access_token.clone());
        }
    }));

    let track = c.get_track(1).await.expect("retry succeeds");
    assert_eq!(track["title"], "Song");
    assert_eq!(*persisted.lock().expect("lock"), vec!["new".to_string()]);
    // The refresh response had no refresh token: the old one is kept, and so is the user id.
    let t = c.tokens.expect("tokens");
    assert_eq!(
        (t.refresh_token.as_str(), t.user_id),
        ("refresh-1", Some(42))
    );
}

#[tokio::test]
async fn a_playbackinfo_401_is_not_treated_as_token_expiry() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/tracks/2"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({"status": 401, "subStatus": 4005})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/auth/token"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let mut c = client(&server);
    c.set_tokens(Some(tokens("tok")));
    let err = c.get_track(2).await.expect_err("401");
    assert!(err.is_terminal_unplayable());
}

#[tokio::test]
async fn http_errors_are_classified() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/tracks/404"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({"status": 404, "subStatus": 2001})),
        )
        .mount(&server)
        .await;
    let mut c = client(&server);
    c.set_tokens(Some(tokens("tok")));
    let err = c.get_track(404).await.expect_err("404");
    assert!(matches!(err, SlateError::Api { status: 404, .. }));
    assert!(err.is_terminal_unplayable());
    assert!(!err.is_rate_limited());
    assert!(!err.is_network());
}

#[tokio::test]
async fn a_429_opens_the_gate_so_the_next_call_sends_nothing() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/tracks/3"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "30"))
        .mount(&server)
        .await;
    let mut c = client(&server);
    c.set_tokens(Some(tokens("tok")));

    let first = c.get_track(3).await.expect_err("429");
    assert!(first.is_rate_limited());
    let second = c.get_track(3).await.expect_err("gated");
    assert!(second.is_rate_limited(), "{second:?}");
    let hits = server.received_requests().await.unwrap_or_default().len();
    assert_eq!(hits, 1, "the cooldown must stop the second request");
}

#[tokio::test]
async fn requests_without_tokens_fail_before_sending() {
    let server = MockServer::start().await;
    let mut c = client(&server);
    assert!(matches!(
        c.get_track(1).await,
        Err(SlateError::NotAuthenticated)
    ));
    assert!(server
        .received_requests()
        .await
        .unwrap_or_default()
        .is_empty());
}

#[tokio::test]
async fn an_unreachable_server_is_a_network_error() {
    // A port that was free a moment ago and has nothing listening on it.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .expect("free port");
    let base = format!("http://127.0.0.1:{port}");
    let http =
        ProxiedHttp::from_settings(&ProxySettings::default(), &HostCaps::assume_all_present());
    let mut c = TidalClient::new(http).with_urls(ApiUrls {
        auth: format!("{base}/auth"),
        api: format!("{base}/v1"),
        api_v2: format!("{base}/v2"),
        openapi: format!("{base}/openapi"),
    });
    c.set_tokens(Some(tokens("tok")));
    let err = c.get_track(1).await.expect_err("connection refused");
    assert!(err.is_network(), "{err:?}");
}

/// Library sorting: the chosen order reaches TIDAL as `order` /
/// `orderDirection`, and a playlist in its own order sends neither.
#[tokio::test]
async fn sort_order_reaches_the_track_list_requests() {
    use wiremock::matchers::query_param;
    let server = MockServer::start().await;
    let empty = json!({ "items": [], "totalNumberOfItems": 0, "limit": 100, "offset": 0 });
    Mock::given(method("GET"))
        .and(path("/v1/users/42/favorites/tracks"))
        .and(query_param("order", "NAME"))
        .and(query_param("orderDirection", "ASC"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty.clone()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/playlists/p-1/items"))
        .and(query_param("order", "LENGTH"))
        .and(query_param("orderDirection", "DESC"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty.clone()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/playlists/p-2/items"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty))
        .expect(1)
        .mount(&server)
        .await;

    let mut c = client(&server);
    c.set_tokens(Some(tokens("access-1")));
    c.get_favorite_tracks(42, 0, 100, "NAME", "ASC")
        .await
        .expect("loved, by name");
    c.get_playlist_tracks_page("p-1", 0, 100, Some("LENGTH"), Some("DESC"))
        .await
        .expect("playlist, longest first");
    c.get_playlist_tracks_page("p-2", 0, 100, None, None)
        .await
        .expect("playlist order");

    let sent = server.received_requests().await.unwrap_or_default();
    let p2 = sent
        .iter()
        .find(|r| r.url.path() == "/v1/playlists/p-2/items")
        .expect("p-2 requested");
    assert!(
        !p2.url.query().unwrap_or("").contains("order"),
        "{:?}",
        p2.url
    );
}

#[tokio::test]
async fn requests_run_at_once_up_to_the_cap() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/tracks/\d+$"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": 1}))
                .set_delay(Duration::from_millis(200)),
        )
        .expect(8)
        .mount(&server)
        .await;
    let mut c = client(&server);
    c.set_tokens(Some(tokens("tok")));
    let cell = Arc::new(ClientCell::new(c));

    let started = Instant::now();
    let mut tasks = Vec::new();
    for id in 0..8u64 {
        let cell = Arc::clone(&cell);
        tasks.push(tokio::spawn(async move {
            cell.lock().await.get_track(id).await
        }));
    }
    for t in tasks {
        t.await.expect("join").expect("request");
    }
    let took = started.elapsed();
    let rounds = 8usize.div_ceil(MAX_IN_FLIGHT) as u32;
    // Fewer rounds than one-at-a-time (8 x 200 ms), but no more than the cap allows.
    assert!(took >= Duration::from_millis(200) * rounds, "{took:?}");
    assert!(took < Duration::from_millis(1400), "{took:?}");
}

#[tokio::test]
async fn several_requests_seeing_a_401_refresh_the_token_once() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/tracks/\d+$"))
        .and(header("authorization", "Bearer old"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"status": 401})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/auth/token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({
                    "access_token": "new", "expires_in": 3600, "token_type": "Bearer"
                }))
                .set_delay(Duration::from_millis(100)),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/tracks/\d+$"))
        .and(header("authorization", "Bearer new"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 1})))
        .mount(&server)
        .await;
    let mut c = client(&server);
    c.set_tokens(Some(tokens("old")));
    let cell = Arc::new(ClientCell::new(c));

    let mut tasks = Vec::new();
    for id in 0..4u64 {
        let cell = Arc::clone(&cell);
        tasks.push(tokio::spawn(async move {
            cell.lock().await.get_track(id).await
        }));
    }
    for t in tasks {
        t.await.expect("join").expect("request");
    }
    // A client handed out afterwards already has the new token.
    let next = cell.lock().await;
    assert_eq!(
        next.tokens.as_ref().map(|t| t.access_token.as_str()),
        Some("new")
    );
}

#[tokio::test]
async fn background_requests_wait_for_the_screen_and_go_one_at_a_time() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/tracks/1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": 1}))
                .set_delay(Duration::from_millis(300)),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/tracks/[23]$"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": 2}))
                .set_delay(Duration::from_millis(100)),
        )
        .mount(&server)
        .await;
    let mut c = client(&server);
    c.set_tokens(Some(tokens("tok")));
    let cell = Arc::new(ClientCell::new(c));

    let started = Instant::now();
    let fg = {
        let cell = Arc::clone(&cell);
        tokio::spawn(async move { cell.lock().await.get_track(1).await })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut bg = Vec::new();
    for id in [2u64, 3] {
        let cell = Arc::clone(&cell);
        bg.push(tokio::spawn(async move {
            as_background(async { cell.lock().await.get_track(id).await }).await?;
            Ok::<_, SlateError>(started.elapsed())
        }));
    }
    fg.await.expect("join").expect("foreground");
    let mut done = Vec::new();
    for b in bg {
        done.push(b.await.expect("join").expect("background"));
    }
    done.sort();
    // Neither started before the foreground request finished (300 ms), and the
    // second waited for the first (100 ms each).
    assert!(done[0] >= Duration::from_millis(400), "{done:?}");
    assert!(done[1] >= done[0] + Duration::from_millis(90), "{done:?}");
}

#[tokio::test]
async fn a_client_copy_made_before_a_refresh_sends_the_new_token() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/tracks/\d+$"))
        .and(header("authorization", "Bearer old"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"status": 401})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/auth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "new", "expires_in": 3600, "token_type": "Bearer"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1/tracks/\d+$"))
        .and(header("authorization", "Bearer new"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 1})))
        .expect(2)
        .mount(&server)
        .await;
    let mut c = client(&server);
    c.set_tokens(Some(tokens("old")));
    let cell = ClientCell::new(c);

    // Taken before the refresh, used after it.
    let mut early = cell.lock().await;
    cell.lock().await.get_track(1).await.expect("refreshed");
    early.get_track(2).await.expect("sent with the new token");
}

#[tokio::test]
async fn signing_out_during_a_refresh_does_not_bring_the_tokens_back() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/tracks/1"))
        .and(header("authorization", "Bearer old"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"status": 401})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/auth/token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({
                    "access_token": "new", "expires_in": 3600, "token_type": "Bearer"
                }))
                .set_delay(Duration::from_millis(300)),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/tracks/1"))
        .and(header("authorization", "Bearer new"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 1})))
        .expect(0)
        .mount(&server)
        .await;
    let mut c = client(&server);
    c.set_tokens(Some(tokens("old")));
    let cell = Arc::new(ClientCell::new(c));

    let request = {
        let cell = Arc::clone(&cell);
        tokio::spawn(async move { cell.lock().await.get_track(1).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    cell.lock().await.sign_out().await;

    let err = request.await.expect("join").expect_err("signed out");
    assert!(matches!(err, SlateError::NotAuthenticated), "{err:?}");
    assert!(cell.lock().await.tokens.is_none());
}
