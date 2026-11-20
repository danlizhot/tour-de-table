//! Écoute dans la fiche d'une rencontre : un paragraphe de la transcription, ou un point du compte rendu.
//!
//! Une seule écoute à la fois ; elle s'arrête quand on change de rencontre, qu'on archive celle-ci ou qu'un
//! enregistrement démarre (le son de l'ordinateur serait capté dans la rencontre).

use std::time::Duration;

use eframe::egui::{self, RichText, Ui};

use super::TourDeTableApp;
use crate::audio::{SAMPLE_RATE, blocs, lecture};
use crate::storage::clock;

/// Ce qui est écouté.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Extrait {
    /// Paragraphe de la transcription, par son début (ms).
    Paragraphe(u64),
    /// Point de l'ordre du jour (indice).
    Point(usize),
}

pub(super) struct Ecoute {
    pub id: String,
    pub extrait: Extrait,
    libelle: String,
    lecture: lecture::Lecture,
}

fn echantillons(ms: u64) -> u64 {
    ms * SAMPLE_RATE as u64 / 1000
}

impl TourDeTableApp {
    /// Écoute de la rencontre `idx` : `plages` en ms, jouées l'une après l'autre.
    pub(super) fn ecouter(&mut self, idx: usize, extrait: Extrait, plages: &[(u64, u64)], libelle: String) {
        self.ecoute = None;
        if self.recorder.is_some() {
            self.notify("L'écoute est impossible pendant un enregistrement.", false);
            return;
        }
        let m = &self.meetings[idx];
        let dir = m.dir();
        let index = match blocs::lire_index(&dir) {
            Ok(i) => i,
            Err(e) => {
                self.notify(format!("Écoute impossible : {e:#}"), true);
                return;
            }
        };
        let plages = plages.iter().map(|&(d, f)| (echantillons(d), echantillons(f))).filter(|(d, f)| f > d).collect();
        let lecture = lecture::jouer_plages(dir, index, plages);
        self.ecoute = Some(Ecoute { id: m.id.clone(), extrait, libelle, lecture });
    }

    /// Extrait en cours d'écoute dans la rencontre `id`.
    pub(super) fn extrait_ecoute(&self, id: &str) -> Option<Extrait> {
        self.ecoute.as_ref().filter(|e| e.id == id).map(|e| e.extrait)
    }

    /// Appelé à chaque image depuis `pump`.
    pub(super) fn surveiller_ecoute(&mut self) {
        let Some(e) = &self.ecoute else { return };
        if self.recorder.is_some() {
            self.ecoute = None;
            return;
        }
        if let Ok(msg) = e.lecture.erreur.try_recv() {
            self.ecoute = None;
            self.notify(msg, true);
        } else if !e.lecture.en_cours() {
            self.ecoute = None;
        } else {
            self.ctx.request_repaint_after(Duration::from_millis(200));
        }
    }

    /// Barre de l'écoute en cours, au-dessus des boutons d'un volet de la rencontre `idx`.
    pub(super) fn barre_ecoute(&mut self, ui: &mut Ui, idx: usize) {
        let Some(e) = self.ecoute.as_ref().filter(|e| e.id == self.meetings[idx].id) else { return };
        let mut arreter = false;
        ui.horizontal(|ui| {
            let pause = e.lecture.en_pause();
            if ui.button(if pause { "Reprendre" } else { "Pause" }).clicked() {
                e.lecture.basculer_pause();
            }
            arreter = ui.button("Arrêter").clicked();
            let (fait, total) = e.lecture.avancement_ms();
            ui.label(RichText::new(&e.libelle).strong());
            let fraction = if total == 0 { 0.0 } else { fait as f32 / total as f32 };
            ui.add(egui::ProgressBar::new(fraction).desired_width(ui.available_width().min(260.0)).text(format!("{} / {}", clock(fait), clock(total))));
        });
        ui.add_space(4.0);
        if arreter {
            self.ecoute = None;
        }
    }
}
