//! About is a presentation page. Creator details come from the shared host data.
use eframe::egui::{self, RichText};

#[derive(Default)]
pub(super) struct AboutPanel {
    portrait: Option<egui::TextureHandle>,
    portrait_error: Option<String>,
}

impl AboutPanel {
    pub(super) fn show(&mut self, ui: &mut egui::Ui, vacuum: &egui::TextureHandle) {
        // Decode only when About is opened. The transparent portrait is embedded
        // in the executable and does not depend on a website or external file.
        if self.portrait.is_none() && self.portrait_error.is_none() {
            match image::load_from_memory(include_bytes!(
                "../../../assets/branding/creator-avatar.png"
            )) {
                Ok(image) => {
                    let image = image.to_rgba8();
                    let pixels = egui::ColorImage::from_rgba_unmultiplied(
                        [image.width() as usize, image.height() as usize],
                        image.as_raw(),
                    );
                    self.portrait = Some(ui.ctx().load_texture(
                        "creator-avatar",
                        pixels,
                        egui::TextureOptions::LINEAR,
                    ));
                }
                Err(_) => {
                    self.portrait_error = Some("The embedded avatar could not be read.".into());
                }
            }
        }

        let about = crate::about::information();
        ui.vertical_centered(|ui| {
            ui.add_space(16.0);
            ui.label(RichText::new(about.name).size(30.0).strong());
            ui.label(RichText::new(format!("Version {}", about.version)).weak());
            let time = ui.input(|input| input.time) as f32;
            let (space, _) = ui.allocate_exact_size(egui::vec2(160.0, 120.0), egui::Sense::hover());
            let center = space.center() + egui::vec2((time * 1.5).sin() * 20.0, (time * 3.0).sin() * 3.0);
            ui.put(egui::Rect::from_center_size(center, egui::vec2(106.0, 106.0)),
                egui::Image::new(vacuum).fit_to_exact_size(egui::vec2(106.0, 106.0))
                    .rotate((time * 1.5).cos() * 0.07, egui::vec2(0.5, 0.8))
                    .alt_text("Herbert's animated vacuum"));
            ui.separator();
            ui.add_space(12.0);
            if let Some(portrait) = &self.portrait {
                let side = ui.available_width().min(160.0);
                ui.add(
                    egui::Image::new(portrait)
                        .fit_to_exact_size(egui::vec2(side, side))
                        .alt_text("Creator avatar"),
                );
            }
            if let Some(error) = &self.portrait_error {
                ui.label(error);
            }
            ui.add_space(8.0);
            ui.label("Creator of Herbert");
            ui.add_space(20.0);
            ui.label(RichText::new(about.dedication).size(18.0));
            ui.add_space(16.0);
            for profile in about.profiles {
                ui.hyperlink_to(profile.label, profile.url);
            }
            ui.add_space(20.0);
            ui.label(RichText::new(about.copyright).small().weak());
            ui.add_space(16.0);
        });
        ui.ctx().request_repaint();
    }
}
