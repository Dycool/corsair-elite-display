use std::sync::atomic::Ordering;
use std::time::Instant;

use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, Stroke, TextureOptions, Vec2};
use image::RgbaImage;

use crate::capture::{MonitorInfo, StreamController};
use crate::corsair::CorsairLcdDevice;
use crate::virtual_display::VirtualDisplayManager;

pub struct EliteDisplayApp {
    controller: StreamController,
    monitors: Vec<MonitorInfo>,
    selected_monitor: usize,
    brightness: u32,
    rotation: u32,
    preview_texture: Option<egui::TextureHandle>,
    last_ui_refresh: Instant,
    cached_status: String,
    cached_fps: f32,
    cached_frame_size: usize,
    cached_latency: f32,
    hardware_detected: bool,
    vdd_installed: bool,
    vdd_480_ready: bool,
    action_message: Option<(String, Instant)>,
}

impl EliteDisplayApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let mut app = Self {
            controller: StreamController::new(),
            monitors: Vec::new(),
            selected_monitor: 0,
            brightness: 100,
            rotation: 0,
            preview_texture: None,
            last_ui_refresh: Instant::now(),
            cached_status: "Ready".to_string(),
            cached_fps: 0.0,
            cached_frame_size: 0,
            cached_latency: 0.0,
            hardware_detected: false,
            vdd_installed: false,
            vdd_480_ready: false,
            action_message: None,
        };

        app.refresh_system_state();
        app
    }

    fn refresh_system_state(&mut self) {
        self.monitors = StreamController::get_monitors();
        // Auto-select 480x480 monitor if found, otherwise keep 0
        if let Some(pos) = self.monitors.iter().position(|m| m.width == 480 && m.height == 480) {
            self.selected_monitor = pos;
            self.controller.selected_monitor_idx.store(pos as u32, Ordering::Relaxed);
        }

        self.hardware_detected = CorsairLcdDevice::find_device_path().is_some();
        self.vdd_installed = VirtualDisplayManager::is_installed();
        self.vdd_480_ready = VirtualDisplayManager::is_480_configured();
    }

    fn update_preview_texture(&mut self, ctx: &egui::Context, img: &RgbaImage) {
        let size = [img.width() as usize, img.height() as usize];
        let pixels = img.as_flat_samples();
        let color_image = egui::ColorImage::from_rgba_unmultiplied(size, pixels.as_slice());

        match &mut self.preview_texture {
            Some(tex) => tex.set(color_image, TextureOptions::LINEAR),
            None => {
                self.preview_texture = Some(ctx.load_texture("lcd_preview", color_image, TextureOptions::LINEAR));
            }
        }
    }
}

impl eframe::App for EliteDisplayApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // Poll preview frame from streamer (drop lock before mutating self)
        let maybe_img = self.controller.preview_image.try_lock().ok().and_then(|mut l| l.take());
        if let Some(img) = maybe_img {
            self.update_preview_texture(&ctx, &img);
        }

        // Poll stats
        if self.last_ui_refresh.elapsed().as_millis() > 200 {
            if let Ok(s) = self.controller.stats.lock() {
                self.cached_status = s.status.clone();
                self.cached_fps = s.fps;
                self.cached_frame_size = s.last_frame_size_bytes;
                self.cached_latency = s.last_latency_ms;
            }
            self.last_ui_refresh = Instant::now();
        }

        // Request continuous repaint while streaming for fluid preview
        if self.controller.is_streaming() {
            ctx.request_repaint();
        }

        ui.add_space(8.0);

        // Header bar
        ui.horizontal(|ui| {
            ui.heading("Corsair Elite LCD Display");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.hardware_detected {
                    ui.label(egui::RichText::new("[Connected]").color(Color32::from_rgb(34, 197, 94)));
                } else {
                    ui.label(egui::RichText::new("[Disconnected]").color(Color32::from_rgb(239, 68, 68)));
                }
            });
        });

        ui.separator();
        ui.add_space(4.0);

        // Main 2-column layout: Left column = Preview & Stats, Right column = Controls
        ui.columns(2, |columns| {
            // LEFT COLUMN: LCD Preview Widget
            columns[0].vertical_centered(|ui| {
                ui.label(egui::RichText::new("Cooler Screen Live Preview").strong());
                ui.add_space(6.0);

                let preview_size = 220.0;
                let (rect, _response) = ui.allocate_exact_size(Vec2::splat(preview_size), egui::Sense::hover());
                let painter = ui.painter();
                let center = rect.center();
                let radius = preview_size / 2.0;

                // Draw outer pump bezel ring
                painter.circle_filled(center, radius + 6.0, Color32::from_rgb(18, 24, 38));
                painter.circle_stroke(
                    center,
                    radius + 4.0,
                    Stroke::new(3.0, Color32::from_rgb(56, 189, 248)),
                );

                // Draw LCD contents or placeholder
                if let Some(tex) = &self.preview_texture {
                    let uv = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0));
                    painter.image(tex.id(), rect, uv, Color32::WHITE);
                    // Overlay circular crop mask
                    painter.circle_stroke(center, radius, Stroke::new(4.0, Color32::from_rgb(15, 23, 42)));
                } else {
                    painter.circle_filled(center, radius, Color32::from_rgb(10, 15, 26));
                    painter.text(
                        center,
                        egui::Align2::CENTER_CENTER,
                        "Stream Inactive\n(480x480 Native)",
                        egui::FontId::proportional(14.0),
                        Color32::from_rgb(148, 163, 184),
                    );
                }

                ui.add_space(10.0);

                // Metrics badges
                ui.horizontal(|ui| {
                    ui.label(format!("{:.1} FPS", self.cached_fps));
                    ui.separator();
                    ui.label(format!("{:.1} ms", self.cached_latency));
                    ui.separator();
                    ui.label(format!("{} KB", self.cached_frame_size / 1024));
                });

                ui.add_space(4.0);
                ui.label(egui::RichText::new(format!("Status: {}", self.cached_status)).weak());
            });

            // RIGHT COLUMN: Controls & Settings
            columns[1].vertical(|ui| {
                ui.label(egui::RichText::new("Display Source").strong());
                ui.horizontal(|ui| {
                    let selected_text = if let Some(m) = self.monitors.get(self.selected_monitor) {
                        format!("{} ({}x{}){}", m.name, m.width, m.height, if m.width == 480 && m.height == 480 { " *" } else { "" })
                    } else {
                        "No displays found".to_string()
                    };

                    egui::ComboBox::from_id_salt("display_picker")
                        .width(220.0)
                        .selected_text(selected_text)
                        .show_ui(ui, |ui| {
                            for (i, m) in self.monitors.iter().enumerate() {
                                let is_opt = m.width == 480 && m.height == 480;
                                let label = format!("{} ({}x{}){}", m.name, m.width, m.height, if is_opt { " * [480x480 Ideal]" } else { "" });
                                if ui.selectable_value(&mut self.selected_monitor, i, label).clicked() {
                                    self.controller.selected_monitor_idx.store(i as u32, Ordering::Relaxed);
                                }
                            }
                        });

                    if ui.button("Refresh").on_hover_text("Refresh monitor list").clicked() {
                        self.refresh_system_state();
                    }
                });

                ui.add_space(10.0);

                // Start / Stop Streaming Button
                let is_streaming = self.controller.is_streaming();
                let (btn_text, btn_color) = if is_streaming {
                    ("Stop Streaming", Color32::from_rgb(220, 38, 38))
                } else {
                    ("Start Streaming to Cooler", Color32::from_rgb(16, 185, 129))
                };

                let btn = egui::Button::new(egui::RichText::new(btn_text).size(15.0).color(Color32::WHITE))
                    .fill(btn_color)
                    .corner_radius(CornerRadius::same(6));

                if ui.add_sized([ui.available_width(), 36.0], btn).clicked() {
                    if is_streaming {
                        self.controller.stop();
                    } else {
                        if let Err(e) = self.controller.start() {
                            self.cached_status = format!("Start failed: {}", e);
                        }
                    }
                }

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                // Screen Hardware Controls
                ui.label(egui::RichText::new("Hardware Adjustments").strong());

                // Brightness slider
                ui.horizontal(|ui| {
                    ui.label("Brightness:");
                    let prev_b = self.brightness;
                    if ui.add(egui::Slider::new(&mut self.brightness, 0..=100).suffix("%")).changed() {
                        if prev_b != self.brightness {
                            self.controller.brightness_req.store(self.brightness, Ordering::Relaxed);
                        }
                    }
                });

                // Rotation selector
                ui.horizontal(|ui| {
                    ui.label("Rotation:");
                    for &angle in &[0u32, 90, 180, 270] {
                        if ui.selectable_value(&mut self.rotation, angle, format!("{} deg", angle)).clicked() {
                            self.controller.rotation_req.store(angle, Ordering::Relaxed);
                        }
                    }
                });

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                // Virtual Display Driver Assistant
                ui.label(egui::RichText::new("Virtual Display Driver (VDD)").strong());
                ui.horizontal(|ui| {
                    ui.label("Driver:");
                    if self.vdd_installed {
                        ui.label(egui::RichText::new("Installed").color(Color32::from_rgb(34, 197, 94)));
                    } else {
                        ui.label(egui::RichText::new("Not Installed").color(Color32::from_rgb(234, 179, 8)));
                    }

                    ui.label("| 480x480:");
                    if self.vdd_480_ready {
                        ui.label(egui::RichText::new("Ready").color(Color32::from_rgb(34, 197, 94)));
                    } else {
                        ui.label(egui::RichText::new("Not Set").color(Color32::from_rgb(234, 179, 8)));
                    }
                });

                ui.add_space(4.0);

                ui.horizontal(|ui| {
                    if ui.button("⚡ Enable 480x480 Virtual Monitor").on_hover_text("Installs/activates the 480x480 second monitor").clicked() {
                        let _ = VirtualDisplayManager::launch_installer_script();
                        self.action_message = Some(("Installer launched! Click 'Yes' on the UAC prompt.".to_string(), Instant::now()));
                    }

                    if ui.button("Configure 480x480").on_hover_text("Writes native 480x480 profile to C:\\VirtualDisplayDriver").clicked() {
                        match VirtualDisplayManager::write_480_config() {
                            Ok(_) => {
                                self.action_message = Some(("480x480 profile written!".to_string(), Instant::now()));
                                self.refresh_system_state();
                            }
                            Err(e) => {
                                self.action_message = Some((format!("Config error: {}", e), Instant::now()));
                            }
                        }
                    }

                    if !self.vdd_installed {
                        if ui.button("Install VDD (Winget)").on_hover_text("Installs Virtual-Display-Driver package via winget").clicked() {
                            let _ = VirtualDisplayManager::install_vdd_via_winget();
                            self.refresh_system_state();
                        }
                    }
                });

                if let Some((msg, time)) = &self.action_message {
                    if time.elapsed().as_secs() < 4 {
                        ui.label(egui::RichText::new(msg).color(Color32::from_rgb(56, 189, 248)));
                    }
                }
            });
        });
    }
}
