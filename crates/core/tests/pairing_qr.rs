#![cfg(feature = "full")]

use clipboard_core::{
    PubBundle,
    crypto::Identity,
    pairing::{Claimer, Offerer, PairingError, QrPayload},
};

fn bundle(byte: u8) -> PubBundle {
    PubBundle {
        sign_pk: [byte; 32],
        dh_pk: [byte.wrapping_add(1); 32],
    }
}

fn identity(sign: u8, dh: u8) -> Identity {
    Identity::from_secret_bytes([sign; 32], [dh; 32], 4)
        .expect("fixture generation should be valid")
}

#[test]
fn pairing_qr_payload_round_trips_canonical_json() {
    // Given
    let payload = QrPayload::new(
        "wss://relay.example/ws",
        "ABC234",
        bundle(7),
        [11_u8; 16],
    )
    .expect("fixture QR payload should be valid");

    // When
    let encoded = payload.to_json().expect("QR payload should serialize");
    let decoded = QrPayload::parse(&encoded).expect("QR payload should parse");

    // Then
    assert_eq!(decoded, payload);
}

#[test]
fn pairing_qr_rejects_non_websocket_server_url() {
    // Given
    let encoded = QrPayload::new(
        "wss://relay.example/ws",
        "ABC234",
        bundle(7),
        [11_u8; 16],
    )
    .expect("fixture QR payload should be valid")
    .to_json()
    .expect("QR payload should serialize")
    .replace("wss://", "https://");

    // When
    let result = QrPayload::parse(&encoded);

    // Then
    assert!(matches!(result, Err(PairingError::InvalidServer)));
}

#[test]
fn pairing_qr_rejects_tampered_nonce_encoding() {
    // Given
    let encoded = QrPayload::new(
        "wss://relay.example/ws",
        "ABC234",
        bundle(7),
        [11_u8; 16],
    )
    .expect("fixture QR payload should be valid")
    .to_json()
    .expect("QR payload should serialize")
    .replace("CwsLCwsLCwsLCwsLCwsLCw==", "not-base64!");

    // When
    let result = QrPayload::parse(&encoded);

    // Then
    assert!(matches!(result, Err(PairingError::Json(_))));
}

#[test]
fn pairing_qr_rejects_unknown_tampered_fields() {
    // Given
    let encoded = QrPayload::new(
        "wss://relay.example/ws",
        "ABC234",
        bundle(7),
        [11_u8; 16],
    )
    .expect("fixture QR payload should be valid")
    .to_json()
    .expect("QR payload should serialize")
    .replace('{', "{\"peer_override\":true,");

    // When
    let result = QrPayload::parse(&encoded);

    // Then
    assert!(matches!(result, Err(PairingError::Json(_))));
}

#[test]
fn pairing_claimer_aborts_same_identity_immediately() {
    // Given
    let local = identity(7, 9);
    let qr = QrPayload::new(
        "wss://relay.example/ws",
        "ABC234",
        local.public_bundle(),
        [11_u8; 16],
    )
    .expect("fixture QR payload should be valid");

    // When
    let result = Claimer::start(&local, qr);

    // Then
    assert!(matches!(result, Err(PairingError::SameIdentity)));
}

#[test]
fn pairing_claimer_pins_qr_server_and_bundle() {
    // Given
    let offerer = identity(7, 9);
    let claimer_identity = identity(8, 10);
    let qr = QrPayload::new(
        "wss://relay.example/ws",
        "ABC234",
        offerer.public_bundle(),
        [11_u8; 16],
    )
    .expect("fixture QR payload should be valid");
    let claimer = Claimer::start(&claimer_identity, qr).expect("identities should be distinct");

    // When
    let pending = claimer
        .receive_peer("wss://relay.example/ws", &offerer.public_bundle())
        .expect("the relay result should match the pinned QR");

    // Then
    assert_eq!(pending.server(), "wss://relay.example/ws");
    assert_eq!(pending.peer_bundle(), &offerer.public_bundle());
}

#[test]
fn pairing_claimer_aborts_server_mismatch() {
    // Given
    let offerer = identity(7, 9);
    let claimer_identity = identity(8, 10);
    let qr = QrPayload::new(
        "wss://relay.example/ws",
        "ABC234",
        offerer.public_bundle(),
        [11_u8; 16],
    )
    .expect("fixture QR payload should be valid");
    let claimer = Claimer::start(&claimer_identity, qr).expect("identities should be distinct");

    // When
    let result = claimer.receive_peer("wss://other.example/ws", &offerer.public_bundle());

    // Then
    assert!(matches!(result, Err(PairingError::ServerMismatch)));
}

#[test]
fn pairing_claimer_aborts_bundle_mismatch() {
    // Given
    let offerer = identity(7, 9);
    let claimer_identity = identity(8, 10);
    let imposter = identity(12, 13);
    let qr = QrPayload::new(
        "wss://relay.example/ws",
        "ABC234",
        offerer.public_bundle(),
        [11_u8; 16],
    )
    .expect("fixture QR payload should be valid");
    let claimer = Claimer::start(&claimer_identity, qr).expect("identities should be distinct");

    // When
    let result = claimer.receive_peer("wss://relay.example/ws", &imposter.public_bundle());

    // Then
    assert!(matches!(result, Err(PairingError::BundleMismatch)));
}

#[test]
fn pairing_offerer_aborts_same_identity_peer() {
    // Given
    let identity = identity(7, 9);
    let own_bundle = identity.public_bundle();
    let qr = QrPayload::new(
        "wss://relay.example/ws",
        "ABC234",
        own_bundle.clone(),
        [11_u8; 16],
    )
    .expect("fixture QR payload should be valid");
    let offerer = Offerer::start(&identity, qr).expect("offer should start");

    // When
    let result = offerer.receive_peer(own_bundle);

    // Then
    assert!(matches!(result, Err(PairingError::SameIdentity)));
}

#[test]
fn pairing_roles_compute_the_same_sas() {
    // Given
    let offerer_identity = identity(7, 9);
    let claimer_identity = identity(8, 10);
    let qr = QrPayload::new(
        "wss://relay.example/ws",
        "ABC234",
        offerer_identity.public_bundle(),
        [11_u8; 16],
    )
    .expect("fixture QR payload should be valid");
    let offerer = Offerer::start(&offerer_identity, qr.clone()).expect("offer should start");
    let claimer = Claimer::start(&claimer_identity, qr).expect("claim should start");

    // When
    let offerer_pending = offerer
        .receive_peer(claimer_identity.public_bundle())
        .expect("offerer should accept the claimer bundle");
    let claimer_pending = claimer
        .receive_peer("wss://relay.example/ws", &offerer_identity.public_bundle())
        .expect("claimer should accept pinned relay data");

    // Then
    assert_eq!(offerer_pending.sas(), claimer_pending.sas());
}
