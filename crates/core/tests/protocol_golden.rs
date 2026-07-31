use clipboard_core::protocol::{
    ContentKind, Envelope, Frame, PROTOCOL_VERSION, PubBundle, decode_envelope, decode_frame,
    encode_envelope, encode_frame,
};

fn bundle() -> PubBundle {
    PubBundle {
        sign_pk: [0; 32],
        dh_pk: [1; 32],
    }
}

#[test]
fn protocol_golden_frames_are_byte_exact_and_round_trip() {
    let frames = [
        (
            Frame::Hello {
                device_id: "0123456789abcdef".to_owned(),
                pub_bundle: bundle(),
                version: PROTOCOL_VERSION,
            },
            include_bytes!("golden/frames/hello.json").as_slice(),
        ),
        (
            Frame::HelloOk {
                server_version: PROTOCOL_VERSION,
                nonce_b64: "bm9uY2U=".to_owned(),
            },
            include_bytes!("golden/frames/hello_ok.json").as_slice(),
        ),
        (
            Frame::PairOffer {
                code: "ABC123".to_owned(),
                pub_bundle: bundle(),
            },
            include_bytes!("golden/frames/pair_offer.json").as_slice(),
        ),
        (
            Frame::PairOfferOk,
            include_bytes!("golden/frames/pair_offer_ok.json").as_slice(),
        ),
        (
            Frame::PairClaim {
                code: "ABC123".to_owned(),
                pub_bundle: bundle(),
            },
            include_bytes!("golden/frames/pair_claim.json").as_slice(),
        ),
        (
            Frame::PairPeer {
                peer_pub_bundle: bundle(),
            },
            include_bytes!("golden/frames/pair_peer.json").as_slice(),
        ),
        (
            Frame::Join {
                room_id: "00112233445566778899aabbccddeeff".to_owned(),
                device_id: "0123456789abcdef".to_owned(),
                pub_bundle: bundle(),
                sig_b64: "c2lnbmF0dXJl".to_owned(),
            },
            include_bytes!("golden/frames/join.json").as_slice(),
        ),
        (
            Frame::JoinOk,
            include_bytes!("golden/frames/join_ok.json").as_slice(),
        ),
        (
            Frame::Clip {
                room_id: "00112233445566778899aabbccddeeff".to_owned(),
                ciphertext_b64: "Y2lwaGVydGV4dA==".to_owned(),
                origin_device: "fedcba9876543210".to_owned(),
                mailbox: true,
            },
            include_bytes!("golden/frames/clip.json").as_slice(),
        ),
        (
            Frame::MailboxEmpty,
            include_bytes!("golden/frames/mailbox_empty.json").as_slice(),
        ),
        (
            Frame::Error {
                code: "bad_frame".to_owned(),
                message: "invalid frame".to_owned(),
            },
            include_bytes!("golden/frames/error.json").as_slice(),
        ),
    ];

    for (frame, golden) in frames {
        assert_eq!(
            encode_frame(&frame).expect("valid frame must encode"),
            golden
        );
        assert_eq!(decode_frame(golden), Ok(frame));
    }
}

#[test]
fn protocol_golden_envelope_is_byte_exact_and_round_trips() {
    let expected = Envelope {
        v: PROTOCOL_VERSION,
        kind: ContentKind::Text,
        item_id: "550e8400-e29b-41d4-a716-446655440000".to_owned(),
        seq: 42,
        ts_ms: 1_700_000_000_000,
        content_b64: "aGVsbG8=".to_owned(),
    };
    let golden = include_bytes!("golden/envelope.json");

    assert_eq!(
        encode_envelope(&expected).expect("valid envelope must encode"),
        golden
    );
    assert_eq!(decode_envelope(golden), Ok(expected));
}
