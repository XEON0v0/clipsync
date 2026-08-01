mod common;

use std::fs;
use std::net::{IpAddr, Ipv4Addr};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use clipboard_core::crypto::{bundle_fp, room_id};
use clipboard_core::protocol::{Frame, PubBundle, decode_frame};
use clipboard_server::pairing::{Connections, PairingConfig, PairingHandler, PairingRelay};
use clipboard_server::registry::{
    PersistentRegistry, Registry, RegistryCommit, spawn_unactivated_sweeper,
};
use clipboard_server::room::{OutboxReceiver, outbox};
use clipboard_server::{NoopMailboxSink, PairingUnavailable, ServerConfig, start};
use common::{Client, TestIdentity, eventually};
use tempfile::TempDir;
use tokio::time::timeout;

const IP: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

fn bundle(seed: u8) -> PubBundle {
    PubBundle {
        sign_pk: [seed; 32],
        dh_pk: [seed.wrapping_add(64); 32],
    }
}

fn persistent() -> (TempDir, Arc<PersistentRegistry>) {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(PersistentRegistry::open(dir.path()).unwrap());
    (dir, registry)
}

fn config() -> PairingConfig {
    PairingConfig {
        attempts_per_window: 100,
        ..PairingConfig::default()
    }
}

async fn frame(rx: &mut OutboxReceiver) -> Frame {
    match timeout(Duration::from_secs(1), rx.recv()).await.unwrap() {
        Some(clipboard_server::room::Outbound::Frame(bytes)) => decode_frame(&bytes).unwrap(),
        _ => panic!("expected frame"),
    }
}

async fn no_frame(rx: &mut OutboxReceiver) {
    match timeout(Duration::from_millis(20), rx.recv()).await {
        Err(_) | Ok(None) => {}
        Ok(Some(_)) => panic!("unexpected outbound item"),
    }
}

fn offer(
    relay: &PairingRelay,
    connections: &Connections,
    connection_id: u64,
    hello: &PubBundle,
    code: &str,
    sent: &PubBundle,
) -> Result<(), Box<Frame>> {
    let outbox = connections.get(&connection_id).unwrap().clone();
    relay.on_pair_frame(
        connection_id,
        IP,
        hello,
        Frame::PairOffer {
            code: code.to_owned(),
            pub_bundle: sent.clone(),
        },
        &outbox,
        connections,
    )
}

fn claim(
    relay: &PairingRelay,
    connections: &Connections,
    connection_id: u64,
    hello: &PubBundle,
    code: &str,
    sent: &PubBundle,
) -> Result<(), Box<Frame>> {
    let outbox = connections.get(&connection_id).unwrap().clone();
    relay.on_pair_frame(
        connection_id,
        IP,
        hello,
        Frame::PairClaim {
            code: code.to_owned(),
            pub_bundle: sent.clone(),
        },
        &outbox,
        connections,
    )
}

fn error_code(result: Result<(), Box<Frame>>) -> String {
    match result.unwrap_err().as_ref() {
        Frame::Error { code, .. } => code.clone(),
        other => panic!("expected error, got {other:?}"),
    }
}

#[test]
fn registry_persists_and_reloads_complete_room() {
    let (dir, registry) = persistent();
    let members = ["aa".repeat(32), "bb".repeat(32)];
    assert_eq!(
        registry
            .commit_room(&"11".repeat(16), &members, 100)
            .unwrap(),
        RegistryCommit::Created
    );
    drop(registry);
    let reopened = PersistentRegistry::open(dir.path()).unwrap();
    assert_eq!(reopened.lookup_members(&"11".repeat(16)), members);
}

#[test]
fn registry_rejects_duplicate_members() {
    let (_dir, registry) = persistent();
    let same = "aa".repeat(32);
    assert!(
        registry
            .commit_room(&"12".repeat(16), &[same.clone(), same], 100)
            .is_err()
    );
    assert!(registry.lookup_members(&"12".repeat(16)).is_empty());
}

#[test]
fn registry_startup_skips_duplicate_member_record() {
    let dir = tempfile::tempdir().unwrap();
    let same = "aa".repeat(32);
    fs::write(
        dir.path().join("rooms.json"),
        format!(
            r#"{{"rooms":[{{"room_id":"{}","members":["{same}","{same}"],"created_at_ms":1,"activated_at_ms":null}}]}}"#,
            "13".repeat(16)
        ),
    )
    .unwrap();
    let registry = PersistentRegistry::open(dir.path()).unwrap();
    assert!(registry.lookup_members(&"13".repeat(16)).is_empty());
}

#[test]
fn registry_same_content_is_idempotent_but_different_content_conflicts() {
    let (_dir, registry) = persistent();
    let room = "14".repeat(16);
    let members = ["aa".repeat(32), "bb".repeat(32)];
    assert_eq!(
        registry.commit_room(&room, &members, 100).unwrap(),
        RegistryCommit::Created
    );
    assert_eq!(
        registry.commit_room(&room, &members, 200).unwrap(),
        RegistryCommit::Existing
    );
    assert!(
        registry
            .commit_room(&room, &[members[0].clone(), "cc".repeat(32)], 300)
            .is_err()
    );
    assert_eq!(registry.lookup_members(&room), members);
}

#[test]
fn registry_write_failure_never_publishes_room() {
    let (dir, registry) = persistent();
    fs::remove_dir_all(dir.path()).unwrap();
    let room = "15".repeat(16);
    assert!(
        registry
            .commit_room(&room, &["aa".repeat(32), "bb".repeat(32)], 100)
            .is_err()
    );
    assert!(registry.lookup_members(&room).is_empty());
}

#[test]
fn registry_first_member_activation_is_durable_and_idempotent() {
    let (dir, registry) = persistent();
    let room = "16".repeat(16);
    let members = ["aa".repeat(32), "bb".repeat(32)];
    registry.commit_room(&room, &members, 100).unwrap();
    assert!(
        registry
            .activate_on_first_join(&room, &members[0], 200)
            .unwrap()
    );
    assert!(
        !registry
            .activate_on_first_join(&room, &members[1], 300)
            .unwrap()
    );
    drop(registry);
    let reopened = PersistentRegistry::open(dir.path()).unwrap();
    assert_eq!(reopened.activated_at_ms(&room), Some(200));
}

#[test]
fn registry_prunes_only_old_unactivated_rooms() {
    let (_dir, registry) = persistent();
    let old = "17".repeat(16);
    let fresh = "18".repeat(16);
    let active = "19".repeat(16);
    let members = ["aa".repeat(32), "bb".repeat(32)];
    registry.commit_room(&old, &members, 100).unwrap();
    registry.commit_room(&fresh, &members, 900).unwrap();
    registry.commit_room(&active, &members, 100).unwrap();
    registry
        .activate_on_first_join(&active, &members[0], 200)
        .unwrap();
    assert_eq!(registry.prune_unactivated(500).unwrap(), 1);
    assert!(registry.lookup_members(&old).is_empty());
    assert_eq!(registry.lookup_members(&fresh), members);
    assert_eq!(registry.lookup_members(&active), members);
    assert_eq!(registry.prune_unactivated(500).unwrap(), 0);
}

#[test]
fn registry_enforces_room_quota() {
    let (_dir, registry) = persistent();
    let members = ["aa".repeat(32), "bb".repeat(32)];
    for n in 0..clipboard_server::REGISTRY_MAX_ROOMS {
        registry
            .commit_room(&format!("{n:032x}"), &members, 1)
            .unwrap();
    }
    assert!(registry.commit_room(&"ff".repeat(16), &members, 1).is_err());
}

#[tokio::test]
async fn pairing_offer_ok_publishes_offer() {
    let (_dir, registry) = persistent();
    let relay = PairingRelay::new(registry, config());
    let connections = Connections::new();
    let (out, mut rx) = outbox(4, 4096);
    connections.insert(1, out);
    let alice = bundle(1);
    offer(&relay, &connections, 1, &alice, "234567", &alice).unwrap();
    assert_eq!(frame(&mut rx).await, Frame::PairOfferOk);
    assert!(relay.is_published("234567"));
}

#[tokio::test]
async fn pairing_claim_commits_registry_then_enqueues_both_peers() {
    let (_dir, registry) = persistent();
    let relay = PairingRelay::new(registry.clone(), config());
    let connections = Connections::new();
    let (a_out, mut a_rx) = outbox(4, 4096);
    let (b_out, mut b_rx) = outbox(4, 4096);
    connections.insert(1, a_out);
    connections.insert(2, b_out);
    let alice = bundle(2);
    let bob = bundle(3);
    offer(&relay, &connections, 1, &alice, "345678", &alice).unwrap();
    assert_eq!(frame(&mut a_rx).await, Frame::PairOfferOk);
    claim(&relay, &connections, 2, &bob, "345678", &bob).unwrap();
    assert_eq!(
        registry.lookup_members(&room_id(&alice, &bob)),
        [bundle_fp(&alice), bundle_fp(&bob)]
    );
    assert_eq!(
        frame(&mut a_rx).await,
        Frame::PairPeer {
            peer_pub_bundle: bob.clone()
        }
    );
    assert_eq!(
        frame(&mut b_rx).await,
        Frame::PairPeer {
            peer_pub_bundle: alice
        }
    );
}

#[tokio::test]
async fn pairing_same_identity_consumes_code_before_registry_commit() {
    let (_dir, registry) = persistent();
    let relay = PairingRelay::new(registry.clone(), config());
    let connections = Connections::new();
    let (a_out, mut a_rx) = outbox(4, 4096);
    let (b_out, _b_rx) = outbox(4, 4096);
    connections.insert(1, a_out);
    connections.insert(2, b_out);
    let alice = bundle(4);
    offer(&relay, &connections, 1, &alice, "456789", &alice).unwrap();
    let _ = frame(&mut a_rx).await;
    assert_eq!(
        error_code(claim(&relay, &connections, 2, &alice, "456789", &alice)),
        "same_identity"
    );
    assert!(registry.lookup_members(&room_id(&alice, &alice)).is_empty());
    assert_eq!(
        error_code(claim(&relay, &connections, 2, &alice, "456789", &alice)),
        "code_expired"
    );
}

#[tokio::test]
async fn pairing_double_claim_is_rejected() {
    let (_dir, registry) = persistent();
    let relay = PairingRelay::new(registry, config());
    let connections = Connections::new();
    let (a_out, mut a_rx) = outbox(4, 4096);
    let (b_out, mut b_rx) = outbox(4, 4096);
    let (c_out, _c_rx) = outbox(4, 4096);
    connections.insert(1, a_out);
    connections.insert(2, b_out);
    connections.insert(3, c_out);
    let alice = bundle(5);
    let bob = bundle(6);
    let carol = bundle(7);
    offer(&relay, &connections, 1, &alice, "56789A", &alice).unwrap();
    let _ = frame(&mut a_rx).await;
    claim(&relay, &connections, 2, &bob, "56789A", &bob).unwrap();
    let _ = frame(&mut a_rx).await;
    let _ = frame(&mut b_rx).await;
    assert_eq!(
        error_code(claim(&relay, &connections, 3, &carol, "56789A", &carol)),
        "code_expired"
    );
}

#[tokio::test]
async fn pairing_unknown_and_expired_codes_are_rejected() {
    let (_dir, registry) = persistent();
    let relay = PairingRelay::new(registry.clone(), config());
    let connections = Connections::new();
    let (out, mut rx) = outbox(4, 4096);
    connections.insert(1, out);
    let alice = bundle(8);
    assert_eq!(
        error_code(claim(&relay, &connections, 1, &alice, "6789AB", &alice)),
        "code_expired"
    );
    let expiring = PairingRelay::new(
        registry,
        PairingConfig {
            offer_ttl: Duration::ZERO,
            attempts_per_window: 100,
            ..PairingConfig::default()
        },
    );
    offer(&expiring, &connections, 1, &alice, "789ABC", &alice).unwrap();
    let _ = frame(&mut rx).await;
    assert_eq!(
        error_code(claim(&expiring, &connections, 1, &alice, "789ABC", &alice)),
        "code_expired"
    );
}

#[tokio::test]
async fn pairing_disconnect_consumes_connection_bound_offer() {
    let (_dir, registry) = persistent();
    let relay = PairingRelay::new(registry, config());
    let connections = Connections::new();
    let (a_out, mut a_rx) = outbox(4, 4096);
    let (b_out, _b_rx) = outbox(4, 4096);
    connections.insert(1, a_out);
    connections.insert(2, b_out);
    let alice = bundle(9);
    let bob = bundle(10);
    offer(&relay, &connections, 1, &alice, "89ABCD", &alice).unwrap();
    let _ = frame(&mut a_rx).await;
    relay.on_disconnect(1);
    assert_eq!(
        error_code(claim(&relay, &connections, 2, &bob, "89ABCD", &bob)),
        "code_expired"
    );
}

#[tokio::test]
async fn pairing_registry_failure_enqueues_neither_peer() {
    let (dir, registry) = persistent();
    let relay = PairingRelay::new(registry, config());
    let connections = Connections::new();
    let (a_out, mut a_rx) = outbox(4, 4096);
    let (b_out, mut b_rx) = outbox(4, 4096);
    connections.insert(1, a_out);
    connections.insert(2, b_out);
    let alice = bundle(11);
    let bob = bundle(12);
    offer(&relay, &connections, 1, &alice, "9ABCDE", &alice).unwrap();
    let _ = frame(&mut a_rx).await;
    fs::remove_dir_all(dir.path()).unwrap();
    assert_eq!(
        error_code(claim(&relay, &connections, 2, &bob, "9ABCDE", &bob)),
        "registry_failed"
    );
    no_frame(&mut a_rx).await;
    no_frame(&mut b_rx).await;
}

#[tokio::test]
async fn pairing_full_offer_fifo_enqueues_neither_pair_peer() {
    let (_dir, registry) = persistent();
    let relay = PairingRelay::new(registry.clone(), config());
    let connections = Connections::new();
    let (a_out, mut a_rx) = outbox(1, 4096);
    let (b_out, mut b_rx) = outbox(2, 4096);
    connections.insert(1, a_out.clone());
    connections.insert(2, b_out);
    let alice = bundle(13);
    let bob = bundle(14);
    offer(&relay, &connections, 1, &alice, "ABCDEF", &alice).unwrap();
    let _ = frame(&mut a_rx).await;
    assert!(a_out.send_frame(&Frame::MailboxEmpty));
    assert_eq!(
        error_code(claim(&relay, &connections, 2, &bob, "ABCDEF", &bob)),
        "peer_unavailable"
    );
    assert_eq!(frame(&mut a_rx).await, Frame::MailboxEmpty);
    no_frame(&mut a_rx).await;
    no_frame(&mut b_rx).await;
    assert_eq!(registry.lookup_members(&room_id(&alice, &bob)).len(), 2);
}

#[tokio::test]
async fn pairing_full_claimer_fifo_enqueues_neither_pair_peer() {
    let (_dir, registry) = persistent();
    let relay = PairingRelay::new(registry, config());
    let connections = Connections::new();
    let (a_out, mut a_rx) = outbox(2, 4096);
    let (b_out, mut b_rx) = outbox(1, 4096);
    connections.insert(1, a_out);
    connections.insert(2, b_out.clone());
    let alice = bundle(15);
    let bob = bundle(16);
    offer(&relay, &connections, 1, &alice, "BCDEFG", &alice).unwrap();
    let _ = frame(&mut a_rx).await;
    assert!(b_out.send_frame(&Frame::MailboxEmpty));
    assert_eq!(
        error_code(claim(&relay, &connections, 2, &bob, "BCDEFG", &bob)),
        "peer_unavailable"
    );
    no_frame(&mut a_rx).await;
    assert_eq!(frame(&mut b_rx).await, Frame::MailboxEmpty);
    no_frame(&mut b_rx).await;
}

#[tokio::test]
async fn pairing_stale_offer_connection_enqueues_neither_peer() {
    let (_dir, registry) = persistent();
    let relay = PairingRelay::new(registry, config());
    let connections = Connections::new();
    let (a_out, mut a_rx) = outbox(4, 4096);
    let (b_out, mut b_rx) = outbox(4, 4096);
    connections.insert(1, a_out);
    connections.insert(2, b_out);
    let alice = bundle(17);
    let bob = bundle(18);
    offer(&relay, &connections, 1, &alice, "CDEFGH", &alice).unwrap();
    let _ = frame(&mut a_rx).await;
    connections.remove(&1);
    assert_eq!(
        error_code(claim(&relay, &connections, 2, &bob, "CDEFGH", &bob)),
        "peer_unavailable"
    );
    no_frame(&mut a_rx).await;
    no_frame(&mut b_rx).await;
}

#[tokio::test]
async fn pairing_bundle_mismatch_consumes_code() {
    let (_dir, registry) = persistent();
    let relay = PairingRelay::new(registry, config());
    let connections = Connections::new();
    let (a_out, mut a_rx) = outbox(4, 4096);
    let (b_out, _b_rx) = outbox(4, 4096);
    connections.insert(1, a_out);
    connections.insert(2, b_out);
    let alice = bundle(19);
    let bob = bundle(20);
    let forged = bundle(21);
    offer(&relay, &connections, 1, &alice, "DEFGHJ", &alice).unwrap();
    let _ = frame(&mut a_rx).await;
    assert_eq!(
        error_code(claim(&relay, &connections, 2, &bob, "DEFGHJ", &forged)),
        "bad_auth"
    );
    assert_eq!(
        error_code(claim(&relay, &connections, 2, &bob, "DEFGHJ", &bob)),
        "code_expired"
    );
}

#[tokio::test]
async fn pairing_code_collision_does_not_replace_first_offer() {
    let (_dir, registry) = persistent();
    let relay = PairingRelay::new(registry, config());
    let connections = Connections::new();
    let (a_out, mut a_rx) = outbox(4, 4096);
    let (b_out, _b_rx) = outbox(4, 4096);
    connections.insert(1, a_out);
    connections.insert(2, b_out);
    let alice = bundle(22);
    let bob = bundle(23);
    offer(&relay, &connections, 1, &alice, "EFGHJK", &alice).unwrap();
    let _ = frame(&mut a_rx).await;
    assert_eq!(
        error_code(offer(&relay, &connections, 2, &bob, "EFGHJK", &bob)),
        "code_in_use"
    );
    assert!(relay.is_published("EFGHJK"));
}

#[tokio::test]
async fn pairing_code_shape_and_per_ip_rate_limit_are_enforced() {
    let (_dir, registry) = persistent();
    let relay = PairingRelay::new(
        registry,
        PairingConfig {
            attempts_per_window: 1,
            ..PairingConfig::default()
        },
    );
    let connections = Connections::new();
    let (out, _rx) = outbox(4, 4096);
    connections.insert(1, out);
    let alice = bundle(24);
    assert_eq!(
        error_code(offer(&relay, &connections, 1, &alice, "bad", &alice)),
        "bad_code"
    );
    assert_eq!(
        error_code(offer(&relay, &connections, 1, &alice, "FGHJKM", &alice)),
        "rate_limited"
    );
}

#[tokio::test]
async fn pairing_offer_limit_rejects_additional_code() {
    let (_dir, registry) = persistent();
    let relay = PairingRelay::new(
        registry,
        PairingConfig {
            max_offers: 1,
            attempts_per_window: 100,
            ..PairingConfig::default()
        },
    );
    let connections = Connections::new();
    let (out, mut rx) = outbox(4, 4096);
    connections.insert(1, out);
    let alice = bundle(25);
    offer(&relay, &connections, 1, &alice, "GHJKMN", &alice).unwrap();
    let _ = frame(&mut rx).await;
    assert_eq!(
        error_code(offer(&relay, &connections, 1, &alice, "HJKMNP", &alice)),
        "server_full"
    );
}

#[tokio::test]
async fn pairing_websocket_flow_registers_joins_and_activates_room() {
    let (dir, registry) = persistent();
    let pairing = Arc::new(PairingRelay::new(registry.clone(), config()));
    let server = start(
        "127.0.0.1:0".parse().unwrap(),
        ServerConfig::default(),
        registry.clone(),
        pairing,
        Arc::new(NoopMailboxSink),
    )
    .await
    .unwrap();
    let alice = TestIdentity::generate();
    let bob = TestIdentity::generate();
    let mut a = Client::connect(server.addr()).await;
    let mut b = Client::connect(server.addr()).await;
    let a_nonce = a.hello(&alice).await;
    a.send(&Frame::PairOffer {
        code: "JKMNPQ".to_owned(),
        pub_bundle: alice.bundle.clone(),
    })
    .await;
    assert_eq!(a.recv_frame().await, Frame::PairOfferOk);
    let b_nonce = b.hello(&bob).await;
    b.send(&Frame::PairClaim {
        code: "JKMNPQ".to_owned(),
        pub_bundle: bob.bundle.clone(),
    })
    .await;
    assert_eq!(
        a.recv_frame().await,
        Frame::PairPeer {
            peer_pub_bundle: bob.bundle.clone()
        }
    );
    assert_eq!(
        b.recv_frame().await,
        Frame::PairPeer {
            peer_pub_bundle: alice.bundle.clone()
        }
    );
    let room = room_id(&alice.bundle, &bob.bundle);
    a.send(&alice.join_frame(&room, &a_nonce)).await;
    b.send(&bob.join_frame(&room, &b_nonce)).await;
    a.join_live_bootstrap().await;
    let reopened = PersistentRegistry::open(dir.path()).unwrap();
    assert!(reopened.activated_at_ms(&room).is_some());
    b.join_live_bootstrap().await;
}

#[tokio::test]
async fn pairing_join_activation_failure_publishes_no_success_frame() {
    let (dir, registry) = persistent();
    let alice = TestIdentity::generate();
    let bob = TestIdentity::generate();
    let room = room_id(&alice.bundle, &bob.bundle);
    registry
        .commit_room(&room, &[alice.fp(), bob.fp()], 1)
        .unwrap();
    fs::remove_dir_all(dir.path()).unwrap();
    let server = start(
        "127.0.0.1:0".parse().unwrap(),
        ServerConfig::default(),
        registry,
        Arc::new(PairingUnavailable),
        Arc::new(NoopMailboxSink),
    )
    .await
    .unwrap();
    let mut client = Client::connect(server.addr()).await;
    let nonce = client.hello(&alice).await;
    client.send(&alice.join_frame(&room, &nonce)).await;
    client.expect_error_close("registry_failed").await;
}

#[tokio::test]
async fn registry_periodic_sweeper_reclaims_old_unactivated_room() {
    let (_dir, registry) = persistent();
    let room = "20".repeat(16);
    registry
        .commit_room(&room, &["aa".repeat(32), "bb".repeat(32)], 1)
        .unwrap();
    let worker = spawn_unactivated_sweeper(
        registry.clone(),
        Duration::from_millis(1),
        Duration::from_millis(1),
    );
    eventually(|| registry.lookup_members(&room).is_empty()).await;
    worker.abort();
}

#[test]
fn registry_prune_cli_is_idempotent() {
    let (dir, registry) = persistent();
    let room = "21".repeat(16);
    registry
        .commit_room(&room, &["aa".repeat(32), "bb".repeat(32)], 1)
        .unwrap();
    drop(registry);
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_clipsync-server"))
            .env("DATA_DIR", dir.path())
            .args(["--prune-unactivated", "--older-than", "24h"])
            .output()
            .unwrap()
    };
    let first = run();
    assert!(first.status.success());
    assert_eq!(
        String::from_utf8(first.stdout).unwrap(),
        "pruned 1 unactivated room(s)\n"
    );
    let second = run();
    assert!(second.status.success());
    assert_eq!(
        String::from_utf8(second.stdout).unwrap(),
        "pruned 0 unactivated room(s)\n"
    );
}
