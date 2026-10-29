use std::time::Duration;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Spawn an axum server that shuts down gracefully when `cancel` fires.
/// The returned task completes only after the listener is closed and all
/// in-flight connections have drained — await it before rebinding the same
/// address, or the bind races the old socket teardown (EADDRINUSE).
pub fn spawn_with_shutdown(
    listener: TcpListener,
    app: axum::Router,
    cancel: CancellationToken,
    name: &'static str,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app)
            .with_graceful_shutdown(cancel.cancelled_owned())
            .await
        {
            log::error!("{name} server exited with error: {e}");
        }
        log::info!("{name} server shut down");
    })
}

/// Cancel the server and wait for it to drain, bounded. A client dribbling
/// bytes can hold graceful drain open forever — after the timeout, abort:
/// the listener was already dropped at the shutdown signal, so the port is
/// free for rebind either way.
pub async fn shutdown_bounded(cancel: CancellationToken, task: JoinHandle<()>, name: &'static str) {
    cancel.cancel();
    let mut task = task;
    match tokio::time::timeout(Duration::from_secs(3), &mut task).await {
        Ok(Err(e)) => log::error!("{name} server task failed: {e}"),
        Ok(Ok(())) => {}
        Err(_) => {
            log::warn!("{name} server drain timed out; aborting");
            task.abort();
            let _ = (&mut task).await;
        }
    }
}

/// True when an HTTP `Host` header names this machine by loopback address or
/// `localhost`. A page on another origin can make the browser resolve its own
/// name to 127.0.0.1 (DNS rebinding) and then read our responses as
/// same-origin; such requests still carry the attacker's host name here.
pub fn is_loopback_host_header(host: &str) -> bool {
    let host = host.trim();
    // `[::1]:5578`, `127.0.0.1:5578`, `localhost:5578`, or without a port.
    let name = if let Some(rest) = host.strip_prefix('[') {
        match rest.split_once(']') {
            Some((inner, tail)) if tail.is_empty() || tail.starts_with(':') => inner,
            _ => return false,
        }
    } else {
        host.rsplit_once(':').map_or(host, |(h, _)| h)
    };
    name.eq_ignore_ascii_case("localhost")
        || name
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// axum middleware: refuse any request whose `Host` is not loopback (403).
pub async fn require_loopback_host(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let ok = req
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|h| h.to_str().ok())
        .is_some_and(is_loopback_host_header);
    if ok {
        next.run(req).await
    } else {
        axum::response::IntoResponse::into_response(axum::http::StatusCode::FORBIDDEN)
    }
}

/// At most `max` bytes of `s`, cut back to a char boundary. For quoting
/// response bodies in errors and logs: `&s[..max]` panics when a multi-byte
/// character straddles the cut.
pub fn clip(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod clip_tests {
    use super::{clip, is_loopback_host_header};

    #[test]
    fn host_header_must_name_loopback() {
        for ok in [
            "127.0.0.1:5578",
            "127.0.0.1",
            "localhost:5577",
            "LOCALHOST",
            "[::1]:5578",
            "[::1]",
            "127.8.9.1:1",
        ] {
            assert!(is_loopback_host_header(ok), "{ok}");
        }
        for bad in [
            "evil.example:5578",
            "evil.example",
            "0.0.0.0:5578",
            "192.168.1.2:5578",
            "localhost.evil.example",
            "[::1]x",
            "",
            "127.0.0.1.nip.io:5578",
        ] {
            assert!(!is_loopback_host_header(bad), "{bad}");
        }
    }

    #[test]
    fn clip_never_splits_a_character() {
        let s = format!("{}é tail", "a".repeat(499)); // 'é' spans bytes 499..501
        assert_eq!(clip(&s, 500), "a".repeat(499));
        assert_eq!(clip("short", 500), "short");
        assert_eq!(clip("日本語", 4), "日");
        assert_eq!(clip("", 3), "");
    }
}
