use clipboard_core::PubBundle;
use clipboard_core::crypto::{bundle_fp, device_id, join_sig_msg, room_id, verify};
use ed25519_dalek::{Signer, SigningKey};

fn bundle_a() -> PubBundle {
    PubBundle {
        sign_pk: std::array::from_fn(|index| u8::try_from(index).expect("index fits u8")),
        dh_pk: std::array::from_fn(|index| {
            u8::try_from(index + 32).expect("index plus 32 fits u8")
        }),
    }
}

fn bundle_b() -> PubBundle {
    PubBundle {
        sign_pk: std::array::from_fn(|index| {
            u8::try_from(index + 64).expect("index plus 64 fits u8")
        }),
        dh_pk: std::array::from_fn(|index| {
            u8::try_from(index + 96).expect("index plus 96 fits u8")
        }),
    }
}

#[test]
fn crypto_bundle_fingerprint_matches_golden_vector() {
    // Given
    let bundle = bundle_a();

    // When
    let fingerprint = bundle_fp(&bundle);

    // Then
    assert_eq!(
        fingerprint,
        "fdeab9acf3710362bd2658cdc9a29e8f9c757fcf9811603a8c447cd1d9151108"
    );
}

#[test]
fn crypto_device_id_matches_golden_vector() {
    // Given
    let bundle = bundle_a();

    // When
    let id = device_id(&bundle.sign_pk);

    // Then
    assert_eq!(id, "630dcd2966c43366");
}

#[test]
fn crypto_room_id_matches_order_independent_golden_vector() {
    // Given
    let first = bundle_a();
    let second = bundle_b();

    // When
    let forward = room_id(&first, &second);
    let reverse = room_id(&second, &first);

    // Then
    assert_eq!(forward, "471fb943aa23c511f6f72f8d1652d9c8");
    assert_eq!(reverse, forward);
}

#[test]
fn crypto_join_signature_message_matches_golden_vector() {
    // Given
    let bundle = bundle_a();

    // When
    let message = join_sig_msg(
        &[1, 2, 3, 4],
        "471fb943aa23c511f6f72f8d1652d9c8",
        "630dcd2966c43366",
        &clipboard_core::bundle_bytes(&bundle),
    )
    .expect("golden join fields fit u32");

    // Then
    assert_eq!(
        message,
        [
            b"clipboard-sync-join-v1".as_slice(),
            &[0, 0, 0, 4, 1, 2, 3, 4],
            &[0, 0, 0, 32],
            b"471fb943aa23c511f6f72f8d1652d9c8",
            &[0, 0, 0, 16],
            b"630dcd2966c43366",
            &[0, 0, 0, 64],
            clipboard_core::bundle_bytes(&bundle).as_slice(),
        ]
        .concat()
    );
}

#[test]
fn crypto_ed25519_verify_accepts_valid_signature() {
    // Given
    let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
    let message = b"signed join";
    let signature = signing_key.sign(message).to_bytes();

    // When
    let result = verify(&signing_key.verifying_key().to_bytes(), message, &signature);

    // Then
    assert!(result.is_ok());
}

#[test]
fn crypto_ed25519_verify_rejects_tampered_message() {
    // Given
    let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
    let signature = signing_key.sign(b"signed join").to_bytes();

    // When
    let result = verify(
        &signing_key.verifying_key().to_bytes(),
        b"tampered join",
        &signature,
    );

    // Then
    assert!(result.is_err());
}
