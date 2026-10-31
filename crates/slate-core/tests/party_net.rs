#![cfg(feature = "party")]
//! A real host and guest over loopback QUIC (relays off): join, sync messages, suggestions,
//! kick, wrong ticket, party full and end.

use std::time::Duration;

use slate_core::party::net::{Event, Guest, Host};
use slate_core::party::proto::HostMsg;
use slate_core::party::requests::{Policy, Reject, RequestMode, RequestTrack, Submitted};
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
    let host = Host::start(Some("none"), "Lolo", Policy::default(), None, htx)
        .await
        .expect("host");
    host.track_changed(42, 1_000, true);
    host.queue_changed(&[1, 2, 3, 4, 5, 6, 7], &[]);

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
    assert!(matches!(q, Event::Host(HostMsg::Queue { next, .. }) if next == vec![1, 2, 3, 4, 5]));

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
    assert!(matches!(sug, Event::Suggestion { guest, track_id: 77, .. } if guest == "Ann"));

    let old_link = host.link();
    assert!(host.kick("Ann"));
    let closed = until(&mut grx, |e| matches!(e, Event::Closed(_))).await;
    assert!(matches!(closed, Event::Closed(r) if r.contains("removed")));
    drop(guest);
    // The kicked guest's link no longer works; the new one does.
    assert_ne!(host.link(), old_link);
    let (gtx2, _g2) = mpsc::unbounded_channel();
    assert!(Guest::join(&old_link, "Ann", Some("none"), gtx2.clone())
        .await
        .is_err());
    assert!(Guest::join(&host.link(), "Bob", Some("none"), gtx2)
        .await
        .is_ok());
    host.end().await;
}

#[tokio::test]
async fn a_wrong_or_stale_ticket_is_refused() {
    let (htx, _hrx) = mpsc::unbounded_channel();
    let host = Host::start(Some("none"), "Lolo", Policy::default(), None, htx)
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
    let host = Host::start(Some("none"), "Lolo", Policy::default(), None, htx)
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

fn rt(id: u64) -> RequestTrack {
    RequestTrack {
        id,
        title: format!("Song {id}"),
        artist: "Artist".into(),
        quality: "LOSSLESS".into(),
        explicit: false,
    }
}

#[tokio::test]
async fn requests_are_capped_deduplicated_and_answered() {
    let (htx, mut hrx) = mpsc::unbounded_channel();
    let policy = Policy {
        cap: 2,
        ..Policy::default()
    };
    let host = Host::start(Some("none"), "Lolo", policy, None, htx)
        .await
        .expect("host");
    host.queue_changed(&[9], &[]);
    let (gtx, mut grx) = mpsc::unbounded_channel();
    let (guest, _) = Guest::join(&host.link(), "Ann", Some("none"), gtx)
        .await
        .expect("join");
    guest.suggest(1).await.expect("send");
    let sug = until(&mut hrx, |e| matches!(e, Event::Suggestion { .. })).await;
    let Event::Suggestion { guest_id, .. } = sug else {
        panic!()
    };
    assert!(matches!(
        host.submit(guest_id, rt(1)),
        Ok(Submitted::Pending(_))
    ));
    assert!(matches!(
        host.submit(guest_id, rt(2)),
        Ok(Submitted::Pending(_))
    ));
    assert_eq!(host.submit(guest_id, rt(3)), Err(Reject::Cap(2)));
    assert_eq!(host.submit(guest_id, rt(9)), Err(Reject::AlreadyQueued));
    assert_eq!(host.pending().len(), 2);

    // The guest is told why the third one was refused.
    let refused = until(&mut grx, |e| {
        matches!(e, Event::Host(HostMsg::RequestResult { ok: false, .. }))
    })
    .await;
    assert!(
        matches!(refused, Event::Host(HostMsg::RequestResult { track_id: 3, reason, .. }) if reason.contains("2 requests"))
    );

    // Approving tells the guest and empties the slot.
    let first = host.pending()[0].0.id;
    assert!(host.decide(first, true).is_some());
    assert_eq!(host.pending().len(), 1);
    assert_eq!(host.decide_all(false).len(), 1);
    host.end().await;
}

#[tokio::test]
async fn requests_off_and_auto_modes_and_guest_unavailability() {
    let (htx, _hrx) = mpsc::unbounded_channel();
    let host = Host::start(Some("none"), "Lolo", Policy::default(), Some(50), htx)
        .await
        .expect("host");
    let (gtx, _grx) = mpsc::unbounded_channel();
    let (guest, welcome) = Guest::join(&host.link(), "Ann", Some("none"), gtx)
        .await
        .expect("join");
    assert!(matches!(welcome, HostMsg::Welcome { vote_skip: true, ref you, .. } if you == "Ann"));
    let id = 1; // first guest of this party
    host.set_policy(Policy {
        mode: RequestMode::Off,
        ..Policy::default()
    });
    assert_eq!(host.submit(id, rt(1)), Err(Reject::Off));
    host.set_policy(Policy {
        mode: RequestMode::Auto,
        ..Policy::default()
    });
    assert!(matches!(host.submit(id, rt(2)), Ok(Submitted::Approved(_))));
    host.set_policy(Policy::default());
    assert!(matches!(host.submit(id, rt(3)), Ok(Submitted::Pending(_))));
    guest.unavailable(3).await.expect("send");
    for _ in 0..50 {
        if host.unavailable_for(3) == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(host.unavailable_for(3), 1);
    host.end().await;
}

#[tokio::test]
async fn a_cohost_gets_the_requests_and_a_plain_guest_cannot_skip() {
    let (htx, mut hrx) = mpsc::unbounded_channel();
    let host = Host::start(Some("none"), "Lolo", Policy::default(), None, htx)
        .await
        .expect("host");
    let (atx, mut arx) = mpsc::unbounded_channel();
    let (ann, _) = Guest::join(&host.link(), "Ann", Some("none"), atx)
        .await
        .expect("join");
    let (btx, _brx) = mpsc::unbounded_channel();
    let (bob, _) = Guest::join(&host.link(), "Bob", Some("none"), btx)
        .await
        .expect("join");
    assert_eq!(host.successor().as_deref(), Some("Ann"));
    assert!(host.set_cohost("Bob", true));
    assert_eq!(host.successor().as_deref(), Some("Bob"));
    assert!(host.submit(1, rt(5)).is_ok());

    // Not a co-host: ignored.
    ann.skip().await.expect("send");
    // Co-host: reaches the host app.
    bob.skip().await.expect("send");
    let e = until(&mut hrx, |e| {
        matches!(e, Event::CohostSkip(_) | Event::CohostDecide { .. })
    })
    .await;
    assert_eq!(e, Event::CohostSkip("Bob".into()));
    // Ann never saw the request list.
    while let Ok(e) = arx.try_recv() {
        assert!(!matches!(e, Event::Host(HostMsg::Requests { .. })));
    }
    host.end().await;
}

#[tokio::test]
async fn a_1_2_guest_is_refused_with_a_clear_message() {
    let (htx, _hrx) = mpsc::unbounded_channel();
    let host = Host::start(Some("none"), "Lolo", Policy::default(), None, htx)
        .await
        .expect("host");
    let (tx, _rx) = mpsc::unbounded_channel();
    // Slate 1.2 spoke protocol 1; it shows the host's reason as is.
    let err = Guest::join_with_protocol(&host.link(), "x", Some("none"), tx, 1)
        .await
        .err()
        .expect("refused");
    assert!(err.contains("host is on a newer version"), "{err}");
    // The refusal does not disturb the party.
    let (tx, _rx) = mpsc::unbounded_channel();
    assert!(Guest::join(&host.link(), "y", Some("none"), tx)
        .await
        .is_ok());
    host.end().await;
}

#[tokio::test]
async fn a_flooding_guest_is_cut_off_and_stray_reports_are_not_kept() {
    let (htx, mut hrx) = mpsc::unbounded_channel();
    let host = Host::start(Some("none"), "Lolo", Policy::default(), None, htx)
        .await
        .expect("host");
    let (gtx, _grx) = mpsc::unbounded_channel();
    let (guest, _) = Guest::join(&host.link(), "Ann", Some("none"), gtx)
        .await
        .expect("join");
    until(&mut hrx, |e| matches!(e, Event::GuestJoined(_))).await;
    // Reports for tracks the party is not dealing with are dropped without a word.
    for t in 1..=30u64 {
        guest.unavailable(10_000 + t).await.expect("send");
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(host.unavailable_for(10_001), 0);
    while let Ok(e) = hrx.try_recv() {
        assert_ne!(e, Event::RequestsChanged, "stray reports are not passed on");
    }
    // Past the flood limit the host drops the connection.
    for t in 1..=80u64 {
        if guest.unavailable(20_000 + t).await.is_err() {
            break;
        }
    }
    let left = until(&mut hrx, |e| matches!(e, Event::GuestLeft(_))).await;
    assert_eq!(left, Event::GuestLeft("Ann".into()));
    assert!(host.guest_names().is_empty());
    host.end().await;
}
