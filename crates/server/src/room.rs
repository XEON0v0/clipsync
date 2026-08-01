//! Room routing: one actor per room serializing Join/Clip/Disconnect events.
//!
//! Actor lifecycle contract (shared with T9):
//! - the actor inbox is bounded to 64 events and 48 MiB aggregate;
//! - every `Clip`/`Disconnect` event carries `(sender_fp, connection_id)` and is
//!   processed only when it matches the current member connection — a new `Join`
//!   replaces the old connection, making the old one's events no-ops;
//! - a `Join` turn enqueues `join_ok` plus exactly one bootstrap frame into the
//!   connection's outbox FIFO *before* registering the member into the routing set,
//!   so a clip sorted before the join appears in the bootstrap and a clip sorted
//!   after is routed live — no loss window;
//! - the actor never blocks on fsync: mailbox persistence is delegated to the
//!   [`MailboxSink`] seam (T9's single publication worker); `Join` reads only the
//!   in-memory mailbox;
//! - members and the mailbox are keyed by full bundle fingerprint; `device_id` is
//!   display-only.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use dashmap::DashMap;
use tokio::sync::{Notify, mpsc};

use clipboard_core::protocol::{Frame, encode_frame};

use crate::config::{Limits, TestHooks};
use crate::mailbox::{MailboxSink, PendingClip};
use crate::registry::Registry;

/// Unique id of one websocket connection for its whole lifetime.
pub type ConnectionId = u64;

/// One outbound unit for a connection's writer task. Frames are pre-encoded so byte
/// accounting is exact.
pub enum Outbound {
    Frame(Vec<u8>),
    Close,
}

/// Sending half of one connection's bounded outbox (64 frames and 48 MiB by default).
/// Cheap to clone; all clones share the queue and the byte counter.
#[derive(Clone)]
pub struct OutboxHandle {
    tx: mpsc::Sender<Outbound>,
    pending_bytes: Arc<AtomicUsize>,
    max_bytes: usize,
    force_close: Arc<Notify>,
}

/// Receiving half, owned by the connection's writer task.
pub struct OutboxReceiver {
    rx: mpsc::Receiver<Outbound>,
    pending_bytes: Arc<AtomicUsize>,
    force_close: Arc<Notify>,
}

/// Creates a bounded outbox pair.
#[must_use]
pub fn outbox(max_frames: usize, max_bytes: usize) -> (OutboxHandle, OutboxReceiver) {
    let (tx, rx) = mpsc::channel(max_frames);
    let pending_bytes = Arc::new(AtomicUsize::new(0));
    let force_close = Arc::new(Notify::new());
    (
        OutboxHandle {
            tx,
            pending_bytes: pending_bytes.clone(),
            max_bytes,
            force_close: force_close.clone(),
        },
        OutboxReceiver {
            rx,
            pending_bytes,
            force_close,
        },
    )
}

impl OutboxHandle {
    /// Encodes and enqueues a frame. Returns `false` when the queue is full or the
    /// aggregate byte budget is exceeded (the caller then disconnects the peer).
    #[must_use]
    pub fn send_frame(&self, frame: &Frame) -> bool {
        let encoded = encode_frame(frame).expect("server-generated frames are valid");
        self.try_frame(encoded)
    }

    /// Enqueues a pre-encoded frame with exact byte accounting.
    #[must_use]
    pub fn try_frame(&self, encoded: Vec<u8>) -> bool {
        let size = encoded.len();
        let previous = self.pending_bytes.fetch_add(size, Ordering::Relaxed);
        if previous + size > self.max_bytes {
            self.pending_bytes.fetch_sub(size, Ordering::Relaxed);
            return false;
        }
        if self.tx.try_send(Outbound::Frame(encoded)).is_err() {
            self.pending_bytes.fetch_sub(size, Ordering::Relaxed);
            return false;
        }
        true
    }

    /// Enqueues a close marker after already-queued frames; falls back to forcing the
    /// writer to close immediately when the queue is full.
    pub fn close(&self) {
        if self.tx.try_send(Outbound::Close).is_err() {
            self.force_close.notify_one();
        }
    }

    /// Sends an error frame followed by a close.
    pub fn error_and_close(&self, code: &str, message: &str) {
        let error = Frame::Error {
            code: code.to_owned(),
            message: message.to_owned(),
        };
        let _ = self.send_frame(&error);
        self.close();
    }
}

impl OutboxReceiver {
    /// Waits for the next outbound unit or a forced close, and releases byte budget
    /// for frames as they leave the queue. A forced close wins over queued frames:
    /// the peer is being disconnected anyway.
    pub async fn recv(&mut self) -> Option<Outbound> {
        let item = tokio::select! {
            _ = self.force_close.notified() => Some(Outbound::Close),
            item = self.rx.recv() => item,
        };
        if let Some(Outbound::Frame(bytes)) = &item {
            self.pending_bytes.fetch_sub(bytes.len(), Ordering::Relaxed);
        }
        item
    }

    /// Signal fired when a forced close is requested (bounded queue full).
    #[must_use]
    pub fn close_signal(&self) -> Arc<Notify> {
        self.force_close.clone()
    }
}

/// Events serialized by a room actor. `Clip`/`Disconnect` carry
/// `(sender_fp, connection_id)`; stale ids are ignored.
pub enum RoomEvent {
    Join {
        fp: String,
        device_id: String,
        connection_id: ConnectionId,
        outbox: OutboxHandle,
    },
    Clip {
        sender_fp: String,
        connection_id: ConnectionId,
        ciphertext_b64: String,
        encoded_size: usize,
    },
    Disconnect {
        fp: String,
        connection_id: ConnectionId,
    },
}

impl RoomEvent {
    fn payload_size(&self) -> usize {
        match self {
            Self::Clip { encoded_size, .. } => *encoded_size,
            Self::Join { .. } | Self::Disconnect { .. } => 0,
        }
    }
}

/// Sending half of a room actor's bounded inbox.
#[derive(Clone)]
pub struct RoomHandle {
    tx: mpsc::Sender<RoomEvent>,
    pending_bytes: Arc<AtomicUsize>,
    max_bytes: usize,
}

impl RoomHandle {
    fn try_send(&self, event: RoomEvent) -> Result<(), RoomEvent> {
        let size = event.payload_size();
        let previous = self.pending_bytes.fetch_add(size, Ordering::Relaxed);
        if previous + size > self.max_bytes {
            self.pending_bytes.fetch_sub(size, Ordering::Relaxed);
            return Err(event);
        }
        if let Err(error) = self.tx.try_send(event) {
            let event = match error {
                mpsc::error::TrySendError::Full(event)
                | mpsc::error::TrySendError::Closed(event) => event,
            };
            self.pending_bytes.fetch_sub(size, Ordering::Relaxed);
            return Err(event);
        }
        Ok(())
    }

    fn same_actor(&self, other: &RoomHandle) -> bool {
        self.tx.same_channel(&other.tx)
    }
}

struct RoomsInner {
    map: DashMap<String, RoomHandle>,
    limits: Limits,
    mailbox: Arc<dyn MailboxSink>,
    registry: Arc<dyn Registry>,
    hooks: TestHooks,
}

/// Room table: `DashMap<room_id, RoomHandle>` with one actor per room.
#[derive(Clone)]
pub struct Rooms {
    inner: Arc<RoomsInner>,
}

impl Rooms {
    #[must_use]
    pub fn new(
        limits: Limits,
        mailbox: Arc<dyn MailboxSink>,
        registry: Arc<dyn Registry>,
        hooks: TestHooks,
    ) -> Self {
        Self {
            inner: Arc::new(RoomsInner {
                map: DashMap::new(),
                limits,
                mailbox,
                registry,
                hooks,
            }),
        }
    }

    /// Routes a successful join to the room actor, spawning it on first use. Only
    /// registry-checked rooms reach this point; unregistered rooms never spawn actors.
    pub fn join(
        &self,
        room_id: &str,
        member_fps: Vec<String>,
        fp: String,
        device_id: String,
        connection_id: ConnectionId,
        outbox: OutboxHandle,
    ) -> Result<(), RoomEvent> {
        let mut event = RoomEvent::Join {
            fp,
            device_id,
            connection_id,
            outbox,
        };
        for _ in 0..2 {
            let handle = self
                .inner
                .map
                .entry(room_id.to_owned())
                .or_insert_with(|| self.spawn(room_id, &member_fps))
                .clone();
            event = match handle.try_send(event) {
                Ok(()) => {
                    self.log_event("join");
                    return Ok(());
                }
                Err(event) => event,
            };
            // The actor exited between our entry lookup and the send; drop the stale
            // handle and respawn once. A full inbox surfaces as Err to the caller.
            if handle.tx.is_closed() {
                self.inner
                    .map
                    .remove_if(room_id, |_, old| old.same_actor(&handle));
                continue;
            }
            return Err(event);
        }
        Err(event)
    }

    /// Routes a live clip from a joined connection.
    pub fn clip(
        &self,
        room_id: &str,
        sender_fp: String,
        connection_id: ConnectionId,
        ciphertext_b64: String,
        encoded_size: usize,
    ) -> Result<(), RoomEvent> {
        let event = RoomEvent::Clip {
            sender_fp,
            connection_id,
            ciphertext_b64,
            encoded_size,
        };
        let Some(handle) = self.inner.map.get(room_id).map(|entry| entry.clone()) else {
            return Err(event);
        };
        handle.try_send(event).inspect(|()| self.log_event("clip"))
    }

    /// Best-effort disconnect notification; stale events are ignored by the actor.
    pub fn disconnect(&self, room_id: &str, fp: String, connection_id: ConnectionId) {
        let event = RoomEvent::Disconnect { fp, connection_id };
        if let Some(handle) = self.inner.map.get(room_id).map(|entry| entry.clone())
            && handle.try_send(event).is_ok()
        {
            self.log_event("disconnect");
        }
    }

    fn log_event(&self, label: &'static str) {
        if let Some(log) = &self.inner.hooks.event_log {
            let _ = log.send(label);
        }
    }

    fn spawn(&self, room_id: &str, member_fps: &[String]) -> RoomHandle {
        let (tx, rx) = mpsc::channel(self.inner.limits.inbox_max_events);
        let handle = RoomHandle {
            tx,
            pending_bytes: Arc::new(AtomicUsize::new(0)),
            max_bytes: self.inner.limits.inbox_max_bytes,
        };
        let actor = RoomActor {
            room_id: room_id.to_owned(),
            member_fps: member_fps.to_vec(),
            online: HashMap::new(),
            mailbox_pending: self
                .inner
                .mailbox
                .load_pending(room_id)
                .into_iter()
                .collect(),
            mailbox: self.inner.mailbox.clone(),
            registry: self.inner.registry.clone(),
            hooks: self.inner.hooks.clone(),
            pending_bytes: handle.pending_bytes.clone(),
        };
        tokio::spawn(actor.run(rx, handle.clone(), self.inner.clone()));
        handle
    }
}

struct OnlineMember {
    connection_id: ConnectionId,
    outbox: OutboxHandle,
}

struct RoomActor {
    room_id: String,
    /// Registered identity set from the registry (bundle fingerprints).
    member_fps: Vec<String>,
    online: HashMap<String, OnlineMember>,
    /// In-memory mailbox keyed by recipient fingerprint; latest wins.
    mailbox_pending: HashMap<String, PendingClip>,
    mailbox: Arc<dyn MailboxSink>,
    registry: Arc<dyn Registry>,
    hooks: TestHooks,
    pending_bytes: Arc<AtomicUsize>,
}

impl RoomActor {
    async fn run(
        mut self,
        mut rx: mpsc::Receiver<RoomEvent>,
        self_handle: RoomHandle,
        rooms: Arc<RoomsInner>,
    ) {
        while let Some(event) = rx.recv().await {
            self.pending_bytes
                .fetch_sub(event.payload_size(), Ordering::Relaxed);
            if let Some(gate) = &self.hooks.room_event_gate
                && gate.acquire().await.is_err()
            {
                return;
            }
            match event {
                RoomEvent::Join {
                    fp,
                    device_id,
                    connection_id,
                    outbox,
                } => {
                    self.on_join(fp, device_id, connection_id, outbox);
                }
                RoomEvent::Clip {
                    sender_fp,
                    connection_id,
                    ciphertext_b64,
                    ..
                } => {
                    self.on_clip(&sender_fp, connection_id, ciphertext_b64);
                }
                RoomEvent::Disconnect { fp, connection_id } => {
                    self.on_disconnect(&fp, connection_id);
                    if self.online.is_empty() {
                        rooms
                            .map
                            .remove_if(&self.room_id, |_, old| old.same_actor(&self_handle));
                        return;
                    }
                }
            }
        }
    }

    fn on_join(
        &mut self,
        fp: String,
        device_id: String,
        connection_id: ConnectionId,
        outbox: OutboxHandle,
    ) {
        let _ = device_id; // display-only; routing is keyed by bundle fingerprint
        // A new Join replaces the old connection for the same fingerprint.
        if let Some(old) = self.online.remove(&fp) {
            old.outbox.close();
        }
        // Same actor turn: enqueue join_ok and exactly one bootstrap frame into the
        // connection FIFO BEFORE registering the member into the routing set.
        let bootstrap = match self.mailbox_pending.remove(&fp) {
            Some(clip) => {
                self.mailbox.clip_consumed(&self.room_id, &fp);
                Frame::Clip {
                    room_id: self.room_id.clone(),
                    ciphertext_b64: clip.ciphertext_b64,
                    origin_device: clip.origin_device,
                    mailbox: true,
                }
            }
            None => Frame::MailboxEmpty,
        };
        if !outbox.send_frame(&Frame::JoinOk) || !outbox.send_frame(&bootstrap) {
            return; // fresh outbox violated its bounds; leave the member unregistered
        }
        self.online.insert(
            fp.clone(),
            OnlineMember {
                connection_id,
                outbox,
            },
        );
        self.registry.activate_on_first_join(&self.room_id, &fp);
    }

    fn on_clip(&mut self, sender_fp: &str, connection_id: ConnectionId, ciphertext_b64: String) {
        // Only the current connection for this fingerprint may emit clips.
        let Some(sender) = self.online.get(sender_fp) else {
            return;
        };
        if sender.connection_id != connection_id {
            return;
        }
        for recipient_fp in self.member_fps.clone() {
            if recipient_fp == sender_fp {
                continue;
            }
            // origin_device is overwritten with the authenticated sender fingerprint
            // and mailbox is forced to false for live routing.
            let frame = Frame::Clip {
                room_id: self.room_id.clone(),
                ciphertext_b64: ciphertext_b64.clone(),
                origin_device: sender_fp.to_owned(),
                mailbox: false,
            };
            match self.online.get(&recipient_fp) {
                Some(member) => {
                    if !member.outbox.send_frame(&frame) {
                        // Slow consumer past the bounded outbox: disconnect it.
                        let error = Frame::Error {
                            code: "outbox_overflow".to_owned(),
                            message: "outbound queue exceeded".to_owned(),
                        };
                        let _ = member.outbox.send_frame(&error);
                        member.outbox.close();
                        self.online.remove(&recipient_fp);
                    }
                }
                None => {
                    let pending = PendingClip {
                        origin_device: sender_fp.to_owned(),
                        ciphertext_b64: ciphertext_b64.clone(),
                    };
                    self.mailbox_pending
                        .insert(recipient_fp.clone(), pending.clone());
                    self.mailbox
                        .clip_pending(&self.room_id, &recipient_fp, pending);
                }
            }
        }
    }

    fn on_disconnect(&mut self, fp: &str, connection_id: ConnectionId) {
        let is_current = self
            .online
            .get(fp)
            .is_some_and(|member| member.connection_id == connection_id);
        if is_current {
            self.online.remove(fp);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outbox_rejects_frames_past_byte_budget() {
        let (handle, _rx) = outbox(8, 10);
        assert!(handle.try_frame(vec![0_u8; 6]));
        assert!(!handle.try_frame(vec![0_u8; 5]));
        assert!(handle.try_frame(vec![0_u8; 4]));
        assert!(!handle.try_frame(vec![0_u8; 1]));
    }

    #[test]
    fn outbox_rejects_frames_past_frame_budget() {
        let (handle, _rx) = outbox(2, usize::MAX);
        assert!(handle.try_frame(vec![1]));
        assert!(handle.try_frame(vec![1]));
        assert!(!handle.try_frame(vec![1]));
    }

    #[tokio::test]
    async fn outbox_receiving_releases_byte_budget() {
        let (handle, mut rx) = outbox(2, 10);
        assert!(handle.try_frame(vec![0_u8; 8]));
        assert!(!handle.try_frame(vec![0_u8; 8]));
        assert!(matches!(rx.recv().await, Some(Outbound::Frame(_))));
        assert!(handle.try_frame(vec![0_u8; 8]));
    }
}
