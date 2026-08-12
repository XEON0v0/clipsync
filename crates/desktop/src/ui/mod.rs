pub mod pairing;

use eframe::egui;
use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle};

/// 统一强调色（indigo-500）。
pub const ACCENT: Color32 = Color32::from_rgb(99, 102, 241);

pub fn install_theme_and_fonts(ctx: &egui::Context) {
    ctx.options_mut(|o| o.theme_preference = egui::ThemePreference::System);
    ctx.style_mut(|style| {
        let radius = CornerRadius::same(8);
        style.visuals.widgets.noninteractive.corner_radius = radius;
        style.visuals.widgets.inactive.corner_radius = radius;
        style.visuals.widgets.hovered.corner_radius = radius;
        style.visuals.widgets.active.corner_radius = radius;
        style.visuals.widgets.open.corner_radius = radius;
        style.visuals.selection.bg_fill = ACCENT.linear_multiply(0.4);
        style.visuals.hyperlink_color = ACCENT;
        style.text_styles.insert(TextStyle::Heading, FontId::proportional(22.0));
        style.text_styles.insert(TextStyle::Body, FontId::proportional(15.0));
        style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    });
    install_cjk_fonts(ctx);
}

/// egui 内置字体无 CJK；从系统字体目录取第一个可用候选。
fn install_cjk_fonts(ctx: &egui::Context) {
    #[cfg(target_os = "macos")]
    const CANDIDATES: &[&str] = &[
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
    ];
    #[cfg(target_os = "windows")]
    const CANDIDATES: &[&str] = &[
        "C:\\Windows\\Fonts\\msyh.ttc",
        "C:\\Windows\\Fonts\\Deng.ttf",
        "C:\\Windows\\Fonts\\simhei.ttf",
        "C:\\Windows\\Fonts\\simsun.ttc",
    ];
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    const CANDIDATES: &[&str] = &[];

    for path in CANDIDATES {
        let Ok(bytes) = std::fs::read(path) else { continue };
        let mut fonts = egui::FontDefinitions::default();
        fonts
            .font_data
            .insert("cjk".to_owned(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push("cjk".to_owned());
        }
        ctx.set_fonts(fonts);
        log::info!("CJK font loaded from {path}");
        return;
    }
    log::warn!("no CJK font found; Chinese text will render as boxes");
}

/// 导航/状态指示用的小圆点。
pub fn status_dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 5.0, color);
}

/// 卡片容器（圆角浅底）。
pub fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style())
        .corner_radius(CornerRadius::same(10))
        .stroke(Stroke::NONE)
        .inner_margin(egui::Margin::same(12))
        .show(ui, add);
}
