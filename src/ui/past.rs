//! Fiche d'une rencontre passée et rédaction du compte rendu.

use std::sync::Arc;
use eframe::egui::{self, Align, Color32, Layout, RichText, Ui};

use super::widgets::short_model;
use super::{apparence, colonne};
use super::{TourDeTableApp, View};
use crate::audio::blocs;
use crate::{config, models};
use crate::storage::{self, clock};

impl TourDeTableApp {
    /// Lance la rédaction automatique, si le modèle est prêt et que rien d'autre ne rédige.
    pub(super) fn auto_compte_rendu(&mut self, idx: usize) {
        if self.summary_job.is_some() {
            return;
        }
        if !models::redaction_locale() {
            self.notify(
                "Transcription terminée et nettoyée. Cet ordinateur a moins de 16 Go de mémoire vive : le compte rendu n'y est pas \
                 rédigé. « Copier pour un assistant en ligne », dans l'onglet Compte rendu, prépare une demande à coller dans Copilot \
                 ou ChatGPT.",
                false,
            );
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

    /// Fin d'un archivage : la rencontre revient dans l'historique si l'archive n'a pas pu être écrite.
    pub(super) fn pump_archivage(&mut self) {
        while let Ok((m, res)) = self.archivage.1.try_recv() {
            self.archivages_en_cours -= 1;
            match res {
                Ok(zip) => {
                    let nom = zip.file_name().unwrap_or_default().to_string_lossy().into_owned();
                    log::info!("« {} » archivée dans {}", m.title, zip.display());
                    self.notify(format!("Rencontre archivée : {nom}, dans le dossier Archives."), false);
                }
                Err(e) => {
                    log::error!("Archivage de « {} » : {e}", m.title);
                    self.notify(format!("{e}. La rencontre reste dans l'historique."), true);
                    let vue = match self.view {
                        View::Past(i) => self.meetings.get(i).map(|v| v.id.clone()),
                        _ => None,
                    };
                    self.meetings.push(m);
                    self.meetings.sort_by_key(|m| std::cmp::Reverse(m.started_at));
                    if let Some(id) = vue
                        && let Some(i) = self.meetings.iter().position(|m| m.id == id)
                    {
                        self.view = View::Past(i);
                    }
                }
            }
        }
    }

    pub(super) fn past_view(&mut self, ui: &mut Ui, idx: usize) {
        // Une autre rencontre est ouverte : son écoute s'arrête.
        if self.ecoute.as_ref().is_some_and(|e| e.id != self.meetings[idx].id) {
            self.ecoute = None;
        }
        let mut action = None;
        let occupe = self.decoupage_job.is_some() || self.import_job.is_some() || self.finishing;
        let peut_poursuivre = !occupe
            && self.recorder.is_none()
            && self.live.is_none()
            && self.fin_apres_panne.is_none()
            && matches!(self.model, super::ModelState::Ready);
        let id = self.meetings[idx].id.as_str();
        let travail_en_cours = self.summary_job.as_ref().is_some_and(|(i, ..)| i == id)
            || self.nettoyage_job.as_ref().is_some_and(|t| t.id == id)
            || self.voix_job.as_ref().is_some_and(|c| c.id == id)
            || self.audio_export.is_some()
            || self.decoupage_job.is_some();
        let p = apparence::palette(ui);
        {
            let m = &mut self.meetings[idx];
            let audio = crate::audio::blocs::existe(&m.dir());
            let decoupable = audio && m.agenda.as_ref().is_some_and(|a| !a.points.is_empty());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let largeur_titre = (ui.available_width() - if m.interrompue { 720.0 } else { 520.0 }).max(260.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    let titre = egui::TextEdit::singleline(&mut m.title)
                        .frame(egui::Frame::NONE)
                        .font(colonne::police_grasse(24.0))
                        .text_color(p.texte)
                        .desired_width(largeur_titre);
                    let resp = ui.add(titre).on_hover_text("Cliquez pour renommer la rencontre");
                    if resp.lost_focus() {
                        let _ = m.save();
                    }
                    let mut morceaux = vec![
                        format!("{}, {}", crate::summary::date_longue(&m.started_at), crate::summary::heure(&m.started_at)),
                        apparence::duree_longue(m.duration_s),
                    ];
                    if let Some(v) = &m.voix {
                        morceaux.push(format!("{} voix", v.nombre()));
                    }
                    if let Some(n) = m.agenda.as_ref().map(|a| a.points.len()).filter(|n| *n > 0) {
                        morceaux.push(format!("{n} point{}", if n > 1 { "s" } else { "" }));
                    }
                    if let Some(pauses) = m.liste_pauses() {
                        morceaux.push(format!("pauses : {pauses}"));
                    }
                    ui.label(RichText::new(morceaux.join(" · ")).size(13.0).color(p.texte2))
                        .on_hover_text(format!("{} segments transcrits avec {}", m.segments.len(), short_model(&m.model)));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if colonne::bouton_icone(ui, colonne::icone_archive, !travail_en_cours)
                        .on_hover_text("Archiver : comprime la rencontre dans un fichier .zip du dossier Archives et la retire de l'historique")
                        .on_disabled_hover_text("Attendez la fin du travail en cours sur cette rencontre")
                        .clicked()
                    {
                        action = Some("archiver");
                    }
                    if colonne::bouton_icone(ui, colonne::icone_dossier, true).on_hover_text("Ouvrir le dossier de la rencontre").clicked() {
                        action = Some("reveal");
                    }
                    let exporter = apparence::bouton_widget(ui, "Exporter      ", apparence::Genre::Principal, 38.0);
                    let (resp, _) = egui::containers::menu::MenuButton::from_button(exporter).ui(ui, |ui| {
                        ui.set_min_width(260.0);
                        if ui.button("Fichiers texte (Word, Markdown, sous-titres)").clicked() {
                            action = Some("export");
                        }
                        if audio && ui.button("Audio complet (WAV)").clicked() {
                            action = Some("audio");
                        }
                    });
                    apparence::chevron(ui, &resp, Color32::WHITE);
                    if ui.add(apparence::bouton_widget(ui, "Copier", apparence::Genre::Secondaire, 38.0)).on_hover_text("Copie la transcription").clicked() {
                        ui.ctx().copy_text(m.plain_text());
                    }
                    if decoupable
                        && ui
                            .add_enabled(!occupe, apparence::bouton_widget(ui, "Découper par points", apparence::Genre::Secondaire, 38.0))
                            .on_hover_text("Répartit l'audio et la transcription entre les points de l'ordre du jour")
                            .clicked()
                    {
                        action = Some("decouper");
                    }
                    if m.interrompue
                        && audio
                        && ui
                            .add_enabled(peut_poursuivre, apparence::bouton_widget(ui, "Poursuivre la rencontre", apparence::Genre::Secondaire, 38.0))
                            .on_hover_text("La rencontre a été interrompue : l'enregistrement reprend à la suite, au point où vous étiez rendus")
                            .clicked()
                    {
                        action = Some("poursuivre");
                    }
                });
            });
            ui.add_space(10.0);
        }
        // Lecteur au bas de la fiche, sous les volets.
        if self.recorder.is_none() && crate::audio::blocs::existe(&self.meetings[idx].dir()) {
            egui::Panel::bottom("lecteur")
                .show_separator_line(false)
                .frame(egui::Frame::new().fill(p.surface).stroke(egui::Stroke::new(1.0, p.bordure)).corner_radius(egui::CornerRadius::same(14)).inner_margin(egui::Margin::symmetric(16, 10)).outer_margin(egui::Margin { left: 0, right: 0, top: 10, bottom: 0 }))
                .show(ui, |ui| self.lecteur(ui, idx));
        }
        action = self.volets(ui, idx).or(action);

        match action {
            Some("archiver") => {
                let id = self.meetings[idx].id.clone();
                self.nettoyages_en_attente.retain(|(i, _)| *i != id);
                self.voix_en_attente.retain(|i| *i != id);
                self.ecoute = None;
                let m = self.meetings.remove(idx);
                self.view = View::Live;
                self.notify(format!("Archivage de « {} »…", m.title), false);
                let tx = self.archivage.0.clone();
                let ctx = self.ctx.clone();
                self.archivages_en_cours += 1;
                std::thread::spawn(move || {
                    let res = storage::archiver(&m).map_err(|e| format!("{e:#}"));
                    let _ = tx.send((m, res));
                    ctx.request_repaint();
                });
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
            Some("assistant") => {
                let demande = crate::summary::demande_assistant(&self.meetings[idx]);
                self.ctx.copy_text(demande);
                self.notify("Demande copiée : collez-la (Ctrl+V) dans Copilot, ChatGPT ou l'assistant permis par votre organisme.", false);
            }
            Some("decouper") => {
                self.ecoute = None;
                self.ouvrir_verification(self.meetings[idx].clone(), false);
            }
            Some("poursuivre") => self.poursuivre(idx),
            _ => {}
        }
    }
}
