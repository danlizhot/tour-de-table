//! Fiche d'une rencontre passée et rédaction du compte rendu.

use std::sync::Arc;
use eframe::egui::{self, Align, Layout, RichText, Ui};

use super::widgets::short_model;
use super::{TourDeTableApp, View};
use crate::audio::blocs;
use crate::config;
use crate::storage::{self, clock};

impl TourDeTableApp {
    /// Lance la rédaction automatique, si le modèle est prêt et que rien d'autre ne rédige.
    pub(super) fn auto_compte_rendu(&mut self, idx: usize) {
        if self.summary_job.is_some() {
            return;
        }
        if !self.redaction_prete() {
            self.notify(
                "Transcription terminée et nettoyée. Le modèle de rédaction n'est pas encore prêt : rédigez le compte rendu plus tard avec le bouton ci-dessous.",
                false,
            );
            return;
        }
        self.notify("Transcription nettoyée : rédaction du compte rendu à partir de l'ordre du jour…", false);
        self.summarize(idx, true);
    }

    /// Rédige le compte rendu dans un fil ; `reprendre` réutilise les résumés de points déjà
    /// enregistrés (resume.md) d'un compte rendu interrompu.
    pub(super) fn summarize(&mut self, idx: usize, reprendre: bool) {
        self.notify("Le compte rendu se rédige en arrière-plan : vous pouvez continuer à utiliser Tour de table.", false);
        let m = &self.meetings[idx];
        let (tx, rx) = crossbeam_channel::bounded(1);
        let moteur = config::moteur();
        let avancement = Arc::new(crate::summary::Avancement::default());
        let avancement_fil = avancement.clone();
        let (points, hors_point) = crate::summary::points_de(m);
        let title = m.title.clone();
        let date = crate::summary::date_longue(&m.started_at);
        let heure = crate::summary::heure(&m.started_at);
        let duration = clock(m.duration_s * 1000);
        let agenda = m.agenda.as_ref().map(|a| a.text.clone());
        let transcript = m.texte_compte_rendu();
        let lexique = crate::quebec::Glossary::parse(&self.cfg.glossary).termes().to_vec();
        std::thread::spawn(move || {
            let req = crate::summary::Request {
                moteur: &moteur,
                avancement: Some(&avancement_fil),
                title: &title,
                date: &date,
                heure: &heure,
                duration: &duration,
                agenda: agenda.as_deref(),
                transcript: &transcript,
                points,
                hors_point,
                reprendre,
                lexique,
            };
            let _ = tx.send(crate::summary::compte_rendu(&req).map_err(|e| format!("{e:#}")));
        });
        self.summary_job = Some((m.id.clone(), rx, avancement, std::time::Instant::now()));
    }

    /// Reconstitue l'audio de la rencontre (ou d'un point) dans un fichier choisi, dans un fil.
    pub(super) fn exporter_audio(&mut self, idx: usize, portee: blocs::Portee) {
        if self.audio_export.is_some() {
            self.notify("Une exportation audio est déjà en cours.", false);
            return;
        }
        let m = self.meetings[idx].clone();
        let nom = match portee {
            blocs::Portee::Tout => format!("{}.wav", m.dossier),
            blocs::Portee::Point(i) => {
                let titres = super::live::titres_points(m.agenda.as_ref());
                format!("{}-{}.wav", m.dossier, blocs::prefixe(Some(i), &titres))
            }
        };
        let Some(chemin) = rfd::FileDialog::new()
            .set_title("Exporter l'audio")
            .add_filter("Audio WAV", &["wav"])
            .set_file_name(nom)
            .save_file()
        else {
            return;
        };
        let (tx, rx) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send(blocs::exporter(&m, portee, &chemin).map_err(|e| format!("{e:#}")));
        });
        self.audio_export = Some(rx);
    }

    pub(super) fn past_view(&mut self, ui: &mut Ui, idx: usize) {
        let mut action = None;
        let occupe = self.decoupage_job.is_some() || self.import_job.is_some() || self.finishing;
        let peut_poursuivre = !occupe
            && self.recorder.is_none()
            && self.live.is_none()
            && self.fin_apres_panne.is_none()
            && matches!(self.model, super::ModelState::Ready);
        let details = {
            let m = &mut self.meetings[idx];
            ui.horizontal(|ui| {
                let resp = ui.add(egui::TextEdit::singleline(&mut m.title).font(egui::TextStyle::Heading).desired_width(ui.available_width() - if m.interrompue { 820.0 } else { 640.0 }));
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
                    if m.interrompue
                        && crate::audio::blocs::existe(&m.dir())
                        && ui
                            .add_enabled(peut_poursuivre, egui::Button::new("Poursuivre la rencontre"))
                            .on_hover_text("La rencontre a été interrompue : l'enregistrement reprend à la suite, au point où vous étiez rendus")
                            .clicked()
                    {
                        action = Some("poursuivre");
                    }
                    if crate::audio::blocs::existe(&m.dir())
                        && m.agenda.as_ref().is_some_and(|a| !a.points.is_empty())
                        && ui
                            .add_enabled(!occupe, egui::Button::new("Découper par points"))
                            .on_hover_text("Répartit l'audio et la transcription entre les points de l'ordre du jour")
                            .clicked()
                    {
                        action = Some("decouper");
                    }
                    if crate::audio::blocs::existe(&m.dir())
                        && ui.button("Exporter l'audio complet").on_hover_text("Fichier WAV de toute la rencontre").clicked()
                    {
                        action = Some("audio");
                    }
                    if ui.button("Exporter").on_hover_text("Markdown, texte et sous-titres SRT").clicked() {
                        action = Some("export");
                    }
                    if ui.button("Copier").clicked() {
                        ui.ctx().copy_text(m.plain_text());
                    }
                });
            });
            let details = format!(
                "{} · {} · {} segments{} · {}",
                m.started_at.format("%Y-%m-%d %H h %M"),
                clock(m.duration_s * 1000),
                m.segments.len(),
                m.liste_pauses().map(|p| format!(" · pauses : {p}")).unwrap_or_default(),
                short_model(&m.model)
            );
            RichText::new(details).small().weak()
        };
        ui.horizontal(|ui| {
            ui.label(details);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| self.bascule_cote_a_cote(ui));
        });
        action = self.volets(ui, idx).or(action);

        match action {
            Some("delete") => {
                let id = self.meetings[idx].id.clone();
                self.arreter_nettoyage(&id);
                let m = self.meetings.remove(idx);
                match storage::delete(&m) {
                    Ok(()) => self.notify("Rencontre déplacée dans la corbeille de Tour de table.", false),
                    Err(e) => self.notify(format!("{e:#}"), true),
                }
                self.view = View::Live;
            }
            Some("reveal") => storage::reveal(&self.meetings[idx].dir()),
            Some("audio") => self.exporter_audio(idx, blocs::Portee::Tout),
            Some("export") => match self.meetings[idx].export_all() {
                Ok(dir) => storage::reveal(&dir),
                Err(e) => self.notify(format!("Export : {e:#}"), true),
            },
            // « Régénérer » rédige tout à nouveau ; sinon, les points déjà résumés sont repris.
            Some("summary") => {
                let reprendre = self.meetings[idx].summary.is_none();
                self.summarize(idx, reprendre);
            }
            // Le .docx est réécrit à chaque export ; une rencontre rédigée avant l'export Word n'en a pas encore.
            Some("visualiser") => match self.meetings[idx].export_all() {
                Ok(dir) => storage::reveal(&dir.join("compte-rendu.docx")),
                Err(e) => self.notify(format!("Export : {e:#}"), true),
            },
            Some("decouper") => self.ouvrir_verification(self.meetings[idx].clone(), false),
            Some("poursuivre") => self.poursuivre(idx),
            _ => {}
        }
    }
}
