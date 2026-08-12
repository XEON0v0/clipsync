//! 配对页：桌面 = offerer（显示 QR，手机扫码），SAS 双向核对。

use clipboard_core::ffi::PairingSnapshot;
use clipboard_core::pairing::QrPayload;
use eframe::egui;
use egui::{ColorImage, RichText};

use crate::app::DesktopApp;
use crate::core_bridge::BridgeCommand;
use crate::settings::validate_relay_url;
use crate::ui;

pub fn show(app: &mut DesktopApp, ui: &mut egui::Ui) {
    let snapshot = app.bridge.pair_poll();
    match snapshot {
        PairingSnapshot::Unpaired => show_unpaired(app, ui),
        PairingSnapshot::Offering { qr_json } => show_offering(app, ui, &qr_json),
        PairingSnapshot::SasReady { sas } => show_sas(app, ui, &sas),
        PairingSnapshot::Paired { room_id } => show_paired(app, ui, &room_id),
    }
}

fn show_unpaired(app: &mut DesktopApp, ui: &mut egui::Ui) {
    ui.heading("配对设备");
    ui.add_space(8.0);
    ui.label("在手机上安装 ClipSync，扫描这里出现的二维码即可完成配对。");
    ui.add_space(12.0);
    if app.settings.relay_url.is_empty() {
        ui.colored_label(egui::Color32::from_rgb(239, 68, 68), "请先在「设置」页填写 relay 服务器地址");
        return;
    }
    // 格式非法时直接拦截（core 也会拒发；这里提前给出可读提示）
    if let Err(reason) = validate_relay_url(&app.settings.relay_url) {
        ui.colored_label(
            egui::Color32::from_rgb(239, 68, 68),
            format!("relay 地址不可用：{reason}（请到「设置」页修正）"),
        );
        return;
    }
    if ui.button("开始配对").clicked() {
        app.bridge.send(BridgeCommand::PairBegin(app.settings.relay_url.clone()));
    }
}

fn show_offering(app: &mut DesktopApp, ui: &mut egui::Ui, qr_json: &str) {
    ui.heading("用手机扫码配对");
    ui.add_space(8.0);
    if let Some(texture) = qr_texture(app, ui, qr_json) {
        let size = egui::vec2(280.0, 280.0);
        ui.image(egui::load::SizedTexture::new(texture.id(), size));
    }
    ui.add_space(8.0);
    // 配对码文本与 QR 同源：core QrPayload 的 code 字段（6 位大写字母数字）
    if let Ok(payload) = QrPayload::parse(qr_json) {
        ui.add(
            egui::Label::new(
                RichText::new(format!("配对码：{}", payload.code))
                    .size(20.0)
                    .family(egui::FontFamily::Monospace)
                    .strong(),
            )
            .selectable(true),
        );
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button("取消").clicked() {
            app.bridge.send(BridgeCommand::PairCancel);
        }
        // 配对码 TTL 300s（服务端），过期后一键换新
        if ui.button("刷新配对码").clicked() {
            app.bridge.send(BridgeCommand::PairCancel);
            app.bridge
                .send(BridgeCommand::PairBegin(app.settings.relay_url.clone()));
        }
    });
}

fn show_sas(app: &mut DesktopApp, ui: &mut egui::Ui, sas: &str) {
    ui.heading("核对安全码");
    ui.add_space(8.0);
    ui.label("请确认手机上显示的安全码与下方一致：");
    ui.add_space(12.0);
    ui.label(
        RichText::new(sas)
            .size(48.0)
            .family(egui::FontFamily::Monospace)
            .strong(),
    );
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        if ui.button("一致，确认配对").clicked() {
            app.bridge.send(BridgeCommand::PairConfirm(sas.to_owned()));
        }
        if ui.button("取消").clicked() {
            app.bridge.send(BridgeCommand::PairCancel);
        }
    });
}

fn show_paired(app: &mut DesktopApp, ui: &mut egui::Ui, room_id: &str) {
    ui.heading("已配对");
    ui.add_space(8.0);
    ui::card(ui, |ui| {
        ui.label(format!("房间号：{room_id}"));
    });
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        if ui.button("发送当前剪贴板").clicked() {
            app.send_current_clipboard();
        }
        let reset = if app.confirm_reset {
            "再次点击确认解绑"
        } else {
            "解除配对"
        };
        if ui.button(reset).clicked() {
            if app.confirm_reset {
                app.confirm_reset = false;
                app.bridge.send(BridgeCommand::ResetPairing);
            } else {
                app.confirm_reset = true;
            }
        }
        // 换绑 = 解绑 + 立即发起新配对（ResetDone 事件里联动 PairBegin），同解绑需二次确认
        let repair = if app.confirm_repair {
            "再次点击确认换绑"
        } else {
            "换绑（重新配对）"
        };
        if ui.button(repair).clicked() {
            if app.confirm_repair {
                app.confirm_repair = false;
                app.pending_repair = true;
                app.bridge.send(BridgeCommand::ResetPairing);
            } else {
                app.confirm_repair = true;
            }
        }
    });
}

/// QR 纹理缓存：同一 qr_json 只渲染一次。
fn qr_texture(
    app: &mut DesktopApp,
    ui: &mut egui::Ui,
    qr_json: &str,
) -> Option<egui::TextureHandle> {
    if let Some((cached, texture)) = &app.qr_cache
        && cached == qr_json
    {
        return Some(texture.clone());
    }
    let code = qrcode::QrCode::new(qr_json.as_bytes()).ok()?;
    // qrcode 0.14 的 Luma 由 image crate 提供（0.25，与 workspace 锁版一致）
    let image = code
        .render::<image::Luma<u8>>()
        .min_dimensions(320, 320)
        .build();
    let (w, h) = (image.width() as usize, image.height() as usize);
    let color = ColorImage::from_gray([w, h], &image.into_raw());
    let texture = ui.ctx().load_texture("pairing-qr", color, egui::TextureOptions::NEAREST);
    app.qr_cache = Some((qr_json.to_owned(), texture.clone()));
    Some(texture)
}
