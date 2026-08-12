//! 历史页：50 条卡片（core 约束），文本摘要 / 图片缩略图懒加载。

use clipboard_core::ffi::{FfiHistoryItem, FfiHistoryKind, FfiHistorySource};
use eframe::egui;
use egui::ColorImage;

use crate::app::{DesktopApp, ThumbState};
use crate::clipboard_monitor::ClipboardPayload;
use crate::core_bridge::BridgeCommand;
use crate::ui;

pub fn show(app: &mut DesktopApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.heading("历史");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let label = if app.confirm_clear { "再次点击确认清空" } else { "清空" };
            if ui.button(label).clicked() {
                if app.confirm_clear {
                    app.confirm_clear = false;
                    app.bridge.send(BridgeCommand::HistoryClear);
                } else {
                    app.confirm_clear = true;
                }
            }
            if ui.button("刷新").clicked() {
                app.bridge.send(BridgeCommand::HistoryRefresh);
            }
        });
    });
    ui.add_space(8.0);
    if app.history.is_empty() {
        ui.label("暂无历史记录");
        return;
    }
    egui::ScrollArea::vertical().show(ui, |ui| {
        // core 返回"最旧在前"，展示时倒序（最新在上）
        let now = now_ms();
        for item in app.history.clone().iter().rev() {
            show_item(app, ui, item, now);
            ui.add_space(6.0);
        }
    });
}

fn show_item(app: &mut DesktopApp, ui: &mut egui::Ui, item: &FfiHistoryItem, now: i64) {
    ui::card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                match &item.kind {
                    FfiHistoryKind::Text { content } => {
                        let summary: String = content.chars().take(200).collect();
                        ui.label(summary);
                    }
                    FfiHistoryKind::Image => {
                        show_thumbnail(app, ui, &item.id);
                    }
                }
                ui.horizontal(|ui| {
                    ui.weak(format!(
                        "{} · {}",
                        source_label(item.source),
                        format_relative_time(item.ts_ms, now)
                    ));
                    if item.source == FfiHistorySource::RemoteDeferred {
                        ui.weak("（未应用）");
                    }
                });
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("应用").clicked() {
                    apply_item(app, item);
                }
            });
        });
    });
}

fn show_thumbnail(app: &mut DesktopApp, ui: &mut egui::Ui, id: &str) {
    match app.thumbs.get(id) {
        Some(ThumbState::Ready(texture)) => {
            let size = texture.size_vec2();
            let scale = (120.0 / size.x.max(size.y)).min(1.0);
            ui.image(egui::load::SizedTexture::new(texture.id(), size * scale));
        }
        Some(ThumbState::Loading) => { ui.spinner(); }
        Some(ThumbState::Failed) => { ui.label("[图片加载失败]"); }
        None => {
            app.thumbs.insert(id.to_owned(), ThumbState::Loading);
            app.bridge.send(BridgeCommand::HistoryImage(id.to_owned()));
            ui.label("[图片]");
        }
    }
    // pending bytes → 建纹理（解码后先降采样，避免原图全分辨率上传 GPU）
    if let Some(bytes) = app.pending_thumbs.remove(id) {
        match decode_thumb(&bytes) {
            Some(color) => {
                let tex = ui.ctx().load_texture(
                    format!("thumb-{id}"), color, egui::TextureOptions::LINEAR);
                app.thumbs.insert(id.to_owned(), ThumbState::Ready(tex));
            }
            None => { app.thumbs.insert(id.to_owned(), ThumbState::Failed); }
        }
    }
}

/// 缩略图最长边：解码后先降到该尺寸再上传 GPU（保持宽高比，只缩不放），
/// 避免 4000×3000 原图 ~48MB 全分辨率纹理随历史翻滚无限累积。
const THUMB_MAX: u32 = 240;

/// PNG 字节 → 缩略图 ColorImage。
fn decode_thumb(bytes: &[u8]) -> Option<ColorImage> {
    let img = image::load_from_memory(bytes).ok()?;
    // image 的 thumbnail 对小图会放大，故只在超过上限时降采样（只缩不放）
    let img = if img.width().max(img.height()) > THUMB_MAX {
        img.thumbnail(THUMB_MAX, THUMB_MAX)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width() as usize, rgba.height() as usize);
    Some(ColorImage::from_rgba_unmultiplied([w, h], &rgba.into_raw()))
}

/// 历史整体替换后淘汰滚出 50 条窗口的缓存项（只保留仍在 items 中的 id）。
pub(crate) fn retain_current<V>(
    map: &mut std::collections::HashMap<String, V>,
    items: &[FfiHistoryItem],
) {
    map.retain(|id, _| items.iter().any(|item| &item.id == id));
}

fn apply_item(app: &mut DesktopApp, item: &FfiHistoryItem) {
    // 先写剪贴板（记录 ownership），再通知 core promote（Deferred→Remote）
    match &item.kind {
        FfiHistoryKind::Text { content } => {
            app.monitor
                .apply_remote(&ClipboardPayload::Text(content.clone()), false);
            app.bridge.send(BridgeCommand::HistoryApply(item.id.clone()));
        }
        FfiHistoryKind::Image => {
            // 二段式：先取回图片字节，on_core_event 的 HistoryImage 分支里
            // 命中 pending_apply 时写剪贴板；同时通知 core promote
            app.pending_apply = Some(item.id.clone());
            app.bridge.send(BridgeCommand::HistoryImage(item.id.clone()));
            app.bridge.send(BridgeCommand::HistoryApply(item.id.clone()));
        }
    }
}

pub fn source_label(source: FfiHistorySource) -> &'static str {
    match source {
        FfiHistorySource::Local => "本机",
        FfiHistorySource::Remote => "对方",
        FfiHistorySource::RemoteDeferred => "离线补投",
    }
}

/// 相对时间：刚刚 / N 分钟前 / N 小时前 / N 天前。
pub fn format_relative_time(ts_ms: i64, now_ms: i64) -> String {
    let delta = (now_ms - ts_ms).max(0);
    const MIN: i64 = 60_000;
    const HOUR: i64 = 3_600_000;
    const DAY: i64 = 86_400_000;
    if delta < MIN {
        "刚刚".to_owned()
    } else if delta < HOUR {
        format!("{} 分钟前", delta / MIN)
    } else if delta < DAY {
        format!("{} 小时前", delta / HOUR)
    } else {
        format!("{} 天前", delta / DAY)
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_time_buckets() {
        let now = 1_000_000_000i64;
        assert_eq!(format_relative_time(now - 5_000, now), "刚刚");
        assert_eq!(format_relative_time(now - 90_000, now), "1 分钟前");
        assert_eq!(format_relative_time(now - 3_600_000, now), "1 小时前");
        assert_eq!(format_relative_time(now - 5 * 3_600_000, now), "5 小时前");
        assert_eq!(format_relative_time(now - 2 * 86_400_000, now), "2 天前");
    }

    #[test]
    fn source_labels() {
        assert_eq!(source_label(FfiHistorySource::Local), "本机");
        assert_eq!(source_label(FfiHistorySource::Remote), "对方");
        assert_eq!(source_label(FfiHistorySource::RemoteDeferred), "离线补投");
    }

    fn png_of(w: u32, h: u32) -> Vec<u8> {
        let rgba = vec![128u8; (w * h * 4) as usize];
        crate::clipboard_monitor::rgba_to_png(w, h, &rgba).unwrap()
    }

    fn text_item(id: &str) -> FfiHistoryItem {
        FfiHistoryItem {
            id: id.to_owned(),
            ts_ms: 0,
            kind: FfiHistoryKind::Text { content: String::new() },
            source: FfiHistorySource::Local,
        }
    }

    #[test]
    fn decode_thumb_downscales_long_edge() {
        let thumb = decode_thumb(&png_of(960, 480)).unwrap();
        assert_eq!(thumb.size, [THUMB_MAX as usize, 120]);
    }

    #[test]
    fn decode_thumb_never_upscales() {
        let thumb = decode_thumb(&png_of(100, 50)).unwrap();
        assert_eq!(thumb.size, [100, 50]);
    }

    #[test]
    fn decode_thumb_rejects_garbage() {
        assert!(decode_thumb(b"not a png").is_none());
    }

    #[test]
    fn retain_current_drops_evicted_ids() {
        let mut map = std::collections::HashMap::from([("a".to_owned(), 1), ("b".to_owned(), 2)]);
        retain_current(&mut map, &[text_item("a")]);
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("a"));
    }
}
