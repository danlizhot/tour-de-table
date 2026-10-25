//! Fenêtre des Paramètres.

use std::sync::atomic::Ordering;

use eframe::egui::{self, Align, Layout, RichText};

use super::TourDeTableApp;
use super::widgets::{SALLE, download_bar};
use crate::config::LlmBackend;
use crate::models;
use crate::storage;

impl TourDeTableApp {
    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_settings;
        let before = (self.cfg.model.clone(), self.cfg.use_gpu);
        let mut changed = false;
        egui::Window::new("Paramètres")
            .open(&mut open)
            .default_width(520.0)
            .collapsible(false)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().max_height(560.0).show(ui, |ui| {
                    ui.heading("Modèle de reconnaissance");
                    for info in models::CATALOG {
                        ui.horizontal(|ui| {
                            let installed = models::installed(info.file);
                            ui.add_enabled_ui(installed, |ui| {
                                ui.radio_value(&mut self.cfg.model, info.file.to_string(), info.label);
                            });
                            ui.label(RichText::new(format!("{} · ~{:.1} Go de RAM", info.note, info.ram_mb as f32 / 1000.0)).small().weak());
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if installed {
                                    ui.label(RichText::new("Installé").small().color(SALLE));
                                } else if self.download.as_ref().is_some_and(|d| d.file == info.file) {
                                    if ui.small_button("Annuler").clicked() {
                                        self.download.as_ref().unwrap().progress.cancel.store(true, Ordering::Relaxed);
                                    }
                                } else if ui
                                    .add_enabled(self.download.is_none(), egui::Button::new(format!("Télécharger · {} Mo", info.size_mb)).small())
                                    .clicked()
                                {
                                    self.start_download(info.file);
                                }
                            });
                        });
                    }
                    if let Some(dl) = &self.download {
                        download_bar(ui, dl);
                    }
                    let extra: Vec<String> = models::local_models()
                        .into_iter()
                        .filter(|f| !models::CATALOG.iter().any(|c| c.file == f))
                        .collect();
                    for f in extra {
                        ui.radio_value(&mut self.cfg.model, f.clone(), format!("{f} (personnalisé)"));
                    }
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Modèle affiné maison? Déposez le fichier ggml .bin dans le dossier des modèles.").small().weak());
                        if ui.small_button("Ouvrir").clicked() {
                            let dir = crate::config::models_dir();
                            let _ = std::fs::create_dir_all(&dir);
                            storage::reveal(&dir);
                        }
                    });

                    ui.separator();
                    ui.heading("Performance");
                    let max = std::thread::available_parallelism().map_or(8, |n| n.get());
                    changed |= ui.add(egui::Slider::new(&mut self.cfg.threads, 1..=max).text("fils de calcul")).changed();
                    changed |= ui
                        .add(egui::Slider::new(&mut self.cfg.beam_size, 1..=8).text("faisceau (précision ↔ vitesse)"))
                        .changed();
                    ui.add_enabled_ui(cfg!(any(feature = "metal", feature = "vulkan", feature = "cuda")), |ui| {
                        ui.checkbox(&mut self.cfg.use_gpu, "Accélération GPU");
                    });

                    ui.separator();
                    ui.heading("Détection de la parole");
                    ui.add(egui::Slider::new(&mut self.cfg.vad_sensitivity, 0.0..=1.0).text("sensibilité"));
                    ui.checkbox(&mut self.cfg.save_audio, "Conserver l'audio (WAV) avec la transcription");

                    ui.separator();
                    ui.heading("Lexique québécois");
                    ui.label(
                        RichText::new(
                            "Un terme par ligne (noms, organismes, acronymes) pour guider la reconnaissance,\nou une correction « mauvais => bon ». Ex. : hydro québec => Hydro-Québec",
                        )
                        .small()
                        .weak(),
                    );
                    changed |= ui
                        .add(egui::TextEdit::multiline(&mut self.cfg.glossary).desired_rows(6).desired_width(f32::INFINITY))
                        .changed();

                    ui.separator();
                    ui.heading("Rédaction des comptes rendus");
                    ui.radio_value(&mut self.cfg.llm_backend, LlmBackend::Integre, "Modèle intégré (recommandé : rien à installer)");
                    ui.radio_value(&mut self.cfg.llm_backend, LlmBackend::Ollama, "Ollama (avancé, si vous l'utilisez déjà)");
                    ui.checkbox(
                        &mut self.cfg.auto_compte_rendu,
                        "Rédiger le compte rendu automatiquement à la fin de la transcription (si un ordre du jour est joint)",
                    );
                    ui.add_space(4.0);
                    if self.cfg.llm_backend == LlmBackend::Integre {
                        for info in models::LLM_CATALOG {
                            ui.horizontal(|ui| {
                                let installed = models::installed(info.file);
                                ui.radio_value(&mut self.cfg.llm_model, info.file.to_string(), info.label);
                                ui.label(
                                    RichText::new(format!("{} · {:.1} Go · ~{:.1} Go de RAM", info.note, info.size_mb as f32 / 1000.0, info.ram_mb as f32 / 1000.0))
                                        .small()
                                        .weak(),
                                );
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if installed {
                                        ui.label(RichText::new("Installé").small().color(SALLE));
                                    } else if self.download.as_ref().is_some_and(|d| d.file == info.file) {
                                        if ui.small_button("Annuler").clicked() {
                                            self.download.as_ref().unwrap().progress.cancel.store(true, Ordering::Relaxed);
                                        }
                                    } else if ui
                                        .add_enabled(self.download.is_none(), egui::Button::new("Télécharger").small())
                                        .clicked()
                                    {
                                        self.start_download(info.file);
                                    }
                                });
                            });
                        }
                        if let Some(dl) = self.download.as_ref().filter(|d| models::llm_info(&d.file).is_some()) {
                            download_bar(ui, dl);
                        }
                    } else {
                        ui.horizontal(|ui| {
                            ui.label("Adresse");
                            ui.text_edit_singleline(&mut self.cfg.ollama_url);
                        });
                        ui.horizontal(|ui| {
                            ui.label("Modèle");
                            ui.text_edit_singleline(&mut self.cfg.ollama_model);
                        });
                    }
                });
            });
        if changed {
            self.push_settings();
        }
        if (self.cfg.model.clone(), self.cfg.use_gpu) != before && self.recorder.is_none() {
            self.load_model();
        }
        if self.show_settings && !open {
            self.cfg.save();
        }
        self.show_settings = open;
    }
}
