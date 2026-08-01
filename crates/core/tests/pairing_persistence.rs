#![cfg(feature = "full")]

use std::fs;
use std::path::{Path, PathBuf};

use clipboard_core::{
    crypto::Identity,
    pairing::{Offerer, PairingError, PairingStore, PendingConfirmation, QrPayload},
};
use uuid::Uuid;

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("clipsync-test-{}", Uuid::new_v4())))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn pending_pairing(store: &PairingStore) -> PendingConfirmation {
    let local = store
        .load_identity()
        .expect("test identity should load or be created");
    let peer = Identity::from_secret_bytes([21_u8; 32], [22_u8; 32], local.generation())
        .expect("peer fixture generation should be valid");
    let qr = QrPayload::new(
        "wss://relay.example/ws",
        "ABC234",
        local.public_bundle(),
        [31_u8; 16],
    )
    .expect("fixture QR payload should be valid");
    Offerer::start(&local, qr)
        .expect("offer should start")
        .receive_peer(peer.public_bundle())
        .expect("peer fixture should be distinct")
}

fn different_sas(sas: &str) -> String {
    if sas == "000000" {
        "999999".to_owned()
    } else {
        "000000".to_owned()
    }
}

#[test]
fn pairing_unconfirmed_state_leaves_load_empty() {
    // Given
    let dir = TestDir::new();
    let store = PairingStore::new(dir.path()).expect("pairing store should open");
    let _pending = pending_pairing(&store);

    // When
    let loaded = store.load_pairing().expect("pairing record load should succeed");

    // Then
    assert!(loaded.is_none());
}

#[test]
fn pairing_sas_mismatch_blocks_done_and_persistence() {
    // Given
    let dir = TestDir::new();
    let store = PairingStore::new(dir.path()).expect("pairing store should open");
    let pending = pending_pairing(&store);
    let wrong_sas = different_sas(&pending.sas());

    // When
    let result = pending.confirm(&wrong_sas, &store);

    // Then
    assert!(matches!(result, Err(PairingError::SasMismatch)));
    assert!(
        store
            .load_pairing()
            .expect("pairing record load should succeed")
            .is_none()
    );
}

#[test]
fn pairing_confirm_persists_only_generation_bound_record_fields() {
    // Given
    let dir = TestDir::new();
    let store = PairingStore::new(dir.path()).expect("pairing store should open");
    let pending = pending_pairing(&store);
    let sas = pending.sas();

    // When
    let done = pending
        .confirm(&sas, &store)
        .expect("matching SAS should complete pairing");
    let document = fs::read_to_string(dir.path().join("pairing.json"))
        .expect("pairing record should be persisted");

    // Then
    assert_eq!(done.record().identity_generation, 1);
    assert!(!document.contains("nonce_a"));
    assert!(!document.contains("ABC234"));
}

#[test]
fn pairing_record_reloads_with_generation_server_room_and_peer() {
    // Given
    let dir = TestDir::new();
    let store = PairingStore::new(dir.path()).expect("pairing store should open");
    let pending = pending_pairing(&store);
    let expected_sas = pending.sas();
    let expected = pending
        .confirm(&expected_sas, &store)
        .expect("matching SAS should complete pairing")
        .record()
        .clone();

    // When
    let loaded = PairingStore::new(dir.path())
        .expect("pairing store should reopen")
        .load_pairing()
        .expect("pairing record should reload")
        .expect("current-generation record should exist");

    // Then
    assert_eq!(loaded, expected);
}

#[test]
fn pairing_record_from_another_generation_is_ignored() {
    // Given
    let dir = TestDir::new();
    let store = PairingStore::new(dir.path()).expect("pairing store should open");
    let pending = pending_pairing(&store);
    let sas = pending.sas();
    pending
        .confirm(&sas, &store)
        .expect("matching SAS should complete pairing");
    let path = dir.path().join("pairing.json");
    let mut document: serde_json::Value = serde_json::from_slice(
        &fs::read(&path).expect("pairing record fixture should be readable"),
    )
    .expect("pairing record fixture should be valid JSON");
    document["identity_generation"] = serde_json::json!(2);
    fs::write(
        &path,
        serde_json::to_vec(&document).expect("fixture should serialize"),
    )
    .expect("generation fixture should be written");

    // When
    let loaded = store.load_pairing().expect("pairing record load should succeed");

    // Then
    assert!(loaded.is_none());
}

#[test]
fn pairing_record_rejects_tampered_peer_fingerprint() {
    // Given
    let dir = TestDir::new();
    let store = PairingStore::new(dir.path()).expect("pairing store should open");
    let pending = pending_pairing(&store);
    let sas = pending.sas();
    pending
        .confirm(&sas, &store)
        .expect("matching SAS should complete pairing");
    let path = dir.path().join("pairing.json");
    let mut document: serde_json::Value = serde_json::from_slice(
        &fs::read(&path).expect("pairing record fixture should be readable"),
    )
    .expect("pairing record fixture should be valid JSON");
    document["peer_bundle_fp"] = serde_json::json!("00");
    fs::write(
        &path,
        serde_json::to_vec(&document).expect("fixture should serialize"),
    )
    .expect("tampered fixture should be written");

    // When
    let result = store.load_pairing();

    // Then
    assert!(matches!(result, Err(PairingError::InvalidRecord)));
}
