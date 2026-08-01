//! Per-connection task: the server side of the protocol state machine.
//!
//! Pre-join, the connection accepts only the frames legal in the current protocol
//! state (`hello`, then `pair_*`/`join`); anything else is `bad_frame`. The join
//! path authenticates in brief order: consume the single-use challenge nonce, count
//! the per-IP attempt, verify the Ed25519 join signature, recompute `device_id`,
//! then check registry membership (unregistered room → `bad_auth`; a registered
//! room with an unknown fingerprint → `room_full`). After join, only `clip` frames
//! for the joined room are accepted; `pair_*` after join is `bad_frame`.

use std::net::IpAddr;
use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use rand::RngCore;
use rand::rngs::OsRng;
use tokio::time::timeout;

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use clipboard_core::crypto::{bundle_fp, device_id, join_sig_msg, verify};
use clipboard_core::protocol::{Frame, PROTOCOL_VERSION, PubBundle, bundle_bytes, decode_frame};

use crate::pairing::Connections;
use crate::room::{self, ConnectionId, OutboxHandle, OutboxReceiver};
use crate::server::ServerState;

/// Bounded wait for the writer task to flush a closing connection's final frames.
const WRITER_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Frames legal after hello while awaiting pair/join are handled by `phase`.
enum Phase {
    PreHello,
    Ready {
        nonce: Option<[u8; 32]>,
        pub_bundle: PubBundle,
    },
    Joined,
}

enum Step {
    Continue,
    Joined { room_id: String, fp: String },
    Close,
}

/// Runs one websocket connection for its whole lifetime.
pub async fn handle_connection(
    state: Arc<ServerState>,
    socket: WebSocket,
    client_ip: IpAddr,
    connection_id: ConnectionId,
) {
    let (sink, stream) = socket.split();
    let (outbox, outbox_rx) = room::outbox(
        state.config.limits.outbox_max_frames,
        state.config.limits.outbox_max_bytes,
    );
    state.connections.insert(connection_id, outbox.clone());
    let mut writer = tokio::spawn(run_writer(
        sink,
        outbox_rx,
        state.config.hooks.writer_gate.clone(),
    ));

    let mut connection = Connection {
        state: &state,
        outbox: &outbox,
        client_ip,
        connection_id,
        phase: Phase::PreHello,
        joined: None,
    };
    connection.read_loop(stream).await;

    if let Some((room_id, fp)) = connection.joined.take() {
        state.rooms.disconnect(&room_id, fp, connection_id);
    }
    state.pairing.on_disconnect(connection_id);
    state.connections.remove(&connection_id);
    drop(connection);
    // Dropping our outbox handle lets the writer drain queued frames (a pending
    // error frame followed by the close marker) before the task ends.
    drop(outbox);
    if timeout(WRITER_DRAIN_TIMEOUT, &mut writer).await.is_err() {
        writer.abort();
    }
}

struct Connection<'a> {
    state: &'a Arc<ServerState>,
    outbox: &'a OutboxHandle,
    client_ip: IpAddr,
    connection_id: ConnectionId,
    phase: Phase,
    joined: Option<(String, String)>,
}

impl Connection<'_> {
    async fn read_loop(&mut self, mut stream: SplitStream<WebSocket>) {
        loop {
            let limits = &self.state.config.limits;
            let next = match self.phase {
                Phase::PreHello => match timeout(limits.handshake_deadline, stream.next()).await {
                    Ok(next) => next,
                    Err(_) => break, // handshake deadline: close without an error frame
                },
                Phase::Ready { .. } => {
                    match timeout(limits.join_idle_deadline, stream.next()).await {
                        Ok(next) => next,
                        Err(_) => break, // pair/join idle deadline
                    }
                }
                Phase::Joined => stream.next().await,
            };
            let Some(Ok(message)) = next else { break };
            match message {
                Message::Binary(_) => {
                    self.outbox
                        .error_and_close("bad_frame", "binary frames are not accepted");
                    break;
                }
                Message::Text(text) => {
                    if !self
                        .state
                        .rate_limits
                        .add_bytes(self.client_ip, text.len() as u64)
                    {
                        self.outbox
                            .error_and_close("rate_limited", "per-IP byte budget exceeded");
                        break;
                    }
                    let raw_len = text.len();
                    let frame = match decode_frame(text.as_bytes()) {
                        Ok(frame) => frame,
                        Err(error) => {
                            let _ = self.outbox.send_frame(&error.to_error_frame());
                            self.outbox.close();
                            break;
                        }
                    };
                    match self.step(frame, raw_len) {
                        Step::Continue => {}
                        Step::Joined { room_id, fp } => {
                            self.phase = Phase::Joined;
                            self.joined = Some((room_id, fp));
                        }
                        Step::Close => break,
                    }
                }
                Message::Close(_) => break,
                Message::Ping(_) | Message::Pong(_) => {} // answered by axum
            }
        }
    }

    fn step(&mut self, frame: Frame, raw_len: usize) -> Step {
        match (&mut self.phase, frame) {
            (
                Phase::PreHello,
                Frame::Hello {
                    device_id: hello_device_id,
                    pub_bundle,
                    ..
                },
            ) => {
                if device_id(&pub_bundle.sign_pk) != hello_device_id {
                    self.outbox
                        .error_and_close("bad_auth", "hello device_id does not match signing key");
                    return Step::Close;
                }
                let mut nonce = [0_u8; 32];
                OsRng.fill_bytes(&mut nonce);
                let hello_ok = Frame::HelloOk {
                    server_version: PROTOCOL_VERSION,
                    nonce_b64: STANDARD.encode(nonce),
                };
                if !self.outbox.send_frame(&hello_ok) {
                    return Step::Close;
                }
                self.phase = Phase::Ready {
                    nonce: Some(nonce),
                    pub_bundle,
                };
                Step::Continue
            }
            (Phase::PreHello, frame) => {
                let _ = frame;
                self.outbox
                    .error_and_close("bad_frame", "expected hello as the first frame");
                Step::Close
            }
            (
                Phase::Ready { pub_bundle, .. },
                frame @ (Frame::PairOffer { .. } | Frame::PairClaim { .. }),
            ) => {
                match self.state.pairing.on_pair_frame(
                    self.connection_id,
                    self.client_ip,
                    pub_bundle,
                    frame,
                    self.outbox,
                    &self.state.connections,
                ) {
                    Ok(()) => Step::Continue,
                    Err(error_frame) => {
                        let _ = self.outbox.send_frame(&error_frame);
                        self.outbox.close();
                        Step::Close
                    }
                }
            }
            (
                Phase::Ready { nonce, .. },
                Frame::Join {
                    room_id,
                    device_id,
                    pub_bundle,
                    sig_b64,
                },
            ) => {
                // The challenge is single-use: any join attempt consumes it.
                let challenge = nonce.take();
                match self.handle_join(challenge, room_id, device_id, pub_bundle, sig_b64) {
                    Ok((room_id, fp)) => Step::Joined { room_id, fp },
                    Err(error_frame) => {
                        let _ = self.outbox.send_frame(&error_frame);
                        self.outbox.close();
                        Step::Close
                    }
                }
            }
            (Phase::Ready { .. }, frame) => {
                let _ = frame;
                self.outbox
                    .error_and_close("bad_frame", "frame is illegal before join");
                Step::Close
            }
            (
                Phase::Joined,
                Frame::Clip {
                    room_id,
                    ciphertext_b64,
                    ..
                },
            ) => {
                let (joined_room, fp) = self.joined.clone().expect("joined state is set");
                if room_id != joined_room {
                    self.outbox
                        .error_and_close("bad_frame", "clip room does not match the joined room");
                    return Step::Close;
                }
                if self
                    .state
                    .rooms
                    .clip(
                        &joined_room,
                        fp,
                        self.connection_id,
                        ciphertext_b64,
                        raw_len,
                    )
                    .is_err()
                {
                    self.outbox
                        .error_and_close("inbox_overflow", "room inbox exceeded");
                    return Step::Close;
                }
                Step::Continue
            }
            (Phase::Joined, frame) => {
                let _ = frame;
                self.outbox
                    .error_and_close("bad_frame", "frame is illegal after join");
                Step::Close
            }
        }
    }

    /// Authenticates and routes a join, in brief order: single-use challenge, per-IP
    /// attempt budget, signature verification, device_id recompute, registry
    /// membership, then the room actor.
    fn handle_join(
        &self,
        challenge: Option<[u8; 32]>,
        room_id: String,
        device_id_frame: String,
        pub_bundle: PubBundle,
        sig_b64: String,
    ) -> Result<(String, String), Box<Frame>> {
        let nonce = challenge.ok_or_else(|| error_frame("bad_auth", "no active join challenge"))?;
        if !self.state.rate_limits.check_join(self.client_ip) {
            return Err(error_frame("rate_limited", "join attempts exceeded"));
        }
        let signature = STANDARD
            .decode(&sig_b64)
            .ok()
            .and_then(|bytes| <[u8; 64]>::try_from(bytes).ok())
            .ok_or_else(|| error_frame("bad_auth", "sig_b64 must decode to 64 bytes"))?;
        let message = join_sig_msg(
            &nonce,
            &room_id,
            &device_id_frame,
            &bundle_bytes(&pub_bundle),
        )
        .map_err(|_| error_frame("bad_auth", "join signature input is invalid"))?;
        if verify(&pub_bundle.sign_pk, &message, &signature).is_err() {
            return Err(error_frame(
                "bad_auth",
                "join signature verification failed",
            ));
        }
        if device_id(&pub_bundle.sign_pk) != device_id_frame {
            return Err(error_frame(
                "bad_auth",
                "device_id does not match the signing key",
            ));
        }
        let members = self.state.registry.lookup_members(&room_id);
        if members.is_empty() {
            return Err(error_frame("bad_auth", "room is not registered"));
        }
        let fp = bundle_fp(&pub_bundle);
        if !members.iter().any(|member| member == &fp) {
            return Err(error_frame("room_full", "room already has two identities"));
        }
        self.state
            .rooms
            .join(
                &room_id,
                members,
                fp.clone(),
                device_id_frame,
                self.connection_id,
                self.outbox.clone(),
            )
            .map_err(|_| error_frame("server_full", "room inbox exceeded"))?;
        Ok((room_id, fp))
    }
}

fn error_frame(code: &str, message: &str) -> Box<Frame> {
    Box::new(Frame::Error {
        code: code.to_owned(),
        message: message.to_owned(),
    })
}

/// Writes outbound units to the websocket until a close marker, a forced close, or a
/// send failure. The test-only writer gate throttles sends; a forced close always
/// wins so a stalled writer cannot keep a disconnected peer open.
async fn run_writer(
    mut sink: SplitSink<WebSocket, Message>,
    mut rx: OutboxReceiver,
    writer_gate: Option<Arc<tokio::sync::Semaphore>>,
) {
    let close_signal = rx.close_signal();
    loop {
        if let Some(gate) = &writer_gate {
            tokio::select! {
                acquired = gate.acquire() => {
                    if acquired.is_err() {
                        return;
                    }
                }
                _ = close_signal.notified() => {
                    let _ = sink.send(Message::Close(Some(CloseFrame {
                        code: 1000,
                        reason: "closing".into(),
                    })))
                    .await;
                    return;
                }
            }
        }
        match rx.recv().await {
            Some(room::Outbound::Frame(bytes)) => {
                let Ok(text) = String::from_utf8(bytes) else {
                    return;
                };
                if sink.send(Message::Text(text.into())).await.is_err() {
                    return;
                }
            }
            Some(room::Outbound::Close) | None => {
                let _ = sink
                    .send(Message::Close(Some(CloseFrame {
                        code: 1000,
                        reason: "closing".into(),
                    })))
                    .await;
                return;
            }
        }
    }
}

/// Re-export used by the pairing seam signature.
pub type ConnectionMap = Connections;
