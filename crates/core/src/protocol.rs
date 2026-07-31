//! Shared JSON protocol types and bounded codecs.
//!
//! The connection state machine is:
//!
//! | State | Accepted frame | Next state |
//! | --- | --- | --- |
//! | Connected | `hello` | AwaitingHelloOk |
//! | AwaitingHelloOk | `hello_ok` | Ready |
//! | Ready (new pairing) | `pair_offer` | AwaitingPairOfferOk |
//! | AwaitingPairOfferOk | `pair_offer_ok` | AwaitingPairPeer |
//! | Ready (claiming pairing) | `pair_claim` | AwaitingPairPeer |
//! | AwaitingPairPeer | `pair_peer` | ReadyToJoin |
//! | Ready or ReadyToJoin | `join` | AwaitingJoinOk |
//! | AwaitingJoinOk | `join_ok` | Bootstrap |
//! | Bootstrap | exactly one `clip` with `mailbox=true` or `mailbox_empty` | Live |
//! | Live | `clip` with `mailbox=false` | Live |
//!
//! Already-paired connections take the direct `Ready -> join` path. Pairing frames are invalid
//! after `join`. An `error` frame closes any active state.
//!
//! A client sends [`Frame::Clip`] with an empty `origin_device` and `mailbox=false`. The server
//! overwrites `origin_device` from the authenticated bundle fingerprint and sets `mailbox=true`
//! only for mailbox delivery. Business metadata is never present in a frame; it belongs only in
//! the encrypted [`Envelope`]. Envelope sequence numbers are monotonically allocated and persisted
//! by the sender. Version mismatches map to `error{code="version_mismatch"}` and the connection is
//! then closed. Frame and message codecs independently enforce a 24 MiB limit.

// allow: SIZE_OK - the task requires the complete protocol contract to remain in protocol.rs.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de, ser::SerializeStruct};
use thiserror::Error;

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 24 * 1024 * 1024;
pub const MAX_MESSAGE_BYTES: usize = 24 * 1024 * 1024;
pub const PUBLIC_KEY_LENGTH: usize = 32;
pub const BUNDLE_BYTES_LENGTH: usize = PUBLIC_KEY_LENGTH * 2;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PubBundle {
    pub sign_pk: [u8; PUBLIC_KEY_LENGTH],
    pub dh_pk: [u8; PUBLIC_KEY_LENGTH],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PubBundleJson {
    sign_pk_b64: String,
    dh_pk_b64: String,
}

impl Serialize for PubBundle {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut fields = serializer.serialize_struct("PubBundle", 2)?;
        fields.serialize_field("sign_pk_b64", &STANDARD.encode(self.sign_pk))?;
        fields.serialize_field("dh_pk_b64", &STANDARD.encode(self.dh_pk))?;
        fields.end()
    }
}

impl<'de> Deserialize<'de> for PubBundle {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = PubBundleJson::deserialize(deserializer)?;
        Ok(Self {
            sign_pk: decode_public_key::<D::Error>(&encoded.sign_pk_b64)?,
            dh_pk: decode_public_key::<D::Error>(&encoded.dh_pk_b64)?,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Frame {
    Hello {
        device_id: String,
        pub_bundle: PubBundle,
        version: u32,
    },
    HelloOk {
        server_version: u32,
        nonce_b64: String,
    },
    PairOffer {
        code: String,
        pub_bundle: PubBundle,
    },
    PairOfferOk,
    PairClaim {
        code: String,
        pub_bundle: PubBundle,
    },
    PairPeer {
        peer_pub_bundle: PubBundle,
    },
    Join {
        room_id: String,
        device_id: String,
        pub_bundle: PubBundle,
        sig_b64: String,
    },
    JoinOk,
    Clip {
        room_id: String,
        ciphertext_b64: String,
        origin_device: String,
        mailbox: bool,
    },
    MailboxEmpty,
    Error {
        code: String,
        message: String,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    Text,
    Image,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub v: u32,
    pub kind: ContentKind,
    pub item_id: String,
    pub seq: u64,
    pub ts_ms: i64,
    pub content_b64: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionState {
    Connected,
    AwaitingHelloOk,
    Ready,
    AwaitingPairOfferOk,
    AwaitingPairPeer,
    ReadyToJoin,
    AwaitingJoinOk,
    Bootstrap,
    Live,
    Closed,
}

#[derive(Debug, Eq, Error, PartialEq)]
pub enum ProtocolError {
    #[error("bad_frame: {message}")]
    BadFrame { message: String },
    #[error("payload is {size} bytes; limit is {limit} bytes")]
    Oversize { size: usize, limit: usize },
    #[error("unsupported protocol version {received}; expected {supported}")]
    VersionMismatch { received: u32, supported: u32 },
}

impl ProtocolError {
    #[must_use]
    pub fn to_error_frame(&self) -> Frame {
        match self {
            Self::BadFrame { message } => Frame::Error {
                code: "bad_frame".to_owned(),
                message: message.clone(),
            },
            Self::Oversize { .. } => Frame::Error {
                code: "bad_frame".to_owned(),
                message: self.to_string(),
            },
            Self::VersionMismatch {
                received,
                supported,
            } => Frame::Error {
                code: "version_mismatch".to_owned(),
                message: format!("unsupported protocol version {received}; expected {supported}"),
            },
        }
    }
}

#[must_use]
pub fn bundle_bytes(bundle: &PubBundle) -> [u8; BUNDLE_BYTES_LENGTH] {
    let mut bytes = [0_u8; BUNDLE_BYTES_LENGTH];
    let (sign_pk, dh_pk) = bytes.split_at_mut(PUBLIC_KEY_LENGTH);
    sign_pk.copy_from_slice(&bundle.sign_pk);
    dh_pk.copy_from_slice(&bundle.dh_pk);
    bytes
}

/// Serializes a validated network frame.
///
/// # Errors
/// Returns a protocol error for invalid fields or encoded output larger than 24 MiB.
pub fn encode_frame(frame: &Frame) -> Result<Vec<u8>, ProtocolError> {
    validate_frame(frame)?;
    encode_json(frame, MAX_FRAME_BYTES)
}

/// Deserializes and validates a bounded network frame.
///
/// # Errors
/// Returns a protocol error for oversized, malformed, or invalid input.
pub fn decode_frame(encoded: &[u8]) -> Result<Frame, ProtocolError> {
    ensure_size(encoded.len(), MAX_FRAME_BYTES)?;
    let frame = serde_json::from_slice(encoded).map_err(bad_json)?;
    validate_frame(&frame)?;
    Ok(frame)
}

/// Serializes a validated encrypted-message envelope.
///
/// # Errors
/// Returns a protocol error for invalid metadata or encoded output larger than 24 MiB.
pub fn encode_envelope(envelope: &Envelope) -> Result<Vec<u8>, ProtocolError> {
    validate_envelope(envelope)?;
    encode_json(envelope, MAX_MESSAGE_BYTES)
}

/// Deserializes and validates a bounded encrypted-message envelope.
///
/// # Errors
/// Returns a protocol error for oversized, malformed, or invalid input.
pub fn decode_envelope(encoded: &[u8]) -> Result<Envelope, ProtocolError> {
    ensure_size(encoded.len(), MAX_MESSAGE_BYTES)?;
    let envelope = serde_json::from_slice(encoded).map_err(bad_json)?;
    validate_envelope(&envelope)?;
    Ok(envelope)
}

/// Applies one frame to the connection-order validator.
///
/// # Errors
/// Returns `bad_frame` when the frame is invalid or illegal in the current state.
pub fn validate_frame_order(
    state: ConnectionState,
    frame: &Frame,
) -> Result<ConnectionState, ProtocolError> {
    validate_frame(frame)?;
    let next = TRANSITIONS[state.index()][FrameEvent::from_frame(frame).index()];
    match next {
        Some(next_state) => Ok(next_state),
        None => Err(ProtocolError::BadFrame {
            message: format!("frame {} is illegal in state {state:?}", frame.type_name()),
        }),
    }
}

fn validate_frame(frame: &Frame) -> Result<(), ProtocolError> {
    match frame {
        Frame::Hello {
            device_id, version, ..
        } => {
            validate_device_id(device_id)?;
            validate_version(*version)
        }
        Frame::HelloOk {
            server_version,
            nonce_b64,
        } => {
            validate_version(*server_version)?;
            validate_base64("nonce_b64", nonce_b64)
        }
        Frame::PairOffer { .. }
        | Frame::PairOfferOk
        | Frame::PairClaim { .. }
        | Frame::PairPeer { .. }
        | Frame::JoinOk
        | Frame::MailboxEmpty
        | Frame::Error { .. } => Ok(()),
        Frame::Join {
            room_id,
            device_id,
            sig_b64,
            ..
        } => {
            validate_room_id(room_id)?;
            validate_device_id(device_id)?;
            validate_base64("sig_b64", sig_b64)
        }
        Frame::Clip {
            room_id,
            ciphertext_b64,
            ..
        } => {
            validate_room_id(room_id)?;
            validate_base64("ciphertext_b64", ciphertext_b64)
        }
    }
}

fn validate_envelope(envelope: &Envelope) -> Result<(), ProtocolError> {
    validate_version(envelope.v)?;
    if !is_uuid_v4(&envelope.item_id) {
        return Err(ProtocolError::BadFrame {
            message: "item_id must be a UUID v4".to_owned(),
        });
    }
    validate_base64("content_b64", &envelope.content_b64)
}

fn validate_device_id(device_id: &str) -> Result<(), ProtocolError> {
    validate_lower_hex(device_id, 16, "device_id")
}

fn validate_room_id(room_id: &str) -> Result<(), ProtocolError> {
    validate_lower_hex(room_id, 32, "room_id")
}

fn validate_lower_hex(value: &str, length: usize, name: &str) -> Result<(), ProtocolError> {
    let valid = value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if valid {
        Ok(())
    } else {
        Err(ProtocolError::BadFrame {
            message: format!("{name} must be {length} lowercase hex characters"),
        })
    }
}

fn validate_version(version: u32) -> Result<(), ProtocolError> {
    if version == PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(ProtocolError::VersionMismatch {
            received: version,
            supported: PROTOCOL_VERSION,
        })
    }
}

fn validate_base64(name: &str, value: &str) -> Result<(), ProtocolError> {
    let decoded = STANDARD
        .decode(value)
        .map_err(|error| ProtocolError::BadFrame {
            message: format!("{name} must be padded STANDARD base64: {error}"),
        })?;
    if STANDARD.encode(decoded) == value {
        Ok(())
    } else {
        Err(ProtocolError::BadFrame {
            message: format!("{name} must be canonical padded STANDARD base64"),
        })
    }
}

fn is_uuid_v4(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes[8] == b'-'
        && bytes[13] == b'-'
        && bytes[14] == b'4'
        && bytes[18] == b'-'
        && bytes[23] == b'-'
        && matches!(bytes[19], b'8' | b'9' | b'a' | b'b' | b'A' | b'B')
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 8 | 13 | 18 | 23) || byte.is_ascii_hexdigit())
}

fn ensure_size(size: usize, limit: usize) -> Result<(), ProtocolError> {
    if size <= limit {
        Ok(())
    } else {
        Err(ProtocolError::Oversize { size, limit })
    }
}

fn encode_json<T>(value: &T, limit: usize) -> Result<Vec<u8>, ProtocolError>
where
    T: Serialize,
{
    let mut encoded = serde_json::to_vec_pretty(value).map_err(bad_json)?;
    encoded.push(b'\n');
    ensure_size(encoded.len(), limit)?;
    Ok(encoded)
}

fn bad_json(error: serde_json::Error) -> ProtocolError {
    ProtocolError::BadFrame {
        message: format!("invalid JSON: {error}"),
    }
}

fn decode_public_key<E>(value: &str) -> Result<[u8; PUBLIC_KEY_LENGTH], E>
where
    E: de::Error,
{
    let decoded = STANDARD.decode(value).map_err(E::custom)?;
    if STANDARD.encode(&decoded) != value {
        return Err(E::custom(
            "public key must use canonical padded STANDARD base64",
        ));
    }
    decoded.try_into().map_err(|bytes: Vec<u8>| {
        E::custom(format!(
            "public key must decode to {PUBLIC_KEY_LENGTH} bytes, got {}",
            bytes.len()
        ))
    })
}

impl Frame {
    const fn type_name(&self) -> &'static str {
        match self {
            Self::Hello { .. } => "hello",
            Self::HelloOk { .. } => "hello_ok",
            Self::PairOffer { .. } => "pair_offer",
            Self::PairOfferOk => "pair_offer_ok",
            Self::PairClaim { .. } => "pair_claim",
            Self::PairPeer { .. } => "pair_peer",
            Self::Join { .. } => "join",
            Self::JoinOk => "join_ok",
            Self::Clip { .. } => "clip",
            Self::MailboxEmpty => "mailbox_empty",
            Self::Error { .. } => "error",
        }
    }
}

impl ConnectionState {
    const fn index(self) -> usize {
        match self {
            Self::Connected => 0,
            Self::AwaitingHelloOk => 1,
            Self::Ready => 2,
            Self::AwaitingPairOfferOk => 3,
            Self::AwaitingPairPeer => 4,
            Self::ReadyToJoin => 5,
            Self::AwaitingJoinOk => 6,
            Self::Bootstrap => 7,
            Self::Live => 8,
            Self::Closed => 9,
        }
    }
}

#[derive(Clone, Copy)]
enum FrameEvent {
    Hello,
    HelloOk,
    PairOffer,
    PairOfferOk,
    PairClaim,
    PairPeer,
    Join,
    JoinOk,
    MailboxClip,
    LiveClip,
    MailboxEmpty,
    Error,
}

impl FrameEvent {
    const fn from_frame(frame: &Frame) -> Self {
        match frame {
            Frame::Hello { .. } => Self::Hello,
            Frame::HelloOk { .. } => Self::HelloOk,
            Frame::PairOffer { .. } => Self::PairOffer,
            Frame::PairOfferOk => Self::PairOfferOk,
            Frame::PairClaim { .. } => Self::PairClaim,
            Frame::PairPeer { .. } => Self::PairPeer,
            Frame::Join { .. } => Self::Join,
            Frame::JoinOk => Self::JoinOk,
            Frame::Clip { mailbox: true, .. } => Self::MailboxClip,
            Frame::Clip { mailbox: false, .. } => Self::LiveClip,
            Frame::MailboxEmpty => Self::MailboxEmpty,
            Frame::Error { .. } => Self::Error,
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Hello => 0,
            Self::HelloOk => 1,
            Self::PairOffer => 2,
            Self::PairOfferOk => 3,
            Self::PairClaim => 4,
            Self::PairPeer => 5,
            Self::Join => 6,
            Self::JoinOk => 7,
            Self::MailboxClip => 8,
            Self::LiveClip => 9,
            Self::MailboxEmpty => 10,
            Self::Error => 11,
        }
    }
}

use ConnectionState::{
    AwaitingHelloOk, AwaitingJoinOk, AwaitingPairOfferOk, AwaitingPairPeer, Bootstrap, Closed,
    Live, Ready, ReadyToJoin,
};

const TRANSITIONS: [[Option<ConnectionState>; 12]; 10] = build_transitions();

const fn build_transitions() -> [[Option<ConnectionState>; 12]; 10] {
    let mut transitions = [[None; 12]; 10];
    transitions[ConnectionState::Connected.index()][FrameEvent::Hello.index()] =
        Some(AwaitingHelloOk);
    transitions[AwaitingHelloOk.index()][FrameEvent::HelloOk.index()] = Some(Ready);
    transitions[Ready.index()][FrameEvent::PairOffer.index()] = Some(AwaitingPairOfferOk);
    transitions[Ready.index()][FrameEvent::PairClaim.index()] = Some(AwaitingPairPeer);
    transitions[Ready.index()][FrameEvent::Join.index()] = Some(AwaitingJoinOk);
    transitions[AwaitingPairOfferOk.index()][FrameEvent::PairOfferOk.index()] =
        Some(AwaitingPairPeer);
    transitions[AwaitingPairPeer.index()][FrameEvent::PairPeer.index()] = Some(ReadyToJoin);
    transitions[ReadyToJoin.index()][FrameEvent::Join.index()] = Some(AwaitingJoinOk);
    transitions[AwaitingJoinOk.index()][FrameEvent::JoinOk.index()] = Some(Bootstrap);
    transitions[Bootstrap.index()][FrameEvent::MailboxClip.index()] = Some(Live);
    transitions[Bootstrap.index()][FrameEvent::MailboxEmpty.index()] = Some(Live);
    transitions[Live.index()][FrameEvent::LiveClip.index()] = Some(Live);

    let mut active_state = 0;
    while active_state < Closed.index() {
        transitions[active_state][FrameEvent::Error.index()] = Some(Closed);
        active_state += 1;
    }
    transitions
}
