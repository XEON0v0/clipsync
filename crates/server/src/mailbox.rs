//! Mailbox: latest-one pending clip per room, in-memory authority plus safe
//! persistence.
//!
//! The room actor keeps the authoritative mailbox **in memory** (`Join` reads only
//! memory). Persistence is owned by [`PersistentMailbox`]'s single publication
//! worker, which serializes writes and merges pending-latest behind this seam; the
//! actor never blocks on fsync. The storage quota (100 rooms x 24 MiB) is enforced
//! by that worker.
//!
//! Persistence contract:
//! - one JSON snapshot per room at `<dir>/<room_id>.json`, written with temp-file +
//!   fsync + rename + directory-fsync, so a crash mid-write leaves the previous
//!   complete file intact; the `room_id` is validated against `^[0-9a-f]{32}$`
//!   before it is ever joined into a path;
//! - per room at most one fsync+rename job is in flight: while a write runs, newer
//!   clips replace a single pending-latest snapshot, and the worker publishes that
//!   snapshot only after the current write completes — physical rename order is
//!   strictly monotonic;
//! - a failed write never rolls back actor memory: the pending-latest snapshot is
//!   retained for a periodic retry, a structured error is logged, and `/healthz`
//!   reports degraded until a later write succeeds;
//! - delivered clips are retained on disk (`clip_consumed` is intentionally a
//!   no-op); idempotent delivery is the client's dedup responsibility;
//! - startup rescans the directory, warning and skipping corrupt files and
//!   deleting snapshots past the 7-day TTL; a periodic sweeper reclaims expired
//!   snapshots while running.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use dashmap::{DashMap, DashSet};
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, Semaphore, mpsc};

use crate::MAILBOX_MAX_BYTES;
use crate::registry::unix_time_ms;

/// Mailbox snapshot TTL: 7 days, swept hourly by [`PersistentMailbox::spawn_ttl_sweeper`].
pub const MAILBOX_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// One pending mailbox clip destined for an offline member.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingClip {
    /// Authenticated sender bundle fingerprint (overwritten by the server).
    pub origin_device: String,
    /// Base64 ciphertext payload, opaque to the relay.
    pub ciphertext_b64: String,
}

/// Notification sink the room actor pushes mailbox changes into. All methods must be
/// non-blocking; [`PersistentMailbox`] implements this with a channel into its
/// publication worker.
pub trait MailboxSink: Send + Sync {
    /// A clip was stored in the in-memory mailbox for an offline member (latest wins).
    fn clip_pending(&self, room_id: &str, recipient_fp: &str, clip: PendingClip);

    /// A pending clip was delivered as the join bootstrap frame. Persistence
    /// retains delivered clips; the client's dedup ring provides idempotency.
    fn clip_consumed(&self, room_id: &str, recipient_fp: &str);

    /// Pending clips restored into a room actor's memory when the actor is created.
    /// Replays the persisted (and not yet published) mailbox.
    fn load_pending(&self, room_id: &str) -> Vec<(String, PendingClip)> {
        let _ = room_id;
        Vec::new()
    }

    /// Health probe flipped by the persistence layer; `/healthz` reports degraded
    /// while a publication is failing. `None` for sinks without persistence.
    fn health_probe(&self) -> Option<HealthProbe> {
        None
    }
}

/// Default sink for tests that do not exercise persistence.
pub struct NoopMailboxSink;

impl MailboxSink for NoopMailboxSink {
    fn clip_pending(&self, room_id: &str, recipient_fp: &str, clip: PendingClip) {
        let _ = (room_id, recipient_fp, clip);
    }

    fn clip_consumed(&self, room_id: &str, recipient_fp: &str) {
        let _ = (room_id, recipient_fp);
    }
}

/// Tunables for [`PersistentMailbox`]. Production uses [`MailboxOptions::default`];
/// tests shrink the retry interval and quota and stall writes through the gate.
#[derive(Clone)]
pub struct MailboxOptions {
    /// Global mailbox storage quota across all rooms.
    pub max_bytes: u64,
    /// Snapshot time-to-live; expired snapshots are deleted by the scan/sweeper.
    pub ttl: Duration,
    /// Delay before retrying a failed publication.
    pub retry_interval: Duration,
    /// Test-only stall point: the worker acquires one permit before every physical
    /// write, letting tests sequence the single-publication order deterministically.
    pub write_gate: Option<Arc<Semaphore>>,
}

impl Default for MailboxOptions {
    fn default() -> Self {
        Self {
            max_bytes: MAILBOX_MAX_BYTES,
            ttl: MAILBOX_TTL,
            retry_interval: Duration::from_secs(5),
            write_gate: None,
        }
    }
}

/// Shared health flag flipped by the publication worker. Cloning is cheap; all
/// clones observe the same state.
#[derive(Clone, Default)]
pub struct HealthProbe {
    inner: Arc<HealthInner>,
}

#[derive(Default)]
struct HealthInner {
    degraded: AtomicBool,
    last_error: Mutex<Option<String>>,
}

impl HealthProbe {
    /// Whether the last publication attempt succeeded (or none ran yet).
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        !self.inner.degraded.load(Ordering::Relaxed)
    }

    /// The most recent publication error, if any.
    #[must_use]
    pub fn last_error(&self) -> Option<String> {
        self.inner
            .last_error
            .lock()
            .expect("health mutex poisoned")
            .clone()
    }

    fn mark_degraded(&self, error: &io::Error) {
        *self
            .inner
            .last_error
            .lock()
            .expect("health mutex poisoned") = Some(error.to_string());
        self.inner.degraded.store(true, Ordering::Relaxed);
    }

    fn mark_healthy(&self) {
        self.inner.degraded.store(false, Ordering::Relaxed);
        *self
            .inner
            .last_error
            .lock()
            .expect("health mutex poisoned") = None;
    }
}

/// One stored mailbox snapshot: the opaque clip frame content plus routing metadata.
/// The ciphertext is never inspected by the relay.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct DiskMailbox {
    version: u32,
    room_id: String,
    recipient_fp: String,
    origin_fp: String,
    ciphertext_b64: String,
    ts_ms: i64,
}

impl DiskMailbox {
    fn into_snapshot(self) -> io::Result<Snapshot> {
        if self.version != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported mailbox version {}", self.version),
            ));
        }
        validate_room_id(&self.room_id)?;
        validate_fingerprint(&self.recipient_fp)?;
        validate_fingerprint(&self.origin_fp)?;
        let decoded = STANDARD
            .decode(&self.ciphertext_b64)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if STANDARD.encode(decoded) != self.ciphertext_b64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "ciphertext_b64 must be canonical padded STANDARD base64",
            ));
        }
        if self.ts_ms <= 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "ts_ms must be positive",
            ));
        }
        Ok(Snapshot {
            recipient_fp: self.recipient_fp,
            origin_fp: self.origin_fp,
            ciphertext_b64: self.ciphertext_b64,
            ts_ms: self.ts_ms,
        })
    }
}

/// In-memory form of a pending-latest clip.
#[derive(Clone)]
struct Snapshot {
    recipient_fp: String,
    origin_fp: String,
    ciphertext_b64: String,
    ts_ms: i64,
}

impl Snapshot {
    fn to_disk(&self, room_id: &str) -> DiskMailbox {
        DiskMailbox {
            version: 1,
            room_id: room_id.to_owned(),
            recipient_fp: self.recipient_fp.clone(),
            origin_fp: self.origin_fp.clone(),
            ciphertext_b64: self.ciphertext_b64.clone(),
            ts_ms: self.ts_ms,
        }
    }

    fn to_pending_clip(&self) -> PendingClip {
        PendingClip {
            origin_device: self.origin_fp.clone(),
            ciphertext_b64: self.ciphertext_b64.clone(),
        }
    }
}

/// Persistent mailbox backed by one JSON snapshot file per room.
///
/// `clip_pending` only updates shared maps and signals the single publication
/// worker over a channel; the room actor never blocks on fsync. The worker
/// serializes all physical writes, coalescing per-room updates into one
/// pending-latest snapshot while a write is in flight.
pub struct PersistentMailbox {
    dir: PathBuf,
    options: MailboxOptions,
    /// Latest published snapshot per room (replayed into room actors on spawn).
    index: DashMap<String, Snapshot>,
    /// Latest not-yet-published snapshot per room (pending-latest coalescing).
    dirty: DashMap<String, Snapshot>,
    /// Rooms with a publication signal already in the channel.
    queued: DashSet<String>,
    /// On-disk file sizes for quota accounting.
    sizes: Mutex<HashMap<String, u64>>,
    tx: mpsc::UnboundedSender<String>,
    health: HealthProbe,
    writes_done: Arc<AtomicUsize>,
    writes_notify: Arc<Notify>,
    in_flight: Arc<AtomicBool>,
}

impl PersistentMailbox {
    /// Opens the mailbox directory, scanning existing snapshots (corrupt files are
    /// skipped with a warning, expired ones deleted), and spawns the publication
    /// worker. Must be called inside a tokio runtime.
    ///
    /// # Errors
    /// Returns the directory creation/scan error from the OS.
    pub fn open(dir: impl AsRef<Path>, options: MailboxOptions) -> io::Result<Arc<Self>> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;
        let ttl_ms = ttl_ms(options.ttl);
        let index = DashMap::new();
        let mut sizes = HashMap::new();
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(room_id) = name.strip_suffix(".json") else {
                continue; // temp files and foreign entries are ignored
            };
            if validate_room_id(room_id).is_err() {
                continue;
            }
            match Self::read_snapshot(&entry.path(), room_id) {
                Ok(snapshot) if is_expired(snapshot.ts_ms, unix_time_ms(), ttl_ms) => {
                    eprintln!("mailbox: deleting expired snapshot room_id={room_id}");
                    let _ = fs::remove_file(entry.path());
                }
                Ok(snapshot) => {
                    sizes.insert(room_id.to_owned(), entry.metadata()?.len());
                    index.insert(room_id.to_owned(), snapshot);
                }
                Err(error) => {
                    eprintln!("mailbox: skipping corrupt snapshot path={name} error={error}");
                }
            }
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let mailbox = Arc::new(Self {
            dir,
            options,
            index,
            dirty: DashMap::new(),
            queued: DashSet::new(),
            sizes: Mutex::new(sizes),
            tx,
            health: HealthProbe::default(),
            writes_done: Arc::new(AtomicUsize::new(0)),
            writes_notify: Arc::new(Notify::new()),
            in_flight: Arc::new(AtomicBool::new(false)),
        });
        tokio::spawn(mailbox.clone().run_worker(rx));
        Ok(mailbox)
    }

    fn read_snapshot(path: &Path, room_id: &str) -> io::Result<Snapshot> {
        let disk: DiskMailbox = serde_json::from_slice(&fs::read(path)?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if disk.room_id != room_id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "snapshot room_id does not match its filename",
            ));
        }
        disk.into_snapshot()
    }

    /// Health probe reported by `/healthz`.
    #[must_use]
    pub fn health_probe(&self) -> HealthProbe {
        self.health.clone()
    }

    /// Test instrumentation: number of completed physical writes.
    #[must_use]
    pub fn completed_writes(&self) -> usize {
        self.writes_done.load(Ordering::Relaxed)
    }

    /// Test instrumentation: waits until at least `n` physical writes completed.
    pub async fn wait_writes(&self, n: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if self.completed_writes() >= n {
                    return;
                }
                self.writes_notify.notified().await;
            }
        })
        .await
        .expect("timed out waiting for mailbox writes");
    }

    /// Test instrumentation: true while the worker holds a snapshot for writing.
    #[must_use]
    pub fn write_in_flight(&self) -> bool {
        self.in_flight.load(Ordering::Relaxed)
    }

    /// Deletes every snapshot whose `ts_ms` is older than the TTL relative to
    /// `now_ms`; returns how many were removed.
    pub fn sweep_expired(&self, now_ms: i64) -> usize {
        let ttl_ms = ttl_ms(self.options.ttl);
        let expired: Vec<String> = self
            .index
            .iter()
            .filter(|entry| is_expired(entry.value().ts_ms, now_ms, ttl_ms))
            .map(|entry| entry.key().clone())
            .collect();
        let mut removed = 0;
        for room_id in expired {
            if self.index.remove(&room_id).is_none() {
                continue;
            }
            self.sizes
                .lock()
                .expect("mailbox sizes mutex poisoned")
                .remove(&room_id);
            let path = self.dir.join(format!("{room_id}.json"));
            if let Err(error) = fs::remove_file(&path)
                && error.kind() != io::ErrorKind::NotFound
            {
                eprintln!(
                    "mailbox: failed to delete expired snapshot room_id={room_id} error={error}"
                );
            }
            if let Ok(dir) = File::open(&self.dir) {
                let _ = dir.sync_all();
            }
            removed += 1;
        }
        removed
    }

    /// Runs the hourly TTL sweep; returns the sweeper task handle.
    pub fn spawn_ttl_sweeper(self: &Arc<Self>, period: Duration) -> tokio::task::JoinHandle<()> {
        let mailbox = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(period);
            interval.tick().await;
            loop {
                interval.tick().await;
                let removed = mailbox.sweep_expired(unix_time_ms());
                if removed > 0 {
                    eprintln!("mailbox: swept {removed} expired snapshot(s)");
                }
            }
        })
    }

    /// The single publication worker. Signals carry only a room id; the snapshot
    /// itself lives in `dirty`, so a burst of clips for one room collapses into one
    /// pending-latest write instead of a queue. Failed rooms are retried on the
    /// retry interval without a signal.
    async fn run_worker(self: Arc<Self>, mut rx: mpsc::UnboundedReceiver<String>) {
        let mut retry: HashSet<String> = HashSet::new();
        let mut retry_tick = tokio::time::interval(self.options.retry_interval);
        retry_tick.tick().await;
        loop {
            let room_id = tokio::select! {
                signal = rx.recv() => {
                    match signal {
                        Some(room_id) => room_id,
                        None => return, // all senders dropped: shutdown
                    }
                }
                _ = retry_tick.tick(), if !retry.is_empty() => {
                    let rooms: Vec<String> = retry.drain().collect();
                    for room_id in rooms {
                        self.queued.remove(&room_id);
                        self.publish(room_id, &mut retry).await;
                    }
                    continue;
                }
            };
            self.queued.remove(&room_id);
            self.publish(room_id, &mut retry).await;
        }
    }

    /// Publishes the pending-latest snapshot of one room. Never runs concurrently
    /// with itself (the worker is a single task), so physical rename order is
    /// strictly monotonic per room.
    async fn publish(&self, room_id: String, retry: &mut HashSet<String>) {
        let Some((_, snapshot)) = self.dirty.remove(&room_id) else {
            return; // already published by a previous signal
        };
        self.in_flight.store(true, Ordering::Relaxed);
        let result = self.write_snapshot(&room_id, &snapshot).await;
        self.in_flight.store(false, Ordering::Relaxed);
        match result {
            Ok(size) => {
                self.sizes
                    .lock()
                    .expect("mailbox sizes mutex poisoned")
                    .insert(room_id.clone(), size);
                self.index.insert(room_id, snapshot);
                self.health.mark_healthy();
                self.writes_done.fetch_add(1, Ordering::Relaxed);
                self.writes_notify.notify_waiters();
            }
            Err(error) => {
                // Never roll back: keep the pending-latest snapshot (unless a newer
                // one arrived while writing) and retry later. Actor memory and the
                // previous file are untouched.
                eprintln!("mailbox: publication failed room_id={room_id} error={error}");
                self.health.mark_degraded(&error);
                self.dirty.entry(room_id.clone()).or_insert(snapshot);
                retry.insert(room_id);
            }
        }
    }

    async fn write_snapshot(&self, room_id: &str, snapshot: &Snapshot) -> io::Result<u64> {
        let encoded = serde_json::to_vec_pretty(&snapshot.to_disk(room_id))
            .map_err(io::Error::other)?;
        let prospective = {
            let sizes = self.sizes.lock().expect("mailbox sizes mutex poisoned");
            sizes.values().sum::<u64>() - sizes.get(room_id).copied().unwrap_or(0)
                + encoded.len() as u64
        };
        if prospective > self.options.max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::QuotaExceeded,
                format!("mailbox storage quota exceeded: {prospective} > {}", self.options.max_bytes),
            ));
        }
        // Test-only stall point for publication-ordering assertions. The permit is
        // forgotten so every physical write needs exactly one test-granted permit.
        if let Some(gate) = &self.options.write_gate
            && let Ok(permit) = gate.acquire().await
        {
            permit.forget();
        }
        let dir = self.dir.clone();
        let tmp_name = format!(".{room_id}.json.tmp");
        let file_name = format!("{room_id}.json");
        let size = encoded.len() as u64;
        tokio::task::spawn_blocking(move || {
            let temp = dir.join(tmp_name);
            let path = dir.join(file_name);
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&temp)?;
            file.write_all(&encoded)?;
            file.sync_all()?;
            fs::rename(&temp, &path)?;
            File::open(&dir)?.sync_all()?;
            Ok(size)
        })
        .await
        .map_err(|join| io::Error::other(format!("mailbox write task panicked: {join}")))?
    }
}

impl MailboxSink for PersistentMailbox {
    fn clip_pending(&self, room_id: &str, recipient_fp: &str, clip: PendingClip) {
        if let Err(error) = validate_room_id(room_id) {
            eprintln!("mailbox: refusing clip for invalid room_id={room_id:?} error={error}");
            return;
        }
        let snapshot = Snapshot {
            recipient_fp: recipient_fp.to_owned(),
            origin_fp: clip.origin_device,
            ciphertext_b64: clip.ciphertext_b64,
            ts_ms: unix_time_ms(),
        };
        self.dirty.insert(room_id.to_owned(), snapshot);
        // Signal only when the room is not already queued or in flight; the worker
        // always publishes the pending-latest snapshot, so extra signals are noise.
        if self.queued.insert(room_id.to_owned()) {
            let _ = self.tx.send(room_id.to_owned());
        }
    }

    fn clip_consumed(&self, room_id: &str, recipient_fp: &str) {
        // Delivered clips are retained on disk by contract; the client's dedup ring
        // makes replay idempotent. Nothing to do.
        let _ = (room_id, recipient_fp);
    }

    fn load_pending(&self, room_id: &str) -> Vec<(String, PendingClip)> {
        let ttl_ms = ttl_ms(self.options.ttl);
        let now = unix_time_ms();
        // An unpublished pending-latest snapshot wins over the last published one.
        let snapshot = self
            .dirty
            .get(room_id)
            .map(|entry| entry.value().clone())
            .or_else(|| self.index.get(room_id).map(|entry| entry.value().clone()));
        match snapshot {
            Some(snapshot) if !is_expired(snapshot.ts_ms, now, ttl_ms) => {
                vec![(snapshot.recipient_fp.clone(), snapshot.to_pending_clip())]
            }
            _ => Vec::new(),
        }
    }

    fn health_probe(&self) -> Option<HealthProbe> {
        Some(self.health.clone())
    }
}

fn ttl_ms(ttl: Duration) -> i64 {
    i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX)
}

fn is_expired(ts_ms: i64, now_ms: i64, ttl_ms: i64) -> bool {
    now_ms.saturating_sub(ts_ms) > ttl_ms
}

fn validate_room_id(room_id: &str) -> io::Result<()> {
    if room_id.len() == 32
        && room_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "room id must be 32 lowercase hex characters",
        ))
    }
}

fn validate_fingerprint(fp: &str) -> io::Result<()> {
    if fp.len() == 64
        && fp.bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "fingerprint must be 64 lowercase hex characters",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Semaphore;

    const TEST_TIMEOUT: Duration = Duration::from_secs(5);

    fn room_id() -> String {
        "aa".repeat(16)
    }

    fn recipient_fp() -> String {
        "bb".repeat(32)
    }

    fn origin_fp() -> String {
        "cc".repeat(32)
    }

    fn clip(payload: &str) -> PendingClip {
        PendingClip {
            origin_device: origin_fp(),
            ciphertext_b64: base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                payload,
            ),
        }
    }

    fn fast_options() -> MailboxOptions {
        MailboxOptions {
            retry_interval: Duration::from_millis(20),
            ..MailboxOptions::default()
        }
    }

    async fn eventually(mut condition: impl FnMut() -> bool) {
        tokio::time::timeout(TEST_TIMEOUT, async {
            while !condition() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("condition did not become true in time");
    }

    fn mailbox_file(dir: &Path, room_id: &str) -> std::path::PathBuf {
        dir.join(format!("{room_id}.json"))
    }

    /// Writes a mailbox snapshot file directly, bypassing the publication worker.
    fn write_disk_file(dir: &Path, room_id: &str, recipient_fp: &str, payload: &str, ts_ms: i64) {
        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, payload);
        let json = format!(
            "{{\"version\":1,\"room_id\":\"{room_id}\",\"recipient_fp\":\"{recipient_fp}\",\"origin_fp\":\"{}\",\"ciphertext_b64\":\"{encoded}\",\"ts_ms\":{ts_ms}}}",
            origin_fp()
        );
        std::fs::write(mailbox_file(dir, room_id), json).unwrap();
    }

    #[tokio::test]
    async fn mailbox_publishes_and_reloads_after_reopen() {
        let dir = tempfile::TempDir::new().unwrap();
        let mailbox = PersistentMailbox::open(dir.path(), fast_options()).unwrap();
        mailbox.clip_pending(&room_id(), &recipient_fp(), clip("hello"));
        mailbox.wait_writes(1).await;
        assert!(mailbox_file(dir.path(), &room_id()).exists());
        assert_eq!(mailbox.completed_writes(), 1);

        let pending = mailbox.load_pending(&room_id());
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].0, recipient_fp());
        assert_eq!(pending[0].1, clip("hello"));
        drop(mailbox);

        // A fresh instance rescans the directory and replays the same clip.
        let reopened = PersistentMailbox::open(dir.path(), fast_options()).unwrap();
        let pending = reopened.load_pending(&room_id());
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].1, clip("hello"));
    }

    #[tokio::test]
    async fn mailbox_single_publication_coalesces_pending_latest() {
        let dir = tempfile::TempDir::new().unwrap();
        let gate = Arc::new(Semaphore::new(0));
        let mailbox = PersistentMailbox::open(
            dir.path(),
            MailboxOptions {
                write_gate: Some(gate.clone()),
                ..fast_options()
            },
        )
        .unwrap();

        mailbox.clip_pending(&room_id(), &recipient_fp(), clip("N"));
        // Wait until the worker picked up N and is blocked inside the gated write.
        eventually(|| mailbox.write_in_flight()).await;

        mailbox.clip_pending(&room_id(), &recipient_fp(), clip("N+1"));
        mailbox.clip_pending(&room_id(), &recipient_fp(), clip("N+2"));
        // Nothing may publish while N is in flight.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(mailbox.completed_writes(), 0);

        // N completes first.
        gate.add_permits(1);
        mailbox.wait_writes(1).await;
        let on_disk = std::fs::read_to_string(mailbox_file(dir.path(), &room_id())).unwrap();
        assert!(on_disk.contains(&clip("N").ciphertext_b64));

        // N+1 is never written on its own: the worker coalesced it into N+2 and is
        // gated again before the next publication.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(mailbox.completed_writes(), 1);

        gate.add_permits(1);
        mailbox.wait_writes(2).await;
        let on_disk = std::fs::read_to_string(mailbox_file(dir.path(), &room_id())).unwrap();
        assert!(on_disk.contains(&clip("N+2").ciphertext_b64));
        assert!(!on_disk.contains(&clip("N+1").ciphertext_b64));
        // Exactly two physical writes: N, then the coalesced N+2.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(mailbox.completed_writes(), 2);
    }

    #[tokio::test]
    async fn mailbox_crash_during_write_preserves_old_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let mailbox = PersistentMailbox::open(dir.path(), fast_options()).unwrap();
        mailbox.clip_pending(&room_id(), &recipient_fp(), clip("old"));
        mailbox.wait_writes(1).await;
        let before = std::fs::read(mailbox_file(dir.path(), &room_id())).unwrap();
        drop(mailbox);

        // Simulate a crash mid-write: a truncated temp file is left behind. The
        // rename never happened, so the old complete file must be intact and the
        // temp garbage must be ignored by the startup scan.
        std::fs::write(
            dir.path().join(format!(".{}.json.tmp", room_id())),
            b"{\"version\":1,\"room_id\":\"aa",
        )
        .unwrap();

        let reopened = PersistentMailbox::open(dir.path(), fast_options()).unwrap();
        let pending = reopened.load_pending(&room_id());
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].1, clip("old"));
        assert_eq!(
            std::fs::read(mailbox_file(dir.path(), &room_id())).unwrap(),
            before
        );
    }

    #[tokio::test]
    async fn mailbox_rejects_invalid_room_id() {
        let dir = tempfile::TempDir::new().unwrap();
        let mailbox = PersistentMailbox::open(dir.path(), fast_options()).unwrap();
        mailbox.clip_pending("../evil", &recipient_fp(), clip("nope"));
        mailbox.clip_pending(&"AA".repeat(16), &recipient_fp(), clip("nope"));
        mailbox.clip_pending(&"aa".repeat(8), &recipient_fp(), clip("nope"));
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(mailbox.completed_writes(), 0);
        assert!(mailbox.health_probe().is_healthy());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn mailbox_corrupt_file_is_skipped_on_startup() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(mailbox_file(dir.path(), &room_id()), b"{not json").unwrap();
        let mailbox = PersistentMailbox::open(dir.path(), fast_options()).unwrap();
        assert!(mailbox.load_pending(&room_id()).is_empty());
        assert!(mailbox.health_probe().is_healthy());
    }

    #[tokio::test]
    async fn mailbox_ttl_sweep_removes_expired_clips() {
        let dir = tempfile::TempDir::new().unwrap();
        let now = crate::registry::unix_time_ms();
        let seven_days_ms = 7 * 24 * 60 * 60 * 1000;
        // Fresh file: survives the sweep.
        write_disk_file(dir.path(), &room_id(), &recipient_fp(), "fresh", now);
        // Expired file: removed at startup scan and by the sweep.
        let old_room = "dd".repeat(16);
        write_disk_file(
            dir.path(),
            &old_room,
            &recipient_fp(),
            "stale",
            now - seven_days_ms - 1,
        );

        let mailbox = PersistentMailbox::open(dir.path(), fast_options()).unwrap();
        assert!(!mailbox_file(dir.path(), &old_room).exists());
        assert_eq!(mailbox.load_pending(&room_id()).len(), 1);
        assert!(mailbox.load_pending(&old_room).is_empty());

        // The periodic sweep removes files that age out while running.
        let removed = mailbox.sweep_expired(now + seven_days_ms + 1);
        assert_eq!(removed, 1);
        assert!(!mailbox_file(dir.path(), &room_id()).exists());
        assert!(mailbox.load_pending(&room_id()).is_empty());
    }

    #[tokio::test]
    async fn mailbox_quota_exceeded_marks_degraded_and_keeps_pending() {
        let dir = tempfile::TempDir::new().unwrap();
        let mailbox = PersistentMailbox::open(
            dir.path(),
            MailboxOptions {
                max_bytes: 16,
                ..fast_options()
            },
        )
        .unwrap();
        mailbox.clip_pending(&room_id(), &recipient_fp(), clip("too-big-for-quota"));
        eventually(|| !mailbox.health_probe().is_healthy()).await;
        assert_eq!(mailbox.completed_writes(), 0);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        // The unpublished latest is retained for a later retry.
        let pending = mailbox.load_pending(&room_id());
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].1, clip("too-big-for-quota"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn mailbox_readonly_dir_keeps_memory_serving_and_recovers() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::TempDir::new().unwrap();
        let mailbox = PersistentMailbox::open(dir.path(), fast_options()).unwrap();
        mailbox.clip_pending(&room_id(), &recipient_fp(), clip("old"));
        mailbox.wait_writes(1).await;
        let old_bytes = std::fs::read(mailbox_file(dir.path(), &room_id())).unwrap();

        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
        let restore = scopeguard_restore(dir.path().to_path_buf());
        mailbox.clip_pending(&room_id(), &recipient_fp(), clip("new"));
        eventually(|| !mailbox.health_probe().is_healthy()).await;

        // Old file intact; memory still serves the pending-latest clip.
        assert_eq!(
            std::fs::read(mailbox_file(dir.path(), &room_id())).unwrap(),
            old_bytes
        );
        let pending = mailbox.load_pending(&room_id());
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].1, clip("new"));

        drop(restore);
        mailbox.wait_writes(2).await;
        eventually(|| mailbox.health_probe().is_healthy()).await;
        let on_disk = std::fs::read_to_string(mailbox_file(dir.path(), &room_id())).unwrap();
        assert!(on_disk.contains(&clip("new").ciphertext_b64));
    }

    /// Restores write permission on drop (test failure safety).
    #[cfg(unix)]
    fn scopeguard_restore(path: std::path::PathBuf) -> impl Drop {
        struct Guard(std::path::PathBuf);
        impl Drop for Guard {
            fn drop(&mut self) {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
            }
        }
        Guard(path)
    }
}
