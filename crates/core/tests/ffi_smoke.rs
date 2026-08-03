//! FFI smoke tests (T10): dual-handle pairing closed loop through the real
//! relay, typed live/mailbox/status callbacks, FIFO ordering, typed
//! backpressure, shutdown join, and the reset generation-fault contract.
#![cfg(feature = "full")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clipboard_core::ffi::{
    CoreCallbacks, CoreError, CoreHandle, CoreStatus, FfiClipContent, FfiClipItem,
    MailboxDisposition, PairingSnapshot,
};
use clipboard_core::history::{HistoryContent, HistorySource, HistoryStore, NewHistoryItem};
use clipboard_core::session::text_hash;
use clipboard_server::{
    InMemoryRegistry, Limits, MailboxOptions, PairingConfig, PairingRelay, PersistentMailbox,
    ServerConfig, ServerHandle, start,
};
use tempfile::TempDir;

const WAIT: Duration = Duration::from_secs(10);

#[derive(Clone, Default)]
struct RecordingCallbacks {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    clips: Mutex<Vec<FfiClipItem>>,
    mailbox: Mutex<Vec<FfiClipItem>>,
    statuses: Mutex<Vec<CoreStatus>>,
    fail_next_live: AtomicBool,
    mailbox_disposition: Mutex<Option<MailboxDisposition>>,
}

impl RecordingCallbacks {
    fn live_texts(&self) -> Vec<String> {
        self.inner.clips
            .lock()
            .unwrap()
            .iter()
            .filter_map(|item| match &item.content {
                FfiClipContent::Text { text } => Some(text.clone()),
                FfiClipContent::Image { .. } => None,
            })
            .collect()
    }

    fn mailbox_texts(&self) -> Vec<String> {
        self.inner.mailbox
            .lock()
            .unwrap()
            .iter()
            .filter_map(|item| match &item.content {
                FfiClipContent::Text { text } => Some(text.clone()),
                FfiClipContent::Image { .. } => None,
            })
            .collect()
    }

    fn saw_error_status(&self) -> bool {
        self.inner.statuses
            .lock()
            .unwrap()
            .iter()
            .any(|status| matches!(status, CoreStatus::Error { .. }))
    }
}

impl CoreCallbacks for RecordingCallbacks {
    fn on_clip(&self, item: FfiClipItem) -> Result<(), CoreError> {
        if self.inner.fail_next_live.swap(false, Ordering::SeqCst) {
            return Err(CoreError::Internal("injected live callback failure".to_owned()));
        }
        self.inner.clips.lock().unwrap().push(item);
        Ok(())
    }

    fn on_mailbox_clip(&self, item: FfiClipItem) -> Result<MailboxDisposition, CoreError> {
        self.inner.mailbox.lock().unwrap().push(item);
        Ok(self
            .inner
            .mailbox_disposition
            .lock()
            .unwrap()
            .unwrap_or(MailboxDisposition::Applied))
    }

    fn on_status(&self, status: CoreStatus) {
        self.inner.statuses.lock().unwrap().push(status);
    }
}

struct Relay {
    _runtime: tokio::runtime::Runtime,
    handle: ServerHandle,
}

impl Relay {
    fn start(mailbox_dir: Option<&TempDir>) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("relay runtime builds");
        let registry = Arc::new(InMemoryRegistry::new());
        let pairing = Arc::new(PairingRelay::new(
            registry.clone(),
            PairingConfig {
                attempts_per_window: 100,
                ..PairingConfig::default()
            },
        ));
        let mailbox: Arc<dyn clipboard_server::MailboxSink> = match mailbox_dir {
            Some(dir) => {
                // PersistentMailbox spawns its persistence worker on open and
                // therefore needs the relay runtime context.
                let _guard = runtime.enter();
                let mailbox = PersistentMailbox::open(
                    dir.path(),
                    MailboxOptions {
                        retry_interval: Duration::from_millis(50),
                        ..MailboxOptions::default()
                    },
                )
                .expect("mailbox opens");
                mailbox as Arc<dyn clipboard_server::MailboxSink>
            }
            None => Arc::new(clipboard_server::NoopMailboxSink),
        };
        let handle = runtime
            .block_on(start(
                "127.0.0.1:0".parse().unwrap(),
                ServerConfig {
                    limits: Limits {
                        join_attempts_per_minute: 100,
                        ..Limits::default()
                    },
                    ..ServerConfig::default()
                },
                registry,
                pairing,
                mailbox,
            ))
            .expect("relay binds");
        Relay {
            _runtime: runtime,
            handle,
        }
    }

    fn url(&self) -> String {
        format!("ws://127.0.0.1:{}/ws", self.handle.addr().port())
    }
}

fn wait_until<T>(mut condition: impl FnMut() -> Option<T>) -> T {
    let started = Instant::now();
    loop {
        if let Some(value) = condition() {
            return value;
        }
        assert!(
            started.elapsed() < WAIT,
            "timed out waiting for the expected condition"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn wait_sas(handle: &CoreHandle) -> String {
    wait_until(|| match handle.pair_poll() {
        PairingSnapshot::SasReady { sas } => Some(sas),
        _ => None,
    })
}

fn wait_live_texts(callbacks: &RecordingCallbacks, count: usize) -> Vec<String> {
    wait_until(|| {
        let texts = callbacks.live_texts();
        (texts.len() >= count).then_some(texts)
    })
}

/// Pairs both handles through the real relay and confirms the SAS on both
/// sides, leaving two connected live sessions.
fn pair_and_confirm(a: &CoreHandle, b: &CoreHandle, url: &str) {
    let qr = a.pair_begin(url.to_owned()).expect("pair_begin succeeds");
    let sas_b = b.pair_claim(qr).expect("pair_claim succeeds");
    let sas_a = wait_sas(a);
    assert_eq!(sas_a, sas_b, "both sides must display the same SAS");
    a.pair_confirm(sas_a).expect("offerer confirm succeeds");
    b.pair_confirm(sas_b).expect("claimer confirm succeeds");
}

fn tempdir() -> TempDir {
    tempfile::tempdir().expect("temp dir")
}

fn handle(dir: &TempDir, callbacks: RecordingCallbacks) -> Arc<CoreHandle> {
    CoreHandle::new(
        dir.path().to_string_lossy().into_owned(),
        Box::new(callbacks),
    )
    .expect("core handle opens")
}

/// Happy path (QA): dual-handle closed loop — pair, confirm, live clip with
/// typed callbacks, echo suppression, and history on both ends.
#[test]
fn ffi_smoke_pairing_closed_loop_live() {
    let relay = Relay::start(None);
    let dir_a = tempdir();
    let dir_b = tempdir();
    let cb_a = RecordingCallbacks::default();
    let cb_b = RecordingCallbacks::default();
    let a = handle(&dir_a, cb_a.clone());
    let b = handle(&dir_b, cb_b.clone());

    pair_and_confirm(&a, &b, &relay.url());
    assert_eq!(
        b.pair_poll(),
        PairingSnapshot::Paired {
            room_id: a.pair_poll().room_id().expect("a is paired")
        }
    );

    let seq = a.send_text("hello host".to_owned()).expect("send succeeds");
    assert_eq!(seq, 1, "first send allocates seq 1");
    let texts = wait_live_texts(&cb_b, 1);
    assert_eq!(texts, ["hello host"]);

    let hash = text_hash("hello host");
    assert!(b.is_echo(hash.clone()), "receiver registers applied echo");
    assert!(a.is_echo(hash), "sender registers sent echo");
    assert!(!b.is_echo(text_hash("other")), "unknown hash is not an echo");

    let history_b = wait_until(|| b.history().ok().filter(|items| !items.is_empty()));
    assert_eq!(history_b.len(), 1);
    let history_a = a.history().expect("sender history");
    assert_eq!(history_a.len(), 1);

    a.shutdown().expect("a shuts down");
    b.shutdown().expect("b shuts down");
}

trait SnapshotRoom {
    fn room_id(&self) -> Option<String>;
}

impl SnapshotRoom for PairingSnapshot {
    fn room_id(&self) -> Option<String> {
        match self {
            PairingSnapshot::Paired { room_id } => Some(room_id.clone()),
            _ => None,
        }
    }
}

/// Mailbox typed callback: the peer is offline when the clip is sent; the
/// join bootstrap delivers it through `on_mailbox_clip` with `Applied`.
#[test]
fn ffi_smoke_mailbox_bootstrap_typed_callback() {
    let mailbox_dir = tempdir();
    let relay = Relay::start(Some(&mailbox_dir));
    let dir_a = tempdir();
    let dir_b = tempdir();
    let cb_a = RecordingCallbacks::default();
    let cb_b = RecordingCallbacks::default();
    let a = handle(&dir_a, cb_a);
    let b = handle(&dir_b, cb_b.clone());

    pair_and_confirm(&a, &b, &relay.url());
    b.shutdown().expect("b goes offline");
    a.send_text("offline clip".to_owned()).expect("send succeeds");

    // Give the relay a moment to persist the mailbox snapshot before b rejoins.
    wait_until(|| {
        std::fs::read_dir(mailbox_dir.path())
            .ok()?
            .filter_map(|entry| entry.ok())
            .any(|entry| entry.file_name().to_string_lossy().ends_with(".json"))
            .then_some(())
    });

    let cb_b2 = RecordingCallbacks::default();
    let b2 = handle(&dir_b, cb_b2.clone());
    assert!(b2.pair_load().expect("pair_load succeeds"), "pairing durable");
    b2.start().expect("b rejoins");

    let texts = wait_until(|| {
        let texts = cb_b2.mailbox_texts();
        (!texts.is_empty()).then_some(texts)
    });
    assert_eq!(texts, ["offline clip"]);
    let history = wait_until(|| b2.history().ok().filter(|items| !items.is_empty()));
    assert_eq!(history.len(), 1);
    assert_eq!(
        history[0].source,
        clipboard_core::ffi::FfiHistorySource::Remote,
        "Applied mailbox clip is promoted to Remote"
    );

    a.shutdown().expect("a shuts down");
    b2.shutdown().expect("b shuts down");
}

/// The host history API supports the two T15 actions without bypassing Rust
/// persistence: applying a deferred item promotes it, and clearing removes
/// the durable entry.
#[test]
fn ffi_smoke_history_apply_and_clear() {
    let mailbox_dir = tempdir();
    let relay = Relay::start(Some(&mailbox_dir));
    let dir_a = tempdir();
    let dir_b = tempdir();
    let cb_a = RecordingCallbacks::default();
    let cb_b = RecordingCallbacks::default();
    *cb_b.inner.mailbox_disposition.lock().unwrap() = Some(MailboxDisposition::Deferred);
    let a = handle(&dir_a, cb_a);
    let b = handle(&dir_b, cb_b);

    pair_and_confirm(&a, &b, &relay.url());
    b.shutdown().expect("b goes offline");
    a.send_text("apply me later".to_owned()).expect("send succeeds");
    wait_until(|| {
        std::fs::read_dir(mailbox_dir.path())
            .ok()?
            .filter_map(|entry| entry.ok())
            .any(|entry| entry.file_name().to_string_lossy().ends_with(".json"))
            .then_some(())
    });

    let deferred_callbacks = RecordingCallbacks::default();
    *deferred_callbacks
        .inner
        .mailbox_disposition
        .lock()
        .unwrap() = Some(MailboxDisposition::Deferred);
    let b2 = handle(&dir_b, deferred_callbacks);
    assert!(b2.pair_load().expect("pair_load succeeds"));
    b2.start().expect("b rejoins");

    let item = wait_until(|| {
        b2.history().ok()?.into_iter().find(|item| {
            item.source == clipboard_core::ffi::FfiHistorySource::RemoteDeferred
        })
    });
    b2.history_apply(item.id.clone())
        .expect("deferred item applies");
    assert_eq!(
        b2.history().expect("history remains readable")[0].source,
        clipboard_core::ffi::FfiHistorySource::Remote
    );

    b2.history_clear().expect("history clears");
    assert!(b2.history().expect("empty history remains readable").is_empty());

    a.shutdown().expect("a shuts down");
    b2.shutdown().expect("b shuts down");
}

/// Durable history belongs to the device, not the relay connection. A host
/// can browse, apply, and clear it before a session is connected.
#[test]
fn ffi_smoke_history_remains_available_without_connection() {
    let dir = tempdir();
    let id = uuid::Uuid::new_v4();
    let mut store = HistoryStore::new(dir.path().join("history")).expect("history opens");
    store
        .add(NewHistoryItem {
            id,
            ts_ms: 42,
            source: HistorySource::RemoteDeferred,
            content: HistoryContent::Text {
                content: "durable while offline".to_owned(),
            },
        })
        .expect("fixture persists");
    drop(store);

    let core = handle(&dir, RecordingCallbacks::default());
    let items = core.history().expect("offline history is readable");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, id.to_string());

    core.history_apply(id.to_string())
        .expect("offline deferred item applies");
    assert_eq!(
        core.history().expect("promoted history")[0].source,
        clipboard_core::ffi::FfiHistorySource::Remote
    );

    core.history_clear().expect("offline history clears");
    assert!(core.history().expect("empty history is readable").is_empty());
    core.shutdown().expect("core shuts down");
}

/// QA failure scenario: a callback returning Err never kills the session —
/// the error is logged and surfaced, and the next clip is still delivered.
#[test]
fn ffi_smoke_callback_error_session_survives() {
    let relay = Relay::start(None);
    let dir_a = tempdir();
    let dir_b = tempdir();
    let cb_a = RecordingCallbacks::default();
    let cb_b = RecordingCallbacks::default();
    let a = handle(&dir_a, cb_a);
    let b = handle(&dir_b, cb_b.clone());

    pair_and_confirm(&a, &b, &relay.url());

    cb_b.inner.fail_next_live.store(true, Ordering::SeqCst);
    a.send_text("rejected clip".to_owned()).expect("send succeeds");
    wait_until(|| cb_b.saw_error_status().then_some(()));

    a.send_text("accepted clip".to_owned())
        .expect("session still sends after a callback error");
    let texts = wait_live_texts(&cb_b, 1);
    assert_eq!(
        texts,
        ["accepted clip"],
        "the session survives a rejected callback and keeps delivering"
    );

    let history = b.history().expect("history readable after callback error");
    assert_eq!(
        history.len(),
        2,
        "history keeps both clips, including the rejected one"
    );

    a.shutdown().expect("a shuts down");
    b.shutdown().expect("b shuts down");
}

/// Bounded FIFO dispatcher: strict ordering, typed QueueFull backpressure,
/// and a shutdown that actually joins the dispatcher.
#[test]
fn ffi_smoke_fifo_backpressure_shutdown_join() {
    let relay = Relay::start(None);
    let dir_a = tempdir();
    let dir_b = tempdir();
    let cb_a = RecordingCallbacks::default();
    let cb_b = RecordingCallbacks::default();
    let a = CoreHandle::with_queue_capacity(
        dir_a.path().to_string_lossy().into_owned(),
        Box::new(cb_a),
        2,
    )
    .expect("core handle opens");
    let b = handle(&dir_b, cb_b.clone());

    pair_and_confirm(&a, &b, &relay.url());

    let stall = a.stall_dispatcher_for_test().expect("dispatcher stalls");
    a.try_send_text_for_test("one".to_owned())
        .expect("first clip fits the queue");
    a.try_send_text_for_test("two".to_owned())
        .expect("second clip fits the queue");
    let full = a
        .try_send_text_for_test("three".to_owned())
        .expect_err("a full queue must fail");
    assert!(
        matches!(full, CoreError::QueueFull),
        "backpressure is the typed QueueFull error, got {full}"
    );
    drop(stall);

    let texts = wait_live_texts(&cb_b, 2);
    assert_eq!(texts, ["one", "two"], "dispatcher drains in FIFO order");

    let seq = a.send_text("three".to_owned()).expect("queue drains");
    assert_eq!(seq, 3);
    let texts = wait_live_texts(&cb_b, 3);
    assert_eq!(texts, ["one", "two", "three"]);

    // Shutdown must join the dispatcher: the call returns only after the task
    // exited, well inside the timeout.
    let shutdown = {
        let a = a.clone();
        std::thread::spawn(move || a.shutdown())
    };
    let joined = shutdown.join().expect("shutdown thread finishes");
    joined.expect("shutdown succeeds");
    assert!(
        matches!(a.send_text("late".to_owned()), Err(CoreError::Shutdown)),
        "a shut-down handle rejects further sends"
    );
    assert!(
        matches!(a.shutdown(), Err(CoreError::Shutdown)),
        "shutdown is terminal"
    );

    b.shutdown().expect("b shuts down");
}

/// Reset generation-fault contract: reset stops and joins everything, rotates
/// the identity generation, drops the old pairing, returns to ReadyUnpaired,
/// and the handle immediately pairs again on the fresh dispatcher/runtime.
#[test]
fn ffi_smoke_reset_pairing_generation_fault_and_repair() {
    let relay = Relay::start(None);
    let dir_a = tempdir();
    let dir_b = tempdir();
    let cb_a = RecordingCallbacks::default();
    let cb_b = RecordingCallbacks::default();
    let a = handle(&dir_a, cb_a);
    let b = handle(&dir_b, cb_b.clone());

    pair_and_confirm(&a, &b, &relay.url());
    a.send_text("before reset".to_owned()).expect("send succeeds");
    wait_live_texts(&cb_b, 1);
    assert_eq!(a.identity_generation_for_test().unwrap(), 1);
    assert_eq!(b.identity_generation_for_test().unwrap(), 1);

    a.reset_pairing().expect("reset succeeds");
    assert_eq!(a.pair_poll(), PairingSnapshot::Unpaired);
    assert_eq!(
        a.identity_generation_for_test().unwrap(),
        2,
        "reset rolls the identity generation forward"
    );
    assert!(
        !a.pair_load().expect("pair_load succeeds"),
        "the old-generation pairing is gone after reset"
    );

    b.reset_pairing().expect("peer reset succeeds");
    assert_eq!(b.identity_generation_for_test().unwrap(), 2);

    // Cancel coverage: begin an offer, cancel it, then pair for real.
    let _qr = a.pair_begin(relay.url()).expect("pair_begin succeeds");
    a.pair_cancel().expect("cancel succeeds");
    assert_eq!(a.pair_poll(), PairingSnapshot::Unpaired);

    let cb_b2 = RecordingCallbacks::default();
    let b_fresh = CoreHandle::new(
        dir_b.path().to_string_lossy().into_owned(),
        Box::new(cb_b2.clone()),
    )
    .expect("core handle reopens");
    assert_eq!(b_fresh.identity_generation_for_test().unwrap(), 2);

    pair_and_confirm(&a, &b_fresh, &relay.url());
    a.send_text("after reset".to_owned())
        .expect("fresh dispatcher pairs and sends immediately");
    let texts = wait_live_texts(&cb_b2, 1);
    assert_eq!(texts, ["after reset"]);

    a.shutdown().expect("a shuts down");
    b_fresh.shutdown().expect("b shuts down");
}
