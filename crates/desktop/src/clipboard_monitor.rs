//! 剪贴板监控：300ms 轮询 + digest ownership token。
//! macOS PasteboardMonitor 用 NSPasteboard.changeCount；arboard 无 change
//! counter，这里用内容 digest 等价实现（text_hash / sha256(png)）。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use clipboard_core::ffi::MailboxDisposition;
use sha2::{Digest, Sha256};

/// 轮询间隔，对齐 macOS PasteboardMonitor.pollInterval。
pub const POLL_INTERVAL: Duration = Duration::from_millis(300);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClipboardPayload {
    Text(String),
    /// PNG 编码字节（协议线上格式，≤10MiB 由 core 约束）。
    ImagePng(Vec<u8>),
}

/// 内容指纹：文本走 core 的 text_hash 契约，图片走 sha256(png)。
pub fn digest(payload: &ClipboardPayload) -> String {
    match payload {
        ClipboardPayload::Text(text) => clipboard_core::session::text_hash(text),
        ClipboardPayload::ImagePng(bytes) => {
            let d = Sha256::digest(bytes);
            d.iter().map(|b| format!("{b:02x}")).collect()
        }
    }
}

/// 平台剪贴板读写抽象；测试用 FakeClipboard 注入。
pub trait ClipboardIo: Send {
    fn read(&mut self) -> Option<ClipboardPayload>;
    fn write(&mut self, payload: &ClipboardPayload) -> bool;
}

/// 监控共享状态（轮询线程 / core 回调线程 / UI 都可能触碰）。
#[derive(Default)]
pub struct MonitorState {
    last_digest: Option<String>,
    ownership_digest: Option<String>,
    disconnect_baseline: Option<String>,
}

impl MonitorState {
    pub fn mark_disconnected(&mut self) {
        self.disconnect_baseline = self.last_digest.clone();
    }
    pub fn mark_connected(&mut self) {
        self.disconnect_baseline = None;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PollDecision {
    Unchanged,
    OwnWrite,
    LocalChange,
}

/// 一轮轮询的纯决策：空剪贴板视为"无变化"并重置基线。
pub fn decide_poll(state: &mut MonitorState, current: Option<&ClipboardPayload>) -> PollDecision {
    let Some(payload) = current else {
        state.last_digest = None;
        return PollDecision::Unchanged;
    };
    let d = digest(payload);
    if state.ownership_digest.as_deref() == Some(d.as_str()) {
        state.ownership_digest = None;
        state.last_digest = Some(d);
        return PollDecision::OwnWrite;
    }
    if state.last_digest.as_deref() == Some(d.as_str()) {
        return PollDecision::Unchanged;
    }
    state.last_digest = Some(d);
    PollDecision::LocalChange
}

/// mailbox clip 的处置：断线期间剪贴板被改动过 → Deferred，否则 Applied。
pub fn decide_mailbox(
    state: &MonitorState,
    current: Option<&ClipboardPayload>,
) -> MailboxDisposition {
    match &state.disconnect_baseline {
        None => MailboxDisposition::Applied,
        Some(baseline) => {
            let now = current.map(digest);
            if now.as_ref() == Some(baseline) {
                MailboxDisposition::Applied
            } else {
                MailboxDisposition::Deferred
            }
        }
    }
}

/// 本壳写入剪贴板后记录 ownership（mailbox 应用同时推进 baseline）。
pub fn record_write(state: &mut MonitorState, payload: &ClipboardPayload, mailbox: bool) {
    let d = digest(payload);
    state.ownership_digest = Some(d.clone());
    state.last_digest = Some(d.clone());
    if mailbox {
        state.disconnect_baseline = Some(d);
    }
}

/// arboard 实现。Windows 上剪贴板被占用时读失败 → 当轮视为空（跳过）。
pub struct ArboardIo {
    inner: arboard::Clipboard,
}

impl ArboardIo {
    pub fn new() -> Option<Self> {
        arboard::Clipboard::new().ok().map(|inner| Self { inner })
    }
}

impl ClipboardIo for ArboardIo {
    fn read(&mut self) -> Option<ClipboardPayload> {
        if let Ok(text) = self.inner.get_text() {
            if !text.is_empty() {
                return Some(ClipboardPayload::Text(text));
            }
        }
        if let Ok(img) = self.inner.get_image() {
            return rgba_to_png(img.width as u32, img.height as u32, &img.bytes)
                .map(ClipboardPayload::ImagePng);
        }
        None
    }

    fn write(&mut self, payload: &ClipboardPayload) -> bool {
        match payload {
            ClipboardPayload::Text(text) => self.inner.set_text(text.clone()).is_ok(),
            ClipboardPayload::ImagePng(bytes) => {
                match png_to_rgba(bytes) {
                    Some((w, h, rgba)) => self
                        .inner
                        .set_image(arboard::ImageData {
                            width: w as usize,
                            height: h as usize,
                            bytes: rgba.into(),
                        })
                        .is_ok(),
                    None => false,
                }
            }
        }
    }
}

/// RGBA → PNG（image crate 只启 png feature，够用）。
pub fn rgba_to_png(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let buffer = image::RgbaImage::from_raw(width, height, rgba.to_vec())?;
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(buffer)
        .write_to(&mut out, image::ImageFormat::Png)
        .ok()?;
    Some(out.into_inner())
}

/// PNG → (width, height, RGBA)。
pub fn png_to_rgba(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    Some((w, h, img.into_raw()))
}

/// 给 core 回调线程使用的远程应用入口。每次操作新建 Clipboard 实例
/// （arboard 非 Sync，且回调线程与轮询线程各自持有实例）。
#[derive(Clone)]
pub struct MonitorHandle {
    state: Arc<Mutex<MonitorState>>,
    factory: Arc<dyn Fn() -> Option<Box<dyn ClipboardIo>> + Send + Sync>,
}

impl MonitorHandle {
    /// 生产实现：回调线程内即时构造 arboard Clipboard。
    pub fn new(state: Arc<Mutex<MonitorState>>) -> Self {
        Self {
            state,
            factory: Arc::new(|| {
                ArboardIo::new().map(|io| Box::new(io) as Box<dyn ClipboardIo>)
            }),
        }
    }

    /// 测试/集成测试注入假剪贴板；工厂每次克隆，可多次应用。
    #[doc(hidden)]
    pub fn with_io(state: Arc<Mutex<MonitorState>>, io: impl ClipboardIo + Clone + Sync + 'static) -> Self {
        Self {
            state,
            factory: Arc::new(move || {
                Some(Box::new(io.clone()) as Box<dyn ClipboardIo>)
            }),
        }
    }

    pub fn state(&self) -> Arc<Mutex<MonitorState>> {
        self.state.clone()
    }

    /// 把远端 clip 写入本机剪贴板。mailbox=true 时先按 baseline 决策；
    /// 返回是否真正写入（false = 写失败或应 Deferred，由调用方决定语义）。
    pub fn apply_remote(&self, payload: &ClipboardPayload, mailbox: bool) -> bool {
        let mut io = match (self.factory)() {
            Some(io) => io,
            None => return false,
        };
        if mailbox {
            let current = io.read();
            let decision = {
                let state = self.state.lock().unwrap();
                decide_mailbox(&state, current.as_ref())
            };
            if decision == MailboxDisposition::Deferred {
                return false;
            }
        }
        if !io.write(payload) {
            return false;
        }
        let mut state = self.state.lock().unwrap();
        record_write(&mut state, payload, mailbox);
        true
    }

    pub fn mark_disconnected(&self) {
        self.state.lock().unwrap().mark_disconnected();
    }

    pub fn mark_connected(&self) {
        self.state.lock().unwrap().mark_connected();
    }
}

/// 启动轮询线程；本地变更通过 tx 上报（决策由 decide_poll 完成）。
pub fn spawn_poller(
    mut io: impl ClipboardIo + 'static,
    state: Arc<Mutex<MonitorState>>,
    tx: std::sync::mpsc::Sender<ClipboardPayload>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("clipsync-clipboard-monitor".to_owned())
        .spawn(move || loop {
            let current = io.read();
            let decision = {
                let mut guard = state.lock().unwrap();
                decide_poll(&mut guard, current.as_ref())
            };
            if decision == PollDecision::LocalChange
                && let Some(payload) = current
                && tx.send(payload).is_err()
            {
                break; // UI 已退出
            }
            std::thread::sleep(POLL_INTERVAL);
        })
        .expect("clipboard monitor thread spawns")
}

#[cfg(test)]
mod tests {
    use super::*;
    use clipboard_core::ffi::MailboxDisposition;

    fn text(s: &str) -> ClipboardPayload { ClipboardPayload::Text(s.to_owned()) }

    #[test]
    fn digest_is_stable_and_kind_sensitive() {
        assert_eq!(digest(&text("hello")), clipboard_core::session::text_hash("hello"));
        assert_ne!(digest(&text("a")), digest(&text("b")));
        assert_ne!(
            digest(&ClipboardPayload::ImagePng(vec![1, 2, 3])),
            digest(&ClipboardPayload::ImagePng(vec![1, 2, 4]))
        );
    }

    #[test]
    fn poll_unchanged_when_digest_matches() {
        let mut state = MonitorState::default();
        let payload = text("x");
        assert_eq!(decide_poll(&mut state, Some(&payload)), PollDecision::LocalChange);
        assert_eq!(decide_poll(&mut state, Some(&payload)), PollDecision::Unchanged);
    }

    #[test]
    fn poll_consumes_own_write_once() {
        let mut state = MonitorState::default();
        let payload = text("remote clip");
        record_write(&mut state, &payload, false);
        assert_eq!(decide_poll(&mut state, Some(&payload)), PollDecision::OwnWrite);
        // 之后再变化仍是本地变更
        let other = text("user typed");
        assert_eq!(decide_poll(&mut state, Some(&other)), PollDecision::LocalChange);
    }

    #[test]
    fn poll_empty_clipboard_is_unchanged() {
        let mut state = MonitorState::default();
        assert_eq!(decide_poll(&mut state, None), PollDecision::Unchanged);
        let payload = text("x");
        assert_eq!(decide_poll(&mut state, Some(&payload)), PollDecision::LocalChange);
        // 剪贴板被清空后再恢复同一内容，视为新的本地变更
        assert_eq!(decide_poll(&mut state, None), PollDecision::Unchanged);
        assert_eq!(decide_poll(&mut state, Some(&payload)), PollDecision::LocalChange);
    }

    #[test]
    fn mailbox_applied_when_baseline_intact() {
        let mut state = MonitorState::default();
        let payload = text("at disconnect");
        decide_poll(&mut state, Some(&payload));
        state.mark_disconnected();
        assert_eq!(
            decide_mailbox(&state, Some(&payload)),
            MailboxDisposition::Applied
        );
    }

    #[test]
    fn mailbox_deferred_when_clipboard_changed_while_disconnected() {
        let mut state = MonitorState::default();
        let payload = text("at disconnect");
        decide_poll(&mut state, Some(&payload));
        state.mark_disconnected();
        let changed = text("user copied during outage");
        assert_eq!(
            decide_mailbox(&state, Some(&changed)),
            MailboxDisposition::Deferred
        );
    }

    #[test]
    fn mailbox_applied_when_connected() {
        let state = MonitorState::default(); // 从未 disconnect
        assert_eq!(
            decide_mailbox(&state, Some(&text("anything"))),
            MailboxDisposition::Applied
        );
    }

    #[test]
    fn mailbox_apply_advances_baseline() {
        let mut state = MonitorState::default();
        let payload = text("at disconnect");
        decide_poll(&mut state, Some(&payload));
        state.mark_disconnected();
        let remote = text("mailbox payload");
        record_write(&mut state, &remote, true);
        // 应用后 baseline 指向新写入内容，下一封 mailbox 不再被误判
        assert_eq!(
            decide_mailbox(&state, Some(&remote)),
            MailboxDisposition::Applied
        );
    }

    #[derive(Clone)]
    struct FakeClipboard {
        content: Option<ClipboardPayload>,
        writes: Vec<ClipboardPayload>,
    }
    impl ClipboardIo for FakeClipboard {
        fn read(&mut self) -> Option<ClipboardPayload> { self.content.clone() }
        fn write(&mut self, payload: &ClipboardPayload) -> bool {
            self.content = Some(payload.clone());
            self.writes.push(payload.clone());
            true
        }
    }

    #[test]
    fn apply_remote_sets_ownership_and_returns_true() {
        let state = Arc::new(Mutex::new(MonitorState::default()));
        let fake = FakeClipboard { content: None, writes: vec![] };
        let handle = MonitorHandle::with_io(state.clone(), fake);
        let payload = text("from peer");
        assert!(handle.apply_remote(&payload, false));
        let guard = state.lock().unwrap();
        assert_eq!(guard.ownership_digest, Some(digest(&payload)));
    }
}
