//! Room-actor integration tests: bootstrap interleaving (clip before join lands in
//! the bootstrap, clip after join routes live), exactly-once delivery across the
//! join boundary, stale-connection event handling, and actor restart.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;

use clipboard_core::protocol::Frame;
use clipboard_server::{Limits, ServerConfig, TestHooks};
use tokio::sync::{Semaphore, mpsc};

async fn wait_event(log_rx: &mut mpsc::UnboundedReceiver<&'static str>, label: &str) {
    tokio::time::timeout(TEST_TIMEOUT, async {
        while let Some(seen) = log_rx.recv().await {
            if seen == label {
                return;
            }
        }
        panic!("event log closed while waiting for {label}");
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn clip_before_join_arrives_in_bootstrap_latest_wins() {
    let server = start_default_server().await;
    let alice = TestIdentity::generate();
    let bob = TestIdentity::generate();
    let room = register_room(&server, &alice, &bob);

    let mut alice_conn = Client::connect(server.addr).await;
    alice_conn.join_live(&alice, &room).await;

    // Barrier: the recording mailbox proves the actor processed both clips before
    // bob's join begins.
    alice_conn
        .send(&Client::clip_frame(&room, &b64(b"old-clip")))
        .await;
    server.mailbox.wait_pending(1).await;
    alice_conn
        .send(&Client::clip_frame(&room, &b64(b"new-clip")))
        .await;
    server.mailbox.wait_pending(2).await;

    let mut bob_conn = Client::connect(server.addr).await;
    let bootstrap = bob_conn.join(&bob, &room).await;
    match bootstrap {
        Frame::Clip {
            room_id,
            ciphertext_b64,
            origin_device,
            mailbox,
        } => {
            assert_eq!(room_id, room);
            assert_eq!(
                ciphertext_b64,
                b64(b"new-clip"),
                "mailbox is pending-latest"
            );
            assert_eq!(origin_device, alice.fp());
            assert!(mailbox, "bootstrap clips carry mailbox=true");
        }
        other => panic!("expected bootstrap clip, got {other:?}"),
    }
    // Delivered exactly once: no live copy follows the bootstrap.
    assert!(matches!(
        bob_conn.read(Duration::from_millis(300)).await,
        Read::Timeout
    ));
    assert_eq!(server.mailbox.consumed.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn clip_after_join_routes_live() {
    let server = start_default_server().await;
    let alice = TestIdentity::generate();
    let bob = TestIdentity::generate();
    let room = register_room(&server, &alice, &bob);

    let mut alice_conn = Client::connect(server.addr).await;
    alice_conn.join_live(&alice, &room).await;
    let mut bob_conn = Client::connect(server.addr).await;
    // Barrier: bootstrap (mailbox_empty) is enqueued before registration, so after
    // receiving it, every later clip is guaranteed to route live.
    let bootstrap = bob_conn.join(&bob, &room).await;
    assert_eq!(bootstrap, Frame::MailboxEmpty);

    alice_conn
        .send(&Client::clip_frame(&room, &b64(b"live-clip")))
        .await;
    match bob_conn.recv_frame().await {
        Frame::Clip {
            ciphertext_b64,
            origin_device,
            mailbox,
            ..
        } => {
            assert_eq!(ciphertext_b64, b64(b"live-clip"));
            assert_eq!(origin_device, alice.fp());
            assert!(!mailbox);
        }
        other => panic!("expected live clip, got {other:?}"),
    }
    assert_eq!(
        server.mailbox.pending_count(),
        0,
        "online members never mailbox"
    );
}

#[tokio::test]
async fn join_clip_race_delivers_exactly_once() {
    for round in 0..12 {
        let server = start_default_server().await;
        let alice = TestIdentity::generate();
        let bob = TestIdentity::generate();
        let room = register_room(&server, &alice, &bob);

        let mut alice_conn = Client::connect(server.addr).await;
        alice_conn.join_live(&alice, &room).await;

        let mut bob_conn = Client::connect(server.addr).await;
        let nonce = bob_conn.hello(&bob).await;
        let payload = format!("race-{round}");
        if round % 2 == 0 {
            bob_conn.send(&bob.join_frame(&room, &nonce)).await;
            alice_conn
                .send(&Client::clip_frame(&room, &b64(payload.as_bytes())))
                .await;
        } else {
            alice_conn
                .send(&Client::clip_frame(&room, &b64(payload.as_bytes())))
                .await;
            bob_conn.send(&bob.join_frame(&room, &nonce)).await;
        }

        assert_eq!(bob_conn.recv_frame().await, Frame::JoinOk);
        let bootstrap = bob_conn.recv_frame().await;
        let mut deliveries = Vec::new();
        match bootstrap {
            Frame::Clip { .. } => deliveries.push(bootstrap),
            Frame::MailboxEmpty => {}
            other => panic!("expected bootstrap frame, got {other:?}"),
        }
        while let Read::Frame(frame) = bob_conn.read(Duration::from_millis(300)).await {
            deliveries.push(frame);
        }
        assert_eq!(
            deliveries.len(),
            1,
            "round {round}: clip must arrive exactly once"
        );
        match &deliveries[0] {
            Frame::Clip {
                ciphertext_b64,
                origin_device,
                ..
            } => {
                assert_eq!(ciphertext_b64, &b64(payload.as_bytes()));
                assert_eq!(origin_device, &alice.fp());
            }
            other => panic!("round {round}: expected clip, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn stale_clip_and_disconnect_after_rejoin_are_ignored() {
    let event_gate = Arc::new(Semaphore::new(0));
    let (log_tx, mut log_rx) = mpsc::unbounded_channel();
    let server = start_server(ServerConfig {
        limits: Limits::default(),
        hooks: TestHooks {
            room_event_gate: Some(event_gate.clone()),
            event_log: Some(log_tx),
            ..TestHooks::default()
        },
        ..ServerConfig::default()
    })
    .await;
    let alice = TestIdentity::generate();
    let bob = TestIdentity::generate();
    let room = register_room(&server, &alice, &bob);

    // Alice's first connection joins (one actor event, one permit).
    let mut conn1 = Client::connect(server.addr).await;
    let nonce1 = conn1.hello(&alice).await;
    conn1.send(&alice.join_frame(&room, &nonce1)).await;
    wait_event(&mut log_rx, "join").await;
    event_gate.add_permits(1);
    conn1.join_live_bootstrap().await;

    // Bob joins (one actor event, one permit).
    let mut bob_conn = Client::connect(server.addr).await;
    let bob_nonce = bob_conn.hello(&bob).await;
    bob_conn.send(&bob.join_frame(&room, &bob_nonce)).await;
    wait_event(&mut log_rx, "join").await;
    event_gate.add_permits(1);
    bob_conn.join_live_bootstrap().await;

    // Alice reconnects: the Join event is enqueued but the actor is gated.
    let mut conn2 = Client::connect(server.addr).await;
    let nonce2 = conn2.hello(&alice).await;
    conn2.send(&alice.join_frame(&room, &nonce2)).await;
    wait_event(&mut log_rx, "join").await;

    // The old connection emits a clip, then disappears: both events land in the
    // actor inbox AFTER the replacement Join.
    conn1
        .send(&Client::clip_frame(&room, &b64(b"stale-clip")))
        .await;
    wait_event(&mut log_rx, "clip").await;
    drop(conn1);
    wait_event(&mut log_rx, "disconnect").await;

    // Release the actor: Join(new) replaces the connection; Clip(old) and
    // Disconnect(old) are stale no-ops.
    event_gate.add_permits(3);
    assert_eq!(conn2.recv_frame().await, Frame::JoinOk);
    assert_eq!(conn2.recv_frame().await, Frame::MailboxEmpty);

    // Bob never sees the stale clip.
    assert!(matches!(
        bob_conn.read(Duration::from_millis(300)).await,
        Read::Timeout
    ));

    // The replacement connection is fully live: its clip routes to bob.
    conn2
        .send_tolerant(&Client::clip_frame(&room, &b64(b"proof-clip")))
        .await;
    wait_event(&mut log_rx, "clip").await;
    event_gate.add_permits(1);
    match bob_conn.recv_frame().await {
        Frame::Clip {
            ciphertext_b64,
            origin_device,
            mailbox,
            ..
        } => {
            assert_eq!(ciphertext_b64, b64(b"proof-clip"));
            assert_eq!(origin_device, alice.fp());
            assert!(!mailbox);
        }
        other => panic!("expected proof clip, got {other:?}"),
    }
}

#[tokio::test]
async fn room_actor_restarts_after_all_members_leave() {
    let server = start_default_server().await;
    let alice = TestIdentity::generate();
    let bob = TestIdentity::generate();
    let room = register_room(&server, &alice, &bob);

    let mut alice_conn = Client::connect(server.addr).await;
    alice_conn.join_live(&alice, &room).await;
    let mut bob_conn = Client::connect(server.addr).await;
    bob_conn.join_live(&bob, &room).await;
    drop(alice_conn);
    drop(bob_conn);

    // Give the actor a moment to process both disconnects and shut down; a fresh
    // actor must then spawn on the next join and route normally.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut alice_conn = Client::connect(server.addr).await;
    alice_conn.join_live(&alice, &room).await;
    let mut bob_conn = Client::connect(server.addr).await;
    bob_conn.join_live(&bob, &room).await;
    alice_conn
        .send(&Client::clip_frame(&room, &b64(b"after-restart")))
        .await;
    match bob_conn.recv_frame().await {
        Frame::Clip { ciphertext_b64, .. } => assert_eq!(ciphertext_b64, b64(b"after-restart")),
        other => panic!("expected clip after actor restart, got {other:?}"),
    }
}
