//! 设置页：relay 地址（未配对时可改）、开机自启、关于。

use clipboard_core::ffi::PairingSnapshot;
use eframe::egui;

use crate::app::DesktopApp;
use crate::autostart;
use crate::settings::validate_relay_url;

pub fn show(app: &mut DesktopApp, ui: &mut egui::Ui) {
    ui.heading("设置");
    ui.add_space(12.0);

    ui.label("Relay 服务器地址");
    let paired = matches!(app.bridge.pair_poll(), PairingSnapshot::Paired { .. });
    let edit = egui::TextEdit::singleline(&mut app.settings.relay_url)
        .hint_text("wss://sync.example.com/ws")
        .desired_width(420.0);
    ui.add_enabled_ui(!paired, |ui| {
        ui.add(edit);
    });
    if paired {
        ui.weak("解除配对后可修改");
    } else if !app.settings.relay_url.is_empty() {
        match validate_relay_url(&app.settings.relay_url) {
            Ok(()) => {}
            Err(message) => {
                ui.colored_label(egui::Color32::from_rgb(239, 68, 68), message);
            }
        }
    }
    // 无效地址不落盘：红字提示随输入实时显示，保存时再次拦截
    let url_invalid = !app.settings.relay_url.is_empty()
        && validate_relay_url(&app.settings.relay_url).is_err();
    if ui.button("保存").clicked() {
        if url_invalid {
            app.toast("relay 地址无效，未保存".to_owned());
        } else {
            match app.settings.save(&app.settings_path) {
                Ok(()) => app.toast("已保存".to_owned()),
                Err(e) => app.toast(format!("保存失败：{e}")),
            }
        }
    }
    // relay 地址只在发起配对/会话时读取，已建立的会话仍用旧地址
    ui.weak("relay 地址修改在重启会话后生效");

    ui.add_space(16.0);
    if ui
        .checkbox(&mut app.settings.autostart, "开机自动启动")
        .changed()
    {
        if let Err(e) = autostart::set_enabled(app.settings.autostart) {
            app.toast(format!("自启动设置失败：{e}"));
            app.settings.autostart = autostart::is_enabled();
        } else {
            let _ = app.settings.save(&app.settings_path);
        }
    }

    ui.add_space(16.0);
    ui.weak(format!("ClipSync Desktop v{}", env!("CARGO_PKG_VERSION")));
}
