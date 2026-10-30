#![cfg(feature = "party")]
//! A real host and guest over loopback QUIC (relays off): join, sync messages, suggestions,
//! kick, wrong ticket, party full and end.

use std::time::Duration;

use slate_core::party::net::{Event, Guest, Host};
use slate_core::party::proto::HostMsg;
use tokio::sync::mpsc;

async fn next(rx: &mut mpsc::UnboundedReceiver<Event>) -> Event {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("event in time")
        .expect("channel open")
}

async fn until<F: Fn(&Event) -> bool>(rx: &mut mpsc::UnboundedReceiver<Event>, f: F) -> Event {
    loop {
        let e = next(rx).await;
        if f(&e) {
            return e;
        }
    }
}

#[tokio::test]
async fn a_guest_joins_follows_the_host_and_can_suggest() {
    let (htx, mut hrx) = mpsc::unbounded_channel();
    let host = Host::start(Some("none"), "Lolo", true, htx)
        .await
        .expect("host");
    host.track_changed(42, 1_000, true);
    host.queue_changed(&[1, 2, 3, 4, 5, 6, 7]);

    let (gtx, mut grx) = mpsc::unbounded_channel();
    let (guest, welcome) = Guest::join(&host.link(), "Ann", Some("none"), gtx)
        .await
        .expect("join");
    match welcome {
        HostMsg::Welcome {
            host,
            guests,
            state,
            allow_suggest,
            ..
        } => {
            assert_eq!(host, "Lolo");
            assert_eq!(guests, vec!["Ann"]);
            assert_eq!(state.track, Some(42));
            assert!(allow_suggest);
        }
        other => panic!("{other:?}"),
    }
    assert!(
        matches!(until(&mut hrx, |e| matches!(e, Event::GuestJoined(_))).await, Event::GuestJoined(n) if n == "Ann")
    );
    // The queue sent to a late joiner is capped at five tracks.
    let q = until(&mut grx, |e| {
        matches!(e, Event::Host(HostMsg::Queue { .. }))
    })
    .await;
    assert!(matches!(q, Event::Host(HostMsg::Queue { next }) if next == vec![1, 2, 3, 4, 5]));

    host.state_changed(5_000, false);
    let s = until(&mut grx, |e| {
        matches!(e, Event::Host(HostMsg::State { .. }))
    })
    .await;
    assert!(matches!(
        s,
        Event::Host(HostMsg::State {
            position_ms: 5_000,
            playing: false,
            ..
        })
    ));

    guest.suggest(77).await.expect("suggest");
    let sug = until(&mut hrx, |e| matches!(e, Event::Suggestion { .. })).await;
    assert_eq!(
        sug,
        Event::Suggestion {
            guest: "Ann".into(),
            track_id: 77
        }
    );

    assert!(host.kick("Ann"));
    let closed = until(&mut grx, |e| matches!(e, Event::Closed(_))).await;
    assert!(matches!(closed, Event::Closed(r) if r.contains("removed")));
    host.end().await;
}

#[tokio::test]
async fn a_wrong_or_stale_ticket_is_refused() {
    let (htx, _hrx) = mpsc::unbounded_channel();
    let host = Host::start(Some("none"), "Lolo", false, htx)
        .await
        .expect("host");
    let link = host.link();
    let (gtx, _g) = mpsc::unbounded_channel();
    assert!(
        Guest::join("slate://party/not-a-ticket", "x", Some("none"), gtx.clone())
            .await
            .is_err()
    );
    // A ticket for the right host with a different secret is rejected by the host.
    let forged = {
        let real = link.trim_start_matches("slate://party/");
        let bytes = base64_decode(real);
        let mut v: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        v["s"] = "0".repeat(64).into();
        format!(
            "slate://party/{}",
            base64_encode(&serde_json::to_vec(&v).expect("vec"))
        )
    };
    let err = Guest::join(&forged, "x", Some("none"), gtx.clone())
        .await
        .err()
        .expect("refused");
    assert!(err.contains("not valid"), "{err}");
    host.end().await;
    // The ticket is dead once the party ended.
    assert!(Guest::join(&link, "x", Some("none"), gtx).await.is_err());
}

fn base64_decode(s: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .expect("b64")
}
fn base64_encode(b: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

#[tokio::test]
async fn the_party_holds_at_most_ten_guests() {
    let (htx, _hrx) = mpsc::unbounded_channel();
    let host = Host::start(Some("none"), "Lolo", false, htx)
        .await
        .expect("host");
    let mut guests = Vec::new();
    for i in 0..10 {
        let (tx, rx) = mpsc::unbounded_channel();
        let (g, _) = Guest::join(&host.link(), &format!("g{i}"), Some("none"), tx)
            .await
            .expect("room");
        guests.push((g, rx));
    }
    let (tx, _rx) = mpsc::unbounded_channel();
    let err = Guest::join(&host.link(), "late", Some("none"), tx)
        .await
        .err()
        .expect("full");
    assert!(err.contains("full"), "{err}");
    host.end().await;
}
