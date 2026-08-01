//! Resource-guard integration tests: size limits, binary frames, deadlines, global
//! connection quota, per-IP rate limits, and bounded-queue disconnects.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;

use clipboard_core::protocol::Frame;
use clipboard_server::{Limits, ServerConfig, TestHooks};
use tokio::sync::Semaphore;

#[tokio::test]
async fn binary_frames_are_rejected() {
    let server = start_default_server().await;
    let alice = TestIdentity::generate();
    let mut conn = Client::connect(server.addr).await;
    let _ = conn.hello(&alice).await;
    conn.send_binary(b"\x00\x01\x02".to_vec()).await;
    conn.expect_error_close("bad_frame").await;
    conn.expect_closed().await;
}

#[tokio::test]
async fn oversize_message_is_rejected_at_message_level() {
    let server = start_default_server().await;
    let alice = TestIdentity::generate();
    let mut conn = Client::connect(server.addr).await;
    let _ = conn.hello(&alice).await;
    // 24 MiB + 1 byte of valid UTF-8 text: rejected by max_message_size.
    let oversize = "A".repeat(24 * 1024 * 1024 + 1);
    conn.send_raw_text(oversize).await;
    conn.expect_closed().await;
}

#[tokio::test]
async fn message_just_under_limit_still_reaches_decoding() {
    let server = start_default_server().await;
    let alice = TestIdentity::generate();
    let mut conn = Client::connect(server.addr).await;
    let _ = conn.hello(&alice).await;
    // Well under 24 MiB but not a valid protocol frame: bad_frame, not a size error.
    conn.send_raw_text("A".repeat(1024)).await;
    conn.expect_error_close("bad_frame").await;
    conn.expect_closed().await;
}

#[tokio::test]
async fn handshake_deadline_closes_silent_connections() {
    let server = start_server(ServerConfig {
        limits: Limits {
            handshake_deadline: Duration::from_millis(300),
            ..Limits::default()
        },
        ..ServerConfig::default()
    })
    .await;
    let mut conn = Client::connect(server.addr).await;
    conn.expect_closed().await;
}

#[tokio::test]
async fn join_idle_deadline_closes_after_hello() {
    let server = start_server(ServerConfig {
        limits: Limits {
            join_idle_deadline: Duration::from_millis(300),
            ..Limits::default()
        },
        ..ServerConfig::default()
    })
    .await;
    let alice = TestIdentity::generate();
    let mut conn = Client::connect(server.addr).await;
    let _ = conn.hello(&alice).await;
    conn.expect_closed().await;
}

#[tokio::test]
async fn global_connection_quota_returns_503() {
    let server = start_server(ServerConfig {
        limits: Limits {
            max_connections: 1,
            ..Limits::default()
        },
        ..ServerConfig::default()
    })
    .await;
    let _first = Client::connect(server.addr).await;
    let result = tokio_tungstenite::connect_async(format!("ws://{}/ws", server.addr)).await;
    assert!(result.is_err(), "second connection must be refused");
}

#[tokio::test]
async fn per_ip_join_attempts_are_capped() {
    let server = start_server(ServerConfig {
        limits: Limits {
            join_attempts_per_minute: 3,
            ..Limits::default()
        },
        ..ServerConfig::default()
    })
    .await;
    let alice = TestIdentity::generate();
    let bob = TestIdentity::generate();
    let room = clipboard_core::crypto::room_id(&alice.bundle, &bob.bundle); // unregistered

    for attempt in 1..=3 {
        let mut conn = Client::connect(server.addr).await;
        let nonce = conn.hello(&alice).await;
        conn.send(&alice.join_frame(&room, &nonce)).await;
        conn.expect_error_close("bad_auth").await;
        conn.expect_closed().await;
        let _ = attempt;
    }
    let mut conn = Client::connect(server.addr).await;
    let nonce = conn.hello(&alice).await;
    conn.send(&alice.join_frame(&room, &nonce)).await;
    conn.expect_error_close("rate_limited").await;
    conn.expect_closed().await;
}

#[tokio::test]
async fn per_ip_byte_budget_is_capped() {
    let server = start_server(ServerConfig {
        limits: Limits {
            bytes_per_minute: 700,
            ..Limits::default()
        },
        ..ServerConfig::default()
    })
    .await;
    let alice = TestIdentity::generate();
    let mut conn = Client::connect(server.addr).await;
    let hello_len = clipboard_core::protocol::encode_frame(&alice.hello_frame())
        .unwrap()
        .len();
    assert!(
        hello_len < 700,
        "hello must fit the test byte budget, got {hello_len}"
    );
    let _ = conn.hello(&alice).await;
    // Push the window over the budget.
    conn.send_raw_text("B".repeat(700)).await;
    conn.expect_error_close("rate_limited").await;
    conn.expect_closed().await;
}

#[tokio::test]
async fn slow_consumer_is_disconnected_on_outbox_overflow() {
    // Stall every writer after the four join-phase frames (join_ok + bootstrap for
    // each of the two members), shrink the outbox to two frames, then overflow it.
    let writer_gate = Arc::new(Semaphore::new(4));
    let server = start_server(ServerConfig {
        limits: Limits {
            outbox_max_frames: 2,
            ..Limits::default()
        },
        hooks: TestHooks {
            writer_gate: Some(writer_gate),
            ..TestHooks::default()
        },
        ..ServerConfig::default()
    })
    .await;
    let alice = TestIdentity::generate();
    let bob = TestIdentity::generate();
    let room = register_room(&server, &alice, &bob);

    let mut alice_conn = Client::connect(server.addr).await;
    alice_conn.join_live(&alice, &room).await;
    let mut bob_conn = Client::connect(server.addr).await;
    bob_conn.join_live(&bob, &room).await;

    // Both writers are now stalled. Fill bob's outbox (2 frames) and overflow it.
    // Bob's writer may flush a couple of clips before stalling for good, so drain
    // until the forced close arrives.
    for index in 0..5 {
        alice_conn
            .send(&Client::clip_frame(
                &room,
                &b64(format!("clip-{index}").as_bytes()),
            ))
            .await;
    }
    let mut early_clips = 0;
    loop {
        match bob_conn.read(TEST_TIMEOUT).await {
            Read::Frame(Frame::Clip { .. }) => early_clips += 1,
            Read::Closed => break,
            other => panic!("expected clips then close, got {other:?}"),
        }
    }
    assert!(
        early_clips <= 2,
        "bounded outbox leaked {early_clips} clips before close"
    );
    // Alice is unaffected and bob now counts as offline: the next clip is mailboxed.
    alice_conn
        .send(&Client::clip_frame(&room, &b64(b"after-overflow")))
        .await;
    server.mailbox.wait_pending(1).await;
}

#[tokio::test]
async fn sender_is_disconnected_on_room_inbox_overflow() {
    // Gate the actor after both joins, shrink the inbox to two events, then flood it.
    let event_gate = Arc::new(Semaphore::new(2));
    let server = start_server(ServerConfig {
        limits: Limits {
            inbox_max_events: 2,
            ..Limits::default()
        },
        hooks: TestHooks {
            room_event_gate: Some(event_gate),
            ..TestHooks::default()
        },
        ..ServerConfig::default()
    })
    .await;
    let alice = TestIdentity::generate();
    let bob = TestIdentity::generate();
    let room = register_room(&server, &alice, &bob);

    let mut alice_conn = Client::connect(server.addr).await;
    alice_conn.join_live(&alice, &room).await;
    let mut bob_conn = Client::connect(server.addr).await;
    bob_conn.join_live(&bob, &room).await;

    // Both gate permits are consumed; the actor is stalled on the next event.
    // The inbox holds 2 events; the 3rd+ clip overflows it.
    for index in 0..6 {
        alice_conn
            .send(&Client::clip_frame(
                &room,
                &b64(format!("flood-{index}").as_bytes()),
            ))
            .await;
    }
    alice_conn.expect_error_close("inbox_overflow").await;
    alice_conn.expect_closed().await;
}
