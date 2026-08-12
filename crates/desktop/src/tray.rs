//! 系统托盘（Windows）/ 菜单栏 status item（macOS）。

use std::sync::mpsc::Sender;

use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrayCommand {
    OpenWindow,
    SendCurrent,
    Quit,
}

pub struct Tray {
    _tray: TrayIcon,
}

impl Tray {
    pub fn spawn(tx: Sender<TrayCommand>) -> Result<Tray, String> {
        let open = MenuItem::new("打开窗口", true, None);
        let send = MenuItem::new("发送当前剪贴板", true, None);
        let quit = MenuItem::new("退出", true, None);
        let ids = (open.id().clone(), send.id().clone(), quit.id().clone());
        let menu = Menu::with_items(&[&open, &send, &quit]).map_err(|e| e.to_string())?;
        let icon = Icon::from_rgba(icon_rgba(), 32, 32).map_err(|e| e.to_string())?;
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("ClipSync")
            .with_icon(icon)
            .build()
            .map_err(|e| e.to_string())?;

        MenuEvent::set_event_handler(Some(Box::new(move |event: MenuEvent| {
            let cmd = if event.id == ids.0 {
                TrayCommand::OpenWindow
            } else if event.id == ids.1 {
                TrayCommand::SendCurrent
            } else if event.id == ids.2 {
                TrayCommand::Quit
            } else {
                return;
            };
            let _ = tx.send(cmd);
        })));

        Ok(Tray { _tray: tray })
    }
}

/// 32×32 RGBA：圆角靛蓝方块 + 两条白色横线（剪贴板意象）。
fn icon_rgba() -> Vec<u8> {
    const N: usize = 32;
    let mut rgba = vec![0u8; N * N * 4];
    for y in 0..N {
        for x in 0..N {
            let i = (y * N + x) * 4;
            let in_rounded_rect = x >= 2 && x < 30 && y >= 2 && y < 30;
            if !in_rounded_rect {
                continue; // 透明
            }
            // 白色横线（剪贴板纸张线条）
            let is_line = (y == 10 || y == 11 || y == 16 || y == 17) && x >= 8 && x < 24;
            if is_line {
                rgba[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
            } else {
                rgba[i..i + 4].copy_from_slice(&[99, 102, 241, 255]); // indigo-500
            }
        }
    }
    rgba
}
