#![cfg(feature = "full")]

use std::fs;
use std::path::{Path, PathBuf};

use clipboard_core::{
    crypto::Identity,
    pairing::{
        Offerer, PairingStore, QrPayload, reset_pairing_state_after_quiesce,
    },
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

fn persist_pairing(store: &PairingStore) {
    let local = store
        .load_identity()
        .expect("test identity should load or be created");
    let peer = Identity::from_secret_bytes([41_u8; 32], [42_u8; 32], local.generation())
        .expect("peer fixture generation should be valid");
    let qr = QrPayload::new(
        "wss://relay.example/ws",
        "ABC234",
        local.public_bundle(),
        [51_u8; 16],
    )
    .expect("fixture QR payload should be valid");
    let pending = Offerer::start(&local, qr)
        .expect("offer should start")
        .receive_peer(peer.public_bundle())
        .expect("peer fixture should be distinct");
    let sas = pending.sas();
    pending
        .confirm(&sas, store)
        .expect("matching SAS should complete pairing");
}

#[test]
fn pairing_reset_rotates_identity_bumps_generation_and_clears_record() {
    // Given
    let dir = TestDir::new();
    let store = PairingStore::new(dir.path()).expect("pairing store should open");
    let old_identity = store
        .load_identity()
        .expect("initial identity should be created");
    let old_bundle = old_identity.public_bundle();
    persist_pairing(&store);

    // When
    let new_identity =
        reset_pairing_state_after_quiesce(&store).expect("reset should commit successfully");

    // Then
    assert_eq!(new_identity.generation(), old_identity.generation() + 1);
    assert_ne!(new_identity.public_bundle(), old_bundle);
    assert!(
        store
            .load_pairing()
            .expect("pairing record load should succeed")
            .is_none()
    );
}

#[test]
fn pairing_reset_post_commit_ignores_restored_old_generation_record() {
    // Given
    let dir = TestDir::new();
    let store = PairingStore::new(dir.path()).expect("pairing store should open");
    persist_pairing(&store);
    let old_record = fs::read(dir.path().join("pairing.json"))
        .expect("old pairing record should be readable");
    reset_pairing_state_after_quiesce(&store).expect("reset should commit successfully");
    fs::write(dir.path().join("pairing.json"), old_record)
        .expect("old record orphan should be restored");

    // When
    let loaded = store
        .load_pairing()
        .expect("old-generation record should be ignored");

    // Then
    assert!(loaded.is_none());
}

#[test]
fn pairing_reset_post_commit_never_selects_old_identity_orphan() {
    // Given
    let dir = TestDir::new();
    let store = PairingStore::new(dir.path()).expect("pairing store should open");
    let old_identity = store
        .load_identity()
        .expect("initial identity should be created");
    let old_document =
        fs::read(dir.path().join("identity.json")).expect("old identity should be readable");
    let committed =
        reset_pairing_state_after_quiesce(&store).expect("reset should commit successfully");
    fs::write(dir.path().join(".identity.json.old"), old_document)
        .expect("old identity orphan should be restored");

    // When
    let reloaded = store
        .load_identity()
        .expect("disk-current identity should reload");

    // Then
    assert_eq!(reloaded.generation(), committed.generation());
    assert_eq!(reloaded.public_bundle(), committed.public_bundle());
    assert_ne!(reloaded.generation(), old_identity.generation());
}

#[test]
fn pairing_reset_repeated_calls_monotonically_advance_generation() {
    // Given
    let dir = TestDir::new();
    let store = PairingStore::new(dir.path()).expect("pairing store should open");
    let initial = store
        .load_identity()
        .expect("initial identity should be created");

    // When
    let second = reset_pairing_state_after_quiesce(&store).expect("first reset should commit");
    let third = reset_pairing_state_after_quiesce(&store).expect("second reset should commit");

    // Then
    assert_eq!(second.generation(), initial.generation() + 1);
    assert_eq!(third.generation(), second.generation() + 1);
}
