//! The MCP server's agent-facing rules, over real HTTP on an ephemeral loopback port: bearer
//! token, read-only mode, `confirm` for destructive tools, tool visibility.

use slate_core::mcp::policy::Policy;
use slate_core::{AppHandle, EventSink, Events};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct NoEvents;
impl EventSink for NoEvents {
    fn emit(&self, _event: &str, _payload: serde_json::Value) {}
}

const TOKEN: &str = "feedfacefeedfacefeedfacefeedface";
const INIT: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"Hermes","version":"1"}}}"#;

struct Reply {
    status: u16,
    session: Option<String>,
    body: String,
}

async fn send(port: u16, session: Option<&str>, bearer: Option<&str>, body: &str) -> Reply {
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect");
    let mut head = format!(
        "POST /{TOKEN}/mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(s) = session {
        head.push_str(&format!("Mcp-Session-Id: {s}\r\n"));
    }
    if let Some(b) = bearer {
        head.push_str(&format!("Authorization: Bearer {b}\r\n"));
    }
    head.push_str("\r\n");
    s.write_all(head.as_bytes()).await.expect("write");
    s.write_all(body.as_bytes()).await.expect("write");
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(3), s.read_to_end(&mut buf)).await;
    let text = String::from_utf8_lossy(&buf).into_owned();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let session = text.lines().find_map(|l| {
        l.to_ascii_lowercase()
            .strip_prefix("mcp-session-id:")
            .map(|v| v.trim().to_string())
    });
    Reply {
        status,
        session,
        body: text,
    }
}

async fn start() -> (slate_core::mcp::McpHandle, u16) {
    let app = AppHandle::new(Events::new(Arc::new(NoEvents)));
    let handle = slate_core::mcp::start_server(app, 0, TOKEN.to_string())
        .await
        .expect("start");
    let port = handle
        .url()
        .rsplit_once(':')
        .and_then(|(_, rest)| rest.split('/').next()?.parse::<u16>().ok())
        .expect("port");
    (handle, port)
}

async fn session(port: u16, bearer: Option<&str>) -> String {
    let r = send(port, None, bearer, INIT).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let id = r.session.expect("session id");
    let _ = send(
        port,
        Some(&id),
        bearer,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    )
    .await;
    id
}

fn call(id: u32, name: &str, args: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"{name}","arguments":{args}}}}}"#
    )
}

#[tokio::test]
async fn a_bearer_token_is_required_once_one_is_set() {
    let (handle, port) = start().await;
    let rt = handle.runtime();
    assert_eq!(send(port, None, None, INIT).await.status, 200);
    rt.set_bearer("sekret".into());
    assert_eq!(send(port, None, None, INIT).await.status, 401);
    assert_eq!(send(port, None, Some("wrong"), INIT).await.status, 401);
    assert_eq!(send(port, None, Some("sekret"), INIT).await.status, 200);
    rt.set_bearer(String::new());
    assert_eq!(send(port, None, None, INIT).await.status, 200);
    handle.shutdown().await;
}

#[tokio::test]
async fn read_only_mode_hides_and_rejects_every_mutating_tool() {
    let (handle, port) = start().await;
    let rt = handle.runtime();
    let id = session(port, None).await;
    let list = |n: u32| format!(r#"{{"jsonrpc":"2.0","id":{n},"method":"tools/list"}}"#);

    let all = send(port, Some(&id), None, &list(2)).await.body;
    assert!(
        all.contains("add_to_queue") && all.contains("search_tracks") && all.contains("batch"),
        "{all}"
    );

    rt.set_policy(Policy {
        read_only: true,
        ask_destructive: true,
    });
    let ro = send(port, Some(&id), None, &list(3)).await.body;
    assert!(
        ro.contains("search_tracks") && ro.contains("get_queue"),
        "{ro}"
    );
    for hidden in [
        "add_to_queue",
        "play_tracks",
        "delete_playlist",
        "clear_queue",
        "batch",
    ] {
        assert!(
            !ro.contains(&format!("\"{hidden}\"")),
            "{hidden} is visible in read-only mode"
        );
    }
    let rejected = send(
        port,
        Some(&id),
        None,
        &call(4, "add_to_queue", r#"{"track_ids":[1]}"#),
    )
    .await
    .body;
    assert!(rejected.contains("read-only"), "{rejected}");
    handle.shutdown().await;
}

#[tokio::test]
async fn destructive_tools_only_describe_without_confirm() {
    let (handle, port) = start().await;
    let id = session(port, None).await;
    for (tool, args) in [
        ("clear_queue", "{}"),
        ("delete_playlist", r#"{"playlist_uuid":"abc"}"#),
        (
            "remove_track_from_playlist",
            r#"{"playlist_uuid":"abc","index":0}"#,
        ),
        ("dedupe_playlist", r#"{"playlist_uuid":"abc"}"#),
        (
            "reorder_playlist",
            r#"{"playlist_uuid":"abc","from_index":0,"to_index":1}"#,
        ),
    ] {
        let r = send(port, Some(&id), None, &call(5, tool, args)).await.body;
        assert!(
            r.contains(r#"done\":false"#) || r.contains(r#""done":false"#),
            "{tool}: {r}"
        );
        assert!(r.contains("confirm"), "{tool}: {r}");
    }
    handle.shutdown().await;
}

#[tokio::test]
async fn an_unknown_tool_is_refused_and_batch_cannot_nest() {
    let (handle, port) = start().await;
    let rt = handle.runtime();
    rt.set_policy(Policy {
        read_only: true,
        ask_destructive: true,
    });
    let id = session(port, None).await;
    let r = send(
        port,
        Some(&id),
        None,
        &call(6, "batch", r#"{"calls":[{"tool":"pause"}]}"#),
    )
    .await
    .body;
    assert!(
        r.contains("read-only") || r.contains("not found") || r.contains("error"),
        "{r}"
    );
    handle.shutdown().await;
}

#[tokio::test]
async fn batch_runs_in_order_and_stops_on_the_first_error() {
    let (handle, port) = start().await;
    let id = session(port, None).await;
    let calls = r#"{"calls":[{"tool":"no_such_tool"},{"tool":"also_missing"}]}"#;
    let stopped = send(port, Some(&id), None, &call(7, "batch", calls))
        .await
        .body;
    assert!(
        stopped.contains("skipped") && stopped.contains("stopped_early"),
        "{stopped}"
    );
    let go_on =
        r#"{"calls":[{"tool":"no_such_tool"},{"tool":"also_missing"}],"stop_on_error":false}"#;
    let all = send(port, Some(&id), None, &call(8, "batch", go_on))
        .await
        .body;
    assert!(!all.contains("skipped"), "{all}");
    assert!(all.contains("Unknown tool"), "{all}");
    let empty = send(port, Some(&id), None, &call(9, "batch", r#"{"calls":[]}"#))
        .await
        .body;
    assert!(empty.contains("1 to 20"), "{empty}");
    handle.shutdown().await;
}

#[tokio::test]
async fn resources_can_be_listed_and_subscribed_to() {
    let (handle, port) = start().await;
    let id = session(port, None).await;
    let list = send(
        port,
        Some(&id),
        None,
        r#"{"jsonrpc":"2.0","id":10,"method":"resources/list"}"#,
    )
    .await
    .body;
    for uri in ["slate://now-playing", "slate://queue", "slate://events"] {
        assert!(list.contains(uri), "{uri}: {list}");
    }
    assert!(!handle.runtime().any_subscribers());
    let sub = send(port, Some(&id), None, r#"{"jsonrpc":"2.0","id":11,"method":"resources/subscribe","params":{"uri":"slate://queue"}}"#).await;
    assert_eq!(sub.status, 200, "{}", sub.body);
    assert!(handle.runtime().any_subscribers());
    let bad = send(port, Some(&id), None, r#"{"jsonrpc":"2.0","id":12,"method":"resources/subscribe","params":{"uri":"slate://nope"}}"#).await.body;
    assert!(bad.contains("error"), "{bad}");
    handle.shutdown().await;
}

struct Recorder(std::sync::Mutex<Vec<(String, serde_json::Value)>>);
impl EventSink for Recorder {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        if let Ok(mut g) = self.0.lock() {
            g.push((event.to_string(), payload));
        }
    }
}

#[tokio::test]
async fn a_confirmed_destructive_call_waits_for_the_user() {
    let rec = Arc::new(Recorder(std::sync::Mutex::new(Vec::new())));
    let app = AppHandle::new(Events::new(rec.clone()));
    let handle = slate_core::mcp::start_server(app, 0, TOKEN.to_string())
        .await
        .expect("start");
    let port = handle
        .url()
        .rsplit_once(':')
        .and_then(|(_, rest)| rest.split('/').next()?.parse::<u16>().ok())
        .expect("port");
    let id = session(port, None).await;
    let rt = handle.runtime();
    let pending = tokio::spawn(async move {
        send(
            port,
            Some(&id),
            None,
            &call(20, "clear_queue", r#"{"confirm":true}"#),
        )
        .await
        .body
    });
    // The dialog request reaches the app with the agent's name.
    let mut request_id = None;
    for _ in 0..100 {
        if let Some((_, p)) = rec
            .0
            .lock()
            .expect("lock")
            .iter()
            .find(|(e, _)| e == "mcp:approval-request")
        {
            assert_eq!(p["client"], "Hermes");
            request_id = p["id"].as_u64();
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let request_id = request_id.expect("approval request emitted");
    assert!(rt.answer(request_id, false));
    let body = pending.await.expect("task");
    assert!(body.contains("did not allow"), "{body}");
    handle.shutdown().await;
}
