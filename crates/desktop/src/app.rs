//! eframe 应用外壳：事件泵 + 导航 + 关窗到托盘。
//! 规则：UI 线程不调用任何阻塞 CoreHandle 方法（一律走 BridgeCommand）。

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use clipboard_core::ffi::{CoreStatus, FfiHistoryItem};
use eframe::egui;
use egui::Color32;

use crate::clipboard_monitor::{
    self, ArboardIo, ClipboardIo, ClipboardPayload, MonitorHandle, MonitorState,
};
use crate::core_bridge::{BridgeCommand, CoreBridge, CoreEvent};
use crate::platform;
use crate::settings::Settings;
use crate::tray::{Tray, TrayCommand};
use crate::ui;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Page {
    Pairing,
    History,
    Settings,
}

/// 缩略图状态：Loading 已发出取图请求 / Ready 纹理已建 / Failed 取图或解码失败。
pub(crate) enum ThumbState {
    Loading,
    Ready(egui::TextureHandle),
    Failed,
}

pub struct DesktopApp {
    pub(crate) bridge: CoreBridge,
    core_rx: Receiver<CoreEvent>,
    tray_rx: Receiver<TrayCommand>,
    clip_rx: Receiver<ClipboardPayload>,
    pub(crate) monitor: MonitorHandle,
    _tray: Tray,
    pub(crate) settings: Settings,
    pub(crate) settings_path: PathBuf,
    page: Page,
    status: Option<CoreStatus>,
    pub(crate) history: Vec<FfiHistoryItem>,
    toast: Option<(String, Instant)>,
    pub(crate) qr_cache: Option<(String, egui::TextureHandle)>,
    pub(crate) confirm_reset: bool,
    pub(crate) confirm_repair: bool,
    pub(crate) pending_repair: bool,
    pub(crate) thumbs: std::collections::HashMap<String, ThumbState>,
    pub(crate) pending_thumbs: std::collections::HashMap<String, Vec<u8>>,
    pub(crate) pending_apply: Option<String>,
    pub(crate) confirm_clear: bool,
}

/// 状态文字 + 圆点颜色（CoreStatus 8 态 → UI 文案）。
pub fn status_label(status: &CoreStatus) -> (&'static str, Color32) {
    match status {
        CoreStatus::ReadyUnpaired => ("未配对", Color32::GRAY),
        CoreStatus::Offering => ("等待手机扫码…", Color32::GOLD),
        CoreStatus::SasReady => ("待确认安全码", Color32::GOLD),
        CoreStatus::Connecting => ("连接中…", Color32::GOLD),
        CoreStatus::Connected => ("已连接", Color32::from_rgb(34, 197, 94)),
        CoreStatus::Reconnecting => ("重连中…", Color32::GOLD),
        CoreStatus::Disconnected => ("已断开", Color32::from_rgb(239, 68, 68)),
        CoreStatus::Error { .. } => ("出错了", Color32::from_rgb(239, 68, 68)),
    }
}

impl DesktopApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Result<Self, String> {
        ui::install_theme_and_fonts(&cc.egui_ctx);
        platform::set_accessory_mode(false);

        let data_dir = directories::BaseDirs::new()
            .ok_or("无法定位用户数据目录")?
            .data_dir()
            .join("clipsync");
        std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;

        let settings_path = data_dir.join("settings.json");
        let settings = Settings::load(&settings_path);

        let state = Arc::new(Mutex::new(MonitorState::default()));
        let monitor = MonitorHandle::new(state.clone());
        let (core_tx, core_rx) = std::sync::mpsc::channel();
        let bridge = CoreBridge::spawn(&data_dir, monitor.clone(), core_tx)
            .map_err(|e| format!("core 启动失败: {e}"))?;
        bridge.send(BridgeCommand::LoadAndStart);

        let (clip_tx, clip_rx) = std::sync::mpsc::channel();
        if let Some(io) = ArboardIo::new() {
            clipboard_monitor::spawn_poller(io, state, clip_tx);
        } else {
            log::error!("clipboard unavailable; sync of local changes disabled");
        }

        let (tray_tx, tray_rx) = std::sync::mpsc::channel();
        let tray = Tray::spawn(tray_tx).map_err(|e| format!("托盘创建失败: {e}"))?;

        Ok(Self {
            bridge,
            core_rx,
            tray_rx,
            clip_rx,
            monitor,
            _tray: tray,
            settings,
            settings_path,
            page: Page::Pairing,
            status: None,
            history: vec![],
            toast: None,
            qr_cache: None,
            confirm_reset: false,
            confirm_repair: false,
            pending_repair: false,
            thumbs: std::collections::HashMap::new(),
            pending_thumbs: std::collections::HashMap::new(),
            pending_apply: None,
            confirm_clear: false,
        })
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.core_rx.try_recv() {
            self.on_core_event(event);
        }
        while let Ok(cmd) = self.tray_rx.try_recv() {
            match cmd {
                TrayCommand::OpenWindow => {
                    platform::set_accessory_mode(false);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                TrayCommand::SendCurrent => self.send_current_clipboard(),
                TrayCommand::Quit => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    std::process::exit(0);
                }
            }
        }
        while let Ok(payload) = self.clip_rx.try_recv() {
            // 仅在已连接时上报本地剪贴板；未连接直接丢弃（core 也会拒发）
            if matches!(&self.status, Some(CoreStatus::Connected)) {
                self.send_payload(payload);
            }
        }
    }

    fn on_core_event(&mut self, event: CoreEvent) {
        match event {
            CoreEvent::Status(status) => self.status = Some(status),
            CoreEvent::LiveClip(_) => {
                self.bridge.send(BridgeCommand::HistoryRefresh);
            }
            CoreEvent::History(Ok(items)) => {
                // 淘汰滚出 50 条窗口的缩略图缓存：GPU 纹理随历史翻滚释放，
                // 在途取图字节同理（否则滚出窗口的 id 永久驻留）
                ui::history::retain_current(&mut self.thumbs, &items);
                ui::history::retain_current(&mut self.pending_thumbs, &items);
                self.history = items;
            }
            CoreEvent::History(Err(e)) => self.toast(format!("历史刷新失败：{e}")),
            CoreEvent::HistoryImage { id, result } => match result {
                Ok(bytes) => {
                    // 若是「应用」触发的取图，先写剪贴板（ownership），再走缩略图缓存
                    if self.pending_apply.as_deref() == Some(id.as_str()) {
                        self.monitor
                            .apply_remote(&ClipboardPayload::ImagePng(bytes.clone()), false);
                        self.pending_apply = None;
                    }
                    self.pending_thumbs.insert(id, bytes); // 纹理在渲染期懒建
                    self.bridge.send(BridgeCommand::HistoryRefresh); // Deferred→Remote 可能已变
                }
                Err(_) => {
                    self.thumbs.insert(id, ThumbState::Failed);
                }
            },
            CoreEvent::Applied(Ok(())) => {
                self.toast("已应用到剪贴板".to_owned());
                self.bridge.send(BridgeCommand::HistoryRefresh);
            }
            CoreEvent::Cleared(Ok(())) => {
                self.history.clear();
                self.thumbs.clear();
                self.pending_thumbs.clear();
            }
            CoreEvent::QrReady(Err(e)) => self.toast(format!("配对失败：{e}")),
            CoreEvent::PairConfirmed(Err(e)) => self.toast(format!("确认失败：{e}")),
            CoreEvent::SessionStarted(Err(e)) => self.toast(format!("连接失败：{e}")),
            CoreEvent::Applied(Err(e)) | CoreEvent::Cleared(Err(e)) => {
                self.toast(format!("操作失败：{e}"))
            }
            // 换绑失败也要解除联动：否则残留 true 会被后续无关的 ResetDone(Ok) 误触发
            CoreEvent::ResetDone(Err(e)) => {
                self.pending_repair = false;
                self.toast(format!("操作失败：{e}"))
            }
            // 换绑联动：解绑成功后立即发起新配对
            CoreEvent::ResetDone(Ok(())) => {
                if self.pending_repair {
                    self.pending_repair = false;
                    self.bridge
                        .send(BridgeCommand::PairBegin(self.settings.relay_url.clone()));
                }
            }
            _ => {}
        }
    }

    pub(crate) fn send_current_clipboard(&mut self) {
        if let Some(mut io) = ArboardIo::new()
            && let Some(payload) = io.read()
        {
            self.send_payload(payload);
        }
    }

    fn send_payload(&self, payload: ClipboardPayload) {
        // 回显抑制由 executor 线程完成（UI 线程不调用阻塞 CoreHandle 方法）
        match payload {
            ClipboardPayload::Text(text) => {
                self.bridge.send(BridgeCommand::SendText(text));
            }
            ClipboardPayload::ImagePng(bytes) => {
                self.bridge.send(BridgeCommand::SendImage(bytes));
            }
        }
    }

    pub(crate) fn toast(&mut self, message: String) {
        self.toast = Some((message, Instant::now()));
    }

    fn show_nav(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("nav")
            .resizable(false)
            .exact_width(200.0)
            .show(ctx, |ui| {
                ui.add_space(16.0);
                ui.heading("ClipSync");
                ui.add_space(16.0);
                for (page, label) in [
                    (Page::Pairing, "配对"),
                    (Page::History, "历史"),
                    (Page::Settings, "设置"),
                ] {
                    if ui.selectable_label(self.page == page, label).clicked() {
                        if page == Page::History {
                            self.bridge.send(BridgeCommand::HistoryRefresh);
                        }
                        self.page = page;
                    }
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.add_space(12.0);
                    let (label, color) = self
                        .status
                        .as_ref()
                        .map(status_label)
                        .unwrap_or(("启动中…", Color32::GRAY));
                    ui.horizontal(|ui| {
                        ui::status_dot(ui, color);
                        ui.label(label);
                    });
                });
            });
    }
}

impl eframe::App for DesktopApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 关窗 = 隐藏到托盘（不退出）
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            platform::set_accessory_mode(true);
        }

        self.drain_events(ctx);
        ctx.request_repaint_after(std::time::Duration::from_millis(300));

        self.show_nav(ctx);
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(12.0);
            match self.page {
                Page::Pairing => crate::ui::pairing::show(self, ui),
                Page::History => crate::ui::history::show(self, ui),
                Page::Settings => crate::ui::settings::show(self, ui),
            }
        });

        // toast：右下角短暂提示
        if let Some((message, at)) = &self.toast {
            if at.elapsed().as_secs() >= 4 {
                self.toast = None;
            } else {
                egui::Area::new(egui::Id::new("toast"))
                    .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -16.0))
                    .show(ctx, |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.label(message);
                        });
                    });
            }
        }
    }
}

pub fn run() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("ClipSync")
            .with_inner_size([900.0, 600.0])
            .with_min_inner_size([720.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native("ClipSync", options, Box::new(|cc| {
        // Box<dyn Error + Send + Sync> 实现了 From<String>，into() 直接可用
        DesktopApp::new(cc)
            .map(|app| Box::new(app) as Box<dyn eframe::App>)
            .map_err(Into::into)
    }))
}
