//! 回显抑制集成测试：executor 线程在 SendText 前调用阻塞式 is_echo，
//! 命中回显环则跳过发送。夹具为真 CoreBridge/CoreHandle + 本地 relay
//! （无 mock），配对闭环对齐 crates/core/tests/ffi_smoke.rs。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clipboard_core::ffi::{
    CoreCallbacks, CoreError, CoreHandle, CoreStatus, FfiClipContent, FfiClipItem,
    MailboxDisposition, PairingSnapshot,
};
use clipboard_core::session::text_hash;
use clipsync_desktop::clipboard_monitor::{
    ClipboardIo, ClipboardPayload, MonitorHandle, MonitorState,
};
use clipsync_desktop::core_bridge::{BridgeCommand, CoreBridge, CoreEvent};
use clipboard_server::{
    InMemoryRegistry, Limits, PairingConfig, PairingRelay, ServerConfig, ServerHandle, start,
};

const WAIT: Duration = Duration::from_secs(10);

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
                Arc::new(clipboard_server::NoopMailboxSink),
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

/// 桌面端注入的假剪贴板：apply_remote 写入内存，避免触碰系统剪贴板。
#[derive(Clone, Default)]
struct FakeClipboard {
    content: Arc<Mutex<Option<ClipboardPayload>>>,
}

impl ClipboardIo for FakeClipboard {
    fn read(&mut self) -> Option<ClipboardPayload> {
        self.content.lock().unwrap().clone()
    }

    fn write(&mut self, payload: &ClipboardPayload) -> bool {
        *self.content.lock().unwrap() = Some(payload.clone());
        true
    }
}

/// 对端（手机侧）录制回调：只记收到的文本。
#[derive(Clone, Default)]
struct RecordingCallbacks {
    texts: Arc<Mutex<Vec<String>>>,
}

impl CoreCallbacks for RecordingCallbacks {
    fn on_clip(&self, item: FfiClipItem) -> Result<(), CoreError> {
        if let FfiClipContent::Text { text } = item.content {
            self.texts.lock().unwrap().push(text);
        }
        Ok(())
    }

    fn on_mailbox_clip(&self, _item: FfiClipItem) -> Result<MailboxDisposition, CoreError> {
        Ok(MailboxDisposition::Applied)
    }

    fn on_status(&self, _status: CoreStatus) {}
}

#[test]
fn executor_suppresses_echo_send() {
    let relay = Relay::start();
    let dir_a = tempfile::tempdir().expect("temp dir a");
    let dir_b = tempfile::tempdir().expect("temp dir b");

    // A 侧：被测桌面桥（走 BridgeCommand → executor 线程）。
    let state = Arc::new(Mutex::new(MonitorState::default()));
    let fake = FakeClipboard::default();
    let monitor = MonitorHandle::with_io(state, fake.clone());
    let (event_tx, event_rx) = std::sync::mpsc::channel();
    let a = CoreBridge::spawn(dir_a.path(), monitor, event_tx).expect("bridge spawns");

    // B 侧：裸 CoreHandle 扮演手机。
    let cb_b = RecordingCallbacks::default();
    let b = CoreHandle::new(
        dir_b.path().to_string_lossy().into_owned(),
        Box::new(cb_b.clone()),
    )
    .expect("core handle opens");

    // 配对闭环：A 走桥命令 + 事件，B 直接调 handle。
    a.send(BridgeCommand::PairBegin(relay.url()));
    let qr = wait_until(|| {
        event_rx.try_recv().ok().and_then(|event| match event {
            CoreEvent::QrReady(Ok(qr)) => Some(qr),
            _ => None,
        })
    });
    let sas_b = b.pair_claim(qr).expect("pair_claim succeeds");
    let sas_a = wait_until(|| match a.pair_poll() {
        PairingSnapshot::SasReady { sas } => Some(sas),
        _ => None,
    });
    assert_eq!(sas_a, sas_b, "both sides must display the same SAS");
    a.send(BridgeCommand::PairConfirm(sas_a));
    b.pair_confirm(sas_b).expect("claimer confirm succeeds");
    wait_until(|| matches!(a.pair_poll(), PairingSnapshot::Paired { .. }).then_some(()));
    wait_until(|| matches!(b.pair_poll(), PairingSnapshot::Paired { .. }).then_some(()));

    // B → A 下发文本：A 应用后进回显环（session 收到即登记）。
    b.send_text("echo me".to_owned()).expect("send succeeds");
    wait_until(|| {
        matches!(&*fake.content.lock().unwrap(), Some(ClipboardPayload::Text(t)) if t == "echo me")
            .then_some(())
    });
    wait_until(|| a.is_echo(text_hash("echo me")).then_some(()));

    // A 侧上报同一文本应被 executor 跳过；紧随的探针必须送达。
    // 两条命令在 executor 上顺序执行、同一条 FIFO 链路投递：
    // 收到探针即证明回显条已被处理且未上线。
    a.send(BridgeCommand::SendText("echo me".to_owned()));
    a.send(BridgeCommand::SendText("probe".to_owned()));
    let received = wait_until(|| {
        let texts = cb_b.texts.lock().unwrap().clone();
        texts.iter().any(|t| t == "probe").then_some(texts)
    });
    assert_eq!(received, ["probe"], "echo send must be suppressed by executor");

    a.shutdown();
    b.shutdown().expect("b shuts down");
}
