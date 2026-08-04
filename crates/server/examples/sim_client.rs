//! Live-container smoke client for the local Caddy TLS path.

use std::error::Error;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use clipboard_core::crypto::{Identity, join_sig_msg, room_id};
use clipboard_core::history::{HistoryKind, HistorySource, HistoryStore};
use clipboard_core::pairing::PairingStore;
use clipboard_core::pairing_client::{LiveLink, PairingClient, tls_config_with_roots};
use clipboard_core::protocol::{Frame, PROTOCOL_VERSION, bundle_bytes, decode_frame, encode_frame};
use clipboard_core::session::{
    CallbackError, ClipContent, ClipItem, InboundOutcome, MailboxDisposition, Session,
    SessionCallback, SessionStore,
};
use futures_util::{SinkExt, StreamExt};
use rustls::ClientConfig;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{Connector, client_async_tls_with_config};

const LOCAL_WSS_URL: &str = "wss://localhost:8443/ws";
const IO_TIMEOUT: Duration = Duration::from_secs(30);

type SmokeResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Default)]
struct RecordingCallback {
    live: Mutex<Vec<ClipItem>>,
    mailbox: Mutex<Vec<ClipItem>>,
}

impl RecordingCallback {
    fn live_items(&self) -> Vec<ClipItem> {
        self.live.lock().expect("live callback lock").clone()
    }

    fn mailbox_items(&self) -> Vec<ClipItem> {
        self.mailbox.lock().expect("mailbox callback lock").clone()
    }
}

impl SessionCallback for RecordingCallback {
    fn on_clip(&self, item: &ClipItem) -> Result<(), CallbackError> {
        self.live
            .lock()
            .expect("live callback lock")
            .push(item.clone());
        Ok(())
    }

    fn on_mailbox_clip(&self, item: &ClipItem) -> Result<MailboxDisposition, CallbackError> {
        self.mailbox
            .lock()
            .expect("mailbox callback lock")
            .push(item.clone());
        Ok(MailboxDisposition::Deferred)
    }
}

struct Endpoint {
    _root: TempDir,
    pairing: PairingStore,
    session_dir: PathBuf,
    history_dir: PathBuf,
    callback: Arc<RecordingCallback>,
}

impl Endpoint {
    fn new() -> SmokeResult<Self> {
        let root = TempDir::new()?;
        let pairing = PairingStore::new(root.path().join("pairing"))?;
        pairing.load_identity()?;
        Ok(Self {
            session_dir: root.path().join("session"),
            history_dir: root.path().join("history"),
            callback: Arc::new(RecordingCallback::default()),
            pairing,
            _root: root,
        })
    }

    fn session(&self, link: &LiveLink) -> SmokeResult<Session> {
        Ok(Session::new(
            link.identity(),
            link.record(),
            SessionStore::new(&self.session_dir)?,
            HistoryStore::new(&self.history_dir)?,
            self.callback.clone(),
        )?)
    }
}

#[tokio::main]
async fn main() -> SmokeResult<()> {
    let (url, ca_path) = parse_args()?;
    require(
        url == LOCAL_WSS_URL,
        format!("refusing non-local relay URL: {url}"),
    )?;
    let ca_der = std::fs::read(&ca_path)?;
    let tls = tls_config_with_roots(&[&ca_der])?;

    run_live_protocol(&url, tls.clone()).await?;
    trigger_forged_xff_rate_limit(&url, tls).await?;
    println!("PASS: live WSS simulation completed");
    Ok(())
}

fn parse_args() -> SmokeResult<(String, PathBuf)> {
    let mut args = std::env::args().skip(1);
    let mut url = None;
    let mut ca = None;
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| io::Error::other(format!("missing value for {flag}")))?;
        match flag.as_str() {
            "--url" => url = Some(value),
            "--ca" => ca = Some(PathBuf::from(value)),
            _ => return Err(io::Error::other(format!("unknown argument: {flag}")).into()),
        }
    }
    Ok((
        url.ok_or_else(|| io::Error::other("--url is required"))?,
        ca.ok_or_else(|| io::Error::other("--ca is required"))?,
    ))
}

async fn run_live_protocol(url: &str, tls: Arc<ClientConfig>) -> SmokeResult<()> {
    let sender = Endpoint::new()?;
    let receiver = Endpoint::new()?;

    let client = PairingClient::connect_with_tls(&sender.pairing, url, tls.clone()).await?;
    let offer = client.pair_begin().await?;
    let qr = offer.qr().clone();
    require(qr.server == url, "pairing QR changed the pinned relay URL")?;
    let (sender_channel, receiver_channel) = tokio::try_join!(
        offer.wait_peer(),
        PairingClient::claim_with_tls(&receiver.pairing, &qr, tls.clone()),
    )?;
    let sas = sender_channel.pending().sas();
    require(
        sas == receiver_channel.pending().sas(),
        "pairing SAS did not match",
    )?;
    require(
        sas.len() == 6 && sas.bytes().all(|byte| byte.is_ascii_digit()),
        "pairing SAS was not six digits",
    )?;
    let (mut sender_link, receiver_link) = tokio::try_join!(
        sender_channel.confirm_pair(&sas, &sender.pairing),
        receiver_channel.confirm_pair(&sas, &receiver.pairing),
    )?;
    println!("PASS: offer, claim, and mandatory six-digit SAS confirmation over WSS");

    let mut sender_session = sender.session(&sender_link)?;
    let mut receiver_session = receiver.session(&receiver_link)?;
    let mut receiver_link = receiver_link;

    send_content(
        &mut sender_session,
        &mut sender_link,
        ClipContent::Text("live container smoke".to_owned()),
    )
    .await?;
    receive_live(&mut receiver_session, &mut receiver_link).await?;
    let live = receiver.callback.live_items();
    require(
        matches!(
            live.last().map(|item| &item.content),
            Some(ClipContent::Text(text)) if text == "live container smoke"
        ),
        "live text did not arrive byte-exact",
    )?;
    println!("PASS: live text roundtrip");

    for size in [2 * 1024 * 1024, 10 * 1024 * 1024] {
        let image = deterministic_image(size);
        let expected_digest = sha256_hex(&image);
        send_content(
            &mut sender_session,
            &mut sender_link,
            ClipContent::Image(image.clone()),
        )
        .await?;
        receive_live(&mut receiver_session, &mut receiver_link).await?;
        let received = receiver.callback.live_items();
        let actual = match received.last().map(|item| &item.content) {
            Some(ClipContent::Image(bytes)) => bytes,
            _ => return Err(io::Error::other("expected received image callback").into()),
        };
        require(
            actual == &image,
            format!("{size}-byte image changed in transit"),
        )?;
        let actual_digest = sha256_hex(actual);
        require(
            actual_digest == expected_digest,
            format!("{size}-byte image SHA-256 mismatch"),
        )?;
        println!(
            "PASS: {} MiB image roundtrip sha256={actual_digest}",
            size / 1024 / 1024
        );
    }

    receiver_link.close().await?;
    sleep(Duration::from_millis(300)).await;
    send_content(
        &mut sender_session,
        &mut sender_link,
        ClipContent::Text("offline older".to_owned()),
    )
    .await?;
    send_content(
        &mut sender_session,
        &mut sender_link,
        ClipContent::Text("offline latest".to_owned()),
    )
    .await?;
    sleep(Duration::from_millis(500)).await;

    let mut rejoined = PairingClient::join_with_tls(&receiver.pairing, tls).await?;
    let (room_id, ciphertext) = rejoined
        .bootstrap_clip()
        .ok_or_else(|| io::Error::other("rejoin did not receive mailbox bootstrap"))?;
    let outcome = receiver_session.handle_clip(&room_id, &ciphertext, true, now_ms())?;
    require(
        outcome == InboundOutcome::MailboxDeferred,
        format!("mailbox outcome was {outcome:?}, expected MailboxDeferred"),
    )?;
    let mailbox = receiver.callback.mailbox_items();
    require(
        mailbox.len() == 1,
        "mailbox callback was not delivered exactly once",
    )?;
    require(
        matches!(
            mailbox.first().map(|item| &item.content),
            Some(ClipContent::Text(text)) if text == "offline latest"
        ),
        "mailbox bootstrap was not the latest offline clip",
    )?;
    let history = receiver_session.history().list();
    require(
        history.iter().any(|item| {
            item.source == HistorySource::RemoteDeferred
                && matches!(&item.kind, HistoryKind::Text { content } if content == "offline latest")
        }),
        "latest mailbox clip was not stored as RemoteDeferred",
    )?;
    require(
        !history.iter().any(
            |item| matches!(&item.kind, HistoryKind::Text { content } if content == "offline older")
        ),
        "older offline clip was unexpectedly bootstrapped",
    )?;
    require(
        timeout(Duration::from_millis(500), rejoined.recv())
            .await
            .is_err(),
        "rejoin delivered more than one bootstrap frame",
    )?;
    println!("PASS: rejoin delivered only latest mailbox clip as RemoteDeferred");
    Ok(())
}

async fn send_content(
    session: &mut Session,
    link: &mut LiveLink,
    content: ClipContent,
) -> SmokeResult<()> {
    let frame = session.send_clip(content, now_ms())?;
    timeout(IO_TIMEOUT, link.send(&frame)).await??;
    Ok(())
}

async fn receive_live(session: &mut Session, link: &mut LiveLink) -> SmokeResult<()> {
    let frame = timeout(IO_TIMEOUT, link.recv()).await??;
    let Frame::Clip {
        room_id,
        ciphertext_b64,
        mailbox,
        ..
    } = frame
    else {
        return Err(io::Error::other("expected live clip frame").into());
    };
    require(!mailbox, "live frame was marked as mailbox")?;
    let outcome = session.handle_clip(&room_id, &ciphertext_b64, mailbox, now_ms())?;
    require(
        outcome == InboundOutcome::LiveApplied,
        format!("live outcome was {outcome:?}"),
    )
}

async fn trigger_forged_xff_rate_limit(url: &str, tls: Arc<ClientConfig>) -> SmokeResult<()> {
    let mut saw_bad_auth = false;
    let mut saw_rate_limit = false;
    for _ in 0..5 {
        let result = forged_join_attempt(url, tls.clone()).await?;
        match result.as_str() {
            "bad_auth" => saw_bad_auth = true,
            "rate_limited" => saw_rate_limit = true,
            code => {
                return Err(
                    io::Error::other(format!("unexpected forged-XFF join result: {code}")).into(),
                );
            }
        }
    }
    require(
        saw_bad_auth,
        "forged-XFF probe never reached join authentication",
    )?;
    require(
        saw_rate_limit,
        "forged-XFF probe did not trigger a join rate limit",
    )?;
    println!("PASS: forged X-Forwarded-For probe triggered the shared Caddy-client rate limit");
    Ok(())
}

async fn forged_join_attempt(url: &str, tls: Arc<ClientConfig>) -> SmokeResult<String> {
    let mut request = url.into_client_request()?;
    request
        .headers_mut()
        .insert("x-forwarded-for", "203.0.113.9".parse()?);
    let tcp = timeout(IO_TIMEOUT, TcpStream::connect(("localhost", 8443))).await??;
    let config = WebSocketConfig::default()
        .max_frame_size(Some(24 * 1024 * 1024))
        .max_message_size(Some(24 * 1024 * 1024));
    let (mut socket, _) = timeout(
        IO_TIMEOUT,
        client_async_tls_with_config(request, tcp, Some(config), Some(Connector::Rustls(tls))),
    )
    .await??;

    let identity = Identity::generate();
    send_frame(
        &mut socket,
        &Frame::Hello {
            device_id: identity.device_id(),
            pub_bundle: identity.public_bundle(),
            version: PROTOCOL_VERSION,
        },
    )
    .await?;
    let nonce = match recv_frame(&mut socket).await? {
        Frame::HelloOk { nonce_b64, .. } => {
            let decoded = STANDARD.decode(nonce_b64)?;
            <[u8; 32]>::try_from(decoded)
                .map_err(|_| io::Error::other("hello nonce was not 32 bytes"))?
        }
        frame => {
            return Err(io::Error::other(format!(
                "expected hello_ok during forged-XFF probe, got {frame:?}"
            ))
            .into());
        }
    };
    let peer = Identity::generate();
    let room = room_id(&identity.public_bundle(), &peer.public_bundle());
    let message = join_sig_msg(
        &nonce,
        &room,
        &identity.device_id(),
        &bundle_bytes(&identity.public_bundle()),
    )?;
    send_frame(
        &mut socket,
        &Frame::Join {
            room_id: room,
            device_id: identity.device_id(),
            pub_bundle: identity.public_bundle(),
            sig_b64: STANDARD.encode(identity.sign(&message)),
        },
    )
    .await?;
    match recv_frame(&mut socket).await? {
        Frame::Error { code, .. } => Ok(code),
        frame => Err(io::Error::other(format!(
            "expected join error during forged-XFF probe, got {frame:?}"
        ))
        .into()),
    }
}

async fn send_frame<S>(socket: &mut S, frame: &Frame) -> SmokeResult<()>
where
    S: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    let text = String::from_utf8(encode_frame(frame)?)?;
    timeout(IO_TIMEOUT, socket.send(Message::Text(text.into()))).await??;
    Ok(())
}

async fn recv_frame<S>(socket: &mut S) -> SmokeResult<Frame>
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    match timeout(IO_TIMEOUT, socket.next()).await? {
        Some(Ok(Message::Text(text))) => Ok(decode_frame(text.as_bytes())?),
        Some(Ok(Message::Close(_))) | None => Err(io::Error::other("websocket closed").into()),
        Some(Ok(message)) => {
            Err(io::Error::other(format!("unexpected websocket message: {message:?}")).into())
        }
        Some(Err(error)) => Err(error.into()),
    }
}

fn deterministic_image(size: usize) -> Vec<u8> {
    (0..size).map(|index| (index % 251) as u8).collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after epoch")
        .as_millis()
        .try_into()
        .expect("current timestamp fits i64")
}

fn require(condition: bool, message: impl Into<String>) -> SmokeResult<()> {
    if condition {
        Ok(())
    } else {
        Err(io::Error::other(message.into()).into())
    }
}
