//! CoreHandle 的桌面壳包装：所有阻塞 FFI 调用在专用 executor 线程上顺序
//! 执行（对应 macOS CoreExecutor），UI 线程只收发 channel 消息。

use std::path::Path;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use clipboard_core::ffi::{
    CoreCallbacks, CoreError, CoreHandle, CoreStatus, FfiClipContent, FfiClipItem,
    FfiHistoryItem, MailboxDisposition, PairingSnapshot,
};

use crate::clipboard_monitor::{ClipboardPayload, MonitorHandle, MonitorState};

#[derive(Debug)]
pub enum BridgeCommand {
    PairBegin(String),
    PairConfirm(String),
    PairCancel,
    LoadAndStart,
    SendText(String),
    SendImage(Vec<u8>),
    HistoryRefresh,
    HistoryImage(String),
    HistoryApply(String),
    HistoryClear,
    ResetPairing,
    Shutdown,
}

#[derive(Debug)]
pub enum CoreEvent {
    Status(CoreStatus),
    LiveClip(FfiClipItem),
    QrReady(Result<String, CoreError>),
    PairConfirmed(Result<(), CoreError>),
    PairCancelled,
    PairedLoaded(bool),
    SessionStarted(Result<(), CoreError>),
    History(Result<Vec<FfiHistoryItem>, CoreError>),
    HistoryImage { id: String, result: Result<Vec<u8>, CoreError> },
    Applied(Result<(), CoreError>),
    Cleared(Result<(), CoreError>),
    ResetDone(Result<(), CoreError>),
}

/// CoreCallbacks → 剪贴板应用 + 事件转发。回调在 core dispatcher 线程上
/// 触发，允许回调内再入 handle（core 契约保证不持锁回调）。
struct BridgeCallbacks {
    monitor: MonitorHandle,
    event_tx: Sender<CoreEvent>,
}

impl BridgeCallbacks {
    fn payload_of(item: &FfiClipItem) -> ClipboardPayload {
        match &item.content {
            FfiClipContent::Text { text } => ClipboardPayload::Text(text.clone()),
            FfiClipContent::Image { bytes } => ClipboardPayload::ImagePng(bytes.clone()),
        }
    }
}

impl CoreCallbacks for BridgeCallbacks {
    fn on_clip(&self, item: FfiClipItem) -> Result<(), CoreError> {
        let payload = Self::payload_of(&item);
        self.monitor.apply_remote(&payload, false);
        let _ = self.event_tx.send(CoreEvent::LiveClip(item));
        Ok(())
    }

    fn on_mailbox_clip(&self, item: FfiClipItem) -> Result<MailboxDisposition, CoreError> {
        let payload = Self::payload_of(&item);
        let applied = self.monitor.apply_remote(&payload, true);
        let _ = self.event_tx.send(CoreEvent::LiveClip(item));
        Ok(if applied {
            MailboxDisposition::Applied
        } else {
            MailboxDisposition::Deferred
        })
    }

    fn on_status(&self, status: CoreStatus) {
        match &status {
            CoreStatus::Connected => self.monitor.mark_connected(),
            CoreStatus::Disconnected => self.monitor.mark_disconnected(),
            _ => {}
        }
        let _ = self.event_tx.send(CoreEvent::Status(status));
    }
}

pub struct CoreBridge {
    tx: Mutex<Option<Sender<BridgeCommand>>>,
    handle: Arc<CoreHandle>,
    monitor_state: Arc<Mutex<MonitorState>>,
    join: Mutex<Option<JoinHandle<()>>>,
}

impl CoreBridge {
    pub fn spawn(
        data_dir: &Path,
        monitor: MonitorHandle,
        event_tx: Sender<CoreEvent>,
    ) -> Result<Self, CoreError> {
        let monitor_state = monitor.state();
        let callbacks = BridgeCallbacks {
            monitor,
            event_tx: event_tx.clone(),
        };
        let handle = CoreHandle::new(
            data_dir.to_string_lossy().into_owned(),
            Box::new(callbacks),
        )?;
        let (tx, rx) = std::sync::mpsc::channel::<BridgeCommand>();
        let executor = std::thread::Builder::new()
            .name("clipsync-core-executor".to_owned())
            .spawn({
                let handle = handle.clone();
                move || run_executor(&handle, rx, &event_tx)
            })
            .map_err(|e| CoreError::Internal(e.to_string()))?;
        Ok(Self {
            tx: Mutex::new(Some(tx)),
            handle,
            monitor_state,
            join: Mutex::new(Some(executor)),
        })
    }

    pub fn send(&self, cmd: BridgeCommand) {
        if let Some(tx) = self.tx.lock().unwrap().as_ref() {
            let _ = tx.send(cmd);
        }
    }

    /// 非阻塞快照，UI 线程每帧调用安全。
    pub fn pair_poll(&self) -> PairingSnapshot {
        self.handle.pair_poll()
    }

    /// 桌面产品形态只做 offerer；claim 仅供 loopback 集成测试驱动对端。
    #[doc(hidden)]
    pub fn pair_claim_for_test(&self, qr_payload: &str) -> Result<String, CoreError> {
        self.handle.pair_claim(qr_payload.to_owned())
    }

    #[doc(hidden)]
    pub fn bridge_monitor_state(&self) -> Arc<Mutex<MonitorState>> {
        self.monitor_state.clone()
    }

    pub fn is_echo(&self, hash: String) -> bool {
        self.handle.is_echo(hash)
    }

    /// 发送 Shutdown、join executor 线程、关闭 core handle（幂等）。
    pub fn shutdown(self) {
        if let Some(tx) = self.tx.lock().unwrap().take() {
            let _ = tx.send(BridgeCommand::Shutdown);
            drop(tx);
        }
        if let Some(join) = self.join.lock().unwrap().take() {
            let _ = join.join();
        }
        let _ = self.handle.shutdown();
    }
}

fn run_executor(handle: &CoreHandle, rx: Receiver<BridgeCommand>, event_tx: &Sender<CoreEvent>) {
    while let Ok(cmd) = rx.recv() {
        let quit = matches!(cmd, BridgeCommand::Shutdown);
        match cmd {
            BridgeCommand::PairBegin(url) => {
                let _ = event_tx.send(CoreEvent::QrReady(handle.pair_begin(url)));
            }
            BridgeCommand::PairConfirm(sas) => {
                let _ = event_tx.send(CoreEvent::PairConfirmed(handle.pair_confirm(sas)));
            }
            BridgeCommand::PairCancel => {
                let _ = handle.pair_cancel();
                let _ = event_tx.send(CoreEvent::PairCancelled);
            }
            BridgeCommand::LoadAndStart => match handle.pair_load() {
                Ok(false) => {
                    let _ = event_tx.send(CoreEvent::PairedLoaded(false));
                }
                Ok(true) => {
                    let _ = event_tx.send(CoreEvent::PairedLoaded(true));
                    let result = handle.start();
                    let _ = event_tx.send(CoreEvent::SessionStarted(result));
                }
                Err(e) => {
                    let _ = event_tx.send(CoreEvent::SessionStarted(Err(e)));
                }
            },
            BridgeCommand::SendText(text) => {
                // 回显抑制在 executor 线程执行：is_echo 是阻塞 CoreHandle 调用，
                // UI 线程一律不碰（core 回显环仅记录文本哈希，图片无需抑制）。
                if !handle.is_echo(clipboard_core::session::text_hash(&text)) {
                    if let Err(e) = handle.send_text(text) {
                        log::warn!("send_text failed: {e}");
                    }
                }
            }
            BridgeCommand::SendImage(bytes) => {
                if let Err(e) = handle.send_image(bytes) {
                    log::warn!("send_image failed: {e}");
                }
            }
            BridgeCommand::HistoryRefresh => {
                let _ = event_tx.send(CoreEvent::History(handle.history()));
            }
            BridgeCommand::HistoryImage(id) => {
                let result = handle.history_image_bytes(id.clone());
                let _ = event_tx.send(CoreEvent::HistoryImage { id, result });
            }
            BridgeCommand::HistoryApply(id) => {
                let _ = event_tx.send(CoreEvent::Applied(handle.history_apply(id)));
            }
            BridgeCommand::HistoryClear => {
                let _ = event_tx.send(CoreEvent::Cleared(handle.history_clear()));
            }
            BridgeCommand::ResetPairing => {
                let _ = event_tx.send(CoreEvent::ResetDone(handle.reset_pairing()));
            }
            BridgeCommand::Shutdown => {}
        }
        if quit {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard_monitor::MonitorState;
    use std::sync::{Arc, Mutex};

    fn monitor() -> MonitorHandle {
        MonitorHandle::new(Arc::new(Mutex::new(MonitorState::default())))
    }

    fn recv_event(rx: &std::sync::mpsc::Receiver<CoreEvent>) -> CoreEvent {
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("event arrives")
    }

    #[test]
    fn load_and_start_reports_unpaired() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let bridge = CoreBridge::spawn(dir.path(), monitor(), tx).unwrap();
        bridge.send(BridgeCommand::LoadAndStart);
        match recv_event(&rx) {
            CoreEvent::PairedLoaded(false) => {}
            other => panic!("expected PairedLoaded(false), got {other:?}"),
        }
        bridge.shutdown();
    }

    #[test]
    fn history_is_empty_on_fresh_dir() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let bridge = CoreBridge::spawn(dir.path(), monitor(), tx).unwrap();
        bridge.send(BridgeCommand::HistoryRefresh);
        match recv_event(&rx) {
            CoreEvent::History(Ok(items)) => assert!(items.is_empty()),
            other => panic!("expected empty History, got {other:?}"),
        }
        bridge.shutdown();
    }

    #[test]
    fn pair_begin_rejects_invalid_url() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let bridge = CoreBridge::spawn(dir.path(), monitor(), tx).unwrap();
        bridge.send(BridgeCommand::PairBegin("not-a-url".to_owned()));
        match recv_event(&rx) {
            CoreEvent::QrReady(Err(CoreError::InvalidInput(_))) => {}
            other => panic!("expected InvalidInput, got {other:?}"),
        }
        assert_eq!(bridge.pair_poll(), PairingSnapshot::Unpaired);
        bridge.shutdown();
    }

    #[test]
    fn is_echo_false_without_session() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let bridge = CoreBridge::spawn(dir.path(), monitor(), tx).unwrap();
        assert!(!bridge.is_echo("deadbeef".to_owned()));
        bridge.shutdown();
    }

    #[test]
    fn status_events_are_forwarded() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let bridge = CoreBridge::spawn(dir.path(), monitor(), tx).unwrap();
        // reset_pairing 在空目录上也会走 quiesce 流程并发 ReadyUnpaired
        bridge.send(BridgeCommand::ResetPairing);
        let mut saw_status = false;
        let mut saw_reset = false;
        while !(saw_status && saw_reset) {
            match recv_event(&rx) {
                CoreEvent::Status(CoreStatus::ReadyUnpaired) => saw_status = true,
                CoreEvent::ResetDone(Ok(())) => saw_reset = true,
                _ => {}
            }
        }
        bridge.shutdown();
    }
}
