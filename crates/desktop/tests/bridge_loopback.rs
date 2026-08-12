//! 桌面壳全链路：两个 CoreBridge 经真实 relay 配对、文本互发、
//! 远端应用写入假剪贴板且 ownership 生效。

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clipboard_server::{
    InMemoryRegistry, Limits, PairingConfig, PairingRelay, ServerConfig, ServerHandle, start,
};
use clipsync_desktop::clipboard_monitor::{
    ClipboardIo, ClipboardPayload, MonitorHandle, MonitorState,
};
use clipsync_desktop::core_bridge::{BridgeCommand, CoreBridge, CoreEvent};

const WAIT: Duration = Duration::from_secs(15);

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

fn wait_event(rx: &Receiver<CoreEvent>, matches: impl Fn(&CoreEvent) -> bool) -> CoreEvent {
    let started = Instant::now();
    loop {
        if let Ok(event) = rx.recv_timeout(Duration::from_millis(100)) {
            if matches(&event) {
                return event;
            }
        }
        assert!(started.elapsed() < WAIT, "timed out waiting for event");
    }
}

fn wait_confirmed_and_connected(rx: &Receiver<CoreEvent>) {
    let started = Instant::now();
    let mut confirmed = false;
    let mut connected = false;
    while !confirmed || !connected {
        if let Ok(event) = rx.recv_timeout(Duration::from_millis(100)) {
            match event {
                CoreEvent::PairConfirmed(Ok(())) => confirmed = true,
                CoreEvent::PairConfirmed(Err(error)) => {
                    panic!("pair confirmation failed: {error}")
                }
                CoreEvent::Status(clipboard_core::ffi::CoreStatus::Connected) => connected = true,
                _ => {}
            }
        }
        assert!(
            started.elapsed() < WAIT,
            "timed out waiting for pairing confirmation and connection"
        );
    }
}

fn wait_until(cond: impl Fn() -> bool) {
    let started = Instant::now();
    while !cond() {
        assert!(started.elapsed() < WAIT, "timed out waiting for condition");
        std::thread::sleep(Duration::from_millis(25));
    }
}

struct Side {
    bridge: CoreBridge,
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
            bridge,
            rx,
            clipboard,
            _dir: dir,
        }
    }
}

#[test]
fn desktop_bridge_pairing_and_live_text_roundtrip() {
    let relay = Relay::start();
    let a = Side::new();
    let b = Side::new();

    a.bridge.send(BridgeCommand::PairBegin(relay.url()));
    let qr = match wait_event(&a.rx, |e| matches!(e, CoreEvent::QrReady(Ok(_)))) {
        CoreEvent::QrReady(Ok(qr)) => qr,
        _ => unreachable!(),
    };

    let sas_b = b.bridge.pair_claim_for_test(&qr).expect("claim succeeds");
    wait_until(|| {
        matches!(
            a.bridge.pair_poll(),
            clipboard_core::ffi::PairingSnapshot::SasReady { .. }
        )
    });
    let sas_a = match a.bridge.pair_poll() {
        clipboard_core::ffi::PairingSnapshot::SasReady { sas } => sas,
        _ => unreachable!(),
    };
    assert_eq!(sas_a, sas_b);
    a.bridge.send(BridgeCommand::PairConfirm(sas_a));
    wait_confirmed_and_connected(&a.rx);
    b.bridge.send(BridgeCommand::PairConfirm(sas_b));
    wait_confirmed_and_connected(&b.rx);

    a.bridge
        .send(BridgeCommand::SendText("hello desktop".to_owned()));
    wait_event(&b.rx, |event| {
        matches!(
            event,
            CoreEvent::LiveClip(clipboard_core::ffi::FfiClipItem {
                content: clipboard_core::ffi::FfiClipContent::Text { text },
                ..
            }) if text == "hello desktop"
        )
    });
    assert!(
        b.clipboard.writes().iter().any(|payload| {
            matches!(
                payload,
                ClipboardPayload::Text(text) if text == "hello desktop"
            )
        }),
        "remote text must be written to B's clipboard"
    );

    let payload = ClipboardPayload::Text("hello desktop".to_owned());
    let decision = {
        let state = b.bridge.bridge_monitor_state();
        let mut guard = state.lock().unwrap();
        clipsync_desktop::clipboard_monitor::decide_poll(&mut guard, Some(&payload))
    };
    assert_eq!(
        decision,
        clipsync_desktop::clipboard_monitor::PollDecision::OwnWrite
    );

    b.bridge.send(BridgeCommand::SendText("reply".to_owned()));
    wait_until(|| {
        a.clipboard
            .writes()
            .iter()
            .any(|payload| matches!(payload, ClipboardPayload::Text(text) if text == "reply"))
    });

    a.bridge.shutdown();
    b.bridge.shutdown();
}
