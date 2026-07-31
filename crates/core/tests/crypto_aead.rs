#![cfg(feature = "full")]

use clipboard_core::PubBundle;
use clipboard_core::crypto::{
    CryptoError, Identity, SessionKey, SessionKeys, aad, bundle_fp, open, room_id, seal,
};

fn identity_a() -> Identity {
    Identity::from_secret_bytes([7_u8; 32], [9_u8; 32], 1).expect("fixture generation is valid")
}

fn identity_b() -> Identity {
    Identity::from_secret_bytes([8_u8; 32], [10_u8; 32], 1).expect("fixture generation is valid")
}

struct PairedEndpoints {
    alice_bundle: PubBundle,
    bob_bundle: PubBundle,
    alice_keys: SessionKeys,
    bob_keys: SessionKeys,
}

impl PairedEndpoints {
    fn create() -> Self {
        let alice = identity_a();
        let bob = identity_b();
        let alice_bundle = alice.public_bundle();
        let bob_bundle = bob.public_bundle();
        let alice_keys = alice
            .session_keys(&bob_bundle)
            .expect("fixture DH is contributory");
        let bob_keys = bob
            .session_keys(&alice_bundle)
            .expect("fixture DH is contributory");
        Self {
            alice_bundle,
            bob_bundle,
            alice_keys,
            bob_keys,
        }
    }

    fn a_to_b_aad(&self) -> Vec<u8> {
        aad(
            &room_id(&self.alice_bundle, &self.bob_bundle),
            &bundle_fp(&self.alice_bundle),
            &bundle_fp(&self.bob_bundle),
        )
    }

    fn b_to_a_aad(&self) -> Vec<u8> {
        aad(
            &room_id(&self.alice_bundle, &self.bob_bundle),
            &bundle_fp(&self.bob_bundle),
            &bundle_fp(&self.alice_bundle),
        )
    }
}

#[test]
fn crypto_a_to_b_seal_opens_with_crossed_directional_key() {
    // Given
    let endpoints = PairedEndpoints::create();
    let associated_data = endpoints.a_to_b_aad();

    // When
    let sealed = seal(
        &endpoints.alice_keys.send,
        &associated_data,
        b"alice to bob",
    )
    .expect("seal");
    let opened = open(&endpoints.bob_keys.recv, &associated_data, &sealed).expect("crossed open");

    // Then
    assert_eq!(opened, b"alice to bob");
}

#[test]
fn crypto_b_to_a_seal_opens_with_crossed_directional_key() {
    // Given
    let endpoints = PairedEndpoints::create();
    let associated_data = endpoints.b_to_a_aad();

    // When
    let sealed = seal(&endpoints.bob_keys.send, &associated_data, b"bob to alice").expect("seal");
    let opened = open(&endpoints.alice_keys.recv, &associated_data, &sealed).expect("crossed open");

    // Then
    assert_eq!(opened, b"bob to alice");
}

#[test]
fn crypto_open_rejects_one_byte_aad_tamper() {
    // Given
    let endpoints = PairedEndpoints::create();
    let associated_data = endpoints.a_to_b_aad();
    let sealed = seal(
        &endpoints.alice_keys.send,
        &associated_data,
        b"authenticated",
    )
    .expect("seal");
    let mut tampered_aad = associated_data.clone();
    tampered_aad[0] ^= 1;

    // When
    let result = open(&endpoints.bob_keys.recv, &tampered_aad, &sealed);

    // Then
    assert!(matches!(result, Err(CryptoError::Decryption)));
}

#[test]
fn crypto_open_rejects_wrong_direction_key() {
    // Given
    let endpoints = PairedEndpoints::create();
    let associated_data = endpoints.a_to_b_aad();
    let sealed = seal(
        &endpoints.alice_keys.send,
        &associated_data,
        b"direction bound",
    )
    .expect("seal");

    // When
    let result = open(&endpoints.bob_keys.send, &associated_data, &sealed);

    // Then
    assert!(matches!(result, Err(CryptoError::Decryption)));
}

#[test]
fn crypto_xchacha_open_matches_cross_endpoint_golden_vector() {
    // Given
    let key = SessionKey::from_bytes(std::array::from_fn(|index| {
        u8::try_from(index).expect("index fits u8")
    }));
    let associated_data = [
        b"471fb943aa23c511f6f72f8d1652d9c8".as_slice(),
        &[0],
        b"fdeab9acf3710362bd2658cdc9a29e8f9c757fcf9811603a8c447cd1d9151108",
        &[0],
        b"9afaeef005e286957ee9a18a2481a75c7fc7ba74bae8de50ffa6127b12a62cae",
    ]
    .concat();
    let sealed = [
        0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xab, 0xac, 0xad, 0xae,
        0xaf, 0xb0, 0xb1, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0x26, 0x80, 0x63, 0x6d, 0xd5, 0x32,
        0xc5, 0xde, 0x05, 0x4f, 0x4a, 0xe6, 0x1d, 0x54, 0xc6, 0xae, 0xaf, 0x87, 0xbf, 0x5c, 0x80,
        0x0b, 0xfb, 0x46, 0xde, 0x51, 0x36, 0x31, 0x75, 0x12, 0x97, 0x94,
    ];

    // When
    let plaintext = open(&key, &associated_data, &sealed).expect("golden vector opens");

    // Then
    assert_eq!(plaintext, b"clipboard golden");
}
