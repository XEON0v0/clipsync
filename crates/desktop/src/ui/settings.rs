//! 设置页：relay 地址（未配对时可改）、开机自启、关于。

use clipboard_core::ffi::PairingSnapshot;
use eframe::egui;

use crate::app::DesktopApp;
use crate::autostart;
use crate::settings::{Settings, validate_relay_url};

/// 无效地址不得落盘的判定：非空且校验失败（空串 = 未配置，允许）。
fn relay_url_invalid(url: &str) -> bool {
    !url.is_empty() && validate_relay_url(url).is_err()
}

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
    let url_invalid = relay_url_invalid(&app.settings.relay_url);
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
            // 此路径整体落盘 Settings：先回退未确认的无效 relay 编辑，
            // 避免勾选自启把红字地址夹带写盘（有效但未点保存的编辑允许随存）
            if relay_url_invalid(&app.settings.relay_url) {
                app.settings.relay_url = Settings::load(&app.settings_path).relay_url;
            }
            if let Err(e) = app.settings.save(&app.settings_path) {
                app.toast(format!("保存失败：{e}"));
            }
        }
    }

    ui.add_space(16.0);
    ui.weak(format!("ClipSync Desktop v{}", env!("CARGO_PKG_VERSION")));
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_url_invalid_flags_only_nonempty_invalid() {
        assert!(!relay_url_invalid("")); // 空串 = 未配置，允许落盘
        assert!(!relay_url_invalid("wss://sync.example.com/ws"));
        assert!(relay_url_invalid("http://bad"));
        assert!(relay_url_invalid("not-a-url"));
    }

    #[test]
    fn autostart_path_never_persists_invalid_relay_url() {
        // 自启 toggle 路径语义（与 show() 中的分支一致）：无效的在内存编辑
        // 先回退为已持久值，再整体落盘 —— settings.json 永不出现无效非空地址
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        Settings {
            relay_url: "wss://persisted.example.com/ws".to_owned(),
            autostart: false,
        }
        .save(&path)
        .unwrap();

        let mut settings = Settings::load(&path);
        settings.relay_url = "http://bad".to_owned(); // 输入框里的未确认无效编辑
        settings.autostart = true; // 用户此时勾选自启
        if relay_url_invalid(&settings.relay_url) {
            settings.relay_url = Settings::load(&path).relay_url;
        }
        settings.save(&path).unwrap();

        let persisted = Settings::load(&path);
        assert_eq!(persisted.relay_url, "wss://persisted.example.com/ws");
        assert!(persisted.autostart);
    }
}
