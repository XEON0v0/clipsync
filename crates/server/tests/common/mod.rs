//! Shared integration-test harness: in-memory server, fake mailbox sink, and a
//! websocket client speaking the real protocol.
//!
//! Every integration-test binary links this module; not all use every helper.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signer, SigningKey};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use rand::RngCore;
use rand::rngs::OsRng;
use tokio::net::TcpStream;
use tokio::sync::Notify;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use clipboard_core::crypto::{bundle_fp, device_id, join_sig_msg, room_id};
use clipboard_core::protocol::{
    Frame, PROTOCOL_VERSION, PubBundle, bundle_bytes, decode_frame, encode_frame,
};
use clipboard_server::{
    InMemoryRegistry, MailboxSink, PairingUnavailable, PendingClip, ServerConfig, ServerHandle,
    start,
};

pub const TEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Mailbox fake recording every seam call, with a notify for barrier-style tests.
#[derive(Default)]
pub struct RecordingMailbox {
    pub pending: Mutex<Vec<(String, String, PendingClip)>>,
    pub consumed: Mutex<Vec<(String, String)>>,
    pub notify: Notify,
}

impl RecordingMailbox {
    pub fn pending_count(&self) -> usize {
        self.pending.lock().unwrap().len()
    }

    /// Waits until at least `n` pending clips were recorded.
    pub async fn wait_pending(&self, n: usize) {
        timeout(TEST_TIMEOUT, async {
            loop {
                if self.pending_count() >= n {
                    return;
                }
                self.notify.notified().await;
            }
        })
        .await
        .expect("timed out waiting for mailbox pending clips");
    }
}

impl MailboxSink for RecordingMailbox {
    fn clip_pending(&self, room_id: &str, recipient_fp: &str, clip: PendingClip) {
        self.pending
            .lock()
            .unwrap()
            .push((room_id.to_owned(), recipient_fp.to_owned(), clip));
        self.notify.notify_waiters();
    }

    fn clip_consumed(&self, room_id: &str, recipient_fp: &str) {
        self.consumed
            .lock()
            .unwrap()
            .push((room_id.to_owned(), recipient_fp.to_owned()));
    }
}

pub struct TestServer {
    pub addr: SocketAddr,
    pub registry: Arc<InMemoryRegistry>,
    pub mailbox: Arc<RecordingMailbox>,
    _handle: ServerHandle,
}

/// Starts a server with the given config on an ephemeral port.
pub async fn start_server(config: ServerConfig) -> TestServer {
    let registry = Arc::new(InMemoryRegistry::new());
    let mailbox = Arc::new(RecordingMailbox::default());
    let handle = start(
        "127.0.0.1:0".parse().unwrap(),
        config,
        registry.clone(),
        Arc::new(PairingUnavailable),
        mailbox.clone(),
    )
    .await
    .expect("server binds");
    TestServer {
        addr: handle.addr(),
        registry,
        mailbox,
        _handle: handle,
    }
}

pub async fn start_default_server() -> TestServer {
    start_server(ServerConfig::default()).await
}

/// A test identity: real Ed25519 signing key, random DH public key placeholder
/// (the relay never uses the DH key beyond fingerprinting).
pub struct TestIdentity {
    pub signing: SigningKey,
    pub bundle: PubBundle,
}

impl TestIdentity {
    pub fn generate() -> Self {
        let mut sign_secret = [0_u8; 32];
        OsRng.fill_bytes(&mut sign_secret);
        let signing = SigningKey::from_bytes(&sign_secret);
        let sign_pk = signing.verifying_key().to_bytes();
        let mut dh_pk = [0_u8; 32];
        OsRng.fill_bytes(&mut dh_pk);
        Self {
            signing,
            bundle: PubBundle { sign_pk, dh_pk },
        }
    }

    pub fn fp(&self) -> String {
        bundle_fp(&self.bundle)
    }

    pub fn device_id(&self) -> String {
        device_id(&self.bundle.sign_pk)
    }

    pub fn hello_frame(&self) -> Frame {
        Frame::Hello {
            device_id: self.device_id(),
            pub_bundle: self.bundle.clone(),
            version: PROTOCOL_VERSION,
        }
    }

    pub fn join_frame(&self, room: &str, nonce: &[u8]) -> Frame {
        self.join_frame_with_device_id(room, nonce, &self.device_id())
    }

    /// Builds a join frame whose `device_id` field (and signed message) uses
    /// `claimed_device_id` — used for tamper tests.
    pub fn join_frame_with_device_id(
        &self,
        room: &str,
        nonce: &[u8],
        claimed_device_id: &str,
    ) -> Frame {
        let message = join_sig_msg(nonce, room, claimed_device_id, &bundle_bytes(&self.bundle))
            .expect("join message");
        let signature = self.signing.sign(&message);
        Frame::Join {
            room_id: room.to_owned(),
            device_id: claimed_device_id.to_owned(),
            pub_bundle: self.bundle.clone(),
            sig_b64: STANDARD.encode(signature.to_bytes()),
        }
    }
}

/// Registers the room for exactly identities `a` and `b` and returns its room_id.
pub fn register_room(server: &TestServer, a: &TestIdentity, b: &TestIdentity) -> String {
    let room = room_id(&a.bundle, &b.bundle);
    server.registry.register_room(&room, &[a.fp(), b.fp()]);
    room
}

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// What a read attempt produced.
#[derive(Debug)]
pub enum Read {
    Frame(Frame),
    Closed,
    Timeout,
}

pub struct Client {
    sink: SplitSink<WsStream, Message>,
    stream: SplitStream<WsStream>,
}

impl Client {
    pub async fn connect(addr: SocketAddr) -> Self {
        let url = format!("ws://{addr}/ws");
        let (ws, _response) = tokio_tungstenite::connect_async(&url)
            .await
            .expect("websocket upgrade");
        let (sink, stream) = ws.split();
        Self { sink, stream }
    }

    pub async fn connect_with_xff(addr: SocketAddr, xff: &str) -> Self {
        let url = format!("ws://{addr}/ws");
        let mut request =
            tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(&url)
                .expect("request");
        request
            .headers_mut()
            .insert("x-forwarded-for", xff.parse().expect("header value"));
        let (ws, _response) = tokio_tungstenite::connect_async(request)
            .await
            .expect("websocket upgrade");
        let (sink, stream) = ws.split();
        Self { sink, stream }
    }

    pub async fn send(&mut self, frame: &Frame) {
        let encoded = encode_frame(frame).expect("frame encodes");
        self.sink
            .send(Message::Text(String::from_utf8(encoded).unwrap().into()))
            .await
            .expect("send frame");
    }

    /// Sends a frame but tolerates the peer having already closed.
    pub async fn send_tolerant(&mut self, frame: &Frame) {
        let encoded = encode_frame(frame).expect("frame encodes");
        let _ = self
            .sink
            .send(Message::Text(String::from_utf8(encoded).unwrap().into()))
            .await;
    }

    /// Sends raw text; tolerant of the peer closing mid-send (size-limit tests).
    pub async fn send_raw_text(&mut self, text: String) {
        let _ = self.sink.send(Message::Text(text.into())).await;
    }

    pub async fn send_binary(&mut self, bytes: Vec<u8>) {
        self.sink
            .send(Message::Binary(bytes.into()))
            .await
            .expect("send binary");
    }

    /// Reads the next protocol frame, a close, or a timeout.
    pub async fn read(&mut self, wait: Duration) -> Read {
        match timeout(wait, self.stream.next()).await {
            Err(_) => Read::Timeout,
            Ok(None) => Read::Closed,
            Ok(Some(Err(_))) => Read::Closed,
            Ok(Some(Ok(Message::Close(_)))) => Read::Closed,
            Ok(Some(Ok(Message::Text(text)))) => {
                Read::Frame(decode_frame(text.as_bytes()).expect("server frame decodes"))
            }
            Ok(Some(Ok(other))) => panic!("unexpected websocket message: {other:?}"),
        }
    }

    pub async fn recv_frame(&mut self) -> Frame {
        match self.read(TEST_TIMEOUT).await {
            Read::Frame(frame) => frame,
            other => panic!("expected frame, got {other:?}"),
        }
    }

    /// Expects an error frame with `code`, returning its message.
    pub async fn expect_error_close(&mut self, code: &str) -> String {
        match self.recv_frame().await {
            Frame::Error {
                code: actual,
                message,
            } => {
                assert_eq!(actual, code, "unexpected error code");
                message
            }
            other => panic!("expected error {code}, got {other:?}"),
        }
    }

    /// Expects an error frame whose code is one of `codes`, returning the code.
    pub async fn expect_error_close_broad(&mut self, codes: &[&str]) -> String {
        match self.recv_frame().await {
            Frame::Error { code, .. } => {
                assert!(
                    codes.contains(&code.as_str()),
                    "unexpected error code {code}"
                );
                code
            }
            other => panic!("expected one of {codes:?}, got {other:?}"),
        }
    }

    pub async fn expect_closed(&mut self) {
        match self.read(TEST_TIMEOUT).await {
            Read::Closed => {}
            other => panic!("expected close, got {other:?}"),
        }
    }

    /// Sends `hello`, expects `hello_ok`, returns the decoded 32-byte nonce.
    pub async fn hello(&mut self, identity: &TestIdentity) -> [u8; 32] {
        self.send(&identity.hello_frame()).await;
        match self.recv_frame().await {
            Frame::HelloOk {
                server_version,
                nonce_b64,
            } => {
                assert_eq!(server_version, PROTOCOL_VERSION);
                let nonce = STANDARD.decode(&nonce_b64).expect("nonce is base64");
                assert_eq!(nonce.len(), 32, "challenge nonce must be 32 bytes");
                nonce.try_into().unwrap()
            }
            other => panic!("expected hello_ok, got {other:?}"),
        }
    }

    /// Full join handshake; returns the bootstrap frame.
    pub async fn join(&mut self, identity: &TestIdentity, room: &str) -> Frame {
        let nonce = self.hello(identity).await;
        self.send(&identity.join_frame(room, &nonce)).await;
        self.expect_joined().await
    }

    /// Expects join_ok followed by the bootstrap frame, returning the latter.
    pub async fn expect_joined(&mut self) -> Frame {
        match self.recv_frame().await {
            Frame::JoinOk => {}
            other => panic!("expected join_ok, got {other:?}"),
        }
        self.recv_frame().await
    }

    /// Expects join_ok and an empty bootstrap (used after a join was already sent).
    pub async fn join_live_bootstrap(&mut self) {
        assert_eq!(self.expect_joined().await, Frame::MailboxEmpty);
    }

    /// Joins and requires an empty bootstrap.
    pub async fn join_live(&mut self, identity: &TestIdentity, room: &str) {
        let bootstrap = self.join(identity, room).await;
        assert_eq!(bootstrap, Frame::MailboxEmpty, "expected empty bootstrap");
    }

    pub fn clip_frame(room: &str, ciphertext_b64: &str) -> Frame {
        Frame::Clip {
            room_id: room.to_owned(),
            ciphertext_b64: ciphertext_b64.to_owned(),
            origin_device: "client-claimed-origin".to_owned(),
            mailbox: false,
        }
    }
}

/// Encodes arbitrary bytes as canonical base64 for clip payloads.
pub fn b64(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

/// Polls `condition` until true or the test timeout elapses.
pub async fn eventually(mut condition: impl FnMut() -> bool) {
    timeout(TEST_TIMEOUT, async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("condition did not become true in time");
}
