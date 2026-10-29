//! The MCP server's only credential is the token in its URL path. These tests
//! talk raw HTTP to a real server on an ephemeral loopback port.

use slate_core::{AppHandle, EventSink, Events};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct NoEvents;
impl EventSink for NoEvents {
    fn emit(&self, _event: &str, _payload: serde_json::Value) {}
}

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#;

async fn post(port: u16, path: &str, host: &str) -> String {
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{INITIALIZE}",
        INITIALIZE.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut buf)).await;
    String::from_utf8_lossy(&buf).into_owned()
}

#[tokio::test]
async fn mcp_requires_the_exact_token_and_a_loopback_host() {
    let app = AppHandle::new(Events::new(Arc::new(NoEvents)));
    let handle = slate_core::mcp::start_server(app, 0, TOKEN.to_string())
        .await
        .unwrap();
    let port = handle
        .url()
        .rsplit_once(':')
        .and_then(|(_, rest)| rest.split('/').next()?.parse::<u16>().ok())
        .unwrap();
    assert!(handle.url().starts_with("http://127.0.0.1:"));
    let host = format!("127.0.0.1:{port}");

    let ok = post(port, &format!("/{TOKEN}/mcp"), &host).await;
    assert!(ok.starts_with("HTTP/1.1 200"), "valid token rejected: {ok}");

    for path in [
        "/mcp".to_string(),
        "/".to_string(),
        "/wrong/mcp".to_string(),
        format!("/{}/mcp", &TOKEN[..31]),
        format!("/{TOKEN}x/mcp"),
        format!("/{}/mcp", TOKEN.to_uppercase()),
    ] {
        let res = post(port, &path, &host).await;
        assert!(res.starts_with("HTTP/1.1 404"), "{path}: {res}");
        assert!(!res.contains("serverInfo"), "{path} leaked a response");
    }

    // DNS rebinding: right token, foreign Host.
    let res = post(port, &format!("/{TOKEN}/mcp"), "attacker.example").await;
    assert!(res.starts_with("HTTP/1.1 403"), "{res}");

    handle.shutdown().await;
}
