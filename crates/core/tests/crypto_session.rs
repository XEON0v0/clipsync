#![cfg(feature = "full")]

use clipboard_core::PubBundle;
use clipboard_core::crypto::{
    CryptoError, Ed25519Keypair, Identity, X25519Keypair, aad, sas_code, verify,
};

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

fn identity_a() -> Identity {
    Identity::from_secret_bytes([7_u8; 32], [9_u8; 32], 1).expect("fixture generation is valid")
}

fn identity_b() -> Identity {
    Identity::from_secret_bytes([8_u8; 32], [10_u8; 32], 1).expect("fixture generation is valid")
}

macro_rules! assert_not_debug {
    ($type:ty) => {
        const _: fn() = || {
            trait AmbiguousIfDebug<Marker> {
                fn marker() {}
            }
            impl<T: ?Sized> AmbiguousIfDebug<()> for T {}
            struct DebugMarker;
            impl<T: ?Sized + std::fmt::Debug> AmbiguousIfDebug<DebugMarker> for T {}
            let _ = <$type as AmbiguousIfDebug<_>>::marker;
        };
    };
}

assert_not_debug!(Identity);
assert_not_debug!(Ed25519Keypair);
assert_not_debug!(X25519Keypair);

#[test]
fn crypto_aad_matches_golden_vector() {
    // Given
    let room = "471fb943aa23c511f6f72f8d1652d9c8";
    let sender = "fdeab9acf3710362bd2658cdc9a29e8f9c757fcf9811603a8c447cd1d9151108";
    let receiver = "9afaeef005e286957ee9a18a2481a75c7fc7ba74bae8de50ffa6127b12a62cae";

    // When
    let associated_data = aad(room, sender, receiver);

    // Then
    assert_eq!(
        associated_data,
        [
            room.as_bytes(),
            &[0],
            sender.as_bytes(),
            &[0],
            receiver.as_bytes(),
        ]
        .concat()
    );
}

#[test]
fn crypto_sas_matches_golden_vector() {
    // Given
    let nonce: [u8; 16] = std::array::from_fn(|index| u8::try_from(index).expect("fits u8"));

    // When
    let code = sas_code(&nonce, &bundle_a(), &bundle_b());

    // Then
    assert_eq!(code, "845759");
}

#[test]
fn crypto_sas_changes_when_qr_nonce_changes() {
    // Given
    let first_nonce = [0_u8; 16];
    let second_nonce = [1_u8; 16];

    // When
    let first_code = sas_code(&first_nonce, &bundle_a(), &bundle_b());
    let second_code = sas_code(&second_nonce, &bundle_a(), &bundle_b());

    // Then
    assert_ne!(first_code, second_code);
}

#[test]
fn crypto_identity_signs_for_verify_surface() {
    // Given
    let identity = identity_a();
    let message = b"identity signature";

    // When
    let signature = identity.sign(message);

    // Then
    assert!(verify(&identity.public_bundle().sign_pk, message, &signature).is_ok());
}

#[test]
fn crypto_session_keys_reject_same_identity() {
    // Given
    let identity = identity_a();
    let own_bundle = identity.public_bundle();

    // When
    let result = identity.session_keys(&own_bundle);

    // Then
    assert!(matches!(result, Err(CryptoError::SameIdentity)));
}

#[test]
fn crypto_session_keys_reject_non_contributory_peer() {
    // Given
    let identity = identity_a();
    let mut low_order_peer = identity_b().public_bundle();
    low_order_peer.dh_pk = [0_u8; 32];

    // When
    let result = identity.session_keys(&low_order_peer);

    // Then
    assert!(matches!(result, Err(CryptoError::NonContributoryDh)));
}
