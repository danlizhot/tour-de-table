//! Suppression définitive d'une rencontre (sans corbeille) et annulation d'un import en cours.
//!
//! Le dossier n'est retiré qu'une fois que plus rien n'y travaille : sinon la sauvegarde qui suit chaque segment
//! le recréerait, avec un `rencontre.json` à moitié rempli. Il est d'abord marqué (`storage::supprimer_dossier`) :
//! une suppression inachevée se termine au prochain démarrage.

use std::sync::atomic::Ordering;

use eframe::egui::{self, Color32, CornerRadius, Margin, RichText};

use super::apparence::{self, Genre, gras, palette_de};
use super::{TourDeTableApp, View};
use crate::audio::import;
use crate::storage;

impl TourDeTableApp {
    /// Un import est en cours (décodage, transcription ou derniers énoncés) et peut être annulé. Jamais un
    /// enregistrement : annuler détruirait le seul exemplaire de la rencontre.
    pub(super) fn import_annulable(&self) -> bool {
        self.recorder.is_none()
            && self.annulation.is_none()
            && self.live.as_ref().is_some_and(|m| self.import_job.is_some() || (self.finishing && import::source_copiee(&m.dir()).is_some()))
    }

    /// « Annuler l'import » : le fil s'arrête, les énoncés encore en route sont jetés, puis le dossier créé pour la
    /// rencontre est retiré (`pump_suppression`). Le fichier importé, lui, n'est jamais touché : seule sa copie
    /// dans le dossier de la rencontre disparaît.
    pub(super) fn annuler_import(&mut self) {
        if !self.import_annulable() {
            return;
        }
        if let Some((_, _, av)) = &self.import_job {
            av.annule.store(true, Ordering::Relaxed);
        }
        let Some(mut m) = self.live.take() else { return };
        let dir = m.dir();
        // Marqué tout de suite : une application fermée d'ici la fin ne reprendra pas l'import au démarrage.
        if let Err(e) = storage::marquer_a_supprimer(&dir) {
            log::warn!("Annulation de l'import « {} » : {e:#}", m.title);
        }
        self.finishing = false;
        self.poursuivie = false;
        self.voix_direct = None;
        self.voix_direct_import = None;
        // L'ordre du jour préparé pour cet import reste là pour le suivant.
        if self.draft_agenda.is_none() {
            self.draft_agenda = m.agenda.take();
        }
        log::info!("Import « {} » annulé", m.title);
        self.notify(format!("Import de « {} » annulé : les fichiers créés pour cette rencontre sont supprimés.", m.title), false);
        self.annulation = Some(dir);
        self.view = View::Live;
    }

    /// Appelé à chaque image depuis `pump` : dossier d'un import annulé retiré quand le fil est arrêté et que Whisper
    /// a rendu les énoncés déjà reçus ; fin des suppressions.
    pub(super) fn pump_suppression(&mut self) {
        if self.annulation.is_some() && self.import_job.is_none() && self.in_flight == 0 && self.utt_rx.is_empty() {
            let dir = self.annulation.take().unwrap_or_default();
            self.lancer_suppression(String::new(), dir);
        }
        while let Ok((titre, res)) = self.suppression.1.try_recv() {
            match res {
                Ok(()) if titre.is_empty() => {}
                Ok(()) => self.notify(format!("« {titre} » est supprimée définitivement."), false),
                Err(e) => {
                    log::error!("Suppression de « {titre} » : {e}");
                    self.notify(format!("{e}. Le dossier sera retiré au prochain démarrage de Tour de table."), true);
                }
            }
        }
    }

    fn lancer_suppression(&mut self, titre: String, dir: std::path::PathBuf) {
        let tx = self.suppression.0.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let res = storage::supprimer_dossier(&dir).map_err(|e| format!("{e:#}"));
            let _ = tx.send((titre, res));
            ctx.request_repaint();
        });
    }

    /// Travail en cours sur une rencontre de l'historique : ni archivage ni suppression d'ici sa fin.
    pub(super) fn travail_sur(&self, id: &str) -> bool {
        self.summary_job.as_ref().is_some_and(|(i, ..)| i == id)
            || self.nettoyage_job.as_ref().is_some_and(|t| t.id == id)
            || self.voix_job.as_ref().is_some_and(|c| c.id == id)
            || self.fin_apres_panne.as_ref().is_some_and(|f| f.id == id)
            || self.audio_export.is_some()
            || self.decoupage_job.is_some()
            || self.verification.is_some()
    }

    /// Supprime définitivement une rencontre de l'historique, après confirmation : retirée des files d'attente, puis
    /// son dossier effacé dans un fil.
    fn supprimer_rencontre(&mut self, id: &str) {
        let Some(idx) = self.meetings.iter().position(|m| m.id == id) else { return };
        if self.travail_sur(id) {
            self.notify("Attendez la fin du travail en cours sur cette rencontre avant de la supprimer.", true);
            return;
        }
        self.nettoyages_en_attente.retain(|(i, _)| i != id);
        self.voix_en_attente.retain(|i| i != id);
        self.imports_a_reprendre.retain(|i| i != id);
        self.fins_a_transcrire.retain(|i| i != id);
        if self.noms_des_voix.as_deref() == Some(id) {
            self.noms_des_voix = None;
        }
        // La lecture garde un fichier audio ouvert : arrêtée avant d'effacer.
        self.ecoute = None;
        let m = self.meetings.remove(idx);
        self.view = View::Live;
        log::info!("Suppression définitive de « {} » ({})", m.title, m.dir().display());
        self.lancer_suppression(m.title.clone(), m.dir());
    }

    /// Fenêtre de confirmation de « Supprimer définitivement ».
    pub(super) fn fenetre_suppression(&mut self, ctx: &egui::Context) {
        let Some(id) = self.confirmer_suppression.clone() else { return };
        let Some(m) = self.meetings.iter().find(|m| m.id == id) else {
            self.confirmer_suppression = None;
            return;
        };
        let titre = m.title.clone();
        let quand = format!("{}, {}", crate::summary::date_longue(&m.started_at), crate::summary::heure(&m.started_at));
        let p = palette_de(ctx);
        let cadre = egui::Frame::new().fill(p.surface).corner_radius(CornerRadius::same(20)).inner_margin(Margin::same(24)).shadow(egui::Shadow {
            offset: [0, 12],
            blur: 40,
            spread: 0,
            color: Color32::from_black_alpha(70),
        });
        let (mut supprimer, mut fermer) = (false, false);
        let reponse = egui::Modal::new(egui::Id::unique("confirmer-suppression")).frame(cadre).backdrop_color(Color32::from_rgba_unmultiplied(20, 28, 45, 96)).show(ctx, |ui| {
            ui.set_width(480.0);
            ui.label(gras("Supprimer définitivement cette rencontre ?", 19.0).color(p.texte));
            ui.add_space(10.0);
            ui.label(RichText::new(format!("« {titre} »")).size(15.0).strong().color(p.texte));
            ui.label(RichText::new(quand).size(13.0).color(p.texte2));
            ui.add_space(10.0);
            ui.label(
                RichText::new(
                    "L'audio, la transcription, le compte rendu et les fichiers exportés dans son dossier seront effacés de \
                     l'ordinateur, sans passer par la corbeille. Cette action est irréversible. Pour garder la rencontre \
                     hors de l'historique, archivez-la plutôt.",
                )
                .size(13.5)
                .color(p.texte),
            );
            ui.add_space(18.0);
            ui.horizontal(|ui| {
                // « Annuler » à gauche, la suppression en rouge à droite : jamais au même endroit qu'« Archiver ».
                if ui.add(apparence::bouton_widget(ui, "Annuler", Genre::Secondaire, 38.0)).clicked() {
                    fermer = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(apparence::bouton_widget(ui, "Supprimer définitivement", Genre::Danger, 38.0)).clicked() {
                        supprimer = true;
                    }
                });
            });
        });
        if reponse.should_close() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            fermer = true;
        }
        if supprimer {
            self.confirmer_suppression = None;
            self.supprimer_rencontre(&id);
        } else if fermer {
            self.confirmer_suppression = None;
        }
    }
}
