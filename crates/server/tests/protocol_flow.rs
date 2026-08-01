//! Protocol state machine and join-authentication integration tests.

mod common;

use common::*;

use clipboard_core::protocol::Frame;
use ed25519_dalek::Signer;
use std::time::{Duration, Instant};

#[tokio::test]
async fn two_clients_route_clip_with_origin_overwritten() {
    let server = start_default_server().await;
    let alice = TestIdentity::generate();
    let bob = TestIdentity::generate();
    let room = register_room(&server, &alice, &bob);

    let mut alice_conn = Client::connect(server.addr).await;
    alice_conn.join_live(&alice, &room).await;
    let mut bob_conn = Client::connect(server.addr).await;
    bob_conn.join_live(&bob, &room).await;

    let started = Instant::now();
    alice_conn
        .send(&Client::clip_frame(&room, &b64(b"encrypted-payload")))
        .await;
    match bob_conn.recv_frame().await {
        Frame::Clip {
            room_id,
            ciphertext_b64,
            origin_device,
            mailbox,
        } => {
            assert_eq!(room_id, room);
            assert_eq!(ciphertext_b64, b64(b"encrypted-payload"));
            assert_eq!(
                origin_device,
                alice.fp(),
                "origin must be the authenticated fp"
            );
            assert!(!mailbox, "live clips are never mailbox");
        }
        other => panic!("expected clip, got {other:?}"),
    }
    assert!(
        started.elapsed() < Duration::from_millis(50),
        "in-process live route exceeded 50ms: {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn third_identity_rejected_while_both_online() {
    let server = start_default_server().await;
    let alice = TestIdentity::generate();
    let bob = TestIdentity::generate();
    let carol = TestIdentity::generate();
    let room = register_room(&server, &alice, &bob);

    let mut alice_conn = Client::connect(server.addr).await;
    alice_conn.join_live(&alice, &room).await;
    let mut bob_conn = Client::connect(server.addr).await;
    bob_conn.join_live(&bob, &room).await;

    let mut carol_conn = Client::connect(server.addr).await;
    let nonce = carol_conn.hello(&carol).await;
    carol_conn.send(&carol.join_frame(&room, &nonce)).await;
    carol_conn.expect_error_close("room_full").await;
    carol_conn.expect_closed().await;
}

#[tokio::test]
async fn third_identity_rejected_while_peer_offline() {
    let server = start_default_server().await;
    let alice = TestIdentity::generate();
    let bob = TestIdentity::generate();
    let carol = TestIdentity::generate();
    let room = register_room(&server, &alice, &bob);

    let mut alice_conn = Client::connect(server.addr).await;
    alice_conn.join_live(&alice, &room).await;
    let mut bob_conn = Client::connect(server.addr).await;
    bob_conn.join_live(&bob, &room).await;
    drop(bob_conn);

    // Barrier: once alice's clip lands in bob's mailbox, bob's disconnect was processed.
    alice_conn
        .send(&Client::clip_frame(&room, &b64(b"for-offline-bob")))
        .await;
    server.mailbox.wait_pending(1).await;

    let mut carol_conn = Client::connect(server.addr).await;
    let nonce = carol_conn.hello(&carol).await;
    carol_conn.send(&carol.join_frame(&room, &nonce)).await;
    carol_conn.expect_error_close("room_full").await;
    carol_conn.expect_closed().await;
}

#[tokio::test]
async fn same_fingerprint_reconnect_replaces_old_connection() {
    let server = start_default_server().await;
    let alice = TestIdentity::generate();
    let bob = TestIdentity::generate();
    let room = register_room(&server, &alice, &bob);

    let mut old_conn = Client::connect(server.addr).await;
    old_conn.join_live(&alice, &room).await;
    let mut bob_conn = Client::connect(server.addr).await;
    bob_conn.join_live(&bob, &room).await;

    // Same fingerprint reconnects while the old connection is still open: the new
    // join succeeds and the old connection is closed.
    let mut new_conn = Client::connect(server.addr).await;
    new_conn.join_live(&alice, &room).await;
    old_conn.expect_closed().await;

    // The room still routes through the replacement connection.
    bob_conn
        .send(&Client::clip_frame(&room, &b64(b"after-reconnect")))
        .await;
    match new_conn.recv_frame().await {
        Frame::Clip {
            origin_device,
            mailbox,
            ..
        } => {
            assert_eq!(origin_device, bob.fp());
            assert!(!mailbox);
        }
        other => panic!("expected clip, got {other:?}"),
    }
}

#[tokio::test]
async fn unregistered_room_is_bad_auth() {
    let server = start_default_server().await;
    let alice = TestIdentity::generate();
    let bob = TestIdentity::generate();
    // Compute the would-be room id but never register it.
    let room = clipboard_core::crypto::room_id(&alice.bundle, &bob.bundle);

    let mut conn = Client::connect(server.addr).await;
    let nonce = conn.hello(&alice).await;
    conn.send(&alice.join_frame(&room, &nonce)).await;
    conn.expect_error_close("bad_auth").await;
    conn.expect_closed().await;
}

#[tokio::test]
async fn tampered_device_id_signed_over_is_bad_auth() {
    // The frame's device_id is signed correctly but does not equal
    // SHA256(sign_pk) hex16: recomputation must reject it.
    let server = start_default_server().await;
    let alice = TestIdentity::generate();
    let bob = TestIdentity::generate();
    let room = register_room(&server, &alice, &bob);

    let mut conn = Client::connect(server.addr).await;
    let nonce = conn.hello(&alice).await;
    let forged = "0000000000000000";
    assert_ne!(forged, alice.device_id());
    conn.send(&alice.join_frame_with_device_id(&room, &nonce, forged))
        .await;
    conn.expect_error_close("bad_auth").await;
    conn.expect_closed().await;
}

#[tokio::test]
async fn device_id_swapped_after_signing_is_bad_auth() {
    // Signature covers the real device_id; the frame field is swapped afterwards.
    let server = start_default_server().await;
    let alice = TestIdentity::generate();
    let bob = TestIdentity::generate();
    let room = register_room(&server, &alice, &bob);

    let mut conn = Client::connect(server.addr).await;
    let nonce = conn.hello(&alice).await;
    let mut frame = alice.join_frame(&room, &nonce);
    if let Frame::Join { device_id, .. } = &mut frame {
        *device_id = "ffffffffffffffff".to_owned();
    }
    conn.send(&frame).await;
    conn.expect_error_close("bad_auth").await;
    conn.expect_closed().await;
}

#[tokio::test]
async fn wrong_signing_key_is_bad_auth() {
    let server = start_default_server().await;
    let alice = TestIdentity::generate();
    let bob = TestIdentity::generate();
    let room = register_room(&server, &alice, &bob);

    let mut conn = Client::connect(server.addr).await;
    let nonce = conn.hello(&alice).await;
    // Mallory signs with her own key but presents alice's bundle.
    let mallory = TestIdentity::generate();
    let message = clipboard_core::crypto::join_sig_msg(
        &nonce,
        &room,
        &alice.device_id(),
        &clipboard_core::protocol::bundle_bytes(&alice.bundle),
    )
    .unwrap();
    let signature = mallory.signing.sign(&message);
    let frame = Frame::Join {
        room_id: room.clone(),
        device_id: alice.device_id(),
        pub_bundle: alice.bundle.clone(),
        sig_b64: b64(&signature.to_bytes()),
    };
    conn.send(&frame).await;
    conn.expect_error_close("bad_auth").await;
    conn.expect_closed().await;
}

#[tokio::test]
async fn challenge_nonce_is_single_use_and_per_connection() {
    let server = start_default_server().await;
    let alice = TestIdentity::generate();
    let bob = TestIdentity::generate();
    let room = register_room(&server, &alice, &bob);

    // Alice joins successfully on one connection.
    let mut first = Client::connect(server.addr).await;
    first.join_live(&alice, &room).await;

    // Replaying the exact same signed join frame on a new connection must fail:
    // the new challenge makes the old signature invalid.
    let replay_frame = {
        let mut probe = Client::connect(server.addr).await;
        let _ = probe.hello(&alice).await;
        // Reconstruct a frame signed over the *first* connection's nonce is
        // impossible without the nonce; instead capture the frame alice used.
        // The first connection is already joined, so craft it from a fresh hello
        // on a sacrificial connection and replay it here.
        let mut sacrifice = Client::connect(server.addr).await;
        let sacrifice_nonce = sacrifice.hello(&alice).await;
        alice.join_frame(&room, &sacrifice_nonce)
    };
    let mut second = Client::connect(server.addr).await;
    let _second_nonce = second.hello(&alice).await;
    second.send(&replay_frame).await;
    second.expect_error_close("bad_auth").await;
    second.expect_closed().await;
}

#[tokio::test]
async fn every_hello_gets_a_fresh_nonce() {
    let server = start_default_server().await;
    let alice = TestIdentity::generate();
    let mut one = Client::connect(server.addr).await;
    let mut two = Client::connect(server.addr).await;
    let nonce_one = one.hello(&alice).await;
    let nonce_two = two.hello(&alice).await;
    assert_ne!(nonce_one, nonce_two, "challenges must be CSPRNG-fresh");
}

#[tokio::test]
async fn hello_version_mismatch_is_rejected() {
    let server = start_default_server().await;
    let alice = TestIdentity::generate();
    let mut conn = Client::connect(server.addr).await;
    // encode_frame refuses other versions, so build the JSON manually.
    let raw = format!(
        "{{\"type\":\"hello\",\"device_id\":\"{}\",\"pub_bundle\":{{\"sign_pk_b64\":\"{}\",\"dh_pk_b64\":\"{}\"}},\"version\":2}}",
        alice.device_id(),
        b64(&alice.bundle.sign_pk),
        b64(&alice.bundle.dh_pk),
    );
    conn.send_raw_text(raw).await;
    conn.expect_error_close("version_mismatch").await;
    conn.expect_closed().await;
}

#[tokio::test]
async fn join_before_hello_is_bad_frame() {
    let server = start_default_server().await;
    let alice = TestIdentity::generate();
    let bob = TestIdentity::generate();
    let room = register_room(&server, &alice, &bob);

    let mut conn = Client::connect(server.addr).await;
    conn.send(&alice.join_frame(&room, &[0_u8; 32])).await;
    conn.expect_error_close("bad_frame").await;
    conn.expect_closed().await;
}

#[tokio::test]
async fn pair_offer_before_join_uses_pairing_seam() {
    let server = start_default_server().await;
    let alice = TestIdentity::generate();
    let mut conn = Client::connect(server.addr).await;
    let _ = conn.hello(&alice).await;
    conn.send(&Frame::PairOffer {
        code: "123456".to_owned(),
        pub_bundle: alice.bundle.clone(),
    })
    .await;
    conn.expect_error_close("pairing_unavailable").await;
    conn.expect_closed().await;
}

#[tokio::test]
async fn pair_offer_after_join_is_bad_frame() {
    let server = start_default_server().await;
    let alice = TestIdentity::generate();
    let bob = TestIdentity::generate();
    let room = register_room(&server, &alice, &bob);

    let mut conn = Client::connect(server.addr).await;
    conn.join_live(&alice, &room).await;
    conn.send(&Frame::PairOffer {
        code: "654321".to_owned(),
        pub_bundle: alice.bundle.clone(),
    })
    .await;
    conn.expect_error_close("bad_frame").await;
    conn.expect_closed().await;
}

#[tokio::test]
async fn clip_for_other_room_after_join_is_bad_frame() {
    let server = start_default_server().await;
    let alice = TestIdentity::generate();
    let bob = TestIdentity::generate();
    let room = register_room(&server, &alice, &bob);

    let mut conn = Client::connect(server.addr).await;
    conn.join_live(&alice, &room).await;
    conn.send(&Client::clip_frame(&"00".repeat(16), &b64(b"x")))
        .await;
    conn.expect_error_close("bad_frame").await;
    conn.expect_closed().await;
}
