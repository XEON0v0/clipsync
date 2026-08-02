//! Pairing and live-session transport over the relay WebSocket protocol.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use rand::{Rng, RngCore, rngs::OsRng};
use thiserror::Error;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream, client_async_tls_with_config,
    connect_async_with_config,
};

use crate::crypto::{Identity, bundle_bytes, join_sig_msg};
use crate::pairing::{
    Claimer, Offerer, PairingError, PairingRecord, PairingStore, PendingConfirmation, QrPayload,
};
use crate::protocol::{
    Frame, MAX_FRAME_BYTES, MAX_MESSAGE_BYTES, PROTOCOL_VERSION, ProtocolError, decode_frame,
    encode_frame,
};

/// Six-character alphabet without visually ambiguous characters.
pub const PAIRING_CODE_ALPHABET: &[u8; 30] = b"23456789ABCDEFGHJKMNPQRSTVWXYZ";

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Transport-level pairing/session failures.
#[derive(Debug, Error)]
pub enum TransportError {
    #[error("pairing state failed")]
    Pairing(#[from] PairingError),
    #[error("protocol failed")]
    Protocol(#[from] ProtocolError),
    #[error("relay websocket failed: {0}")]
    WebSocket(String),
    #[error("relay rejected the request ({code}): {message}")]
    Server { code: String, message: String },
    #[error("no durable pairing record exists")]
    NotPaired,
    #[error("relay closed the websocket")]
    Closed,
}

/// Connected, hello-authenticated relay client ready to offer a pairing code.
pub struct PairingClient {
    server: String,
    identity: Identity,
    challenge: [u8; 32],
    socket: Socket,
}

impl PairingClient {
    /// Connects to a relay and completes `hello -> hello_ok`.
    pub async fn connect(store: &PairingStore, server: &str) -> Result<Self, TransportError> {
        let identity = store.load_identity()?;
        let (socket, _) = connect_async_with_config(server, Some(relay_websocket_config()), false)
            .await
            .map_err(websocket_error)?;
        Self::handshake(server, identity, socket).await
    }

    /// Connects over `wss://` with a caller-provided TLS configuration and
    /// completes `hello -> hello_ok`.
    ///
    /// This is the test-CA injection path used by end-to-end tests behind a
    /// local TLS front proxy. Production callers use [`PairingClient::connect`],
    /// which trusts only the bundled webpki roots.
    pub async fn connect_with_tls(
        store: &PairingStore,
        server: &str,
        tls: Arc<rustls::ClientConfig>,
    ) -> Result<Self, TransportError> {
        let identity = store.load_identity()?;
        let request = server.into_client_request().map_err(websocket_error)?;
        let host = request
            .uri()
            .host()
            .ok_or_else(|| TransportError::WebSocket(format!("relay URL {server} has no host")))?
            .to_owned();
        let port = request.uri().port_u16().unwrap_or(443);
        let stream = TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|error| TransportError::WebSocket(format!("tcp connect failed: {error}")))?;
        let (socket, _) = client_async_tls_with_config(
            request,
            stream,
            Some(relay_websocket_config()),
            Some(Connector::Rustls(tls)),
        )
        .await
        .map_err(websocket_error)?;
        Self::handshake(server, identity, socket).await
    }

    async fn handshake(
        server: &str,
        identity: Identity,
        mut socket: Socket,
    ) -> Result<Self, TransportError> {
        send_frame(
            &mut socket,
            &Frame::Hello {
                device_id: identity.device_id(),
                pub_bundle: identity.public_bundle(),
                version: PROTOCOL_VERSION,
            },
        )
        .await?;
        let challenge = match receive_frame(&mut socket).await? {
            Frame::HelloOk {
                server_version,
                nonce_b64,
            } if server_version == PROTOCOL_VERSION => decode_challenge(&nonce_b64)?,
            Frame::Error { code, message } => return Err(TransportError::Server { code, message }),
            frame => return Err(unexpected("hello_ok", &frame)),
        };
        Ok(Self {
            server: server.to_owned(),
            identity,
            challenge,
            socket,
        })
    }

    /// Publishes a fresh one-time pairing offer and returns its QR only after
    /// the relay acknowledges publication.
    pub async fn pair_begin(mut self) -> Result<PairingOffer, TransportError> {
        let code = generate_pairing_code();
        send_frame(
            &mut self.socket,
            &Frame::PairOffer {
                code: code.clone(),
                pub_bundle: self.identity.public_bundle(),
            },
        )
        .await?;
        match receive_frame(&mut self.socket).await? {
            Frame::PairOfferOk => {}
            Frame::Error { code, message } => return Err(TransportError::Server { code, message }),
            frame => return Err(unexpected("pair_offer_ok", &frame)),
        }
        let mut nonce_a = [0_u8; 16];
        OsRng.fill_bytes(&mut nonce_a);
        let qr = QrPayload::new(&self.server, &code, self.identity.public_bundle(), nonce_a)?;
        Ok(PairingOffer {
            offerer: Offerer::start(&self.identity, qr.clone())?,
            qr,
            identity: self.identity,
            challenge: self.challenge,
            socket: self.socket,
        })
    }

    /// Claims a QR-pinned offer and returns the mandatory SAS gate.
    pub async fn claim(
        store: &PairingStore,
        qr: &QrPayload,
    ) -> Result<PairingChannel, TransportError> {
        let claimer_identity = store.load_identity()?;
        let claimer = Claimer::start(&claimer_identity, qr.clone())?;
        let client = Self::connect(store, &qr.server).await?;
        Self::claim_channel(qr, claimer, client).await
    }

    /// Claims a QR-pinned offer over `wss://` with a caller-provided TLS
    /// configuration (the test-CA injection path; see
    /// [`PairingClient::connect_with_tls`]).
    pub async fn claim_with_tls(
        store: &PairingStore,
        qr: &QrPayload,
        tls: Arc<rustls::ClientConfig>,
    ) -> Result<PairingChannel, TransportError> {
        let claimer_identity = store.load_identity()?;
        let claimer = Claimer::start(&claimer_identity, qr.clone())?;
        let client = Self::connect_with_tls(store, &qr.server, tls).await?;
        Self::claim_channel(qr, claimer, client).await
    }

    async fn claim_channel(
        qr: &QrPayload,
        claimer: Claimer,
        mut client: PairingClient,
    ) -> Result<PairingChannel, TransportError> {
        send_frame(
            &mut client.socket,
            &Frame::PairClaim {
                code: qr.code.clone(),
                pub_bundle: client.identity.public_bundle(),
            },
        )
        .await?;
        let peer = match receive_frame(&mut client.socket).await? {
            Frame::PairPeer { peer_pub_bundle } => peer_pub_bundle,
            Frame::Error { code, message } => return Err(TransportError::Server { code, message }),
            frame => return Err(unexpected("pair_peer", &frame)),
        };
        let pending = claimer.receive_peer(&qr.server, &peer)?;
        Ok(PairingChannel {
            pending,
            identity: client.identity,
            challenge: client.challenge,
            socket: client.socket,
        })
    }

    /// Rejoins using the durable pairing record and consumes exactly one
    /// bootstrap frame before entering live mode.
    pub async fn join(store: &PairingStore) -> Result<LiveLink, TransportError> {
        let record = store.load_pairing()?.ok_or(TransportError::NotPaired)?;
        let client = Self::connect(store, &record.server).await?;
        join_live(client.identity, record, client.challenge, client.socket).await
    }
}

fn relay_websocket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_frame_size(Some(MAX_FRAME_BYTES))
        .max_message_size(Some(MAX_MESSAGE_BYTES))
}

/// Builds a rustls client configuration trusting exactly the given
/// DER-encoded root certificates.
///
/// This exists so end-to-end tests can inject a locally generated test CA
/// behind a TLS front proxy; production connections keep the bundled webpki
/// roots via [`PairingClient::connect`].
///
/// # Errors
/// Returns an error when any root fails to parse as an X.509 certificate.
pub fn tls_config_with_roots(
    root_ders: &[&[u8]],
) -> Result<Arc<rustls::ClientConfig>, TransportError> {
    let mut roots = rustls::RootCertStore::empty();
    for der in root_ders {
        roots
            .add(rustls::pki_types::CertificateDer::from(der.to_vec()))
            .map_err(|error| {
                TransportError::WebSocket(format!("test CA certificate is invalid: {error}"))
            })?;
    }
    Ok(Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    ))
}

/// Published offer waiting for the peer bundle.
pub struct PairingOffer {
    qr: QrPayload,
    offerer: Offerer,
    identity: Identity,
    challenge: [u8; 32],
    socket: Socket,
}

impl PairingOffer {
    #[must_use]
    pub const fn qr(&self) -> &QrPayload {
        &self.qr
    }

    pub async fn wait_peer(mut self) -> Result<PairingChannel, TransportError> {
        let peer = match receive_frame(&mut self.socket).await? {
            Frame::PairPeer { peer_pub_bundle } => peer_pub_bundle,
            Frame::Error { code, message } => return Err(TransportError::Server { code, message }),
            frame => return Err(unexpected("pair_peer", &frame)),
        };
        let pending = self.offerer.receive_peer(peer)?;
        Ok(PairingChannel {
            pending,
            identity: self.identity,
            challenge: self.challenge,
            socket: self.socket,
        })
    }
}

/// Paired identities waiting for explicit SAS confirmation.
pub struct PairingChannel {
    pending: PendingConfirmation,
    identity: Identity,
    challenge: [u8; 32],
    socket: Socket,
}

impl fmt::Debug for PairingChannel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PairingChannel")
            .field("peer", &self.pending.peer_bundle())
            .finish_non_exhaustive()
    }
}

impl PairingChannel {
    #[must_use]
    pub const fn pending(&self) -> &PendingConfirmation {
        &self.pending
    }

    /// Confirms the SAS, durably records the pairing, then joins over the same
    /// challenge-bound websocket.
    pub async fn confirm_pair(
        self,
        sas: &str,
        store: &PairingStore,
    ) -> Result<LiveLink, TransportError> {
        let done = self.pending.confirm(sas, store)?;
        join_live(
            self.identity,
            done.record().clone(),
            self.challenge,
            self.socket,
        )
        .await
    }
}

/// Live relay connection after the required single bootstrap frame.
pub struct LiveLink {
    identity: Identity,
    record: PairingRecord,
    socket: Socket,
    bootstrap_clip: Option<(String, String)>,
}

/// Event produced by the resilient receive path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LiveEvent {
    Frame(Frame),
    Reconnected {
        bootstrap_clip: Option<(String, String)>,
    },
}

impl fmt::Debug for LiveLink {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LiveLink")
            .field("room_id", &self.record.room_id)
            .field("bootstrap", &self.bootstrap_clip.is_some())
            .finish_non_exhaustive()
    }
}

impl LiveLink {
    #[must_use]
    pub const fn identity(&self) -> &Identity {
        &self.identity
    }

    #[must_use]
    pub const fn record(&self) -> &PairingRecord {
        &self.record
    }

    #[must_use]
    pub fn bootstrap_clip(&self) -> Option<(String, String)> {
        self.bootstrap_clip.clone()
    }

    pub async fn send(&mut self, frame: &Frame) -> Result<(), TransportError> {
        send_frame(&mut self.socket, frame).await
    }

    pub async fn recv(&mut self) -> Result<Frame, TransportError> {
        let frame = receive_frame(&mut self.socket).await?;
        match frame {
            frame @ Frame::Clip { mailbox: false, .. } => Ok(frame),
            Frame::Error { code, message } => Err(TransportError::Server { code, message }),
            frame => Err(unexpected("live clip", &frame)),
        }
    }

    /// Receives a live frame, automatically rejoining after transport closure.
    /// Reconnect failures use the protocol's 1s-to-30s jittered backoff and the
    /// successful join's bootstrap is surfaced exactly once to the caller.
    pub async fn recv_reconnecting(
        &mut self,
        store: &PairingStore,
    ) -> Result<LiveEvent, TransportError> {
        match self.recv().await {
            Ok(frame) => Ok(LiveEvent::Frame(frame)),
            Err(TransportError::Closed | TransportError::WebSocket(_)) => {
                self.reconnect(store).await?;
                Ok(LiveEvent::Reconnected {
                    bootstrap_clip: self.bootstrap_clip(),
                })
            }
            Err(error) => Err(error),
        }
    }

    /// Replaces this link with a freshly joined connection, retrying only
    /// transient WebSocket failures. Authentication/protocol failures fail closed.
    pub async fn reconnect(&mut self, store: &PairingStore) -> Result<(), TransportError> {
        let mut attempt = 0;
        loop {
            match PairingClient::join(store).await {
                Ok(link) => {
                    *self = link;
                    return Ok(());
                }
                Err(TransportError::Closed | TransportError::WebSocket(_)) => {
                    tokio::time::sleep(backoff_delay(attempt)).await;
                    attempt = attempt.saturating_add(1);
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub async fn close(mut self) -> Result<(), TransportError> {
        self.socket.close(None).await.map_err(websocket_error)
    }
}

async fn join_live(
    identity: Identity,
    record: PairingRecord,
    challenge: [u8; 32],
    mut socket: Socket,
) -> Result<LiveLink, TransportError> {
    let device_id = identity.device_id();
    let bundle = identity.public_bundle();
    let message = join_sig_msg(
        &challenge,
        &record.room_id,
        &device_id,
        &bundle_bytes(&bundle),
    )
    .map_err(PairingError::Crypto)?;
    send_frame(
        &mut socket,
        &Frame::Join {
            room_id: record.room_id.clone(),
            device_id,
            pub_bundle: bundle,
            sig_b64: STANDARD.encode(identity.sign(&message)),
        },
    )
    .await?;
    match receive_frame(&mut socket).await? {
        Frame::JoinOk => {}
        Frame::Error { code, message } => return Err(TransportError::Server { code, message }),
        frame => return Err(unexpected("join_ok", &frame)),
    }
    let bootstrap_clip = match receive_frame(&mut socket).await? {
        Frame::MailboxEmpty => None,
        Frame::Clip {
            room_id,
            ciphertext_b64,
            mailbox: true,
            ..
        } => Some((room_id, ciphertext_b64)),
        Frame::Error { code, message } => return Err(TransportError::Server { code, message }),
        frame => return Err(unexpected("bootstrap", &frame)),
    };
    Ok(LiveLink {
        identity,
        record,
        socket,
        bootstrap_clip,
    })
}

async fn send_frame(socket: &mut Socket, frame: &Frame) -> Result<(), TransportError> {
    let encoded = encode_frame(frame)?;
    let text = String::from_utf8(encoded).map_err(|_| ProtocolError::BadFrame {
        message: "encoded frame was not UTF-8".to_owned(),
    })?;
    socket
        .send(Message::Text(text.into()))
        .await
        .map_err(websocket_error)
}

async fn receive_frame(socket: &mut Socket) -> Result<Frame, TransportError> {
    match socket.next().await {
        Some(Ok(Message::Text(text))) => Ok(decode_frame(text.as_bytes())?),
        Some(Ok(Message::Close(_))) | None => Err(TransportError::Closed),
        Some(Ok(_)) => Err(ProtocolError::BadFrame {
            message: "relay sent a non-text websocket frame".to_owned(),
        }
        .into()),
        Some(Err(error)) => Err(websocket_error(error)),
    }
}

fn decode_challenge(encoded: &str) -> Result<[u8; 32], TransportError> {
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| ProtocolError::BadFrame {
            message: "hello_ok nonce is not base64".to_owned(),
        })?;
    bytes.try_into().map_err(|_| {
        ProtocolError::BadFrame {
            message: "hello_ok nonce must be 32 bytes".to_owned(),
        }
        .into()
    })
}

fn unexpected(expected: &str, actual: &Frame) -> TransportError {
    ProtocolError::BadFrame {
        message: format!("expected {expected}, received {actual:?}"),
    }
    .into()
}

fn websocket_error(error: tokio_tungstenite::tungstenite::Error) -> TransportError {
    TransportError::WebSocket(error.to_string())
}

/// Generates a random six-character pairing code.
#[must_use]
pub fn generate_pairing_code() -> String {
    let mut rng = OsRng;
    (0..6)
        .map(|_| char::from(PAIRING_CODE_ALPHABET[rng.gen_range(0..PAIRING_CODE_ALPHABET.len())]))
        .collect()
}

/// Exponential reconnect delay from 1s to 30s with random +/-20% jitter.
#[must_use]
pub fn backoff_delay(attempt: u32) -> Duration {
    backoff_delay_with_jitter(attempt, OsRng.gen_range(-0.2..=0.2))
}

#[doc(hidden)]
#[must_use]
pub fn backoff_delay_with_jitter(attempt: u32, jitter: f64) -> Duration {
    let seconds = 2_u64.saturating_pow(attempt.min(31)).min(30);
    let factor = 1.0 + jitter.clamp(-0.2, 0.2);
    Duration::from_millis(((seconds as f64) * 1000.0 * factor).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_websocket_uses_protocol_frame_and_message_limits() {
        let config = relay_websocket_config();
        assert_eq!(config.max_frame_size, Some(MAX_FRAME_BYTES));
        assert_eq!(config.max_message_size, Some(MAX_MESSAGE_BYTES));
    }
}
