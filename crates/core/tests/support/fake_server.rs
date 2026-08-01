//! In-process WebSocket relay used only by the core transport tests.

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use rand::{RngCore, rngs::OsRng};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

use clipboard_core::crypto::{bundle_bytes, bundle_fp, device_id, join_sig_msg, room_id, verify};
use clipboard_core::protocol::{Frame, PROTOCOL_VERSION, PubBundle, decode_frame, encode_frame};

#[derive(Clone, Copy, Debug)]
pub enum FakeCommand {
    ExtraBootstrap,
}

struct Offer {
    connection_id: u64,
    bundle: PubBundle,
}

#[derive(Default)]
struct Room {
    members: HashSet<String>,
    online: HashMap<String, (u64, mpsc::UnboundedSender<Frame>)>,
    mailbox: Option<(String, String)>,
}

#[derive(Default)]
struct RelayState {
    offers: HashMap<String, Offer>,
    connections: HashMap<u64, mpsc::UnboundedSender<Frame>>,
    rooms: HashMap<String, Room>,
}

struct Shared {
    state: Mutex<RelayState>,
    next_connection: AtomicU64,
    extra_bootstrap: AtomicBool,
}

pub struct FakeServer {
    url: String,
    shared: Arc<Shared>,
    task: JoinHandle<()>,
}

impl FakeServer {
    pub async fn start() -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("ws://{}", listener.local_addr()?);
        let shared = Arc::new(Shared {
            state: Mutex::new(RelayState::default()),
            next_connection: AtomicU64::new(1),
            extra_bootstrap: AtomicBool::new(false),
        });
        let accept_shared = shared.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let connection_shared = accept_shared.clone();
                tokio::spawn(async move {
                    let _ = serve_connection(stream, connection_shared).await;
                });
            }
        });
        Ok(Self { url, shared, task })
    }

    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn command(&self, command: FakeCommand) {
        match command {
            FakeCommand::ExtraBootstrap => {
                self.shared.extra_bootstrap.store(true, Ordering::SeqCst)
            }
        }
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve_connection(stream: TcpStream, shared: Arc<Shared>) -> Result<(), ()> {
    let socket = accept_async(stream).await.map_err(|_| ())?;
    let connection_id = shared.next_connection.fetch_add(1, Ordering::Relaxed);
    let (mut sink, mut incoming) = socket.split();
    let (outgoing, mut outgoing_rx) = mpsc::unbounded_channel::<Frame>();
    shared
        .state
        .lock()
        .await
        .connections
        .insert(connection_id, outgoing.clone());
    let writer = tokio::spawn(async move {
        while let Some(frame) = outgoing_rx.recv().await {
            let Ok(encoded) = encode_frame(&frame) else {
                break;
            };
            let Ok(text) = String::from_utf8(encoded) else {
                break;
            };
            if sink.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    });

    let mut challenge = None;
    let mut hello_bundle = None;
    let mut joined: Option<(String, String)> = None;
    while let Some(Ok(message)) = incoming.next().await {
        let Message::Text(text) = message else { break };
        let Ok(frame) = decode_frame(text.as_bytes()) else {
            break;
        };
        match frame {
            Frame::Hello {
                pub_bundle,
                version,
                ..
            } if version == PROTOCOL_VERSION && challenge.is_none() => {
                let mut nonce = [0_u8; 32];
                OsRng.fill_bytes(&mut nonce);
                challenge = Some(nonce);
                hello_bundle = Some(pub_bundle);
                let _ = outgoing.send(Frame::HelloOk {
                    server_version: PROTOCOL_VERSION,
                    nonce_b64: STANDARD.encode(nonce),
                });
            }
            Frame::PairOffer { code, pub_bundle } if challenge.is_some() => {
                let mut state = shared.state.lock().await;
                state.offers.insert(
                    code,
                    Offer {
                        connection_id,
                        bundle: pub_bundle,
                    },
                );
                let _ = outgoing.send(Frame::PairOfferOk);
            }
            Frame::PairClaim { code, pub_bundle } if challenge.is_some() => {
                let mut state = shared.state.lock().await;
                let Some(offer) = state.offers.remove(&code) else {
                    let _ = outgoing.send(Frame::Error {
                        code: "code_expired".to_owned(),
                        message: "pairing code is unavailable".to_owned(),
                    });
                    continue;
                };
                if bundle_fp(&offer.bundle) == bundle_fp(&pub_bundle) {
                    let _ = outgoing.send(Frame::Error {
                        code: "same_identity".to_owned(),
                        message: "identity cannot pair with itself".to_owned(),
                    });
                    continue;
                }
                let room_key = room_id(&offer.bundle, &pub_bundle);
                state.rooms.insert(
                    room_key,
                    Room {
                        members: HashSet::from([bundle_fp(&offer.bundle), bundle_fp(&pub_bundle)]),
                        ..Room::default()
                    },
                );
                if let Some(peer) = state.connections.get(&offer.connection_id) {
                    let _ = peer.send(Frame::PairPeer {
                        peer_pub_bundle: pub_bundle.clone(),
                    });
                }
                let _ = outgoing.send(Frame::PairPeer {
                    peer_pub_bundle: offer.bundle,
                });
            }
            Frame::Join {
                room_id: requested_room,
                device_id: claimed_device,
                pub_bundle,
                sig_b64,
            } => {
                let Some(nonce) = challenge.take() else { break };
                if hello_bundle.as_ref() != Some(&pub_bundle) {
                    break;
                }
                let signature = STANDARD
                    .decode(sig_b64)
                    .ok()
                    .and_then(|bytes| <[u8; 64]>::try_from(bytes).ok());
                let message = join_sig_msg(
                    &nonce,
                    &requested_room,
                    &claimed_device,
                    &bundle_bytes(&pub_bundle),
                )
                .ok();
                let authenticated = claimed_device == device_id(&pub_bundle.sign_pk)
                    && signature.zip(message).is_some_and(|(signature, message)| {
                        verify(&pub_bundle.sign_pk, &message, &signature).is_ok()
                    });
                let fp = bundle_fp(&pub_bundle);
                let mut state = shared.state.lock().await;
                let Some(room) = state.rooms.get_mut(&requested_room) else {
                    let _ = outgoing.send(server_error("bad_auth", "room is not registered"));
                    continue;
                };
                if !authenticated || !room.members.contains(&fp) {
                    let _ = outgoing.send(server_error("bad_auth", "join authentication failed"));
                    continue;
                }
                let _ = outgoing.send(Frame::JoinOk);
                let bootstrap = match &room.mailbox {
                    Some((origin, ciphertext)) if origin != &fp => Frame::Clip {
                        room_id: requested_room.clone(),
                        ciphertext_b64: ciphertext.clone(),
                        origin_device: origin.clone(),
                        mailbox: true,
                    },
                    _ => Frame::MailboxEmpty,
                };
                let _ = outgoing.send(bootstrap);
                if shared.extra_bootstrap.swap(false, Ordering::SeqCst) {
                    let _ = outgoing.send(Frame::MailboxEmpty);
                }
                room.online
                    .insert(fp.clone(), (connection_id, outgoing.clone()));
                joined = Some((requested_room, fp));
            }
            Frame::Clip {
                room_id: clip_room,
                ciphertext_b64,
                ..
            } => {
                let Some((joined_room, sender_fp)) = &joined else {
                    break;
                };
                if &clip_room != joined_room {
                    break;
                }
                let mut state = shared.state.lock().await;
                let Some(room) = state.rooms.get_mut(joined_room) else {
                    break;
                };
                let recipients: Vec<String> = room
                    .members
                    .iter()
                    .filter(|member| *member != sender_fp)
                    .cloned()
                    .collect();
                for recipient in recipients {
                    if let Some((_, recipient_tx)) = room.online.get(&recipient) {
                        let _ = recipient_tx.send(Frame::Clip {
                            room_id: clip_room.clone(),
                            ciphertext_b64: ciphertext_b64.clone(),
                            origin_device: sender_fp.clone(),
                            mailbox: false,
                        });
                    } else {
                        room.mailbox = Some((sender_fp.clone(), ciphertext_b64.clone()));
                    }
                }
            }
            _ => break,
        }
    }

    let mut state = shared.state.lock().await;
    state.connections.remove(&connection_id);
    if let Some((room_id, fp)) = joined
        && let Some(room) = state.rooms.get_mut(&room_id)
        && room
            .online
            .get(&fp)
            .is_some_and(|(id, _)| *id == connection_id)
    {
        room.online.remove(&fp);
    }
    drop(state);
    writer.abort();
    Ok(())
}

fn server_error(code: &str, message: &str) -> Frame {
    Frame::Error {
        code: code.to_owned(),
        message: message.to_owned(),
    }
}
