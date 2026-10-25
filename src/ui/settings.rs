//! Fenêtre des Paramètres : seulement ce que l'application ne peut pas décider
//! seule. Modèles, fils de calcul, GPU, détection de voix… sont automatiques.

use eframe::egui::{self, RichText};

use super::TourDeTableApp;
use super::widgets::device_combo;
use crate::config::CaptureMode;

impl TourDeTableApp {
    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_settings;
        let mut changed = false;
        egui::Window::new("Paramètres")
            .open(&mut open)
            .default_width(520.0)
            .collapsible(false)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().max_height(560.0).show(ui, |ui| {
                    ui.heading("Son");
                    egui::Grid::new("peripheriques").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                        ui.label("Micro de la salle");
                        device_combo(ui, "mic", &mut self.cfg.mic_device, &self.mics, "Par défaut");
                        ui.end_row();
                        ui.label("Sortie audio à enregistrer");
                        device_combo(ui, "sys", &mut self.cfg.system_device, &self.system_sources, "Par défaut");
                        ui.end_row();
                    });

                    ui.add_space(8.0);
                    ui.label("Les participants à distance sortent-ils dans les haut-parleurs de la salle?");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut self.cfg.mode, CaptureMode::Separe, "Non")
                            .on_hover_text("Casque ou haut-parleur de conférence : la transcription distingue « Salle » et « À distance ».");
                        ui.radio_value(&mut self.cfg.mode, CaptureMode::Mixe, "Oui")
                            .on_hover_text("Le micro capte aussi les participants à distance : les deux sources sont réunies pour éviter les doublons.");
                    });

                    ui.separator();
                    ui.heading("Lexique");
                    ui.label(
                        RichText::new(
                            "Un terme par ligne (noms propres, organismes, sigles) pour guider la reconnaissance,\nou une correction « mauvais => bon ». Ex. : hydro québec => Hydro-Québec",
                        )
                        .small()
                        .weak(),
                    );
                    changed |= ui
                        .add(egui::TextEdit::multiline(&mut self.cfg.glossary).desired_rows(6).desired_width(f32::INFINITY))
                        .changed();
                });
            });
        if changed {
            self.push_settings();
        }
        if self.show_settings && !open {
            self.cfg.save();
        }
        self.show_settings = open;
    }
}
