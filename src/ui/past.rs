//! Fiche d'une rencontre passée et rédaction du compte rendu.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use eframe::egui::{self, Align, Layout, RichText, Ui};

use super::widgets::{markdown, short_model, transcript, transcript_nettoye};
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
        self.liberer_whisper();
        self.passer_en_arriere_plan("Le compte rendu se rédige en arrière-plan : Tour de table réapparaîtra dès qu'il sera prêt.");
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
        {
            let m = &mut self.meetings[idx];
            ui.horizontal(|ui| {
                let resp = ui.add(egui::TextEdit::singleline(&mut m.title).font(egui::TextStyle::Heading).desired_width(ui.available_width() - 640.0));
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
            .map(|(_, _, a, t0)| {
                let (point, points) = (a.point.load(Ordering::Relaxed), a.points.load(Ordering::Relaxed));
                let etape = match (point, points) {
                    (_, 0) => String::new(),
                    (p, n) if p > n => "Compte rendu — assemblage · ".to_string(),
                    (p, n) => format!("Compte rendu — point {p} sur {n} · "),
                };
                (etape, a.jetons.load(Ordering::Relaxed), t0.elapsed().as_secs())
            });
        let redaction_prete = self.redaction_prete();
        let has_agenda = self.meetings[idx].agenda.is_some();
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(if has_agenda { "Compte rendu" } else { "Résumé" }).strong());
                if let Some((etape, jetons, secs)) = busy {
                    ui.spinner();
                    let etat = if jetons == 0 {
                        format!("{etape}Lecture de la transcription par le modèle local… {}", clock(secs * 1000))
                    } else {
                        // ≈ 0,75 mot par jeton en français
                        format!("{etape}Rédaction : environ {} mots · {}", jetons * 3 / 4, clock(secs * 1000))
                    };
                    ui.label(RichText::new(etat).weak());
                } else if !redaction_prete {
                    let texte = if self.installation.is_some() {
                        "Vérification du modèle de rédaction…"
                    } else {
                        "Modèle de rédaction introuvable : redémarrez Tour de table pour le réinstaller."
                    };
                    ui.label(RichText::new(texte).weak());
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
                        .add_enabled(!self.nettoyage_en_cours(), egui::Button::new(label).small())
                        .on_hover_text(format!("{hover}\nModèle local : {}", config::moteur().nom()))
                        .on_disabled_hover_text("Attendez la fin du nettoyage de la transcription")
                        .clicked()
                    {
                        action = Some("summary");
                    }
                    if self.meetings[idx].summary.is_some()
                        && ui.small_button("Visualiser").on_hover_text("Ouvre le compte rendu dans Word (compte-rendu.docx)").clicked()
                    {
                        action = Some("visualiser");
                    }
                    if let Some(sum) = &self.meetings[idx].summary
                        && ui.small_button("Copier").clicked()
                    {
                        ui.ctx().copy_text(sum.clone());
                    }
                }
            });
            if let Some(s) = &self.meetings[idx].summary {
                egui::ScrollArea::vertical().id_salt("cr").max_height(360.0).show(ui, |ui| markdown(ui, s));
            }
        });
        ui.add_space(8.0);
        self.barre_nettoyage(ui, idx);
        let m = &self.meetings[idx];
        match m.nettoyage.as_ref().filter(|_| !self.vue_brute) {
            Some(n) => transcript_nettoye(ui, m, n),
            None => transcript(ui, m, false),
        }

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
            _ => {}
        }
    }
}
