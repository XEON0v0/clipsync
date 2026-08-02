//! UniFFI FFI surface for the platform shells (macOS Swift, Android Kotlin).
//!
//! Contract (normative):
//!
//! - [`CoreCallbacks`] is the typed callback surface: `on_clip` (live),
//!   `on_mailbox_clip` (returns [`MailboxDisposition`]), and `on_status`.
//!   A callback error or panic never kills the session: the failure is logged
//!   and surfaced through `on_status(CoreStatus::Error)`, history is kept.
//! - [`CoreHandle`] owns a bounded FIFO dispatcher plus a dedicated tokio
//!   runtime. All clipboard operations are serialized through one command
//!   channel; a full queue fails `send_*` with the typed
//!   [`CoreError::QueueFull`] backpressure error, never a silent drop.
//! - `reset_pairing` first stops and joins the session and dispatcher, then
//!   runs the T5 post-quiescence generation reset, then installs a fresh empty
//!   dispatcher/runtime so the handle returns to `ReadyUnpaired` and can
//!   immediately pair again.
//! - No raw pointers, no private-key material, and no ownership-transfer APIs
//!   cross the FFI boundary; all state lives behind this handle.
// allow: SIZE_OK - T10 locks the complete FFI contract (callbacks, dispatcher, reset) here.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use thiserror::Error;
use tokio::runtime::{Handle, Runtime};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::history::{HistoryKind, HistorySource, HistoryStore};
use crate::pairing::{
    PairingError, PairingRecord, PairingStore, QrPayload, reset_pairing_state_after_quiesce,
    validate_server,
};
use crate::pairing_client::{LiveEvent, LiveLink, PairingChannel, PairingClient, TransportError};
use crate::session::{
    CallbackError, ClipContent, ClipItem, MailboxDisposition as SessionDisposition, Session,
    SessionCallback, SessionError, SessionStore,
};

/// Default bound of the outbound dispatcher queue.
pub const DEFAULT_QUEUE_CAPACITY: usize = 32;
/// Network bound for one pairing/join round trip driven from an FFI call.
const IO_TIMEOUT: Duration = Duration::from_secs(15);
/// How long reset/shutdown waits for the dispatcher to drain and exit.
const JOIN_TIMEOUT: Duration = Duration::from_secs(30);
/// Bound for the final runtime shutdown after the dispatcher exited.
const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// Clipboard payload crossing the FFI boundary.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum FfiClipContent {
    Text { text: String },
    Image { bytes: Vec<u8> },
}

/// One authenticated clipboard item delivered to the host shell.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct FfiClipItem {
    pub id: String,
    pub ts_ms: i64,
    pub seq: u64,
    pub content: FfiClipContent,
}

/// Host decision for a mailbox (or stale-live) clip.
#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum MailboxDisposition {
    Applied,
    Deferred,
}

/// Typed lifecycle status pushed to the host.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum CoreStatus {
    /// No durable pairing; ready to pair.
    ReadyUnpaired,
    /// Pairing offer published; waiting for the peer to claim.
    Offering,
    /// Peer bundle arrived; the SAS must be confirmed by the user.
    SasReady,
    /// Joining the relay with the durable pairing record.
    Connecting,
    /// Live session established (also fired after a successful reconnect).
    Connected,
    /// Transport dropped; automatic reconnect with backoff is in progress.
    Reconnecting,
    /// Live session ended; `start()` reconnects while the pairing survives.
    Disconnected,
    /// Non-fatal failure (for example a rejected clipboard callback).
    Error { message: String },
}

/// Point-in-time pairing state for hosts that prefer polling over callbacks.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum PairingSnapshot {
    Unpaired,
    Offering { qr_json: String },
    SasReady { sas: String },
    Paired { room_id: String },
}

/// One history entry as shown by the host.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct FfiHistoryItem {
    pub id: String,
    pub ts_ms: i64,
    pub kind: FfiHistoryKind,
    pub source: FfiHistorySource,
}

/// History payload kind; image bytes are fetched via `history_image_bytes`.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum FfiHistoryKind {
    Text { content: String },
    Image,
}

/// History provenance, mirroring the T4 store sources.
#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum FfiHistorySource {
    Local,
    Remote,
    RemoteDeferred,
}

/// Typed errors crossing the FFI boundary.
///
/// `QueueFull` is the typed backpressure signal: the bounded dispatcher queue
/// is full and the host must retry later instead of dropping the clip.
#[derive(Debug, Error, uniffi::Error)]
pub enum CoreError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("another pairing or session transition is in progress")]
    Busy,
    #[error("no live session is connected")]
    NotPaired,
    #[error("outbound dispatcher queue is full")]
    QueueFull,
    #[error("relay transport failed: {0}")]
    Transport(String),
    #[error("the confirmed SAS does not match")]
    SasMismatch,
    #[error("core handle is shut down")]
    Shutdown,
    #[error("internal failure: {0}")]
    Internal(String),
}

/// Typed callback surface implemented by the platform shell.
///
/// Implementations must be thread-safe; callbacks fire from dispatcher worker
/// threads, never while the handle holds its state lock, so hosts may call
/// back into the handle (for example `pair_poll`) from any callback.
#[uniffi::export(callback_interface)]
pub trait CoreCallbacks: Send + Sync {
    /// Fresh live clip; core has already durably recorded history.
    fn on_clip(&self, item: FfiClipItem) -> Result<(), CoreError>;
    /// Mailbox or stale clip; the returned disposition decides whether the
    /// history entry is promoted from `RemoteDeferred` to `Remote`.
    fn on_mailbox_clip(&self, item: FfiClipItem) -> Result<MailboxDisposition, CoreError>;
    /// Typed lifecycle status transition.
    fn on_status(&self, status: CoreStatus);
}

enum CoreCommand {
    Send {
        content: ClipContent,
        ts_ms: i64,
        reply: std::sync::mpsc::Sender<Result<u64, CoreError>>,
    },
    History {
        reply: std::sync::mpsc::Sender<Result<Vec<FfiHistoryItem>, CoreError>>,
    },
    HistoryImage {
        id: Uuid,
        reply: std::sync::mpsc::Sender<Result<Vec<u8>, CoreError>>,
    },
    IsEcho {
        hash: String,
        reply: std::sync::mpsc::Sender<bool>,
    },
    Connect {
        connection: Box<Connection>,
        reply: std::sync::mpsc::Sender<()>,
    },
    Disconnect {
        reply: std::sync::mpsc::Sender<()>,
    },
    #[doc(hidden)]
    Stall {
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    },
}

struct Connection {
    link: LiveLink,
    session: Session,
    store: Arc<PairingStore>,
}

enum PairingPhase {
    Unpaired,
    /// A blocking FFI pairing operation holds the phase; concurrent starters fail.
    Busy,
    Offering {
        qr_json: String,
        wait: JoinHandle<()>,
        channel: Arc<Mutex<Option<PairingChannel>>>,
    },
    SasReady {
        sas: String,
        channel: Arc<Mutex<Option<PairingChannel>>>,
    },
    Paired {
        record: PairingRecord,
    },
}

struct Active {
    runtime: Runtime,
    cmd_tx: mpsc::Sender<CoreCommand>,
    dispatcher: JoinHandle<()>,
    joined: std::sync::mpsc::Receiver<()>,
    stopped: Arc<AtomicBool>,
    phase: PairingPhase,
}

enum HandleState {
    Active(Box<Active>),
    /// reset/shutdown in progress on another thread.
    Transitioning,
    Shutdown,
}

/// The single FFI entry point: pairing lifecycle, clipboard session, and history.
#[derive(uniffi::Object)]
pub struct CoreHandle {
    root: PathBuf,
    callbacks: Arc<dyn CoreCallbacks>,
    queue_capacity: usize,
    this: Weak<CoreHandle>,
    state: Mutex<HandleState>,
}

#[uniffi::export]
impl CoreHandle {
    /// Opens (or creates) the core state under `data_dir` and starts the
    /// bounded FIFO dispatcher on a fresh runtime.
    #[uniffi::constructor]
    pub fn new(data_dir: String, callbacks: Box<dyn CoreCallbacks>) -> Result<Arc<Self>, CoreError> {
        Self::build(data_dir, callbacks, DEFAULT_QUEUE_CAPACITY)
    }

    /// Publishes a fresh pairing offer against `server_url` and returns the QR
    /// payload JSON the host must display. The peer's arrival is announced via
    /// `on_status(SasReady)` / `pair_poll`.
    ///
    /// `server_url` is validated in core (`wss://`; `ws://` only in debug
    /// builds) before any network I/O.
    pub fn pair_begin(&self, server_url: String) -> Result<String, CoreError> {
        validate_server(&server_url).map_err(|error| CoreError::InvalidInput(error.to_string()))?;
        let store = Arc::new(self.pairing_store()?);
        let handle = self.begin_phase_transition()?;
        let offer = block_on_io(&handle, async move {
            let client = PairingClient::connect(&store, &server_url)
                .await
                .map_err(CoreError::from_transport)?;
            client.pair_begin().await.map_err(CoreError::from_transport)
        });
        let offer = match offer {
            Ok(offer) => offer,
            Err(error) => {
                self.abort_phase_transition(&error);
                return Err(error);
            }
        };
        let qr_json = offer.qr().to_json().map_err(CoreError::from_pairing)?;
        let slot: Arc<Mutex<Option<PairingChannel>>> = Arc::new(Mutex::new(None));
        let wait = handle.spawn(wait_for_peer(offer, slot.clone(), self.this.clone()));
        self.with_active(|active| {
            active.phase = PairingPhase::Offering {
                qr_json: qr_json.clone(),
                wait,
                channel: slot,
            };
            Ok(())
        })?;
        self.emit(CoreStatus::Offering);
        Ok(qr_json)
    }

    /// Claims the offer pinned in `qr_payload` (the scanned QR JSON) and
    /// returns the SAS the user must compare with the offerer's display.
    pub fn pair_claim(&self, qr_payload: String) -> Result<String, CoreError> {
        let qr = QrPayload::parse(&qr_payload)
            .map_err(|error| CoreError::InvalidInput(error.to_string()))?;
        let store = Arc::new(self.pairing_store()?);
        let handle = self.begin_phase_transition()?;
        let channel = block_on_io(&handle, async move {
            PairingClient::claim(&store, &qr)
                .await
                .map_err(CoreError::from_transport)
        });
        let channel = match channel {
            Ok(channel) => channel,
            Err(error) => {
                self.abort_phase_transition(&error);
                return Err(error);
            }
        };
        let sas = channel.pending().sas();
        let slot: Arc<Mutex<Option<PairingChannel>>> = Arc::new(Mutex::new(Some(channel)));
        self.with_active(|active| {
            active.phase = PairingPhase::SasReady {
                sas: sas.clone(),
                channel: slot,
            };
            Ok(())
        })?;
        self.emit(CoreStatus::SasReady);
        Ok(sas)
    }

    /// Point-in-time pairing state snapshot.
    pub fn pair_poll(&self) -> PairingSnapshot {
        let Ok(guard) = self.lock_state() else {
            return PairingSnapshot::Unpaired;
        };
        let HandleState::Active(active) = &*guard else {
            return PairingSnapshot::Unpaired;
        };
        match &active.phase {
            PairingPhase::Unpaired | PairingPhase::Busy => PairingSnapshot::Unpaired,
            PairingPhase::Offering { qr_json, .. } => PairingSnapshot::Offering {
                qr_json: qr_json.clone(),
            },
            PairingPhase::SasReady { sas, .. } => PairingSnapshot::SasReady { sas: sas.clone() },
            PairingPhase::Paired { record } => PairingSnapshot::Paired {
                room_id: record.room_id.clone(),
            },
        }
    }

    /// Confirms the user-verified SAS, durably records the pairing, joins the
    /// relay, and starts the live session.
    pub fn pair_confirm(&self, sas: String) -> Result<(), CoreError> {
        let (handle, slot) = self.with_active(|active| {
            let phase = std::mem::replace(&mut active.phase, PairingPhase::Busy);
            match phase {
                PairingPhase::SasReady { channel, .. } => {
                    Ok((active.runtime.handle().clone(), channel))
                }
                other => {
                    active.phase = other;
                    Err(CoreError::Busy)
                }
            }
        })?;
        let channel = lock(&slot).take().ok_or(CoreError::Busy)?;
        let store = Arc::new(self.pairing_store()?);
        let link = block_on_io(&handle, async move {
            channel
                .confirm_pair(&sas, &store)
                .await
                .map_err(CoreError::from_transport)
        });
        match link {
            Ok(link) => self.activate_connection(link),
            Err(error) => {
                self.abort_phase_transition(&error);
                Err(error)
            }
        }
    }

    /// Cancels an in-flight pairing offer or pending SAS confirmation. A
    /// durable pairing is never touched.
    pub fn pair_cancel(&self) -> Result<(), CoreError> {
        let cancelled = self.with_active(|active| {
            let phase = std::mem::replace(&mut active.phase, PairingPhase::Unpaired);
            match phase {
                PairingPhase::Offering { wait, .. } => {
                    wait.abort();
                    Ok(true)
                }
                PairingPhase::SasReady { .. } => Ok(true),
                other => {
                    active.phase = other;
                    Ok(false)
                }
            }
        })?;
        if cancelled {
            self.emit(CoreStatus::ReadyUnpaired);
        }
        Ok(())
    }

    /// Loads the durable pairing record (app restart path). Returns `true`
    /// when a record exists; `start()` then connects the live session.
    pub fn pair_load(&self) -> Result<bool, CoreError> {
        let record = self.pairing_store()?.load_pairing().map_err(CoreError::from_pairing)?;
        self.with_active(|active| {
            match (&active.phase, record) {
                (PairingPhase::Paired { .. }, _) => Ok(true),
                (PairingPhase::Unpaired, Some(record)) => {
                    active.phase = PairingPhase::Paired { record };
                    Ok(true)
                }
                (PairingPhase::Unpaired, None) => Ok(false),
                (_, Some(_)) => Err(CoreError::Busy),
                (_, None) => Ok(false),
            }
        })
    }

    /// Joins the relay with the durable pairing record and starts the live
    /// session (app restart path; pairing flows connect automatically).
    pub fn start(&self) -> Result<(), CoreError> {
        self.emit(CoreStatus::Connecting);
        let (handle, record) = self.with_active(|active| match &active.phase {
            PairingPhase::Paired { record } => {
                let record = record.clone();
                active.phase = PairingPhase::Busy;
                Ok((active.runtime.handle().clone(), record))
            }
            PairingPhase::Unpaired => Err(CoreError::NotPaired),
            _ => Err(CoreError::Busy),
        })?;
        let store = Arc::new(self.pairing_store()?);
        let link = block_on_io(&handle, async move {
            PairingClient::join(&store)
                .await
                .map_err(CoreError::from_transport)
        });
        match link {
            Ok(link) => self.activate_connection(link),
            Err(error) => {
                self.with_active(|active| {
                    active.phase = PairingPhase::Paired { record };
                    Ok(())
                })?;
                self.emit(CoreStatus::Error {
                    message: error.to_string(),
                });
                Err(error)
            }
        }
    }

    /// Queues a text clip for the peer; returns its sequence number once the
    /// frame is written to the relay. Full queue → typed `QueueFull`.
    pub fn send_text(&self, text: String) -> Result<u64, CoreError> {
        self.enqueue_send(ClipContent::Text(text))
    }

    /// Queues an image clip (encoded bytes, ≤10 MiB) for the peer.
    pub fn send_image(&self, bytes: Vec<u8>) -> Result<u64, CoreError> {
        self.enqueue_send(ClipContent::Image(bytes))
    }

    /// Snapshot of the local history store (most recent last).
    pub fn history(&self) -> Result<Vec<FfiHistoryItem>, CoreError> {
        let (reply, rx) = std::sync::mpsc::channel();
        self.try_command(CoreCommand::History { reply })?;
        rx.recv()
            .map_err(|_| CoreError::Internal("dispatcher stopped".to_owned()))?
    }

    /// Raw encoded bytes of an image history item.
    pub fn history_image_bytes(&self, id: String) -> Result<Vec<u8>, CoreError> {
        let id = Uuid::parse_str(&id).map_err(|error| CoreError::InvalidInput(error.to_string()))?;
        let (reply, rx) = std::sync::mpsc::channel();
        self.try_command(CoreCommand::HistoryImage { id, reply })?;
        rx.recv()
            .map_err(|_| CoreError::Internal("dispatcher stopped".to_owned()))?
    }

    /// True when `hash` (see the shared `text_hash` contract) matches a
    /// recently sent or applied text clip; false when no session is live.
    pub fn is_echo(&self, hash: String) -> bool {
        let (reply, rx) = std::sync::mpsc::channel();
        if self.try_command(CoreCommand::IsEcho { hash, reply }).is_err() {
            return false;
        }
        rx.recv().unwrap_or(false)
    }

    /// Stops and joins the session and dispatcher, runs the T5 generation
    /// reset, then installs a fresh empty dispatcher/runtime. On success the
    /// handle is `ReadyUnpaired` and can immediately pair again.
    pub fn reset_pairing(&self) -> Result<(), CoreError> {
        let active = self.take_active()?;
        let stop_result = self.stop_active(active);
        let reset_result = self
            .pairing_store()
            .and_then(|store| reset_pairing_state_after_quiesce(&store).map_err(CoreError::from_pairing));
        // Whatever happened, leave the handle usable: a fresh empty dispatcher
        // returns the host to ReadyUnpaired instead of a wedged state.
        match Self::spawn_active(self.queue_capacity, &self.callbacks) {
            Ok(fresh) => {
                if let Ok(mut guard) = self.lock_state() {
                    *guard = HandleState::Active(Box::new(fresh));
                }
                self.emit(CoreStatus::ReadyUnpaired);
            }
            Err(error) => {
                if let Ok(mut guard) = self.lock_state() {
                    *guard = HandleState::Shutdown;
                }
                return Err(error);
            }
        }
        stop_result.and(reset_result.map(|_| ()))
    }

    /// Stops and joins the session and dispatcher, then shuts the runtime
    /// down. The handle is terminal afterwards; every call fails `Shutdown`.
    pub fn shutdown(&self) -> Result<(), CoreError> {
        let active = self.take_active()?;
        self.stop_active(active)?;
        if let Ok(mut guard) = self.lock_state() {
            *guard = HandleState::Shutdown;
        }
        Ok(())
    }
}

impl CoreHandle {
    fn build(
        data_dir: String,
        callbacks: Box<dyn CoreCallbacks>,
        queue_capacity: usize,
    ) -> Result<Arc<Self>, CoreError> {
        let callbacks: Arc<dyn CoreCallbacks> = callbacks.into();
        let root = PathBuf::from(data_dir);
        std::fs::create_dir_all(&root)
            .map_err(|error| CoreError::Internal(format!("data dir is not writable: {error}")))?;
        let handle = Arc::new_cyclic(|this| Self {
            root,
            callbacks: callbacks.clone(),
            queue_capacity: queue_capacity.max(1),
            this: this.clone(),
            state: Mutex::new(HandleState::Transitioning),
        });
        let active = Self::spawn_active(handle.queue_capacity, &handle.callbacks)?;
        *handle.lock_state()? = HandleState::Active(Box::new(active));
        Ok(handle)
    }

    fn spawn_active(
        queue_capacity: usize,
        callbacks: &Arc<dyn CoreCallbacks>,
    ) -> Result<Active, CoreError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("clipsync-core")
            .build()
            .map_err(|error| CoreError::Internal(format!("runtime failed to start: {error}")))?;
        let (cmd_tx, cmd_rx) = mpsc::channel(queue_capacity.max(1));
        let (joined_tx, joined) = std::sync::mpsc::channel();
        let stopped = Arc::new(AtomicBool::new(false));
        let dispatcher = runtime.spawn(run_dispatcher(
            cmd_rx,
            callbacks.clone(),
            joined_tx,
            stopped.clone(),
        ));
        Ok(Active {
            runtime,
            cmd_tx,
            dispatcher,
            joined,
            stopped,
            phase: PairingPhase::Unpaired,
        })
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, HandleState>, CoreError> {
        Ok(self.state.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn with_active<R>(
        &self,
        f: impl FnOnce(&mut Active) -> Result<R, CoreError>,
    ) -> Result<R, CoreError> {
        let mut guard = self.lock_state()?;
        match &mut *guard {
            HandleState::Active(active) => f(active),
            HandleState::Transitioning => Err(CoreError::Busy),
            HandleState::Shutdown => Err(CoreError::Shutdown),
        }
    }

    fn take_active(&self) -> Result<Active, CoreError> {
        let mut guard = self.lock_state()?;
        match std::mem::replace(&mut *guard, HandleState::Transitioning) {
            HandleState::Active(active) => Ok(*active),
            HandleState::Transitioning => Err(CoreError::Busy),
            HandleState::Shutdown => {
                *guard = HandleState::Shutdown;
                Err(CoreError::Shutdown)
            }
        }
    }

    fn begin_phase_transition(&self) -> Result<Handle, CoreError> {
        self.with_active(|active| {
            if !matches!(active.phase, PairingPhase::Unpaired) {
                return Err(CoreError::Busy);
            }
            active.phase = PairingPhase::Busy;
            Ok(active.runtime.handle().clone())
        })
    }

    fn abort_phase_transition(&self, error: &CoreError) {
        if let Ok(mut guard) = self.lock_state()
            && let HandleState::Active(active) = &mut *guard
            && matches!(active.phase, PairingPhase::Busy)
        {
            active.phase = PairingPhase::Unpaired;
        }
        self.emit(CoreStatus::Error {
            message: error.to_string(),
        });
    }

    fn pairing_store(&self) -> Result<PairingStore, CoreError> {
        PairingStore::new(self.root.join("pairing")).map_err(CoreError::from_pairing)
    }

    fn build_connection(&self, link: LiveLink) -> Result<Connection, CoreError> {
        let store = Arc::new(self.pairing_store()?);
        let identity = store.load_identity().map_err(CoreError::from_pairing)?;
        let session_store = SessionStore::new(self.root.join("session")).map_err(CoreError::from_session)?;
        let history = HistoryStore::new(self.root.join("history")).map_err(CoreError::from_history)?;
        let callback: Arc<dyn SessionCallback> = Arc::new(FfiSessionCallback {
            callbacks: self.callbacks.clone(),
        });
        let session = Session::new(&identity, link.record(), session_store, history, callback)
            .map_err(CoreError::from_session)?;
        Ok(Connection {
            link,
            session,
            store,
        })
    }

    fn activate_connection(&self, link: LiveLink) -> Result<(), CoreError> {
        let record = link.record().clone();
        let connection = self.build_connection(link)?;
        let (reply, rx) = std::sync::mpsc::channel();
        self.try_command(CoreCommand::Connect {
            connection: Box::new(connection),
            reply,
        })?;
        rx.recv_timeout(JOIN_TIMEOUT).map_err(|_| {
            CoreError::Internal("dispatcher did not accept the connection".to_owned())
        })?;
        self.with_active(|active| {
            active.phase = PairingPhase::Paired { record };
            Ok(())
        })?;
        Ok(())
    }

    fn enqueue_send(&self, content: ClipContent) -> Result<u64, CoreError> {
        let (reply, rx) = std::sync::mpsc::channel();
        self.try_command(CoreCommand::Send {
            content,
            ts_ms: now_ms(),
            reply,
        })?;
        rx.recv()
            .map_err(|_| CoreError::Internal("dispatcher stopped".to_owned()))?
    }

    fn try_command(&self, command: CoreCommand) -> Result<(), CoreError> {
        let cmd_tx = self.with_active(|active| Ok(active.cmd_tx.clone()))?;
        cmd_tx.try_send(command).map_err(|error| match error {
            TrySendError::Full(_) => CoreError::QueueFull,
            TrySendError::Closed(_) => CoreError::Shutdown,
        })
    }

    /// Stops the session and dispatcher of an extracted [`Active`] and joins
    /// both: the session link is closed cooperatively, the command queue
    /// drains, the dispatcher task exits, and the runtime shuts down. Uses
    /// only blocking std synchronization, so it is safe to call from a
    /// background executor while a callback awaits the host main actor.
    fn stop_active(&self, active: Active) -> Result<(), CoreError> {
        let Active {
            runtime,
            cmd_tx,
            dispatcher,
            joined,
            stopped,
            phase,
        } = active;
        if let PairingPhase::Offering { wait, .. } = &phase {
            wait.abort();
        }
        drop(phase);
        let (reply, rx) = std::sync::mpsc::channel();
        let mut disconnect_sent = false;
        for _ in 0..100 {
            match cmd_tx.try_send(CoreCommand::Disconnect {
                reply: reply.clone(),
            }) {
                Ok(()) => {
                    disconnect_sent = true;
                    break;
                }
                Err(TrySendError::Full(_)) => std::thread::sleep(Duration::from_millis(100)),
                Err(TrySendError::Closed(_)) => break,
            }
        }
        if disconnect_sent {
            let _ = rx.recv_timeout(JOIN_TIMEOUT);
        }
        drop(cmd_tx);
        joined.recv_timeout(JOIN_TIMEOUT).map_err(|_| {
            CoreError::Internal("dispatcher did not stop within the join timeout".to_owned())
        })?;
        if !stopped.load(Ordering::SeqCst) {
            return Err(CoreError::Internal(
                "dispatcher exited without signalling completion".to_owned(),
            ));
        }
        drop(dispatcher);
        runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);
        Ok(())
    }

    fn emit(&self, status: CoreStatus) {
        emit_status(&self.callbacks, status);
    }

    #[doc(hidden)]
    pub fn with_queue_capacity(
        data_dir: String,
        callbacks: Box<dyn CoreCallbacks>,
        queue_capacity: usize,
    ) -> Result<Arc<Self>, CoreError> {
        Self::build(data_dir, callbacks, queue_capacity)
    }

    /// Enqueues without waiting for processing; used to prove FIFO ordering
    /// and typed backpressure deterministically.
    #[doc(hidden)]
    pub fn try_send_text_for_test(&self, text: String) -> Result<(), CoreError> {
        let (reply, _rx) = std::sync::mpsc::channel();
        self.try_command(CoreCommand::Send {
            content: ClipContent::Text(text),
            ts_ms: now_ms(),
            reply,
        })
    }

    /// Parks the dispatcher on a blocking receive until the returned guard
    /// drops, giving tests a deterministic full-queue window.
    #[doc(hidden)]
    pub fn stall_dispatcher_for_test(&self) -> Result<StallGuard, CoreError> {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        self.try_command(CoreCommand::Stall {
            entered: entered_tx,
            release: release_rx,
        })?;
        entered_rx
            .recv_timeout(JOIN_TIMEOUT)
            .map_err(|_| CoreError::Internal("dispatcher did not stall".to_owned()))?;
        Ok(StallGuard {
            release: Some(release_tx),
        })
    }

    #[doc(hidden)]
    pub fn identity_generation_for_test(&self) -> Result<u64, CoreError> {
        Ok(self
            .pairing_store()?
            .load_identity()
            .map_err(CoreError::from_pairing)?
            .generation())
    }
}

/// Releases a stalled dispatcher on drop.
#[doc(hidden)]
pub struct StallGuard {
    release: Option<std::sync::mpsc::Sender<()>>,
}

impl Drop for StallGuard {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }
}

async fn wait_for_peer(
    offer: crate::pairing_client::PairingOffer,
    slot: Arc<Mutex<Option<PairingChannel>>>,
    this: Weak<CoreHandle>,
) {
    match offer.wait_peer().await {
        Ok(channel) => {
            let sas = channel.pending().sas();
            *lock(&slot) = Some(channel);
            let Some(me) = this.upgrade() else {
                return;
            };
            let announced = me
                .with_active(|active| {
                    if matches!(&active.phase, PairingPhase::Offering { channel, .. } if Arc::ptr_eq(channel, &slot))
                    {
                        active.phase = PairingPhase::SasReady {
                            sas: sas.clone(),
                            channel: slot.clone(),
                        };
                    }
                    Ok(())
                })
                .is_ok();
            if announced {
                me.emit(CoreStatus::SasReady);
            }
        }
        Err(error) => {
            let Some(me) = this.upgrade() else {
                return;
            };
            // Report only when the offer is still ours; a cancel/reset swap
            // means the closure is expected and stays silent.
            let still_offering = me
                .with_active(|active| {
                    if matches!(&active.phase, PairingPhase::Offering { channel, .. } if Arc::ptr_eq(channel, &slot))
                    {
                        active.phase = PairingPhase::Unpaired;
                        return Ok(true);
                    }
                    Ok(false)
                })
                .unwrap_or(false);
            if still_offering {
                me.emit(CoreStatus::Error {
                    message: error.to_string(),
                });
            }
        }
    }
}

async fn run_dispatcher(
    mut rx: mpsc::Receiver<CoreCommand>,
    callbacks: Arc<dyn CoreCallbacks>,
    joined: std::sync::mpsc::Sender<()>,
    stopped: Arc<AtomicBool>,
) {
    let mut connection: Option<Connection> = None;
    loop {
        tokio::select! {
            command = rx.recv() => {
                let Some(command) = command else { break };
                if matches!(command, CoreCommand::Disconnect { .. }) {
                    let CoreCommand::Disconnect { reply } = command else {
                        unreachable!();
                    };
                    if let Some(active) = connection.take() {
                        close_connection(active, &callbacks).await;
                    }
                    let _ = reply.send(());
                    continue;
                }
                handle_command(command, &mut connection, &callbacks).await;
            }
            event = async {
                let active = connection.as_mut().expect("polled only while connected");
                active.link.recv_reconnecting(&active.store).await
            }, if connection.is_some() => {
                match event {
                    Ok(event) => {
                        let active = connection.as_mut().expect("polled only while connected");
                        handle_live_event(event, active, &callbacks);
                    }
                    Err(error) => {
                        eprintln!("live session terminated: {error}");
                        if let Some(active) = connection.take() {
                            close_connection(active, &callbacks).await;
                        }
                        emit_status(&callbacks, CoreStatus::Disconnected);
                    }
                }
            }
        }
    }
    stopped.store(true, Ordering::SeqCst);
    let _ = joined.send(());
}

async fn handle_command(
    command: CoreCommand,
    connection: &mut Option<Connection>,
    callbacks: &Arc<dyn CoreCallbacks>,
) {
    match command {
        CoreCommand::Send {
            content,
            ts_ms,
            reply,
        } => {
            let result = match connection.as_mut() {
                Some(active) => send_over_connection(active, content, ts_ms, callbacks).await,
                None => Err(CoreError::NotPaired),
            };
            let _ = reply.send(result);
        }
        CoreCommand::History { reply } => {
            let result = match connection.as_mut() {
                Some(active) => Ok(active
                    .session
                    .history()
                    .list()
                    .iter()
                    .map(ffi_history_item)
                    .collect()),
                None => Err(CoreError::NotPaired),
            };
            let _ = reply.send(result);
        }
        CoreCommand::HistoryImage { id, reply } => {
            let result = match connection.as_mut() {
                Some(active) => active
                    .session
                    .history()
                    .image_bytes(id)
                    .map_err(|error| CoreError::Internal(error.to_string())),
                None => Err(CoreError::NotPaired),
            };
            let _ = reply.send(result);
        }
        CoreCommand::IsEcho { hash, reply } => {
            let echo = connection
                .as_ref()
                .is_some_and(|active| active.session.is_echo(&hash));
            let _ = reply.send(echo);
        }
        CoreCommand::Connect {
            connection: mut fresh,
            reply,
        } => {
            if let Some(previous) = connection.take() {
                close_connection(previous, callbacks).await;
            }
            // The join bootstrap is consumed exactly once, as a mailbox clip,
            // before the host observes Connected.
            if let Some((room_id, ciphertext_b64)) = fresh.link.bootstrap_clip() {
                handle_inbound_clip(&mut fresh, &room_id, &ciphertext_b64, true, callbacks);
            }
            *connection = Some(*fresh);
            let _ = reply.send(());
            emit_status(callbacks, CoreStatus::Connected);
        }
        CoreCommand::Disconnect { .. } => unreachable!("handled in the dispatcher loop"),
        CoreCommand::Stall { entered, release } => {
            let _ = entered.send(());
            let _ = release.recv();
        }
    }
}

async fn send_over_connection(
    connection: &mut Connection,
    content: ClipContent,
    ts_ms: i64,
    callbacks: &Arc<dyn CoreCallbacks>,
) -> Result<u64, CoreError> {
    let seq = connection.session.next_seq();
    let frame = connection
        .session
        .send_clip(content, ts_ms)
        .map_err(CoreError::from_session)?;
    match connection.link.send(&frame).await {
        Ok(()) => Ok(seq),
        Err(TransportError::Closed | TransportError::WebSocket(_)) => {
            emit_status(callbacks, CoreStatus::Reconnecting);
            connection
                .link
                .reconnect(&connection.store)
                .await
                .map_err(CoreError::from_transport)?;
            if let Some((room_id, ciphertext_b64)) = connection.link.bootstrap_clip() {
                handle_inbound_clip(connection, &room_id, &ciphertext_b64, true, callbacks);
            }
            emit_status(callbacks, CoreStatus::Connected);
            connection
                .link
                .send(&frame)
                .await
                .map_err(CoreError::from_transport)?;
            Ok(seq)
        }
        Err(error) => Err(CoreError::from_transport(error)),
    }
}

fn handle_live_event(
    event: LiveEvent,
    connection: &mut Connection,
    callbacks: &Arc<dyn CoreCallbacks>,
) {
    match event {
        LiveEvent::Frame(crate::protocol::Frame::Clip {
            room_id,
            ciphertext_b64,
            mailbox,
            ..
        }) => handle_inbound_clip(connection, &room_id, &ciphertext_b64, mailbox, callbacks),
        LiveEvent::Frame(frame) => {
            eprintln!("unexpected live frame dropped: {frame:?}");
        }
        LiveEvent::Reconnected { bootstrap_clip } => {
            emit_status(callbacks, CoreStatus::Connected);
            if let Some((room_id, ciphertext_b64)) = bootstrap_clip {
                handle_inbound_clip(connection, &room_id, &ciphertext_b64, true, callbacks);
            }
        }
    }
}

/// Feeds one authenticated clip into the session. Callback failures are
/// logged and surfaced as `CoreStatus::Error`; the session always survives.
fn handle_inbound_clip(
    connection: &mut Connection,
    room_id: &str,
    ciphertext_b64: &str,
    mailbox: bool,
    callbacks: &Arc<dyn CoreCallbacks>,
) {
    match connection
        .session
        .handle_clip(room_id, ciphertext_b64, mailbox, now_ms())
    {
        Ok(_) => {}
        Err(SessionError::Callback(error)) => {
            eprintln!("clipboard callback failed; session continues: {error}");
            emit_status(
                callbacks,
                CoreStatus::Error {
                    message: error.to_string(),
                },
            );
        }
        Err(error) => {
            eprintln!("inbound clip processing failed; session continues: {error}");
            emit_status(
                callbacks,
                CoreStatus::Error {
                    message: error.to_string(),
                },
            );
        }
    }
}

async fn close_connection(connection: Connection, callbacks: &Arc<dyn CoreCallbacks>) {
    if let Err(error) = connection.link.close().await {
        eprintln!("closing the relay link failed: {error}");
    }
    drop(connection.session);
    emit_status(callbacks, CoreStatus::Disconnected);
}

fn emit_status(callbacks: &Arc<dyn CoreCallbacks>, status: CoreStatus) {
    let _ = catch_unwind(AssertUnwindSafe(|| callbacks.on_status(status)));
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

fn block_on_io<F, T>(handle: &Handle, future: F) -> Result<T, CoreError>
where
    F: std::future::Future<Output = Result<T, CoreError>>,
{
    handle.block_on(async move {
        match tokio::time::timeout(IO_TIMEOUT, future).await {
            Ok(result) => result,
            Err(_) => Err(CoreError::Transport(
                "relay operation timed out".to_owned(),
            )),
        }
    })
}

struct FfiSessionCallback {
    callbacks: Arc<dyn CoreCallbacks>,
}

impl SessionCallback for FfiSessionCallback {
    fn on_clip(&self, item: &ClipItem) -> Result<(), CallbackError> {
        self.callbacks
            .on_clip(ffi_clip_item(item))
            .map_err(|error| CallbackError::Rejected(error.to_string()))
    }

    fn on_mailbox_clip(&self, item: &ClipItem) -> Result<SessionDisposition, CallbackError> {
        let disposition = self
            .callbacks
            .on_mailbox_clip(ffi_clip_item(item))
            .map_err(|error| CallbackError::Rejected(error.to_string()))?;
        Ok(match disposition {
            MailboxDisposition::Applied => SessionDisposition::Applied,
            MailboxDisposition::Deferred => SessionDisposition::Deferred,
        })
    }
}

fn ffi_clip_item(item: &ClipItem) -> FfiClipItem {
    FfiClipItem {
        id: item.id.to_string(),
        ts_ms: item.ts_ms,
        seq: item.seq,
        content: match &item.content {
            ClipContent::Text(text) => FfiClipContent::Text { text: text.clone() },
            ClipContent::Image(bytes) => FfiClipContent::Image {
                bytes: bytes.clone(),
            },
        },
    }
}

fn ffi_history_item(item: &crate::history::HistoryItem) -> FfiHistoryItem {
    FfiHistoryItem {
        id: item.id.to_string(),
        ts_ms: item.ts_ms,
        kind: match &item.kind {
            HistoryKind::Text { content } => FfiHistoryKind::Text {
                content: content.clone(),
            },
            HistoryKind::Image { .. } => FfiHistoryKind::Image,
        },
        source: match item.source {
            HistorySource::Local => FfiHistorySource::Local,
            HistorySource::Remote => FfiHistorySource::Remote,
            HistorySource::RemoteDeferred => FfiHistorySource::RemoteDeferred,
        },
    }
}

impl CoreError {
    fn from_transport(error: TransportError) -> Self {
        match error {
            TransportError::Pairing(PairingError::SasMismatch) => Self::SasMismatch,
            other => Self::Transport(other.to_string()),
        }
    }

    fn from_pairing(error: PairingError) -> Self {
        match error {
            PairingError::SasMismatch => Self::SasMismatch,
            PairingError::InvalidServer | PairingError::InvalidCode => {
                Self::InvalidInput(error.to_string())
            }
            _ => Self::Internal(error.to_string()),
        }
    }

    fn from_session(error: SessionError) -> Self {
        Self::Internal(error.to_string())
    }

    fn from_history(error: crate::history::HistoryError) -> Self {
        Self::Internal(error.to_string())
    }
}
