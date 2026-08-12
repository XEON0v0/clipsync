mod settings;

use eframe::egui;

fn main() -> eframe::Result<()> {
    env_logger::init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("ClipSync")
            .with_inner_size([900.0, 600.0])
            .with_min_inner_size([720.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "ClipSync",
        options,
        Box::new(|_cc| Ok(Box::new(HelloApp))),
    )
}

struct HelloApp;

impl eframe::App for HelloApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("ClipSync Desktop 脚手架");
        });
    }
}
