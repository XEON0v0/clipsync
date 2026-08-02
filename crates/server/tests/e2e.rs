//! End-to-end integration suite (T11): twenty-nine scenarios driven in-process
//! against the real axum relay and the real core `PairingClient`/`Session`.
//!
//! Every scenario speaks the wire protocol over real websockets on 127.0.0.1
//! ephemeral ports; scenario 16 terminates `wss://` at a dev-only rcgen TLS
//! front proxy (X-Forwarded-For rewritten from the TLS TCP peer) with the test
//! CA injected into the core client. Timeouts are generous but bounded; no
//! sleeps are used for synchronization (event-log hooks, mailbox waiters, and
//! timeout channels instead).

mod common;

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tempfile::TempDir;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, mpsc};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream, client_async_tls_with_config};

use clipboard_core::crypto::{
    CryptoError, Identity, SessionKey, aad, bundle_fp, join_sig_msg, open, room_id,
};
use clipboard_core::history::{HistoryKind, HistorySource, HistoryStore};
use clipboard_core::pairing::{
    PairingError, PairingStore, QrPayload, reset_pairing_state_after_quiesce,
};
use clipboard_core::pairing_client::{
    LiveLink, PairingClient, TransportError, tls_config_with_roots,
};
use clipboard_core::protocol::{
    ContentKind, Frame, MAX_FRAME_BYTES, MAX_MESSAGE_BYTES, PROTOCOL_VERSION, ProtocolError,
    PubBundle, bundle_bytes, decode_envelope, decode_frame, encode_envelope, encode_frame,
};
use clipboard_core::session::{
    CallbackError, ClipContent, ClipItem, InboundOutcome, MailboxDisposition, SendStage, Session,
    SessionCallback, SessionError, SessionStore, text_hash,
};
use clipboard_server::{
    InMemoryRegistry, IpNet, Limits, MailboxOptions, PairingConfig, PairingRelay, PersistentMailbox,
    PersistentRegistry, Registry, ServerConfig, ServerHandle, TestHooks, start,
};

use common::{Client, Read, RecordingMailbox, TestIdentity, eventually};

/// Generous bound for any single network round trip.
const TIMEOUT: Duration = Duration::from_secs(10);
/// Bound for the 10 MiB payload round trip.
const BIG_TIMEOUT: Duration = Duration::from_secs(30);
/// Bound for negative assertions ("nothing arrives").
const NEG_TIMEOUT: Duration = Duration::from_millis(500);

fn now_ms() -> i64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the epoch")
        .as_millis();
    i64::try_from(millis).expect("millis fit i64")
}

// ---------------------------------------------------------------------------
// Endpoint and callback helpers
// ---------------------------------------------------------------------------

struct RecordingCallback {
    live: Mutex<Vec<ClipItem>>,
    mailbox: Mutex<Vec<ClipItem>>,
    disposition: Mutex<MailboxDisposition>,
}

impl RecordingCallback {
    fn new() -> Self {
        Self {
            live: Mutex::new(Vec::new()),
            mailbox: Mutex::new(Vec::new()),
            disposition: Mutex::new(MailboxDisposition::Applied),
        }
    }

    fn set_disposition(&self, disposition: MailboxDisposition) {
        *self.disposition.lock().expect("disposition lock") = disposition;
    }

    fn live_items(&self) -> Vec<ClipItem> {
        self.live.lock().expect("live lock").clone()
    }

    fn mailbox_items(&self) -> Vec<ClipItem> {
        self.mailbox.lock().expect("mailbox lock").clone()
    }
}

impl SessionCallback for RecordingCallback {
    fn on_clip(&self, item: &ClipItem) -> Result<(), CallbackError> {
        self.live.lock().expect("live lock").push(item.clone());
        Ok(())
    }

    fn on_mailbox_clip(&self, item: &ClipItem) -> Result<MailboxDisposition, CallbackError> {
        self.mailbox.lock().expect("mailbox lock").push(item.clone());
        Ok(*self.disposition.lock().expect("disposition lock"))
    }
}

/// One device: pairing store, session store, history store, and callback sink.
struct Endpoint {
    _dir: TempDir,
    store: PairingStore,
    session_dir: PathBuf,
    history_dir: PathBuf,
    callback: Arc<RecordingCallback>,
}

impl Endpoint {
    fn new() -> Self {
        let dir = TempDir::new().expect("temp dir");
        Self::from_dir(dir)
    }

    /// An endpoint whose identity is fixed to the T3 golden fixtures.
    fn with_identity(sign_secret: [u8; 32], dh_secret: [u8; 32]) -> Self {
        let dir = TempDir::new().expect("temp dir");
        let pairing_dir = dir.path().join("pairing");
        std::fs::create_dir_all(&pairing_dir).expect("pairing dir");
        let document = serde_json::json!({
            "generation": 1,
            "sign_sk": STANDARD.encode(sign_secret),
            "dh_sk": STANDARD.encode(dh_secret),
        });
        std::fs::write(
            pairing_dir.join("identity.json"),
            serde_json::to_vec(&document).expect("identity document"),
        )
        .expect("seed identity");
        Self::from_dir(dir)
    }

    fn from_dir(dir: TempDir) -> Self {
        let store = PairingStore::new(dir.path().join("pairing")).expect("pairing store");
        store.load_identity().expect("identity loads");
        Self {
            session_dir: dir.path().join("session"),
            history_dir: dir.path().join("history"),
            callback: Arc::new(RecordingCallback::new()),
            store,
            _dir: dir,
        }
    }

    fn identity(&self) -> Identity {
        self.store.load_identity().expect("identity loads")
    }

    fn bundle(&self) -> PubBundle {
        self.identity().public_bundle()
    }

    fn fp(&self) -> String {
        bundle_fp(&self.bundle())
    }

    fn session(&self, link: &LiveLink) -> Session {
        self.try_session(link).expect("session opens")
    }

    fn try_session(&self, link: &LiveLink) -> Result<Session, SessionError> {
        Session::new(
            link.identity(),
            link.record(),
            SessionStore::new(&self.session_dir).expect("session store"),
            HistoryStore::new(&self.history_dir).expect("history store"),
            self.callback.clone(),
        )
    }
}

// ---------------------------------------------------------------------------
// Relay helpers
// ---------------------------------------------------------------------------

struct Relay {
    addr: SocketAddr,
    url: String,
    registry: Arc<InMemoryRegistry>,
    mailbox: Arc<RecordingMailbox>,
    events: mpsc::UnboundedReceiver<&'static str>,
    _handle: ServerHandle,
}

async fn start_relay() -> Relay {
    start_relay_with(ServerConfig::default()).await
}

async fn start_relay_with(mut config: ServerConfig) -> Relay {
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    config.hooks.event_log = Some(event_tx);
    let registry = Arc::new(InMemoryRegistry::new());
    let pairing = Arc::new(PairingRelay::new(registry.clone(), PairingConfig::default()));
    let mailbox = Arc::new(RecordingMailbox::default());
    let handle = start(
        "127.0.0.1:0".parse().expect("bind address"),
        config,
        registry.clone(),
        pairing,
        mailbox.clone(),
    )
    .await
    .expect("server binds");
    let addr = handle.addr();
    Relay {
        addr,
        url: format!("ws://{addr}/ws"),
        registry,
        mailbox,
        events: event_rx,
        _handle: handle,
    }
}

/// Waits until the room actor inbox accepted an event with `label`, skipping
/// any earlier labels.
async fn wait_event(events: &mut mpsc::UnboundedReceiver<&'static str>, label: &'static str) {
    timeout(TIMEOUT, async {
        while let Some(got) = events.recv().await {
            if got == label {
                return;
            }
        }
        panic!("event channel closed before {label}");
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {label} event"));
}

fn drain_events(events: &mut mpsc::UnboundedReceiver<&'static str>) {
    while events.try_recv().is_ok() {}
}

/// Full pairing dance between two endpoints; returns both live links.
async fn pair(url: &str, a: &Endpoint, b: &Endpoint) -> (LiveLink, LiveLink) {
    let client = PairingClient::connect(&a.store, url)
        .await
        .expect("offerer connects");
    let offer = client.pair_begin().await.expect("pair_begin");
    let qr = offer.qr().clone();
    let (channel_a, channel_b) = tokio::join!(
        async { offer.wait_peer().await.expect("offerer waits for peer") },
        async {
            PairingClient::claim(&b.store, &qr)
                .await
                .expect("claimer claims")
        },
    );
    confirm_channels(channel_a, channel_b, a, b).await
}

/// Full pairing dance over `wss://` with an injected test CA.
async fn pair_tls(
    url: &str,
    tls: &Arc<rustls::ClientConfig>,
    a: &Endpoint,
    b: &Endpoint,
) -> (LiveLink, LiveLink) {
    let client = PairingClient::connect_with_tls(&a.store, url, tls.clone())
        .await
        .expect("offerer connects over wss");
    let offer = client.pair_begin().await.expect("pair_begin");
    let qr = offer.qr().clone();
    let (channel_a, channel_b) = tokio::join!(
        async { offer.wait_peer().await.expect("offerer waits for peer") },
        async {
            PairingClient::claim_with_tls(&b.store, &qr, tls.clone())
                .await
                .expect("claimer claims over wss")
        },
    );
    confirm_channels(channel_a, channel_b, a, b).await
}

async fn confirm_channels(
    channel_a: clipboard_core::pairing_client::PairingChannel,
    channel_b: clipboard_core::pairing_client::PairingChannel,
    a: &Endpoint,
    b: &Endpoint,
) -> (LiveLink, LiveLink) {
    let sas = channel_a.pending().sas();
    assert_eq!(
        sas,
        channel_b.pending().sas(),
        "SAS must match on both endpoints"
    );
    tokio::join!(
        async {
            channel_a
                .confirm_pair(&sas, &a.store)
                .await
                .expect("offerer confirms")
        },
        async {
            channel_b
                .confirm_pair(&sas, &b.store)
                .await
                .expect("claimer confirms")
        },
    )
}

// ---------------------------------------------------------------------------
// Wire helpers
// ---------------------------------------------------------------------------

async fn recv_clip(link: &mut LiveLink) -> Frame {
    recv_clip_within(link, TIMEOUT).await
}

async fn recv_clip_within(link: &mut LiveLink, wait: Duration) -> Frame {
    timeout(wait, link.recv())
        .await
        .expect("timed out waiting for a live clip")
        .expect("recv live clip")
}

/// Negative assertion: no clip arrives within `wait`.
async fn expect_no_clip(link: &mut LiveLink, wait: Duration) {
    match timeout(wait, link.recv()).await {
        Err(_) => {}
        Ok(Ok(frame)) => panic!("expected no clip, received {frame:?}"),
        Ok(Err(error)) => panic!("expected no clip, connection failed: {error}"),
    }
}

fn clip_fields(frame: &Frame) -> (String, String, bool, String) {
    match frame {
        Frame::Clip {
            room_id,
            ciphertext_b64,
            origin_device,
            mailbox,
        } => (
            room_id.clone(),
            ciphertext_b64.clone(),
            *mailbox,
            origin_device.clone(),
        ),
        other => panic!("expected clip frame, got {other:?}"),
    }
}

/// Sends a text clip from `session` over `link` and returns the wire frame.
async fn send_text(session: &mut Session, link: &mut LiveLink, text: &str, ts_ms: i64) -> Frame {
    let frame = session
        .send_clip(ClipContent::Text(text.to_owned()), ts_ms)
        .expect("send_clip");
    link.send(&frame).await.expect("send over relay");
    frame
}

/// Delivers a received wire clip into the receiving session.
fn deliver(session: &mut Session, frame: &Frame, now: i64) -> InboundOutcome {
    let (room, ciphertext, mailbox, _) = clip_fields(frame);
    session
        .handle_clip(&room, &ciphertext, mailbox, now)
        .expect("handle_clip")
}

fn history_texts(session: &Session) -> Vec<(String, HistorySource)> {
    session
        .history()
        .list()
        .iter()
        .filter_map(|item| match &item.kind {
            HistoryKind::Text { content } => Some((content.clone(), item.source.clone())),
            HistoryKind::Image { .. } => None,
        })
        .collect()
}

/// Manual hello + join against a relay using a core identity; returns the
/// bootstrap frame.
async fn raw_join(client: &mut Client, identity: &Identity, room: &str) -> Frame {
    let device = identity.device_id();
    let bundle = identity.public_bundle();
    client
        .send(&Frame::Hello {
            device_id: device.clone(),
            pub_bundle: bundle.clone(),
            version: PROTOCOL_VERSION,
        })
        .await;
    let nonce: [u8; 32] = match client.recv_frame().await {
        Frame::HelloOk {
            server_version,
            nonce_b64,
        } => {
            assert_eq!(server_version, PROTOCOL_VERSION);
            STANDARD
                .decode(nonce_b64)
                .expect("nonce is base64")
                .try_into()
                .expect("nonce is 32 bytes")
        }
        other => panic!("expected hello_ok, got {other:?}"),
    };
    let message = join_sig_msg(&nonce, room, &device, &bundle_bytes(&bundle))
        .expect("join message fits");
    client
        .send(&Frame::Join {
            room_id: room.to_owned(),
            device_id: device,
            pub_bundle: bundle,
            sig_b64: STANDARD.encode(identity.sign(&message)),
        })
        .await;
    match client.recv_frame().await {
        Frame::JoinOk => {}
        other => panic!("expected join_ok, got {other:?}"),
    }
    client.recv_frame().await
}

// ---------------------------------------------------------------------------
// Dev-only TLS front proxy (scenario 16)
// ---------------------------------------------------------------------------

struct TestPki {
    ca_der: Vec<u8>,
    acceptor: tokio_rustls::TlsAcceptor,
}

/// rcgen CA plus a CA-signed leaf for `localhost`.
fn test_pki() -> TestPki {
    use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair};

    let ca_key = KeyPair::generate().expect("ca key");
    let mut ca_params = CertificateParams::new(vec!["localhost".to_owned()]).expect("ca params");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "clipsync e2e test ca");
    let ca_cert = ca_params.self_signed(&ca_key).expect("ca cert");

    let leaf_key = KeyPair::generate().expect("leaf key");
    let mut leaf_params = CertificateParams::new(vec!["localhost".to_owned()]).expect("leaf params");
    leaf_params
        .distinguished_name
        .push(DnType::CommonName, "localhost");
    let leaf_cert = leaf_params
        .signed_by(&leaf_key, &Issuer::from_params(&ca_params, &ca_key))
        .expect("leaf cert");

    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(leaf_key.serialize_der().into());
    let server_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![leaf_cert.der().clone(), ca_cert.der().clone()], key_der)
        .expect("proxy server tls config");
    TestPki {
        ca_der: ca_cert.der().to_vec(),
        acceptor: tokio_rustls::TlsAcceptor::from(Arc::new(server_config)),
    }
}

struct TlsProxy {
    addr: SocketAddr,
    _task: tokio::task::JoinHandle<()>,
}

/// Starts a TLS-terminating WebSocket front proxy. The proxy drops any
/// client-supplied X-Forwarded-For and rewrites it from the TLS TCP peer.
async fn start_tls_proxy(backend: SocketAddr, pki: &TestPki) -> TlsProxy {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("proxy binds");
    let addr = listener.local_addr().expect("proxy addr");
    let acceptor = pki.acceptor.clone();
    let task = tokio::spawn(async move {
        loop {
            let Ok((tcp, peer)) = listener.accept().await else {
                return;
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(tcp).await else {
                    return;
                };
                if let Err(error) = proxy_forward(&mut tls, peer.ip(), backend).await {
                    eprintln!("tls proxy connection failed: {error}");
                }
            });
        }
    });
    TlsProxy { addr, _task: task }
}

async fn proxy_forward<T>(tls: &mut T, peer: IpAddr, backend: SocketAddr) -> std::io::Result<()>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let (head, rest) = read_http_head(tls).await?;
    let rewritten = rewrite_xff(&head, peer);
    let mut upstream = TcpStream::connect(backend).await?;
    upstream.write_all(&rewritten).await?;
    upstream.write_all(&rest).await?;
    tokio::io::copy_bidirectional(tls, &mut upstream).await?;
    Ok(())
}

async fn read_http_head<T>(stream: &mut T) -> std::io::Result<(Vec<u8>, Vec<u8>)>
where
    T: AsyncReadExt + Unpin,
{
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0_u8; 4096];
    loop {
        if let Some(pos) = buf
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
        {
            let rest = buf.split_off(pos + 4);
            return Ok((buf, rest));
        }
        if buf.len() > 64 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "http head exceeds 64 KiB",
            ));
        }
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed before the http head completed",
            ));
        }
        buf.extend_from_slice(&chunk[..read]);
    }
}

/// Rewrites the upgrade request head: drops every client-supplied
/// X-Forwarded-For line and appends exactly one carrying the TLS TCP peer.
fn rewrite_xff(head: &[u8], peer: IpAddr) -> Vec<u8> {
    let text = String::from_utf8_lossy(head);
    let body = text.trim_end_matches("\r\n\r\n");
    let mut out = String::with_capacity(body.len() + 64);
    for (index, line) in body.split("\r\n").enumerate() {
        if index > 0 && line.to_ascii_lowercase().starts_with("x-forwarded-for:") {
            continue;
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    out.push_str(&format!("x-forwarded-for: {peer}\r\n\r\n"));
    out.into_bytes()
}

/// Raw `wss://` client used to send a forged X-Forwarded-For header.
struct WssClient {
    sink: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
    stream: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
}

impl WssClient {
    async fn connect(
        proxy: SocketAddr,
        tls: Arc<rustls::ClientConfig>,
        forged_xff: &str,
    ) -> Self {
        let url = format!("wss://localhost:{}/ws", proxy.port());
        let mut request = url.into_client_request().expect("wss request");
        request
            .headers_mut()
            .insert("x-forwarded-for", forged_xff.parse().expect("xff header"));
        let stream = TcpStream::connect(proxy).await.expect("tcp connect");
        let (ws, _) = client_async_tls_with_config(
            request,
            stream,
            Some(
                WebSocketConfig::default()
                    .max_frame_size(Some(MAX_FRAME_BYTES))
                    .max_message_size(Some(MAX_MESSAGE_BYTES)),
            ),
            Some(Connector::Rustls(tls)),
        )
        .await
        .expect("wss upgrade");
        let (sink, stream) = ws.split();
        Self { sink, stream }
    }

    async fn send(&mut self, frame: &Frame) {
        let encoded = encode_frame(frame).expect("frame encodes");
        self.sink
            .send(Message::Text(String::from_utf8(encoded).unwrap().into()))
            .await
            .expect("send frame");
    }

    async fn recv_frame(&mut self) -> Frame {
        match timeout(TIMEOUT, self.stream.next()).await {
            Err(_) => panic!("timed out waiting for a wss frame"),
            Ok(None) => panic!("wss closed while awaiting a frame"),
            Ok(Some(Err(error))) => panic!("wss error: {error}"),
            Ok(Some(Ok(Message::Text(text)))) => {
                decode_frame(text.as_bytes()).expect("server frame decodes")
            }
            Ok(Some(Ok(other))) => panic!("unexpected wss message: {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Scenarios 1-7: pairing gate, text/history, 2 MiB, 10 MiB, bootstrap,
// loopback echo, tamper
// ---------------------------------------------------------------------------

/// ① Pairing gate: a same-bundle claim is rejected before any registry
/// commit, unknown codes are expired, and core refuses to claim its own QR.
#[tokio::test]
async fn e2e_01_pairing_gate_rejects_same_bundle_and_unknown_code() {
    let relay = start_relay().await;

    // Same-bundle offer/claim: the server rejects before committing a room.
    let identity = TestIdentity::generate();
    let mut offerer = Client::connect(relay.addr).await;
    offerer.hello(&identity).await;
    offerer
        .send(&Frame::PairOffer {
            code: "AB2345".to_owned(),
            pub_bundle: identity.bundle.clone(),
        })
        .await;
    match offerer.recv_frame().await {
        Frame::PairOfferOk => {}
        other => panic!("expected pair_offer_ok, got {other:?}"),
    }
    let mut claimant = Client::connect(relay.addr).await;
    claimant.hello(&identity).await;
    claimant
        .send(&Frame::PairClaim {
            code: "AB2345".to_owned(),
            pub_bundle: identity.bundle.clone(),
        })
        .await;
    claimant.expect_error_close("same_identity").await;
    let same_room = room_id(&identity.bundle, &identity.bundle);
    assert!(
        relay.registry.lookup_members(&same_room).is_empty(),
        "same-bundle claim must not commit a registry room"
    );

    // Unknown code is expired.
    let mut stranger_client = Client::connect(relay.addr).await;
    let stranger = TestIdentity::generate();
    stranger_client.hello(&stranger).await;
    stranger_client
        .send(&Frame::PairClaim {
            code: "ZZ9999".to_owned(),
            pub_bundle: stranger.bundle.clone(),
        })
        .await;
    stranger_client.expect_error_close("code_expired").await;

    // Core refuses to claim its own QR before touching the network.
    let a = Endpoint::new();
    let b = Endpoint::new();
    let client = PairingClient::connect(&a.store, &relay.url)
        .await
        .expect("connect");
    let offer = client.pair_begin().await.expect("pair_begin");
    let qr = offer.qr().clone();
    let error = PairingClient::claim(&a.store, &qr)
        .await
        .expect_err("claiming the own QR must fail");
    assert!(
        matches!(error, TransportError::Pairing(PairingError::SameIdentity)),
        "unexpected error: {error}"
    );
    drop(offer);
    drop(b);
}

/// ② Text roundtrip plus history on both ends, surviving a session reopen.
#[tokio::test]
async fn e2e_02_text_roundtrip_records_history_on_both_ends() {
    let relay = start_relay().await;
    let a = Endpoint::new();
    let b = Endpoint::new();
    let (mut link_a, mut link_b) = pair(&relay.url, &a, &b).await;
    let mut session_a = a.session(&link_a);
    let mut session_b = b.session(&link_b);

    let frame = send_text(&mut session_a, &mut link_a, "hello e2e", now_ms()).await;
    let got = recv_clip(&mut link_b).await;
    assert_eq!(
        deliver(&mut session_b, &got, now_ms()),
        InboundOutcome::LiveApplied
    );

    let live = b.callback.live_items();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].content, ClipContent::Text("hello e2e".to_owned()));
    assert_eq!(live[0].seq, 1);
    assert_eq!(clip_fields(&frame).0, clip_fields(&got).0);

    assert_eq!(
        history_texts(&session_a),
        vec![("hello e2e".to_owned(), HistorySource::Local)]
    );
    assert_eq!(
        history_texts(&session_b),
        vec![("hello e2e".to_owned(), HistorySource::Remote)]
    );

    // History survives a session reopen (crash-safe persistence).
    let reopened_a = a.session(&link_a);
    let reopened_b = b.session(&link_b);
    assert_eq!(history_texts(&reopened_a), history_texts(&session_a));
    assert_eq!(history_texts(&reopened_b), history_texts(&session_b));
}

/// ③ A 2 MiB text clip roundtrips intact through the relay.
#[tokio::test]
async fn e2e_03_two_mib_text_roundtrip() {
    let relay = start_relay().await;
    let a = Endpoint::new();
    let b = Endpoint::new();
    let (mut link_a, mut link_b) = pair(&relay.url, &a, &b).await;
    let mut session_a = a.session(&link_a);
    let mut session_b = b.session(&link_b);

    let text = "x".repeat(2 * 1024 * 1024);
    assert_eq!(text.len(), 2 * 1024 * 1024);
    send_text(&mut session_a, &mut link_a, &text, now_ms()).await;
    let got = recv_clip_within(&mut link_b, BIG_TIMEOUT).await;
    assert_eq!(
        deliver(&mut session_b, &got, now_ms()),
        InboundOutcome::LiveApplied
    );
    let live = b.callback.live_items();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].content, ClipContent::Text(text));
}

/// ④ A 10 MiB image (the protocol maximum) roundtrips byte-exact.
#[tokio::test]
async fn e2e_04_ten_mib_image_roundtrip() {
    let relay = start_relay().await;
    let a = Endpoint::new();
    let b = Endpoint::new();
    let (mut link_a, mut link_b) = pair(&relay.url, &a, &b).await;
    let mut session_a = a.session(&link_a);
    let mut session_b = b.session(&link_b);

    let image: Vec<u8> = (0..10 * 1024 * 1024_u32)
        .map(|index| (index % 251) as u8)
        .collect();
    let frame = session_a
        .send_clip(ClipContent::Image(image.clone()), now_ms())
        .expect("send_clip");
    link_a.send(&frame).await.expect("send over relay");
    let got = recv_clip_within(&mut link_b, BIG_TIMEOUT).await;
    assert_eq!(
        deliver(&mut session_b, &got, now_ms()),
        InboundOutcome::LiveApplied
    );

    let live = b.callback.live_items();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].content, ClipContent::Image(image.clone()));
    let stored = session_b
        .history()
        .image_bytes(live[0].id)
        .expect("history keeps image bytes");
    assert_eq!(stored, image);
}

/// ⑤ A clip sent while the peer is offline arrives as the join bootstrap
/// mailbox frame exactly once.
#[tokio::test]
async fn e2e_05_offline_clip_bootstraps_as_mailbox_on_join() {
    let mut relay = start_relay().await;
    let a = Endpoint::new();
    let b = Endpoint::new();
    let (mut link_a, link_b) = pair(&relay.url, &a, &b).await;
    let mut session_a = a.session(&link_a);

    link_b.close().await.expect("close");
    wait_event(&mut relay.events, "disconnect").await;
    send_text(&mut session_a, &mut link_a, "while you were out", now_ms()).await;
    relay.mailbox.wait_pending(1).await;

    let link_b2 = PairingClient::join(&b.store).await.expect("rejoin");
    let (room, ciphertext) = link_b2
        .bootstrap_clip()
        .expect("bootstrap must carry the mailbox clip");
    let mut session_b = b.session(&link_b2);
    let outcome = session_b
        .handle_clip(&room, &ciphertext, true, now_ms())
        .expect("handle_clip");
    assert_eq!(outcome, InboundOutcome::MailboxApplied);
    let mailbox = b.callback.mailbox_items();
    assert_eq!(mailbox.len(), 1);
    assert_eq!(
        mailbox[0].content,
        ClipContent::Text("while you were out".to_owned())
    );
    assert!(b.callback.live_items().is_empty());
}

/// ⑥ Loopback: the canonical echo hash lets a shell suppress its own text
/// when it comes back around through the peer.
#[tokio::test]
async fn e2e_06_loopback_text_is_echo_recognized() {
    let relay = start_relay().await;
    let a = Endpoint::new();
    let b = Endpoint::new();
    let (mut link_a, mut link_b) = pair(&relay.url, &a, &b).await;
    let mut session_a = a.session(&link_a);
    let mut session_b = b.session(&link_b);

    send_text(&mut session_a, &mut link_a, "loopback", now_ms()).await;
    let got = recv_clip(&mut link_b).await;
    assert_eq!(
        deliver(&mut session_b, &got, now_ms()),
        InboundOutcome::LiveApplied
    );
    assert!(session_b.is_echo(&text_hash("loopback")));

    // The peer echoes the same text back; the sender recognizes its own hash.
    send_text(&mut session_b, &mut link_b, "loopback", now_ms()).await;
    let echoed = recv_clip(&mut link_a).await;
    assert_eq!(
        deliver(&mut session_a, &echoed, now_ms()),
        InboundOutcome::LiveApplied
    );
    assert!(
        session_a.is_echo(&text_hash("loopback")),
        "sender must recognize its own text hash after the loop"
    );
}

/// ⑦ Tampering is unauthenticated end to end: a flipped ciphertext byte and
/// a foreign room binding are both dropped without state change or callback.
#[tokio::test]
async fn e2e_07_tampered_ciphertext_or_room_is_unauthenticated() {
    let relay = start_relay().await;
    let a = Endpoint::new();
    let b = Endpoint::new();
    let (mut link_a, mut link_b) = pair(&relay.url, &a, &b).await;
    let mut session_a = a.session(&link_a);
    let mut session_b = b.session(&link_b);

    send_text(&mut session_a, &mut link_a, "fragile", now_ms()).await;
    let got = recv_clip(&mut link_b).await;
    let (room, ciphertext, mailbox, _) = clip_fields(&got);

    let mut raw = STANDARD.decode(&ciphertext).expect("ciphertext is base64");
    let last = raw.len() - 1;
    raw[last] ^= 1;
    let tampered = STANDARD.encode(raw);
    assert_eq!(
        session_b
            .handle_clip(&room, &tampered, mailbox, now_ms())
            .expect("handle_clip"),
        InboundOutcome::Unauthenticated
    );
    assert_eq!(
        session_b
            .handle_clip(&"ff".repeat(16), &ciphertext, mailbox, now_ms())
            .expect("handle_clip"),
        InboundOutcome::Unauthenticated
    );
    assert!(b.callback.live_items().is_empty());
    assert!(session_b.history().list().is_empty());
    assert_eq!(session_b.last_seq(), 0, "tampering must not move the watermark");

    // The honest frame still applies afterwards; the session is unaffected.
    assert_eq!(
        deliver(&mut session_b, &got, now_ms()),
        InboundOutcome::LiveApplied
    );
}

// ---------------------------------------------------------------------------
// Scenarios 8-14: origin/AAD, oversize, third identity, dedup, mailbox
// disposition, low-order point, cross-endpoint golden
// ---------------------------------------------------------------------------

/// ⑧ The relay overwrites `origin_device` with the authenticated sender
/// fingerprint, and the AAD stays the canonical room‖0‖sender‖0‖receiver.
#[tokio::test]
async fn e2e_08_origin_overwritten_and_aad_canonical() {
    let relay = start_relay().await;
    let a = Endpoint::new();
    let b = Endpoint::new();
    let (mut link_a, mut link_b) = pair(&relay.url, &a, &b).await;
    let mut session_a = a.session(&link_a);
    let mut session_b = b.session(&link_b);

    send_text(&mut session_a, &mut link_a, "attributed", now_ms()).await;
    let got = recv_clip(&mut link_b).await;
    let (room, ciphertext, _mailbox, origin) = clip_fields(&got);
    assert_eq!(
        origin,
        a.fp(),
        "relay must overwrite origin_device with the authenticated fingerprint"
    );
    assert_eq!(
        deliver(&mut session_b, &got, now_ms()),
        InboundOutcome::LiveApplied
    );

    // Canonical AAD construction and its enforcement on the wire ciphertext.
    let associated = aad(&room, &a.fp(), &b.fp());
    assert_eq!(
        associated,
        [
            room.as_bytes(),
            &[0],
            a.fp().as_bytes(),
            &[0],
            b.fp().as_bytes()
        ]
        .concat()
    );
    let sealed = STANDARD.decode(&ciphertext).expect("ciphertext is base64");
    let recv_keys = b
        .identity()
        .session_keys(&a.bundle())
        .expect("session keys");
    open(&recv_keys.recv, &associated, &sealed).expect("canonical AAD opens");
    let mut wrong_room_aad = associated.clone();
    wrong_room_aad[0] ^= 1;
    assert!(
        matches!(
            open(&recv_keys.recv, &wrong_room_aad, &sealed),
            Err(CryptoError::Decryption)
        ),
        "one-byte AAD tamper must fail decryption"
    );
}

/// ⑨ Oversize is rejected at both layers: the session refuses an image past
/// 10 MiB and the relay drops a websocket message past 24 MiB.
#[tokio::test]
async fn e2e_09_oversize_rejected_at_session_and_relay() {
    let relay = start_relay().await;
    let a = Endpoint::new();
    let b = Endpoint::new();
    let (link_a, _link_b) = pair(&relay.url, &a, &b).await;
    let mut session_a = a.session(&link_a);

    let error = session_a
        .send_clip(
            ClipContent::Image(vec![0_u8; 10 * 1024 * 1024 + 1]),
            now_ms(),
        )
        .expect_err("image past 10 MiB must be rejected");
    assert!(
        matches!(
            error,
            SessionError::Protocol(ProtocolError::Oversize { limit, .. })
            if limit == 10 * 1024 * 1024
        ),
        "unexpected error: {error}"
    );
    assert_eq!(session_a.next_seq(), 1, "rejected send must not consume seq");

    // Relay layer: a websocket message past the 24 MiB cap kills the connection.
    let sender = TestIdentity::generate();
    let receiver = TestIdentity::generate();
    let raw_room = room_id(&sender.bundle, &receiver.bundle);
    assert!(
        relay
            .registry
            .register_room(&raw_room, &[sender.fp(), receiver.fp()]),
        "raw room registers"
    );
    let mut raw = Client::connect(relay.addr).await;
    raw.join(&sender, &raw_room).await;
    raw.send_raw_text("A".repeat(MAX_MESSAGE_BYTES + 1)).await;
    raw.expect_closed().await;
}

/// ⑩ A third identity cannot enter a paired room: registered rooms answer
/// `room_full` and unregistered rooms answer `bad_auth`.
#[tokio::test]
async fn e2e_10_third_identity_cannot_join_room() {
    let relay = start_relay().await;
    let a = Endpoint::new();
    let b = Endpoint::new();
    let (link_a, _link_b) = pair(&relay.url, &a, &b).await;
    let room = link_a.record().room_id.clone();

    let stranger = TestIdentity::generate();
    let mut client = Client::connect(relay.addr).await;
    let nonce = client.hello(&stranger).await;
    client.send(&stranger.join_frame(&room, &nonce)).await;
    client.expect_error_close("room_full").await;

    let ghost = TestIdentity::generate();
    let mut client = Client::connect(relay.addr).await;
    let nonce = client.hello(&ghost).await;
    client
        .send(&ghost.join_frame(&"ab".repeat(16), &nonce))
        .await;
    client.expect_error_close("bad_auth").await;
}

/// ⑪ Duplicate delivery is idempotent: the second copy changes nothing and
/// never re-fires the callback.
#[tokio::test]
async fn e2e_11_duplicate_delivery_is_idempotent() {
    let relay = start_relay().await;
    let a = Endpoint::new();
    let b = Endpoint::new();
    let (mut link_a, mut link_b) = pair(&relay.url, &a, &b).await;
    let mut session_a = a.session(&link_a);
    let mut session_b = b.session(&link_b);

    send_text(&mut session_a, &mut link_a, "exactly once", now_ms()).await;
    let got = recv_clip(&mut link_b).await;
    let (room, ciphertext, mailbox, _) = clip_fields(&got);
    assert_eq!(
        session_b
            .handle_clip(&room, &ciphertext, mailbox, now_ms())
            .expect("handle_clip"),
        InboundOutcome::LiveApplied
    );
    assert_eq!(
        session_b
            .handle_clip(&room, &ciphertext, mailbox, now_ms())
            .expect("handle_clip"),
        InboundOutcome::Duplicate
    );
    assert_eq!(b.callback.live_items().len(), 1);
    assert_eq!(session_b.history().list().len(), 1);
    assert_eq!(session_b.last_seq(), 1);
}

/// ⑫ Mailbox disposition: a deferred bootstrap clip stays `RemoteDeferred`
/// and the user-driven `apply_deferred` promotes it idempotently.
#[tokio::test]
async fn e2e_12_mailbox_disposition_deferred_then_applied() {
    let mut relay = start_relay().await;
    let a = Endpoint::new();
    let b = Endpoint::new();
    b.callback.set_disposition(MailboxDisposition::Deferred);
    let (mut link_a, link_b) = pair(&relay.url, &a, &b).await;
    let mut session_a = a.session(&link_a);

    link_b.close().await.expect("close");
    wait_event(&mut relay.events, "disconnect").await;
    send_text(&mut session_a, &mut link_a, "deferred mailbox", now_ms()).await;
    relay.mailbox.wait_pending(1).await;

    let link_b2 = PairingClient::join(&b.store).await.expect("rejoin");
    let (room, ciphertext) = link_b2.bootstrap_clip().expect("bootstrap clip");
    let mut session_b = b.session(&link_b2);
    let outcome = session_b
        .handle_clip(&room, &ciphertext, true, now_ms())
        .expect("handle_clip");
    assert_eq!(outcome, InboundOutcome::MailboxDeferred);

    let items = session_b.history().list();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].source, HistorySource::RemoteDeferred);

    session_b.apply_deferred(items[0].id).expect("apply_deferred");
    session_b
        .apply_deferred(items[0].id)
        .expect("apply_deferred is idempotent");
    assert_eq!(
        session_b.history().list()[0].source,
        HistorySource::Remote
    );
    assert!(session_b.is_echo(&text_hash("deferred mailbox")));
}

/// ⑬ A peer advertising a low-order X25519 point pairs transport-wise but
/// session derivation fails closed (`NonContributoryDh`).
#[tokio::test]
async fn e2e_13_low_order_point_peer_fails_session_derivation() {
    let relay = start_relay().await;

    let mut evil = TestIdentity::generate();
    evil.bundle.dh_pk = [0_u8; 32];
    let mut offerer = Client::connect(relay.addr).await;
    offerer.hello(&evil).await;
    offerer
        .send(&Frame::PairOffer {
            code: "PQR234".to_owned(),
            pub_bundle: evil.bundle.clone(),
        })
        .await;
    match offerer.recv_frame().await {
        Frame::PairOfferOk => {}
        other => panic!("expected pair_offer_ok, got {other:?}"),
    }

    let qr = QrPayload::new(&relay.url, "PQR234", evil.bundle.clone(), [7_u8; 16])
        .expect("qr payload");
    let b = Endpoint::new();
    let channel = PairingClient::claim(&b.store, &qr)
        .await
        .expect("claim");
    let sas = channel.pending().sas();
    let link = channel
        .confirm_pair(&sas, &b.store)
        .await
        .expect("confirm joins");
    let result = b.try_session(&link);
    assert!(
        matches!(
            result,
            Err(SessionError::Crypto(CryptoError::NonContributoryDh))
        ),
        "low-order peer key must fail session derivation closed"
    );
}

/// ⑭ Cross-endpoint golden: the T3 AEAD vector opens here, and a wire clip
/// between fixed-secret identities decrypts independently into the canonical
/// T2 envelope.
#[tokio::test]
async fn e2e_14_cross_endpoint_golden_vector_holds_over_relay() {
    // Re-anchor the T3 cross-endpoint AEAD golden vector.
    let key = SessionKey::from_bytes(std::array::from_fn(|index| {
        u8::try_from(index).expect("index fits u8")
    }));
    let golden_aad = [
        b"471fb943aa23c511f6f72f8d1652d9c8".as_slice(),
        &[0],
        b"fdeab9acf3710362bd2658cdc9a29e8f9c757fcf9811603a8c447cd1d9151108",
        &[0],
        b"9afaeef005e286957ee9a18a2481a75c7fc7ba74bae8de50ffa6127b12a62cae",
    ]
    .concat();
    let golden_sealed = [
        0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xab, 0xac, 0xad, 0xae,
        0xaf, 0xb0, 0xb1, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0x26, 0x80, 0x63, 0x6d, 0xd5, 0x32,
        0xc5, 0xde, 0x05, 0x4f, 0x4a, 0xe6, 0x1d, 0x54, 0xc6, 0xae, 0xaf, 0x87, 0xbf, 0x5c, 0x80,
        0x0b, 0xfb, 0x46, 0xde, 0x51, 0x36, 0x31, 0x75, 0x12, 0x97, 0x94,
    ];
    assert_eq!(
        open(&key, &golden_aad, &golden_sealed).expect("golden vector opens"),
        b"clipboard golden"
    );

    // Fixed-secret identities (the T3 fixtures) over the real relay.
    let relay = start_relay().await;
    let a = Endpoint::with_identity([7_u8; 32], [9_u8; 32]);
    let b = Endpoint::with_identity([8_u8; 32], [10_u8; 32]);
    let (mut link_a, mut link_b) = pair(&relay.url, &a, &b).await;
    let mut session_a = a.session(&link_a);
    let mut session_b = b.session(&link_b);

    send_text(&mut session_a, &mut link_a, "cross endpoint", now_ms()).await;
    let got = recv_clip(&mut link_b).await;
    let (room, ciphertext, mailbox, _) = clip_fields(&got);
    assert_eq!(
        session_b
            .handle_clip(&room, &ciphertext, mailbox, now_ms())
            .expect("handle_clip"),
        InboundOutcome::LiveApplied
    );

    // Independent cross-endpoint decryption of the same wire ciphertext.
    let keys_b = b
        .identity()
        .session_keys(&a.bundle())
        .expect("session keys");
    let plaintext = open(
        &keys_b.recv,
        &aad(&room, &a.fp(), &b.fp()),
        &STANDARD.decode(&ciphertext).expect("ciphertext is base64"),
    )
    .expect("wire ciphertext opens with the crossed key");
    let envelope = decode_envelope(&plaintext).expect("canonical envelope");
    assert_eq!(envelope.v, PROTOCOL_VERSION);
    assert!(matches!(envelope.kind, ContentKind::Text));
    assert_eq!(envelope.seq, 1);
    assert_eq!(
        STANDARD
            .decode(&envelope.content_b64)
            .expect("content is base64"),
        b"cross endpoint"
    );
}

// ---------------------------------------------------------------------------
// Scenarios 15-21: registry commit boundary, WSS, T2 vectors, double claim,
// ancient replay, expired-live deferred, bootstrap barrier
// ---------------------------------------------------------------------------

/// ⑮ The room registry commit (durably persisted) precedes the dual
/// `pair_peer` enqueue: once both sides hold `pair_peer`, the committed room
/// is on disk and immediately joinable.
#[tokio::test]
async fn e2e_15_registry_commits_before_pair_peer_delivery() {
    let dir = TempDir::new().expect("temp dir");
    let registry = Arc::new(
        PersistentRegistry::open(dir.path().join("registry")).expect("registry opens"),
    );
    let pairing = Arc::new(PairingRelay::new(registry.clone(), PairingConfig::default()));
    let mailbox = Arc::new(RecordingMailbox::default());
    let handle = start(
        "127.0.0.1:0".parse().expect("bind address"),
        ServerConfig::default(),
        registry.clone(),
        pairing,
        mailbox,
    )
    .await
    .expect("server binds");
    let addr = handle.addr();

    let offerer_id = TestIdentity::generate();
    let claimer_id = TestIdentity::generate();
    let mut offerer = Client::connect(addr).await;
    offerer.hello(&offerer_id).await;
    offerer
        .send(&Frame::PairOffer {
            code: "QWE456".to_owned(),
            pub_bundle: offerer_id.bundle.clone(),
        })
        .await;
    match offerer.recv_frame().await {
        Frame::PairOfferOk => {}
        other => panic!("expected pair_offer_ok, got {other:?}"),
    }

    let mut claimer = Client::connect(addr).await;
    claimer.hello(&claimer_id).await;
    claimer
        .send(&Frame::PairClaim {
            code: "QWE456".to_owned(),
            pub_bundle: claimer_id.bundle.clone(),
        })
        .await;
    match claimer.recv_frame().await {
        Frame::PairPeer { peer_pub_bundle } => {
            assert_eq!(peer_pub_bundle, offerer_id.bundle)
        }
        other => panic!("expected pair_peer, got {other:?}"),
    }
    match offerer.recv_frame().await {
        Frame::PairPeer { peer_pub_bundle } => {
            assert_eq!(peer_pub_bundle, claimer_id.bundle)
        }
        other => panic!("expected pair_peer, got {other:?}"),
    }

    // Both sides already hold pair_peer, so the commit must be durable now.
    let room = room_id(&offerer_id.bundle, &claimer_id.bundle);
    let mut members = registry.lookup_members(&room);
    members.sort();
    let mut expected = vec![offerer_id.fp(), claimer_id.fp()];
    expected.sort();
    assert_eq!(members, expected);
    assert!(
        dir.path().join("registry").join("rooms.json").exists(),
        "registry commit must be persisted before pair_peer delivery"
    );

    // The committed room is immediately joinable by both members.
    let mut first = Client::connect(addr).await;
    assert_eq!(
        first.join(&offerer_id, &room).await,
        Frame::MailboxEmpty
    );
    let mut second = Client::connect(addr).await;
    assert_eq!(
        second.join(&claimer_id, &room).await,
        Frame::MailboxEmpty
    );
    drop(handle);
}

/// ⑯ WSS: a dev-only rcgen TLS front proxy terminates `wss://` and forwards
/// to the plain ws relay; the core client injects the test CA. The proxy
/// drops client-supplied X-Forwarded-For and rewrites it from the TLS TCP
/// peer, so forged XFF values never reach the relay's rate limiter.
#[tokio::test]
async fn e2e_16_wss_proxy_injected_ca_and_xff_rewrite() {
    let config = ServerConfig {
        trusted_proxy: Some(IpNet::parse("127.0.0.1").expect("trusted proxy")),
        limits: Limits {
            join_attempts_per_minute: 4,
            ..Limits::default()
        },
        ..ServerConfig::default()
    };
    let relay = start_relay_with(config).await;

    let pki = test_pki();
    let proxy = start_tls_proxy(relay.addr, &pki).await;
    let client_tls = tls_config_with_roots(&[&pki.ca_der]).expect("client tls config");
    let wss_url = format!("wss://localhost:{}/ws", proxy.addr.port());

    // Full E2E pairing and clip roundtrip over wss with the injected CA.
    let a = Endpoint::new();
    let b = Endpoint::new();
    let (mut link_a, mut link_b) = pair_tls(&wss_url, &client_tls, &a, &b).await;
    assert_eq!(link_a.record().server, wss_url);
    let mut session_a = a.session(&link_a);
    let mut session_b = b.session(&link_b);
    send_text(&mut session_a, &mut link_a, "over wss", now_ms()).await;
    let got = recv_clip(&mut link_b).await;
    assert_eq!(
        deliver(&mut session_b, &got, now_ms()),
        InboundOutcome::LiveApplied
    );
    assert_eq!(
        b.callback.live_items()[0].content,
        ClipContent::Text("over wss".to_owned())
    );

    // Forged XFF must be overwritten with the real TLS TCP peer (127.0.0.1):
    // three joins carrying three distinct forged addresses share the loopback
    // join budget (4/min, 2 already spent pairing) and the third is limited.
    // Had the forged values been honored, each would own a fresh budget.
    for (index, forged) in ["203.0.113.1", "203.0.113.2", "203.0.113.3"]
        .iter()
        .enumerate()
    {
        let mut client = WssClient::connect(proxy.addr, client_tls.clone(), forged).await;
        let identity = TestIdentity::generate();
        client.send(&identity.hello_frame()).await;
        let nonce = match client.recv_frame().await {
            Frame::HelloOk { nonce_b64, .. } => STANDARD.decode(nonce_b64).expect("nonce"),
            other => panic!("expected hello_ok, got {other:?}"),
        };
        client
            .send(&identity.join_frame(&"00".repeat(16), &nonce))
            .await;
        let expected = if index < 2 { "bad_auth" } else { "rate_limited" };
        match client.recv_frame().await {
            Frame::Error { code, .. } => assert_eq!(
                code, expected,
                "join #{index} with forged XFF {forged} must hit the loopback budget"
            ),
            other => panic!("expected error frame, got {other:?}"),
        }
    }
}

/// ⑰ T2 golden vectors: every frame and the envelope decode and re-encode
/// byte-exact, and the golden clip frame routes through the real relay.
#[tokio::test]
async fn e2e_17_t2_golden_vectors_decode_reencode_and_route() {
    let frames: [(&str, &[u8]); 11] = [
        ("hello", include_bytes!("../../core/tests/golden/frames/hello.json")),
        ("hello_ok", include_bytes!("../../core/tests/golden/frames/hello_ok.json")),
        ("pair_offer", include_bytes!("../../core/tests/golden/frames/pair_offer.json")),
        ("pair_offer_ok", include_bytes!("../../core/tests/golden/frames/pair_offer_ok.json")),
        ("pair_claim", include_bytes!("../../core/tests/golden/frames/pair_claim.json")),
        ("pair_peer", include_bytes!("../../core/tests/golden/frames/pair_peer.json")),
        ("join", include_bytes!("../../core/tests/golden/frames/join.json")),
        ("join_ok", include_bytes!("../../core/tests/golden/frames/join_ok.json")),
        ("clip", include_bytes!("../../core/tests/golden/frames/clip.json")),
        ("mailbox_empty", include_bytes!("../../core/tests/golden/frames/mailbox_empty.json")),
        ("error", include_bytes!("../../core/tests/golden/frames/error.json")),
    ];
    for (name, bytes) in frames {
        let frame = decode_frame(bytes).unwrap_or_else(|error| panic!("{name} decodes: {error}"));
        let encoded = encode_frame(&frame).expect("frame re-encodes");
        assert_eq!(&encoded, &bytes, "{name} must re-encode byte-exact");
    }
    let envelope_bytes: &[u8] = include_bytes!("../../core/tests/golden/envelope.json");
    let envelope = decode_envelope(envelope_bytes).expect("envelope decodes");
    assert_eq!(envelope.v, PROTOCOL_VERSION);
    assert!(matches!(envelope.kind, ContentKind::Text));
    assert_eq!(envelope.seq, 42);
    assert_eq!(
        encode_envelope(&envelope).expect("envelope re-encodes"),
        envelope_bytes,
        "envelope must re-encode byte-exact"
    );

    // The golden clip frame routes through the real relay end to end.
    let relay = start_relay().await;
    let golden_room = "00112233445566778899aabbccddeeff";
    let sender = TestIdentity::generate();
    let receiver = TestIdentity::generate();
    assert!(
        relay
            .registry
            .register_room(golden_room, &[sender.fp(), receiver.fp()]),
        "golden room registers"
    );
    let mut send_client = Client::connect(relay.addr).await;
    send_client.join(&sender, golden_room).await;
    let mut recv_client = Client::connect(relay.addr).await;
    recv_client.join(&receiver, golden_room).await;
    let golden_clip: &[u8] = include_bytes!("../../core/tests/golden/frames/clip.json");
    send_client
        .send_raw_text(String::from_utf8(golden_clip.to_vec()).expect("golden clip is utf-8"))
        .await;
    let got = recv_client.recv_frame().await;
    let (room, ciphertext, mailbox, origin) = clip_fields(&got);
    assert_eq!(room, golden_room);
    assert_eq!(ciphertext, "Y2lwaGVydGV4dA==");
    assert!(!mailbox, "live routing forces mailbox=false");
    assert_eq!(origin, sender.fp());
}

/// ⑱ A code transitions exactly once: the second claim of the same code is
/// `code_expired`.
#[tokio::test]
async fn e2e_18_second_claim_of_same_code_is_expired() {
    let relay = start_relay().await;
    let offerer_id = TestIdentity::generate();
    let mut offerer = Client::connect(relay.addr).await;
    offerer.hello(&offerer_id).await;
    offerer
        .send(&Frame::PairOffer {
            code: "DEFXYZ".to_owned(),
            pub_bundle: offerer_id.bundle.clone(),
        })
        .await;
    match offerer.recv_frame().await {
        Frame::PairOfferOk => {}
        other => panic!("expected pair_offer_ok, got {other:?}"),
    }

    let first_id = TestIdentity::generate();
    let mut first = Client::connect(relay.addr).await;
    first.hello(&first_id).await;
    first
        .send(&Frame::PairClaim {
            code: "DEFXYZ".to_owned(),
            pub_bundle: first_id.bundle.clone(),
        })
        .await;
    match first.recv_frame().await {
        Frame::PairPeer { peer_pub_bundle } => {
            assert_eq!(peer_pub_bundle, offerer_id.bundle)
        }
        other => panic!("expected pair_peer, got {other:?}"),
    }

    let second_id = TestIdentity::generate();
    let mut second = Client::connect(relay.addr).await;
    second.hello(&second_id).await;
    second
        .send(&Frame::PairClaim {
            code: "DEFXYZ".to_owned(),
            pub_bundle: second_id.bundle.clone(),
        })
        .await;
    second.expect_error_close("code_expired").await;
}

/// ⑲ Ancient replays never auto-apply: a days-old live-flagged clip is
/// deferred, and an out-of-order seq at or below the high-water mark is a
/// replay that changes nothing.
#[tokio::test]
async fn e2e_19_ancient_replay_and_stale_seq_never_apply() {
    let relay = start_relay().await;
    let a = Endpoint::new();
    let b = Endpoint::new();
    b.callback.set_disposition(MailboxDisposition::Deferred);
    let (mut link_a, mut link_b) = pair(&relay.url, &a, &b).await;
    let mut session_a = a.session(&link_a);
    let mut session_b = b.session(&link_b);

    // Ancient live-flagged clip: freshness fails, it goes to the mailbox path.
    let ancient_ts = now_ms() - 10 * 24 * 60 * 60 * 1000;
    send_text(&mut session_a, &mut link_a, "ancient", ancient_ts).await;
    let got = recv_clip(&mut link_b).await;
    assert_eq!(
        deliver(&mut session_b, &got, now_ms()),
        InboundOutcome::MailboxDeferred
    );
    assert!(b.callback.live_items().is_empty(), "ancient clip must not auto-apply");
    assert_eq!(
        history_texts(&session_b),
        vec![("ancient".to_owned(), HistorySource::RemoteDeferred)]
    );

    // Out-of-order seq at or below the watermark: Replay, no callback, no history.
    let first = session_a
        .send_clip(ClipContent::Text("seq-first".to_owned()), now_ms())
        .expect("send_clip");
    let second = session_a
        .send_clip(ClipContent::Text("seq-second".to_owned()), now_ms())
        .expect("send_clip");
    link_a.send(&second).await.expect("send second");
    link_a.send(&first).await.expect("send first");
    let got_second = recv_clip(&mut link_b).await;
    assert_eq!(
        deliver(&mut session_b, &got_second, now_ms()),
        InboundOutcome::LiveApplied
    );
    let got_first = recv_clip(&mut link_b).await;
    assert_eq!(
        deliver(&mut session_b, &got_first, now_ms()),
        InboundOutcome::Replay
    );
    assert_eq!(b.callback.live_items().len(), 1);
    assert_eq!(session_b.last_seq(), 3);
    assert!(
        !history_texts(&session_b)
            .iter()
            .any(|(text, _)| text == "seq-first"),
        "replayed seq must not enter history"
    );
}

/// ⑳ The live freshness window is enforced on the wire: just-past-window and
/// beyond-skew clips are deferred while an in-window clip applies.
#[tokio::test]
async fn e2e_20_expired_live_clip_is_deferred() {
    let relay = start_relay().await;
    let a = Endpoint::new();
    let b = Endpoint::new();
    b.callback.set_disposition(MailboxDisposition::Deferred);
    let (mut link_a, mut link_b) = pair(&relay.url, &a, &b).await;
    let mut session_a = a.session(&link_a);
    let mut session_b = b.session(&link_b);
    let now = now_ms();

    // One second past the five-minute freshness window.
    send_text(&mut session_a, &mut link_a, "just stale", now - 5 * 60 * 1000 - 1_000).await;
    // Beyond the two-minute future clock-skew tolerance.
    send_text(&mut session_a, &mut link_a, "too futuristic", now + 2 * 60 * 1000 + 1_000).await;
    // In-window control.
    send_text(&mut session_a, &mut link_a, "fresh", now).await;

    for expected in [
        InboundOutcome::MailboxDeferred,
        InboundOutcome::MailboxDeferred,
        InboundOutcome::LiveApplied,
    ] {
        let got = recv_clip(&mut link_b).await;
        assert_eq!(deliver(&mut session_b, &got, now), expected);
    }
    assert_eq!(b.callback.live_items().len(), 1);
    assert_eq!(b.callback.mailbox_items().len(), 2);
    let sources: Vec<HistorySource> = session_b
        .history()
        .list()
        .iter()
        .map(|item| item.source.clone())
        .collect();
    assert_eq!(
        sources,
        vec![
            HistorySource::RemoteDeferred,
            HistorySource::RemoteDeferred,
            HistorySource::Remote
        ]
    );
}

/// ㉑ Bootstrap barrier: the join consumes exactly one bootstrap frame before
/// any live frame; nothing live is delivered ahead of it.
#[tokio::test]
async fn e2e_21_bootstrap_barrier_orders_before_live_frames() {
    let mut relay = start_relay().await;
    let a = Endpoint::new();
    let b = Endpoint::new();
    let (mut link_a, link_b) = pair(&relay.url, &a, &b).await;
    let mut session_a = a.session(&link_a);

    link_b.close().await.expect("close");
    wait_event(&mut relay.events, "disconnect").await;
    send_text(&mut session_a, &mut link_a, "bootstrapped", now_ms()).await;
    relay.mailbox.wait_pending(1).await;

    let mut link_b2 = PairingClient::join(&b.store).await.expect("rejoin");
    let (room, ciphertext) = link_b2
        .bootstrap_clip()
        .expect("bootstrap frame arrives before live mode");
    let mut session_b = b.session(&link_b2);

    // Only after the bootstrap was consumed may live frames arrive.
    send_text(&mut session_a, &mut link_a, "live after bootstrap", now_ms()).await;
    let live = recv_clip(&mut link_b2).await;
    let (live_room, live_ct, live_mailbox, _) = clip_fields(&live);
    assert!(!live_mailbox, "post-bootstrap frames are live");

    assert_eq!(
        session_b
            .handle_clip(&room, &ciphertext, true, now_ms())
            .expect("bootstrap handle"),
        InboundOutcome::MailboxApplied
    );
    assert_eq!(
        session_b
            .handle_clip(&live_room, &live_ct, live_mailbox, now_ms())
            .expect("live handle"),
        InboundOutcome::LiveApplied
    );
    let texts = history_texts(&session_b);
    assert!(
        texts.iter().any(|(text, _)| text == "bootstrapped")
            && texts.iter().any(|(text, _)| text == "live after bootstrap")
    );
}

// ---------------------------------------------------------------------------
// Scenarios 22-29: concurrent seq, crash reservation, re-pair namespace,
// mailbox idempotency, stale connections, pending-latest, atomic pair_peer
// reservation, verify/full vector agreement
// ---------------------------------------------------------------------------

/// ㉒ Concurrent bidirectional traffic: both directions allocate unique,
/// monotonic seqs starting at 1 and every clip applies exactly once.
#[tokio::test]
async fn e2e_22_concurrent_bidirectional_seq_unique_monotonic() {
    const COUNT: u64 = 20;
    let relay = start_relay().await;
    let a = Endpoint::new();
    let b = Endpoint::new();
    let (mut link_a, mut link_b) = pair(&relay.url, &a, &b).await;
    let mut session_a = a.session(&link_a);
    let mut session_b = b.session(&link_b);

    let send_a = async {
        for _ in 0..COUNT {
            let frame = session_a
                .send_clip(ClipContent::Text("a-to-b".to_owned()), now_ms())
                .expect("send_clip");
            link_a.send(&frame).await.expect("send a");
        }
    };
    let send_b = async {
        for _ in 0..COUNT {
            let frame = session_b
                .send_clip(ClipContent::Text("b-to-a".to_owned()), now_ms())
                .expect("send_clip");
            link_b.send(&frame).await.expect("send b");
        }
    };
    tokio::join!(send_a, send_b);

    let recv_on_b = async {
        for _ in 0..COUNT {
            let got = recv_clip(&mut link_b).await;
            assert_eq!(
                deliver(&mut session_b, &got, now_ms()),
                InboundOutcome::LiveApplied
            );
        }
    };
    let recv_on_a = async {
        for _ in 0..COUNT {
            let got = recv_clip(&mut link_a).await;
            assert_eq!(
                deliver(&mut session_a, &got, now_ms()),
                InboundOutcome::LiveApplied
            );
        }
    };
    tokio::join!(recv_on_a, recv_on_b);

    let seqs_at_b: Vec<u64> = b.callback.live_items().iter().map(|item| item.seq).collect();
    let seqs_at_a: Vec<u64> = a.callback.live_items().iter().map(|item| item.seq).collect();
    let expected: Vec<u64> = (1..=COUNT).collect();
    assert_eq!(seqs_at_b, expected, "b must apply seqs in order, unique");
    assert_eq!(seqs_at_a, expected, "a must apply seqs in order, unique");
    assert_eq!(session_a.next_seq(), COUNT + 1);
    assert_eq!(session_b.next_seq(), COUNT + 1);
    assert_eq!(session_a.last_seq(), COUNT);
    assert_eq!(session_b.last_seq(), COUNT);
}

/// ㉓ A crash after the seq reservation skips the number but never reuses it:
/// the next clip after restart carries the following seq.
#[tokio::test]
async fn e2e_23_crash_after_seq_reservation_skips_never_reuses() {
    let relay = start_relay().await;
    let a = Endpoint::new();
    let b = Endpoint::new();
    let (mut link_a, mut link_b) = pair(&relay.url, &a, &b).await;
    let mut session_b = b.session(&link_b);

    {
        let mut session_a = a.session(&link_a);
        let error = session_a
            .send_clip_with_fault(
                ClipContent::Text("lost in crash".to_owned()),
                now_ms(),
                Some(SendStage::SeqReserved),
            )
            .expect_err("injected crash after reservation");
        assert!(matches!(error, SessionError::InjectedFault));
    } // process "crashes": the session is dropped mid-flight

    let mut session_a = a.session(&link_a);
    assert_eq!(
        session_a.next_seq(),
        2,
        "reservation was persisted before the crash"
    );
    let frame = session_a
        .send_clip(ClipContent::Text("after crash".to_owned()), now_ms())
        .expect("send_clip");
    link_a.send(&frame).await.expect("send");
    let got = recv_clip(&mut link_b).await;
    assert_eq!(
        deliver(&mut session_b, &got, now_ms()),
        InboundOutcome::LiveApplied
    );
    let live = b.callback.live_items();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].seq, 2, "seq 1 was skipped, never reused");
    assert!(
        !history_texts(&session_b)
            .iter()
            .any(|(text, _)| text == "lost in crash")
    );
}

/// ㉔ Re-pair after reset: a new identity generation pairs into a new room
/// whose seq namespace restarts at 1; the old watermark cannot suppress it.
#[tokio::test]
async fn e2e_24_repair_after_reset_uses_new_namespace() {
    let relay = start_relay().await;
    let a = Endpoint::new();
    let b = Endpoint::new();
    let (mut link_a, mut link_b) = pair(&relay.url, &a, &b).await;
    let old_room = link_a.record().room_id.clone();
    let old_fp_a = a.fp();
    {
        let mut session_a = a.session(&link_a);
        let mut session_b = b.session(&link_b);
        for text in ["gen one first", "gen one second"] {
            send_text(&mut session_a, &mut link_a, text, now_ms()).await;
            let got = recv_clip(&mut link_b).await;
            assert_eq!(
                deliver(&mut session_b, &got, now_ms()),
                InboundOutcome::LiveApplied
            );
        }
        assert_eq!(session_b.last_seq(), 2);
    }
    link_a.close().await.expect("close a");
    link_b.close().await.expect("close b");

    reset_pairing_state_after_quiesce(&a.store).expect("reset a");
    reset_pairing_state_after_quiesce(&b.store).expect("reset b");
    assert!(a.store.load_pairing().expect("load").is_none());
    assert_ne!(a.fp(), old_fp_a, "reset rotates the identity");

    let (mut link_a2, mut link_b2) = pair(&relay.url, &a, &b).await;
    let new_room = link_a2.record().room_id.clone();
    assert_ne!(old_room, new_room, "re-pair creates a new room");

    let mut session_a2 = a.session(&link_a2);
    let mut session_b2 = b.session(&link_b2);
    assert_eq!(session_a2.next_seq(), 1, "new namespace restarts seq");
    send_text(&mut session_a2, &mut link_a2, "new era", now_ms()).await;
    let got = recv_clip(&mut link_b2).await;
    assert_eq!(
        deliver(&mut session_b2, &got, now_ms()),
        InboundOutcome::LiveApplied,
        "seq 1 in the new namespace must apply despite the old watermark"
    );
    let live = b.callback.live_items();
    assert_eq!(live.last().expect("callback").seq, 1);
    assert_eq!(
        live.last().expect("callback").content,
        ClipContent::Text("new era".to_owned())
    );
}

/// ㉕ Mailbox redelivery across a relay restart is deduplicated by the
/// client's idempotency ring: the callback fires exactly once.
#[tokio::test]
async fn e2e_25_mailbox_redelivery_after_restart_is_deduplicated() {
    let dir = TempDir::new().expect("temp dir");
    let registry_dir = dir.path().join("registry");
    let mailbox_dir = dir.path().join("mailbox");
    let (event_tx, mut events) = mpsc::unbounded_channel();
    let mut config = ServerConfig::default();
    config.hooks.event_log = Some(event_tx);

    let registry = Arc::new(PersistentRegistry::open(&registry_dir).expect("registry"));
    let mailbox = PersistentMailbox::open(&mailbox_dir, MailboxOptions::default())
        .expect("mailbox opens");
    let pairing = Arc::new(PairingRelay::new(registry.clone(), PairingConfig::default()));
    let handle1 = start(
        "127.0.0.1:0".parse().expect("bind address"),
        config,
        registry,
        pairing,
        mailbox.clone(),
    )
    .await
    .expect("server one binds");
    let url1 = format!("ws://{}/ws", handle1.addr());

    let a = Endpoint::new();
    let b = Endpoint::new();
    let (mut link_a, link_b) = pair(&url1, &a, &b).await;
    let room = link_a.record().room_id.clone();
    let mut session_a = a.session(&link_a);

    link_b.close().await.expect("close b");
    wait_event(&mut events, "disconnect").await;
    send_text(&mut session_a, &mut link_a, "redeliver me", now_ms()).await;
    mailbox.wait_writes(1).await;

    // First delivery: bootstrap from the live actor's memory.
    let link_b1 = PairingClient::join(&b.store).await.expect("first rejoin");
    let (b_room, b_ct) = link_b1.bootstrap_clip().expect("first bootstrap");
    let mut session_b = b.session(&link_b1);
    assert_eq!(
        session_b
            .handle_clip(&b_room, &b_ct, true, now_ms())
            .expect("handle_clip"),
        InboundOutcome::MailboxApplied
    );
    assert_eq!(b.callback.mailbox_items().len(), 1);

    // Everyone leaves; the relay restarts over the same durable state.
    link_b1.close().await.expect("close b1");
    wait_event(&mut events, "disconnect").await;
    link_a.close().await.expect("close a");
    wait_event(&mut events, "disconnect").await;
    drop(handle1);
    drop(mailbox);

    let registry2 = Arc::new(PersistentRegistry::open(&registry_dir).expect("registry reopens"));
    let mailbox2 = PersistentMailbox::open(&mailbox_dir, MailboxOptions::default())
        .expect("mailbox reopens");
    let pairing2 = Arc::new(PairingRelay::new(registry2.clone(), PairingConfig::default()));
    let handle2 = start(
        "127.0.0.1:0".parse().expect("bind address"),
        ServerConfig::default(),
        registry2,
        pairing2,
        mailbox2,
    )
    .await
    .expect("server two binds");

    // The pairing record still points at the dead port; rejoin manually.
    let identity_b = b.identity();
    let mut raw = Client::connect(handle2.addr()).await;
    let bootstrap = raw_join(&mut raw, &identity_b, &room).await;
    let (redelivered_room, redelivered_ct, redelivered_mailbox, _) = clip_fields(&bootstrap);
    assert!(redelivered_mailbox, "redelivery is a mailbox bootstrap");
    assert_eq!(redelivered_ct, b_ct, "delivered clips are retained on disk");
    assert_eq!(
        session_b
            .handle_clip(&redelivered_room, &redelivered_ct, true, now_ms())
            .expect("handle_clip"),
        InboundOutcome::Duplicate,
        "dedup ring makes the redelivery idempotent"
    );
    assert_eq!(
        b.callback.mailbox_items().len(),
        1,
        "the callback must not fire again"
    );
    assert!(b.callback.live_items().is_empty());
    drop(handle2);
}

/// ㉖ Stale connection events are no-ops: after a rejoin replaces the old
/// connection, a clip event queued by the old connection is dropped by the
/// room actor and the old connection is closed.
#[tokio::test]
async fn e2e_26_stale_connection_events_are_dropped() {
    let gate = Arc::new(Semaphore::new(0));
    let config = ServerConfig {
        hooks: TestHooks {
            room_event_gate: Some(gate.clone()),
            ..TestHooks::default()
        },
        ..ServerConfig::default()
    };
    let mut relay = start_relay_with(config).await;

    gate.add_permits(2); // the two pairing joins
    let a = Endpoint::new();
    let b = Endpoint::new();
    let (mut link_a, mut link_b1) = pair(&relay.url, &a, &b).await;
    let mut session_a = a.session(&link_a);
    let mut session_b = b.session(&link_b1);
    drain_events(&mut relay.events);

    // B rejoins while the actor is gated; the Join is queued, not processed.
    let mut rejoin = Box::pin(PairingClient::join(&b.store));
    tokio::select! {
        result = &mut rejoin => panic!("rejoin completed while gated: {result:?}"),
        _ = wait_event(&mut relay.events, "join") => {}
    }
    // The stale connection queues a clip event behind the Join.
    let stale = session_b
        .send_clip(ClipContent::Text("stale connection".to_owned()), now_ms())
        .expect("send_clip");
    link_b1.send(&stale).await.expect("stale send");
    wait_event(&mut relay.events, "clip").await;

    // Actor order: Join replaces B's connection, then the stale clip is dropped.
    gate.add_permits(2);
    let mut link_b2 = rejoin.await.expect("rejoin completes");
    assert!(link_b2.bootstrap_clip().is_none());
    match timeout(TIMEOUT, link_b1.recv()).await {
        Ok(Err(TransportError::Closed)) | Ok(Err(TransportError::WebSocket(_))) => {}
        other => panic!("replaced connection must be closed, got {other:?}"),
    }
    expect_no_clip(&mut link_a, NEG_TIMEOUT).await;

    // The replacement connection is fully live.
    gate.add_permits(1);
    send_text(&mut session_a, &mut link_a, "live replacement", now_ms()).await;
    let got = recv_clip(&mut link_b2).await;
    assert_eq!(
        deliver(&mut session_b, &got, now_ms()),
        InboundOutcome::LiveApplied
    );
}

/// ㉗ Pending-latest coalescing: two clips for an offline peer collapse to
/// the latest one; the earlier clip is never delivered.
#[tokio::test]
async fn e2e_27_pending_latest_mailbox_coalesces() {
    let mut relay = start_relay().await;
    let a = Endpoint::new();
    let b = Endpoint::new();
    let (mut link_a, link_b) = pair(&relay.url, &a, &b).await;
    let mut session_a = a.session(&link_a);

    link_b.close().await.expect("close");
    wait_event(&mut relay.events, "disconnect").await;
    send_text(&mut session_a, &mut link_a, "stale news", now_ms()).await;
    relay.mailbox.wait_pending(1).await;
    send_text(&mut session_a, &mut link_a, "fresh news", now_ms()).await;
    relay.mailbox.wait_pending(2).await;

    let link_b2 = PairingClient::join(&b.store).await.expect("rejoin");
    let (room, ciphertext) = link_b2
        .bootstrap_clip()
        .expect("exactly one bootstrap clip");
    let mut session_b = b.session(&link_b2);
    assert_eq!(
        session_b
            .handle_clip(&room, &ciphertext, true, now_ms())
            .expect("handle_clip"),
        InboundOutcome::MailboxApplied
    );
    let mailbox = b.callback.mailbox_items();
    assert_eq!(mailbox.len(), 1, "only the latest clip is delivered");
    assert_eq!(
        mailbox[0].content,
        ClipContent::Text("fresh news".to_owned())
    );
    assert!(
        !history_texts(&session_b)
            .iter()
            .any(|(text, _)| text == "stale news"),
        "the superseded clip never enters history"
    );
}

/// ㉘ `pair_peer` dual-FIFO reservation is atomic: when either reservation
/// fails, neither side observes a `pair_peer` frame.
#[tokio::test]
async fn e2e_28_pair_peer_reservation_failure_enqueues_nothing() {
    let gate = Arc::new(Semaphore::new(0));
    let mut config = ServerConfig::default();
    config.limits.outbox_max_frames = 2;
    config.hooks.writer_gate = Some(gate.clone());
    let registry = Arc::new(InMemoryRegistry::new());
    let pairing = Arc::new(PairingRelay::new(registry.clone(), PairingConfig::default()));
    let mailbox = Arc::new(RecordingMailbox::default());
    let handle = start(
        "127.0.0.1:0".parse().expect("bind address"),
        config,
        registry,
        pairing.clone(),
        mailbox,
    )
    .await
    .expect("server binds");
    let addr = handle.addr();

    // Arm one: the offerer's outbox is full, so its reservation fails; the
    // claimer must observe no pair_peer either.
    let offerer_id = TestIdentity::generate();
    let mut offerer = Client::connect(addr).await;
    offerer.send(&offerer_id.hello_frame()).await;
    offerer
        .send(&Frame::PairOffer {
            code: "ABC234".to_owned(),
            pub_bundle: offerer_id.bundle.clone(),
        })
        .await;
    eventually(|| pairing.is_published("ABC234")).await;

    let claimer_id = TestIdentity::generate();
    let mut claimer = Client::connect(addr).await;
    claimer.send(&claimer_id.hello_frame()).await;
    claimer
        .send(&Frame::PairClaim {
            code: "ABC234".to_owned(),
            pub_bundle: claimer_id.bundle.clone(),
        })
        .await;
    // The claimer is force-closed while its writer is stalled; crucially no
    // pair_peer is ever delivered to it.
    match claimer.read(TIMEOUT).await {
        Read::Closed => {}
        other => panic!("claimer must be closed without pair_peer, got {other:?}"),
    }
    gate.add_permits(1); // unpause every writer
    match offerer.recv_frame().await {
        Frame::HelloOk { .. } => {}
        other => panic!("expected hello_ok, got {other:?}"),
    }
    match offerer.recv_frame().await {
        Frame::PairOfferOk => {}
        other => panic!("expected pair_offer_ok, got {other:?}"),
    }
    match offerer.read(NEG_TIMEOUT).await {
        Read::Timeout => {}
        other => panic!("offerer must not receive pair_peer, got {other:?}"),
    }

    // Arm two: the offerer disconnects after publishing; the claim fails
    // closed with an error and again no pair_peer anywhere.
    let offerer2_id = TestIdentity::generate();
    let mut offerer2 = Client::connect(addr).await;
    offerer2.send(&offerer2_id.hello_frame()).await;
    match offerer2.recv_frame().await {
        Frame::HelloOk { .. } => {}
        other => panic!("expected hello_ok, got {other:?}"),
    }
    offerer2
        .send(&Frame::PairOffer {
            code: "DEF456".to_owned(),
            pub_bundle: offerer2_id.bundle.clone(),
        })
        .await;
    match offerer2.recv_frame().await {
        Frame::PairOfferOk => {}
        other => panic!("expected pair_offer_ok, got {other:?}"),
    }
    assert!(pairing.is_published("DEF456"));
    drop(offerer2);
    eventually(|| !pairing.is_published("DEF456")).await;

    let claimer2_id = TestIdentity::generate();
    let mut claimer2 = Client::connect(addr).await;
    claimer2.send(&claimer2_id.hello_frame()).await;
    match claimer2.recv_frame().await {
        Frame::HelloOk { .. } => {}
        other => panic!("expected hello_ok, got {other:?}"),
    }
    claimer2
        .send(&Frame::PairClaim {
            code: "DEF456".to_owned(),
            pub_bundle: claimer2_id.bundle.clone(),
        })
        .await;
    claimer2.expect_error_close("code_expired").await;
    drop(handle);
}

/// ㉙ The verify-only feature surface the relay compiles against agrees with
/// the full client stack: fixed golden vectors hold and a live pairing's
/// room/members match on both sides of the feature boundary.
#[tokio::test]
async fn e2e_29_verify_and_full_vectors_agree() {
    // Fixed T3 golden vectors through the verify surface.
    let bundle_a = PubBundle {
        sign_pk: std::array::from_fn(|index| u8::try_from(index).expect("fits")),
        dh_pk: std::array::from_fn(|index| u8::try_from(index + 32).expect("fits")),
    };
    let bundle_b = PubBundle {
        sign_pk: std::array::from_fn(|index| u8::try_from(index + 64).expect("fits")),
        dh_pk: std::array::from_fn(|index| u8::try_from(index + 96).expect("fits")),
    };
    assert_eq!(
        bundle_fp(&bundle_a),
        "fdeab9acf3710362bd2658cdc9a29e8f9c757fcf9811603a8c447cd1d9151108"
    );
    assert_eq!(
        clipboard_core::crypto::device_id(&bundle_a.sign_pk),
        "630dcd2966c43366"
    );
    let forward = room_id(&bundle_a, &bundle_b);
    assert_eq!(forward, "471fb943aa23c511f6f72f8d1652d9c8");
    assert_eq!(room_id(&bundle_b, &bundle_a), forward);
    let join_message = join_sig_msg(
        &[1, 2, 3, 4],
        "471fb943aa23c511f6f72f8d1652d9c8",
        "630dcd2966c43366",
        &bundle_bytes(&bundle_a),
    )
    .expect("join message");
    assert_eq!(
        join_message,
        [
            b"clipboard-sync-join-v1".as_slice(),
            &[0, 0, 0, 4, 1, 2, 3, 4],
            &[0, 0, 0, 32],
            b"471fb943aa23c511f6f72f8d1652d9c8",
            &[0, 0, 0, 16],
            b"630dcd2966c43366",
            &[0, 0, 0, 64],
            bundle_bytes(&bundle_a).as_slice(),
        ]
        .concat()
    );

    // Live agreement: the full-stack pairing and the relay's verify-side
    // registry compute the same room and member fingerprints.
    let relay = start_relay().await;
    let a = Endpoint::new();
    let b = Endpoint::new();
    let (link_a, link_b) = pair(&relay.url, &a, &b).await;
    let room = link_a.record().room_id.clone();
    assert_eq!(room, room_id(&a.bundle(), &b.bundle()));
    assert_eq!(room, link_b.record().room_id);
    let mut members = relay.registry.lookup_members(&room);
    members.sort();
    let mut expected = vec![a.fp(), b.fp()];
    expected.sort();
    assert_eq!(members, expected);
    // The successful joins themselves prove sign(full) / verify(server) agree.
}
