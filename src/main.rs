// Hide console window in release mode
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod capture;
mod corsair;
mod ui;
mod virtual_display;

use eframe::egui;
use ui::EliteDisplayApp;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([640.0, 400.0])
            .with_resizable(false)
            .with_maximize_button(false)
            .with_title("Corsair Elite LCD - Second Display"),
        ..Default::default()
    };

    eframe::run_native(
        "Corsair Elite LCD - Second Display",
        options,
        Box::new(|cc| {
            // Apply dark mode visuals
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            Ok(Box::new(EliteDisplayApp::new(cc)))
        }),
    )
}
