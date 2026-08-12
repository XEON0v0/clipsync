//! 桌面壳全链路：两个 CoreBridge 经真实 relay 配对、文本互发、
//! 远端应用写入假剪贴板且 ownership 生效。

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clipboard_core::ffi::{CoreStatus, FfiClipContent, PairingSnapshot};
use clipboard_server::{
    InMemoryRegistry, Limits, PairingConfig, PairingRelay, ServerConfig, ServerHandle, start,
};
use clipsync_desktop::clipboard_monitor::{
    ClipboardIo, ClipboardPayload, MonitorHandle, MonitorState,
};
use clipsync_desktop::core_bridge::{BridgeCommand, CoreBridge, CoreEvent};

const WAIT: Duration = Duration::from_secs(15);
const DELIVERY_QUIESCENCE: Duration = Duration::from_millis(200);

#[derive(Clone, Default)]
struct FakeClipboard {
    inner: Arc<Mutex<FakeInner>>,
}

#[derive(Default)]
struct FakeInner {
    content: Option<ClipboardPayload>,
    writes: Vec<ClipboardPayload>,
}

impl ClipboardIo for FakeClipboard {
    fn read(&mut self) -> Option<ClipboardPayload> {
        self.inner.lock().unwrap().content.clone()
    }

    fn write(&mut self, payload: &ClipboardPayload) -> bool {
        let mut guard = self.inner.lock().unwrap();
        guard.content = Some(payload.clone());
        guard.writes.push(payload.clone());
        true
    }
}

impl FakeClipboard {
    fn writes(&self) -> Vec<ClipboardPayload> {
        self.inner.lock().unwrap().writes.clone()
    }
}

struct Relay {
    _runtime: tokio::runtime::Runtime,
    handle: ServerHandle,
}

impl Relay {
    fn start() -> Self {
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
        let mailbox = Arc::new(clipboard_server::NoopMailboxSink);
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

fn fail_on_fatal(event: &CoreEvent, awaited: &str, observed: &[String]) {
    match event {
        CoreEvent::QrReady(Err(error)) => {
            panic!("QR generation failed while waiting for {awaited}: {error}; observed={observed:?}")
        }
        CoreEvent::PairConfirmed(Err(error)) => {
            panic!(
                "pair confirmation failed while waiting for {awaited}: {error}; observed={observed:?}"
            )
        }
        CoreEvent::Status(CoreStatus::Error { message }) => {
            panic!("core error while waiting for {awaited}: {message}; observed={observed:?}")
        }
        CoreEvent::Status(CoreStatus::Disconnected) => {
            panic!("core disconnected while waiting for {awaited}; observed={observed:?}")
        }
        _ => {}
    }
}

fn recv_before(
    rx: &Receiver<CoreEvent>,
    deadline: Instant,
    awaited: &str,
    observed: &[String],
) -> CoreEvent {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        panic!("timed out waiting for {awaited}; observed={observed:?}");
    }
    match rx.recv_timeout(remaining) {
        Ok(event) => {
            fail_on_fatal(&event, awaited, observed);
            event
        }
        Err(RecvTimeoutError::Timeout) => {
            panic!("timed out waiting for {awaited}; observed={observed:?}")
        }
        Err(RecvTimeoutError::Disconnected) => {
            panic!("event channel disconnected while waiting for {awaited}; observed={observed:?}")
        }
    }
}

fn wait_qr(rx: &Receiver<CoreEvent>) -> String {
    let deadline = Instant::now() + WAIT;
    let mut observed = Vec::new();
    loop {
        let event = recv_before(rx, deadline, "successful QR payload", &observed);
        match event {
            CoreEvent::QrReady(Ok(qr)) => return qr,
            other => observed.push(format!("{other:?}")),
        }
    }
}

fn wait_confirmed_and_connected(rx: &Receiver<CoreEvent>) {
    let deadline = Instant::now() + WAIT;
    let mut confirmed = false;
    let mut connected = false;
    let mut observed = Vec::new();
    while !confirmed || !connected {
        let awaited = format!(
            "pair confirmation and connection (confirmed={confirmed}, connected={connected})"
        );
        let event = recv_before(rx, deadline, &awaited, &observed);
        match &event {
            CoreEvent::PairConfirmed(Ok(())) => confirmed = true,
            CoreEvent::Status(CoreStatus::Connected) => connected = true,
            _ => {}
        }
        observed.push(format!("{event:?}"));
    }
}

fn wait_sas(bridge: &CoreBridge, rx: &Receiver<CoreEvent>) -> String {
    let deadline = Instant::now() + WAIT;
    let mut observed = Vec::new();
    let mut last_snapshot = PairingSnapshot::Unpaired;
    loop {
        let snapshot = bridge.pair_poll();
        if let PairingSnapshot::SasReady { sas } = snapshot {
            return sas;
        }
        last_snapshot = snapshot;

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            panic!(
                "timed out waiting for offerer SAS; last_snapshot={last_snapshot:?}; observed={observed:?}"
            );
        }
        match rx.recv_timeout(remaining.min(Duration::from_millis(25))) {
            Ok(event) => {
                fail_on_fatal(&event, "offerer SAS", &observed);
                observed.push(format!("{event:?}"));
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                panic!(
                    "event channel disconnected while waiting for offerer SAS; last_snapshot={last_snapshot:?}; observed={observed:?}"
                )
            }
        }
    }
}

fn wait_text_delivery(
    rx: &Receiver<CoreEvent>,
    clipboard: &FakeClipboard,
    side: &str,
    expected_text: &str,
) {
    let awaited = format!("{side} live text {expected_text:?}");
    let deadline = Instant::now() + WAIT;
    let mut observed = Vec::new();
    loop {
        let event = recv_before(rx, deadline, &awaited, &observed);
        match &event {
            CoreEvent::LiveClip(item) => match &item.content {
                FfiClipContent::Text { text } if text == expected_text => break,
                content => panic!(
                    "unexpected live clipboard content while waiting for {awaited}: {content:?}; observed={observed:?}"
                ),
            },
            _ => observed.push(format!("{event:?}")),
        }
    }

    let quiet_deadline = Instant::now() + DELIVERY_QUIESCENCE;
    loop {
        let remaining = quiet_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match rx.recv_timeout(remaining) {
            Ok(event) => {
                fail_on_fatal(&event, "delivery quiescence", &observed);
                if matches!(event, CoreEvent::LiveClip(_)) {
                    panic!(
                        "duplicate live clipboard event during {side} delivery quiescence: {event:?}; observed={observed:?}"
                    );
                }
                observed.push(format!("{event:?}"));
            }
            Err(RecvTimeoutError::Timeout) => break,
            Err(RecvTimeoutError::Disconnected) => {
                panic!(
                    "event channel disconnected during {side} delivery quiescence; observed={observed:?}"
                )
            }
        }
    }

    assert_eq!(
        clipboard.writes(),
        vec![ClipboardPayload::Text(expected_text.to_owned())],
        "{side} clipboard write sequence after delivery quiescence; observed={observed:?}"
    );
}

struct Side {
    bridge: Option<CoreBridge>,
    rx: Receiver<CoreEvent>,
    clipboard: FakeClipboard,
    _dir: tempfile::TempDir,
}

impl Side {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let clipboard = FakeClipboard::default();
        let state = Arc::new(Mutex::new(MonitorState::default()));
        let monitor = MonitorHandle::with_io(state, clipboard.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        let bridge = CoreBridge::spawn(dir.path(), monitor, tx).unwrap();
        Side {
            bridge: Some(bridge),
            rx,
            clipboard,
            _dir: dir,
        }
    }

    fn bridge(&self) -> &CoreBridge {
        self.bridge.as_ref().expect("bridge is running")
    }

    fn shutdown(&mut self) {
        if let Some(bridge) = self.bridge.take() {
            bridge.shutdown();
        }
    }
}

impl Drop for Side {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[test]
fn desktop_bridge_pairing_and_live_text_roundtrip() {
    let relay = Relay::start();
    let mut a = Side::new();
    let mut b = Side::new();

    a.bridge().send(BridgeCommand::PairBegin(relay.url()));
    let qr = wait_qr(&a.rx);

    let sas_b = b
        .bridge()
        .pair_claim_for_test(&qr)
        .expect("claim succeeds");
    let sas_a = wait_sas(a.bridge(), &a.rx);
    assert_eq!(sas_a, sas_b);
    a.bridge().send(BridgeCommand::PairConfirm(sas_a));
    wait_confirmed_and_connected(&a.rx);
    b.bridge().send(BridgeCommand::PairConfirm(sas_b));
    wait_confirmed_and_connected(&b.rx);

    a.bridge()
        .send(BridgeCommand::SendText("hello desktop".to_owned()));
    wait_text_delivery(&b.rx, &b.clipboard, "B", "hello desktop");

    let payload = ClipboardPayload::Text("hello desktop".to_owned());
    let decision = {
        let state = b.bridge().bridge_monitor_state();
        let mut guard = state.lock().unwrap();
        clipsync_desktop::clipboard_monitor::decide_poll(&mut guard, Some(&payload))
    };
    assert_eq!(
        decision,
        clipsync_desktop::clipboard_monitor::PollDecision::OwnWrite
    );

    b.bridge()
        .send(BridgeCommand::SendText("reply".to_owned()));
    wait_text_delivery(&a.rx, &a.clipboard, "A", "reply");

    a.shutdown();
    b.shutdown();
}
