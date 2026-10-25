//! Fiche d'une rencontre passée et rédaction du compte rendu.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use eframe::egui::{self, Align, Layout, RichText, Ui};

use super::widgets::{download_bar, markdown, short_model, transcript};
use super::{TourDeTableApp, View};
use crate::config::LlmBackend;
use crate::models;
use crate::storage::{self, clock};

impl TourDeTableApp {
    /// Lance la rédaction automatique, si le modèle est prêt et que rien d'autre ne rédige.
    pub(super) fn auto_compte_rendu(&mut self, idx: usize) {
        if self.summary_job.is_some() {
            return;
        }
        if self.cfg.llm_backend == LlmBackend::Integre && !models::installed(&self.cfg.llm_model) {
            self.notify(
                "Transcription terminée. Pour la rédaction automatique du compte rendu, installez d'abord le modèle de rédaction (bouton ci-dessous).",
                false,
            );
            return;
        }
        self.notify("Transcription terminée : rédaction du compte rendu à partir de l'ordre du jour…", false);
        self.summarize(idx);
    }

    pub(super) fn summarize(&mut self, idx: usize) {
        let m = &self.meetings[idx];
        let (tx, rx) = crossbeam_channel::bounded(1);
        let moteur = self.cfg.moteur();
        let progress = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let progress_thread = progress.clone();
        let title = m.title.clone();
        let date = m.started_at.format("%Y-%m-%d à %H h %M").to_string();
        let duration = clock(m.duration_s * 1000);
        let agenda = m.agenda.as_ref().map(|a| a.text.clone());
        let transcript = m.plain_text();
        std::thread::spawn(move || {
            let req = crate::summary::Request {
                moteur: &moteur,
                progress: Some(&progress_thread),
                title: &title,
                date: &date,
                duration: &duration,
                agenda: agenda.as_deref(),
                transcript: &transcript,
            };
            let _ = tx.send(crate::summary::compte_rendu(&req).map_err(|e| format!("{e:#}")));
        });
        self.summary_job = Some((m.id.clone(), rx, progress, std::time::Instant::now()));
    }

    pub(super) fn past_view(&mut self, ui: &mut Ui, idx: usize) {
        let mut action = None;
        {
            let m = &mut self.meetings[idx];
            ui.horizontal(|ui| {
                let resp = ui.add(egui::TextEdit::singleline(&mut m.title).font(egui::TextStyle::Heading).desired_width(ui.available_width() - 330.0));
                if resp.lost_focus() {
                    let _ = m.save();
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Supprimer").clicked() {
                        action = Some("delete");
                    }
                    if ui.button("Dossier").clicked() {
                        action = Some("reveal");
                    }
                    if ui.button("Exporter").on_hover_text("Markdown, texte et sous-titres SRT").clicked() {
                        action = Some("export");
                    }
                    if ui.button("Copier").clicked() {
                        ui.ctx().copy_text(m.plain_text());
                    }
                });
            });
            ui.label(
                RichText::new(format!(
                    "{} · {} · {} segments · {}",
                    m.started_at.format("%Y-%m-%d %H h %M"),
                    clock(m.duration_s * 1000),
                    m.segments.len(),
                    short_model(&m.model)
                ))
                .small()
                .weak(),
            );
        }
        ui.add_space(8.0);

        self.agenda_card(ui);
        ui.add_space(6.0);

        let busy = self
            .summary_job
            .as_ref()
            .filter(|(id, ..)| *id == self.meetings[idx].id)
            .map(|(_, _, p, t0)| (p.load(Ordering::Relaxed), t0.elapsed().as_secs()));
        // Modèle de rédaction intégré pas encore téléchargé : on propose de l'installer.
        let llm_missing = (self.cfg.llm_backend == LlmBackend::Integre && !models::installed(&self.cfg.llm_model))
            .then(|| models::llm_info(&self.cfg.llm_model))
            .flatten();
        let has_agenda = self.meetings[idx].agenda.is_some();
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(if has_agenda { "Compte rendu" } else { "Résumé" }).strong());
                if let Some((jetons, secs)) = busy {
                    ui.spinner();
                    let etat = if jetons == 0 {
                        format!("Lecture de la transcription par le modèle local… {}", clock(secs * 1000))
                    } else {
                        // ≈ 0,75 mot par jeton en français
                        format!("Rédaction en cours : environ {} mots · {}", jetons * 3 / 4, clock(secs * 1000))
                    };
                    ui.label(RichText::new(etat).weak());
                } else if let Some(info) = llm_missing {
                    if let Some(dl) = self.download.as_ref().filter(|d| d.file == info.file) {
                        download_bar(ui, dl);
                    } else if ui
                        .add_enabled(self.download.is_none(), egui::Button::new(format!(
                            "Installer le modèle de rédaction ({:.1} Go, une seule fois)",
                            info.size_mb as f32 / 1000.0
                        )).small())
                        .on_hover_text(format!("{} · {}", info.label, info.note))
                        .clicked()
                    {
                        action = Some("install-llm");
                    }
                } else {
                    let label = match (self.meetings[idx].summary.is_some(), has_agenda) {
                        (true, _) => "Régénérer",
                        (false, true) => "Rédiger le compte rendu",
                        (false, false) => "Rédiger un résumé",
                    };
                    let hover = if has_agenda {
                        "Suit l'ordre du jour, point par point, à partir de la transcription"
                    } else {
                        "Ajoutez un ordre du jour pour obtenir un compte rendu point par point"
                    };
                    if ui
                        .small_button(label)
                        .on_hover_text(format!("{hover}\nModèle local : {}", self.cfg.moteur().nom()))
                        .clicked()
                    {
                        action = Some("summary");
                    }
                    if let Some(sum) = &self.meetings[idx].summary {
                        if ui.small_button("Copier").clicked() {
                            ui.ctx().copy_text(sum.clone());
                        }
                    }
                }
            });
            if let Some(s) = &self.meetings[idx].summary {
                egui::ScrollArea::vertical().id_salt("cr").max_height(360.0).show(ui, |ui| markdown(ui, s));
            }
        });
        ui.add_space(8.0);
        transcript(ui, &self.meetings[idx], false);

        match action {
            Some("delete") => {
                let m = self.meetings.remove(idx);
                match storage::delete(&m) {
                    Ok(()) => self.notify("Rencontre déplacée dans la corbeille de Tour de table.", false),
                    Err(e) => self.notify(format!("{e:#}"), true),
                }
                self.view = View::Live;
            }
            Some("reveal") => storage::reveal(&self.meetings[idx].dir()),
            Some("export") => match self.meetings[idx].export_all() {
                Ok(dir) => storage::reveal(&dir),
                Err(e) => self.notify(format!("Export : {e:#}"), true),
            },
            Some("summary") => self.summarize(idx),
            Some("install-llm") => {
                let file = self.cfg.llm_model.clone();
                self.start_download(&file);
            }
            _ => {}
        }
    }
}
