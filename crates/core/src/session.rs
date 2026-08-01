//! Clipboard session state machine with crash-safe delivery guarantees.
//!
//! Sequence/idempotency contract (normative):
//!
//! - `next_seq` and the receive-side `DeliveryState` are namespaced by
//!   `(room_id, sender_fp, receiver_fp)` and scoped to one identity generation.
//! - The sender atomically persists `next_seq` BEFORE seal/send; a crash may
//!   skip sequence numbers but must never reuse one.
//! - After a successful AEAD open the receiver, in order:
//!   1. transaction: persist `last_seq` + `seen_ids` + `pending_history`;
//!   2. idempotent [`HistoryStore`] add (live = `Remote`, mailbox/stale =
//!      `RemoteDeferred`);
//!   3. second transaction: clear that `pending_history` entry;
//!   4. only then attempt the clipboard callback.
//! - Startup replays `pending_history` into the history only and NEVER re-fires
//!   a callback, so the callback is at-most-once; a crash may leave an item
//!   history-only but never writes the clipboard twice.
//! - Any persistence failure suppresses the callback (fail closed).
//! - `seq <= last_seq` never triggers a callback.
//!
//! Mailbox disposition contract: the shell callback returns
//! [`MailboxDisposition`]; `Applied` promotes the entry via `set_source(Remote)`.
//! If the process crashes after the callback wrote the clipboard but before
//! `set_source`, the entry may stay `RemoteDeferred`; restart must not re-fire
//! the callback and the user can apply the item manually with idempotent
//! `set_source` recovery ([`Session::apply_deferred`]). A callback error is not
//! retried either; history is kept and the error is reported.
//!
//! Core keeps only the text `is_echo(hash)` helper; platform ownership tokens
//! live entirely in the platform shells.
// allow: SIZE_OK - T7 locks the session state machine and crash-window contract to this module.

use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::crypto::{self, CryptoError, Identity, SessionKeys};
use crate::history::{
    HistoryContent, HistoryError, HistoryKind, HistorySource, HistoryStore, NewHistoryItem,
};
use crate::pairing::PairingRecord;
use crate::protocol::{self, ContentKind, Envelope, Frame, PROTOCOL_VERSION, ProtocolError};

/// Maximum number of item ids remembered for inbound deduplication.
pub const SEEN_IDS_RING: usize = 64;
/// A live clip is fresh only within five minutes of its envelope timestamp.
pub const LIVE_FRESH_WINDOW_MS: i64 = 5 * 60 * 1000;
/// Future clock skew tolerated on the live freshness window.
pub const LIVE_FUTURE_SKEW_MS: i64 = 2 * 60 * 1000;
/// In-memory echo hash ring shared by sent and applied text clips.
const ECHO_RING: usize = 64;
const SESSION_FILE: &str = "session.json";

/// Clipboard payload carried inside an encrypted envelope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClipContent {
    Text(String),
    Image(Vec<u8>),
}

/// One authenticated clipboard item handed to the platform shell.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClipItem {
    pub id: Uuid,
    pub ts_ms: i64,
    pub seq: u64,
    pub content: ClipContent,
}

/// Shell decision for a mailbox (or stale-live) clip.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MailboxDisposition {
    Applied,
    Deferred,
}

/// Synchronous clipboard callback surface implemented by the platform shell.
///
/// Implementations must not panic; core still guards every invocation so a
/// panicking callback degrades to [`CallbackError::Panicked`] instead of
/// crashing the session.
pub trait SessionCallback: Send + Sync {
    /// Fresh live clip; core has already durably recorded history.
    fn on_clip(&self, item: &ClipItem) -> Result<(), CallbackError>;
    /// Mailbox or stale clip; core has already durably cleared pending and
    /// recorded `RemoteDeferred` history.
    fn on_mailbox_clip(&self, item: &ClipItem) -> Result<MailboxDisposition, CallbackError>;
}

/// Errors a shell callback may report.
#[derive(Debug, Error)]
pub enum CallbackError {
    #[error("clipboard callback rejected the clip: {0}")]
    Rejected(String),
    #[error("clipboard callback panicked")]
    Panicked,
}

/// Result of handling one inbound authenticated clip frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InboundOutcome {
    /// Fresh live clip: history `Remote` and `on_clip` fired.
    LiveApplied,
    /// Mailbox/stale clip the shell applied: history promoted to `Remote`.
    MailboxApplied,
    /// Mailbox/stale clip the shell deferred: history stays `RemoteDeferred`.
    MailboxDeferred,
    /// Item id already seen; nothing was persisted or called back.
    Duplicate,
    /// Sequence number at or below the persisted high-water mark.
    Replay,
    /// AEAD, decoding, or room binding failed; the frame was dropped.
    Unauthenticated,
}

/// Errors returned by the session boundary.
#[derive(Debug, Error)]
pub enum SessionError {
    #[error("pairing record does not match the presented identity")]
    RecordMismatch,
    #[error("session crypto failed")]
    Crypto(#[from] CryptoError),
    #[error("session protocol failed")]
    Protocol(#[from] ProtocolError),
    #[error("session history failed")]
    History(#[from] HistoryError),
    #[error("session persistence I/O failed")]
    Io(#[from] std::io::Error),
    #[error("session persistence JSON is invalid")]
    Json(#[from] serde_json::Error),
    #[error("clipboard callback failed")]
    Callback(#[from] CallbackError),
    #[doc(hidden)]
    #[error("injected crash fault")]
    InjectedFault,
}

/// A pending-history entry replayed into the history store on restart.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PendingItem {
    pub id: Uuid,
    pub ts_ms: i64,
    pub source: HistorySource,
    pub content: ClipContent,
}

/// Durable per-direction state namespaced by `(room_id, sender_fp, receiver_fp)`.
///
/// `next_seq` is meaningful on the send direction; the remaining fields form
/// the receive-side `DeliveryState`.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DirectionState {
    pub identity_generation: u64,
    pub next_seq: u64,
    pub last_seq: u64,
    pub seen_ids: Vec<String>,
    pub pending_history: Vec<PendingItem>,
}

impl DirectionState {
    fn new(identity_generation: u64) -> Self {
        Self {
            identity_generation,
            next_seq: 1,
            last_seq: 0,
            seen_ids: Vec::new(),
            pending_history: Vec::new(),
        }
    }
}

#[derive(Default, Deserialize, Serialize)]
struct SessionDocument {
    #[serde(default)]
    directions: BTreeMap<String, DirectionState>,
}

/// Filesystem-backed owner of durable per-direction session state.
pub struct SessionStore {
    root: PathBuf,
}

impl SessionStore {
    /// Opens or creates an isolated session state directory.
    ///
    /// # Errors
    /// Returns an I/O error when the directory cannot be created.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, SessionError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    fn load_document(&self) -> Result<SessionDocument, SessionError> {
        match fs::read(self.root.join(SESSION_FILE)) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(SessionDocument::default())
            }
            Err(error) => Err(SessionError::Io(error)),
        }
    }

    /// Atomically persists one direction state, leaving every other direction
    /// untouched. This is the only write path; each call is one transaction.
    fn commit_direction(
        &self,
        key: &str,
        state: &DirectionState,
        extra: Option<(&str, &DirectionState)>,
    ) -> Result<(), SessionError> {
        let mut document = self.load_document()?;
        document.directions.insert(key.to_owned(), state.clone());
        if let Some((extra_key, extra_state)) = extra {
            document
                .directions
                .insert(extra_key.to_owned(), extra_state.clone());
        }
        atomic_write(
            &self.root.join(SESSION_FILE),
            &serde_json::to_vec(&document)?,
        )
    }

    fn load_direction(&self, key: &str, generation: u64) -> Result<DirectionState, SessionError> {
        let document = self.load_document()?;
        match document.directions.get(key) {
            Some(state) if state.identity_generation == generation => Ok(state.clone()),
            _ => Ok(DirectionState::new(generation)),
        }
    }
}

fn direction_key(room_id: &str, sender_fp: &str, receiver_fp: &str) -> String {
    format!("{room_id}:{sender_fp}:{receiver_fp}")
}

/// Crash-window checkpoints for tests. A fault is injected AFTER the named
/// step completes, simulating a process crash in that window.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SendStage {
    /// After `next_seq` was atomically persisted, before seal/send.
    SeqReserved,
}

/// Receive-path crash-window checkpoints for tests.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceiveStage {
    /// After the DeliveryState transaction (step 1), before history add.
    DeliveryCommitted,
    /// After the history add (step 2), before the pending-clear transaction.
    HistoryAdded,
    /// After the pending-clear transaction (step 3), before the callback.
    PendingCleared,
    /// After `on_mailbox_clip` returned `Applied`, before `set_source(Remote)`.
    MailboxApplied,
}

/// One end-to-end encrypted clipboard session with a single paired peer.
pub struct Session {
    room_id: String,
    self_fp: String,
    peer_fp: String,
    keys: SessionKeys,
    store: SessionStore,
    send_key: String,
    recv_key: String,
    send_state: DirectionState,
    recv_state: DirectionState,
    history: HistoryStore,
    callback: Arc<dyn SessionCallback>,
    echo_hashes: VecDeque<String>,
}

impl Session {
    /// Opens a session, replaying any pending history into the store.
    ///
    /// Restart recovery adds `pending_history` entries to the [`HistoryStore`]
    /// idempotently and NEVER fires a clipboard callback, preserving
    /// at-most-once clipboard application across crashes.
    ///
    /// # Errors
    /// Rejects a pairing record that does not match the presented identity and
    /// any persistence or history failure (fail closed).
    pub fn new(
        identity: &Identity,
        record: &PairingRecord,
        store: SessionStore,
        mut history: HistoryStore,
        callback: Arc<dyn SessionCallback>,
    ) -> Result<Self, SessionError> {
        let generation = identity.generation();
        if record.identity_generation != generation
            || record.local_bundle_fp != identity.bundle_fp()
            || record.local_bundle_fp == record.peer_bundle_fp
        {
            return Err(SessionError::RecordMismatch);
        }
        let keys = identity.session_keys(&record.peer_bundle)?;
        let self_fp = record.local_bundle_fp.clone();
        let peer_fp = record.peer_bundle_fp.clone();
        let send_key = direction_key(&record.room_id, &self_fp, &peer_fp);
        let recv_key = direction_key(&record.room_id, &peer_fp, &self_fp);
        let send_state = store.load_direction(&send_key, generation)?;
        let mut recv_state = store.load_direction(&recv_key, generation)?;

        if !recv_state.pending_history.is_empty() {
            for pending in &recv_state.pending_history {
                history.add(NewHistoryItem {
                    id: pending.id,
                    ts_ms: pending.ts_ms,
                    source: pending.source.clone(),
                    content: history_content(&pending.content),
                })?;
            }
            recv_state.pending_history.clear();
            store.commit_direction(&recv_key, &recv_state, None)?;
        }

        Ok(Self {
            room_id: record.room_id.clone(),
            self_fp,
            peer_fp,
            keys,
            store,
            send_key,
            recv_key,
            send_state,
            recv_state,
            history,
            callback,
            echo_hashes: VecDeque::new(),
        })
    }

    #[must_use]
    pub fn room_id(&self) -> &str {
        &self.room_id
    }

    /// Next sequence number the send direction will allocate.
    #[must_use]
    pub fn next_seq(&self) -> u64 {
        self.send_state.next_seq
    }

    /// Persisted receive high-water mark; `seq <= last_seq` never callbacks.
    #[must_use]
    pub fn last_seq(&self) -> u64 {
        self.recv_state.last_seq
    }

    #[must_use]
    pub fn history(&self) -> &HistoryStore {
        &self.history
    }

    /// Returns true when `hash` matches a recently sent or applied text clip.
    #[must_use]
    pub fn is_echo(&self, hash: &str) -> bool {
        self.echo_hashes.iter().any(|known| known == hash)
    }

    /// Reserves a sequence number, seals an envelope, and builds the wire frame.
    ///
    /// The `next_seq` reservation is atomically persisted BEFORE sealing so a
    /// crash may skip but never reuse a sequence number.
    ///
    /// # Errors
    /// Returns persistence, protocol, crypto, or history errors. A failed
    /// reservation leaves the sequence untouched; later failures consume it.
    pub fn send_clip(&mut self, content: ClipContent, ts_ms: i64) -> Result<Frame, SessionError> {
        self.send_clip_with_fault(content, ts_ms, None)
    }

    #[doc(hidden)]
    pub fn send_clip_with_fault(
        &mut self,
        content: ClipContent,
        ts_ms: i64,
        fail_at: Option<SendStage>,
    ) -> Result<Frame, SessionError> {
        let seq = self.send_state.next_seq;
        let mut reserved = self.send_state.clone();
        reserved.next_seq = seq.checked_add(1).ok_or(SessionError::RecordMismatch)?;
        self.store
            .commit_direction(&self.send_key, &reserved, None)?;
        self.send_state = reserved;
        if fail_at == Some(SendStage::SeqReserved) {
            return Err(SessionError::InjectedFault);
        }

        let (kind, raw) = match &content {
            ClipContent::Text(text) => (ContentKind::Text, text.clone().into_bytes()),
            ClipContent::Image(bytes) => (ContentKind::Image, bytes.clone()),
        };
        let item_id = Uuid::new_v4();
        let envelope = Envelope {
            v: PROTOCOL_VERSION,
            kind,
            item_id: item_id.to_string(),
            seq,
            ts_ms,
            content_b64: STANDARD.encode(raw),
        };
        let encoded = protocol::encode_envelope(&envelope)?;
        let associated = crypto::aad(&self.room_id, &self.self_fp, &self.peer_fp);
        let sealed = crypto::seal(&self.keys.send, &associated, &encoded)?;

        self.history.add(NewHistoryItem {
            id: item_id,
            ts_ms,
            source: HistorySource::Local,
            content: history_content(&content),
        })?;
        if let ClipContent::Text(text) = &content {
            self.register_echo(&text_hash(text));
        }

        Ok(Frame::Clip {
            room_id: self.room_id.clone(),
            ciphertext_b64: STANDARD.encode(sealed),
            origin_device: String::new(),
            mailbox: false,
        })
    }

    /// Handles one inbound clip frame according to the receive contract.
    ///
    /// Unauthenticated frames (wrong room, AEAD failure, malformed envelope)
    /// are dropped without any state change or callback.
    ///
    /// # Errors
    /// Returns persistence failures (callback suppressed, fail closed) and
    /// callback errors (history kept, never retried).
    pub fn handle_clip(
        &mut self,
        room_id: &str,
        ciphertext_b64: &str,
        mailbox: bool,
        now_ms: i64,
    ) -> Result<InboundOutcome, SessionError> {
        self.handle_clip_with_fault(room_id, ciphertext_b64, mailbox, now_ms, None)
    }

    #[doc(hidden)]
    pub fn handle_clip_with_fault(
        &mut self,
        room_id: &str,
        ciphertext_b64: &str,
        mailbox: bool,
        now_ms: i64,
        fail_at: Option<ReceiveStage>,
    ) -> Result<InboundOutcome, SessionError> {
        let Some(item) = self.authenticate(room_id, ciphertext_b64) else {
            return Ok(InboundOutcome::Unauthenticated);
        };
        if self
            .recv_state
            .seen_ids
            .iter()
            .any(|id| id == &item.id.to_string())
        {
            return Ok(InboundOutcome::Duplicate);
        }
        if item.seq <= self.recv_state.last_seq {
            return Ok(InboundOutcome::Replay);
        }

        let fresh = !mailbox
            && item.ts_ms >= now_ms - LIVE_FRESH_WINDOW_MS
            && item.ts_ms <= now_ms + LIVE_FUTURE_SKEW_MS;
        let source = if fresh {
            HistorySource::Remote
        } else {
            HistorySource::RemoteDeferred
        };

        // Step 1: transaction persisting last_seq + seen_ids + pending_history.
        let mut committed = self.recv_state.clone();
        committed.last_seq = committed.last_seq.max(item.seq);
        committed.seen_ids.push(item.id.to_string());
        if committed.seen_ids.len() > SEEN_IDS_RING {
            committed
                .seen_ids
                .drain(..committed.seen_ids.len() - SEEN_IDS_RING);
        }
        committed.pending_history.push(PendingItem {
            id: item.id,
            ts_ms: item.ts_ms,
            source: source.clone(),
            content: item.content.clone(),
        });
        self.store
            .commit_direction(&self.recv_key, &committed, None)?;
        self.recv_state = committed;
        if fail_at == Some(ReceiveStage::DeliveryCommitted) {
            return Err(SessionError::InjectedFault);
        }

        // Step 2: idempotent history add.
        self.history.add(NewHistoryItem {
            id: item.id,
            ts_ms: item.ts_ms,
            source: source.clone(),
            content: history_content(&item.content),
        })?;
        if fail_at == Some(ReceiveStage::HistoryAdded) {
            return Err(SessionError::InjectedFault);
        }

        // Step 3: second transaction clearing the pending entry.
        let mut cleared = self.recv_state.clone();
        cleared
            .pending_history
            .retain(|pending| pending.id != item.id);
        self.store
            .commit_direction(&self.recv_key, &cleared, None)?;
        self.recv_state = cleared;
        if fail_at == Some(ReceiveStage::PendingCleared) {
            return Err(SessionError::InjectedFault);
        }

        // Step 4: only now may the clipboard callback run.
        if fresh {
            invoke_callback(&self.callback, &item, false).map_err(SessionError::Callback)?;
            if let ClipContent::Text(text) = &item.content {
                self.register_echo(&text_hash(text));
            }
            Ok(InboundOutcome::LiveApplied)
        } else {
            let disposition = match invoke_callback(&self.callback, &item, true) {
                Ok(disposition) => disposition,
                Err(error) => return Err(SessionError::Callback(error)),
            };
            match disposition {
                MailboxDisposition::Applied => {
                    if fail_at == Some(ReceiveStage::MailboxApplied) {
                        return Err(SessionError::InjectedFault);
                    }
                    self.history.set_source(item.id, HistorySource::Remote)?;
                    if let ClipContent::Text(text) = &item.content {
                        self.register_echo(&text_hash(text));
                    }
                    Ok(InboundOutcome::MailboxApplied)
                }
                MailboxDisposition::Deferred => Ok(InboundOutcome::MailboxDeferred),
            }
        }
    }

    /// Manually applies a deferred history item, idempotently promoting its
    /// source to `Remote`. This is the crash-recovery and user-initiated path.
    ///
    /// # Errors
    /// Returns history errors for unknown items or persistence failures.
    pub fn apply_deferred(&mut self, id: Uuid) -> Result<(), SessionError> {
        self.history.set_source(id, HistorySource::Remote)?;
        if let Some(item) = self.history.list().iter().find(|item| item.id == id)
            && let HistoryKind::Text { content } = &item.kind
        {
            self.register_echo(&text_hash(content));
        }
        Ok(())
    }

    fn authenticate(&self, room_id: &str, ciphertext_b64: &str) -> Option<ClipItem> {
        if room_id != self.room_id {
            return None;
        }
        let sealed = STANDARD.decode(ciphertext_b64).ok()?;
        let associated = crypto::aad(&self.room_id, &self.peer_fp, &self.self_fp);
        let plaintext = crypto::open(&self.keys.recv, &associated, &sealed).ok()?;
        let envelope = protocol::decode_envelope(&plaintext).ok()?;
        let id = Uuid::parse_str(&envelope.item_id).ok()?;
        let content = match envelope.kind {
            ContentKind::Text => {
                let raw = STANDARD.decode(envelope.content_b64).ok()?;
                ClipContent::Text(String::from_utf8(raw).ok()?)
            }
            ContentKind::Image => ClipContent::Image(STANDARD.decode(envelope.content_b64).ok()?),
        };
        Some(ClipItem {
            id,
            ts_ms: envelope.ts_ms,
            seq: envelope.seq,
            content,
        })
    }

    fn register_echo(&mut self, hash: &str) {
        if self.echo_hashes.iter().any(|known| known == hash) {
            return;
        }
        self.echo_hashes.push_back(hash.to_owned());
        while self.echo_hashes.len() > ECHO_RING {
            self.echo_hashes.pop_front();
        }
    }
}

fn invoke_callback(
    callback: &Arc<dyn SessionCallback>,
    item: &ClipItem,
    mailbox: bool,
) -> Result<MailboxDisposition, CallbackError> {
    let result = catch_unwind(AssertUnwindSafe(|| {
        if mailbox {
            callback.on_mailbox_clip(item)
        } else {
            callback.on_clip(item).map(|()| MailboxDisposition::Applied)
        }
    }));
    match result {
        Ok(outcome) => outcome,
        Err(_) => Err(CallbackError::Panicked),
    }
}

/// Returns the canonical text echo hash shared with the platform shells.
#[must_use]
pub fn text_hash(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        encoded.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn history_content(content: &ClipContent) -> HistoryContent {
    match content {
        ClipContent::Text(text) => HistoryContent::Text {
            content: text.clone(),
        },
        ClipContent::Image(bytes) => HistoryContent::Image {
            bytes: bytes.clone(),
        },
    }
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), SessionError> {
    let parent = path.parent().ok_or(SessionError::RecordMismatch)?;
    let temp_path = parent.join(format!(".session.json.{}.tmp", Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut pending = PendingFile::new(temp_path, options)?;
    pending.file.write_all(contents)?;
    pending.file.sync_all()?;
    fs::rename(&pending.path, path)?;
    pending.commit();
    File::open(parent)?.sync_all()?;
    Ok(())
}

struct PendingFile {
    path: PathBuf,
    file: File,
    committed: bool,
}

impl PendingFile {
    fn new(path: PathBuf, options: OpenOptions) -> Result<Self, SessionError> {
        let file = options.open(&path)?;
        Ok(Self {
            path,
            file,
            committed: false,
        })
    }

    const fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for PendingFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

impl Serialize for ClipContent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Text(content) => {
                #[derive(Serialize)]
                struct Text<'a> {
                    r#type: &'static str,
                    content: &'a str,
                }
                Text {
                    r#type: "text",
                    content,
                }
                .serialize(serializer)
            }
            Self::Image(bytes) => {
                #[derive(Serialize)]
                struct Image {
                    r#type: &'static str,
                    bytes_b64: String,
                }
                Image {
                    r#type: "image",
                    bytes_b64: STANDARD.encode(bytes),
                }
                .serialize(serializer)
            }
        }
    }
}

impl<'de> Deserialize<'de> for ClipContent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
        enum Helper {
            Text { content: String },
            Image { bytes_b64: String },
        }
        match Helper::deserialize(deserializer)? {
            Helper::Text { content } => Ok(Self::Text(content)),
            Helper::Image { bytes_b64 } => {
                let decoded = STANDARD.decode(&bytes_b64).map_err(de::Error::custom)?;
                if STANDARD.encode(&decoded) != bytes_b64 {
                    return Err(de::Error::custom(
                        "image bytes must use canonical padded STANDARD base64",
                    ));
                }
                Ok(Self::Image(decoded))
            }
        }
    }
}
