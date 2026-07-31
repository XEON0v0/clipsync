use clipboard_core::protocol::{
    ConnectionState, ContentKind, Envelope, Frame, MAX_FRAME_BYTES, MAX_MESSAGE_BYTES,
    PROTOCOL_VERSION, ProtocolError, PubBundle, decode_envelope, decode_frame, encode_envelope,
    encode_frame, validate_frame_order,
};

fn bundle() -> PubBundle {
    PubBundle {
        sign_pk: [0; 32],
        dh_pk: [1; 32],
    }
}

fn envelope() -> Envelope {
    Envelope {
        v: PROTOCOL_VERSION,
        kind: ContentKind::Text,
        item_id: "550e8400-e29b-41d4-a716-446655440000".to_owned(),
        seq: 42,
        ts_ms: 1_700_000_000_000,
        content_b64: "aGVsbG8=".to_owned(),
    }
}

#[test]
fn protocol_frame_limit_accepts_24_mib_minus_one() {
    let expected = Frame::Error {
        code: "bad_frame".to_owned(),
        message: "within limit".to_owned(),
    };
    let mut encoded = serde_json::to_vec(&expected).expect("fixture must serialize");
    encoded.resize(MAX_FRAME_BYTES - 1, b' ');

    assert_eq!(decode_frame(&encoded), Ok(expected));
}

#[test]
fn protocol_frame_limit_rejects_24_mib_plus_one() {
    let encoded = vec![b' '; MAX_FRAME_BYTES + 1];

    assert_eq!(
        decode_frame(&encoded),
        Err(ProtocolError::Oversize {
            size: MAX_FRAME_BYTES + 1,
            limit: MAX_FRAME_BYTES,
        })
    );
}

#[test]
fn protocol_message_limit_accepts_24_mib_minus_one() {
    let expected = envelope();
    let mut encoded = serde_json::to_vec(&expected).expect("fixture must serialize");
    encoded.resize(MAX_MESSAGE_BYTES - 1, b' ');

    assert_eq!(decode_envelope(&encoded), Ok(expected));
}

#[test]
fn protocol_message_limit_rejects_24_mib_plus_one() {
    let encoded = vec![b' '; MAX_MESSAGE_BYTES + 1];

    assert_eq!(
        decode_envelope(&encoded),
        Err(ProtocolError::Oversize {
            size: MAX_MESSAGE_BYTES + 1,
            limit: MAX_MESSAGE_BYTES,
        })
    );
}

#[test]
fn protocol_invalid_room_id_is_bad_frame() {
    let frame = Frame::Join {
        room_id: "ABCDEF".to_owned(),
        device_id: "0123456789abcdef".to_owned(),
        pub_bundle: bundle(),
        sig_b64: "c2lnbmF0dXJl".to_owned(),
    };
    let encoded = serde_json::to_vec(&frame).expect("fixture must serialize");

    assert_eq!(
        decode_frame(&encoded),
        Err(ProtocolError::BadFrame {
            message: "room_id must be 32 lowercase hex characters".to_owned(),
        })
    );
}

#[test]
fn protocol_invalid_device_id_is_bad_frame() {
    let frame = Frame::Hello {
        device_id: "ABCDEF0123456789".to_owned(),
        pub_bundle: bundle(),
        version: PROTOCOL_VERSION,
    };
    let encoded = serde_json::to_vec(&frame).expect("fixture must serialize");

    assert_eq!(
        decode_frame(&encoded),
        Err(ProtocolError::BadFrame {
            message: "device_id must be 16 lowercase hex characters".to_owned(),
        })
    );
}

#[test]
fn protocol_non_standard_pub_bundle_base64_is_bad_frame() {
    let encoded = br#"{"type":"hello","device_id":"0123456789abcdef","pub_bundle":{"sign_pk_b64":"not-base64","dh_pk_b64":"AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE="},"version":1}"#;

    let error = decode_frame(encoded).expect_err("invalid STANDARD base64 must be rejected");
    assert!(matches!(error, ProtocolError::BadFrame { .. }));
}

#[test]
fn protocol_version_mismatch_maps_to_error_frame() {
    let frame = Frame::Hello {
        device_id: "0123456789abcdef".to_owned(),
        pub_bundle: bundle(),
        version: 2,
    };
    let encoded = serde_json::to_vec(&frame).expect("fixture must serialize");

    let error = decode_frame(&encoded).expect_err("unsupported version must be rejected");
    assert_eq!(
        error,
        ProtocolError::VersionMismatch {
            received: 2,
            supported: PROTOCOL_VERSION,
        }
    );
    assert_eq!(
        error.to_error_frame(),
        Frame::Error {
            code: "version_mismatch".to_owned(),
            message: "unsupported protocol version 2; expected 1".to_owned(),
        }
    );
}

#[test]
fn protocol_pair_offer_after_join_is_bad_frame() {
    let state = validate_frame_order(
        ConnectionState::Connected,
        &Frame::Hello {
            device_id: "0123456789abcdef".to_owned(),
            pub_bundle: bundle(),
            version: PROTOCOL_VERSION,
        },
    )
    .expect("hello is legal after connect");
    let state = validate_frame_order(
        state,
        &Frame::HelloOk {
            server_version: PROTOCOL_VERSION,
            nonce_b64: "bm9uY2U=".to_owned(),
        },
    )
    .expect("hello_ok is legal after hello");
    let state = validate_frame_order(
        state,
        &Frame::Join {
            room_id: "00112233445566778899aabbccddeeff".to_owned(),
            device_id: "0123456789abcdef".to_owned(),
            pub_bundle: bundle(),
            sig_b64: "c2lnbmF0dXJl".to_owned(),
        },
    )
    .expect("join is legal for an already-paired connection");

    let error = validate_frame_order(
        state,
        &Frame::PairOffer {
            code: "ABC123".to_owned(),
            pub_bundle: bundle(),
        },
    )
    .expect_err("pairing is illegal after join");

    assert!(matches!(error, ProtocolError::BadFrame { .. }));
}

#[test]
fn protocol_encode_rejects_invalid_envelope_metadata() {
    let mut invalid = envelope();
    invalid.item_id = "not-a-uuid".to_owned();

    assert!(matches!(
        encode_envelope(&invalid),
        Err(ProtocolError::BadFrame { .. })
    ));

    let invalid_clip = Frame::Clip {
        room_id: "invalid".to_owned(),
        ciphertext_b64: "Y2lwaGVydGV4dA==".to_owned(),
        origin_device: String::new(),
        mailbox: false,
    };
    assert!(matches!(
        encode_frame(&invalid_clip),
        Err(ProtocolError::BadFrame { .. })
    ));
}
