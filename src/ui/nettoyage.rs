//! Nettoyage de la transcription : file d'attente, fil de travail et barre de la fiche.
//!
//! Un seul nettoyage à la fois, jamais pendant un enregistrement, une transcription ou un
//! compte rendu : un enregistrement qui démarre l'interrompt, et il reprend ensuite au point
//! où il en était.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use crossbeam_channel::Receiver;
use eframe::egui::{self, Color32, CornerRadius, RichText, Ui};

use super::TourDeTableApp;
use crate::nettoyage::{self, Avancement, Etape};

pub(super) struct TacheNettoyage {
    id: String,
    rx: Receiver<Etape>,
    avancement: Arc<Avancement>,
    /// Compte rendu à rédiger ensuite (rencontre qui vient de finir, avec un ordre du jour).
    puis_compte_rendu: bool,
    /// Arrêt demandé par l'utilisateur ; sinon, interruption par un enregistrement, reprise ensuite.
    par_utilisateur: bool,
}

enum Action {
    Arreter,
    Lancer { depuis_zero: bool },
}

impl TourDeTableApp {
    /// Met la rencontre `id` dans la file du nettoyage ; `depuis_zero` efface d'abord le
    /// nettoyage existant (sinon, il reprend là où il s'est arrêté).
    pub(super) fn demander_nettoyage(&mut self, id: &str, puis_compte_rendu: bool, depuis_zero: bool) {
        if self.nettoyage_job.as_ref().is_some_and(|t| t.id == id) {
            return;
        }
        if depuis_zero && let Some(m) = self.meetings.iter_mut().find(|m| m.id == id) {
            m.nettoyage = None;
            if let Err(e) = m.save() {
                log::warn!("Sauvegarde : {e:#}");
            }
        }
        self.nettoyages_en_attente.retain(|(i, _)| i != id);
        self.nettoyages_en_attente.push((id.to_string(), puis_compte_rendu));
        self.demarrer_nettoyage();
    }

    /// Rencontres dont le nettoyage a été interrompu par une fermeture : reprises au démarrage.
    pub(super) fn reprendre_nettoyages(&mut self) {
        let a_reprendre: Vec<(String, bool)> = self
            .meetings
            .iter()
            .filter(|m| m.nettoyage.as_ref().is_some_and(|n| !n.termine() && !n.suspendu))
            .map(|m| (m.id.clone(), m.agenda.is_some() && m.summary.is_none()))
            .collect();
        for (id, cr) in a_reprendre {
            self.demander_nettoyage(&id, cr, false);
        }
    }

    fn libre_pour_nettoyer(&self) -> bool {
        self.recorder.is_none()
            && !self.finishing
            && self.import_job.is_none()
            && self.verification.is_none()
            && self.decoupage_job.is_none()
            && self.summary_job.is_none()
    }

    fn demarrer_nettoyage(&mut self) {
        if self.nettoyage_job.is_some() || !self.libre_pour_nettoyer() {
            return;
        }
        while !self.nettoyages_en_attente.is_empty() {
            let (id, puis_compte_rendu) = self.nettoyages_en_attente.remove(0);
            let Some(m) = self.meetings.iter().find(|m| m.id == id).filter(|m| !m.segments.is_empty()) else { continue };
            log::info!("Nettoyage de « {} »", m.title);
            let m = m.clone();
            let avancement = Arc::new(Avancement::default());
            let rx = nettoyage::lancer(m, self.cfg.glossary.clone(), avancement.clone());
            self.nettoyage_job = Some(TacheNettoyage { id, rx, avancement, puis_compte_rendu, par_utilisateur: false });
            return;
        }
    }

    /// Un enregistrement ou un import démarre : le nettoyage en cours s'arrête et reprendra.
    pub(super) fn suspendre_nettoyage(&mut self) {
        if let Some(t) = &self.nettoyage_job {
            t.avancement.annuler.store(true, Ordering::Relaxed);
        }
    }

    /// Bouton « Arrêter », ou rencontre supprimée.
    pub(super) fn arreter_nettoyage(&mut self, id: &str) {
        self.nettoyages_en_attente.retain(|(i, _)| i != id);
        if let Some(t) = self.nettoyage_job.as_mut().filter(|t| t.id == id) {
            t.par_utilisateur = true;
            t.avancement.annuler.store(true, Ordering::Relaxed);
        }
    }

    pub(super) fn nettoyage_en_cours(&self) -> bool {
        self.nettoyage_job.is_some()
    }

    pub(super) fn pump_nettoyage(&mut self) {
        let etapes: Vec<Etape> = self.nettoyage_job.iter().flat_map(|t| t.rx.try_iter()).collect();
        for etape in etapes {
            let Some(t) = &self.nettoyage_job else { break };
            let (id, puis_compte_rendu, par_utilisateur) = (t.id.clone(), t.puis_compte_rendu, t.par_utilisateur);
            let idx = self.meetings.iter().position(|m| m.id == id);
            match etape {
                Etape::Groupe(n) => {
                    if let Some(m) = idx.map(|i| &mut self.meetings[i]) {
                        m.nettoyage = Some(n);
                        if let Err(e) = m.save() {
                            log::warn!("Sauvegarde du nettoyage : {e:#}");
                        }
                    }
                }
                Etape::Fin(mut n) => {
                    self.nettoyage_job = None;
                    let termine = n.termine();
                    if !termine && !par_utilisateur {
                        // Interrompu par un enregistrement : reprendra en tête de file.
                        self.nettoyages_en_attente.insert(0, (id.clone(), puis_compte_rendu));
                    }
                    n.suspendu = !termine && par_utilisateur;
                    let Some(i) = idx else { continue };
                    let m = &mut self.meetings[i];
                    m.nettoyage = Some(n);
                    let enregistre = if termine { m.save().and_then(|_| m.export_all().map(|_| ())) } else { m.save() };
                    if let Err(e) = enregistre {
                        self.notify(format!("Sauvegarde du nettoyage : {e:#}"), true);
                    }
                    if termine && puis_compte_rendu {
                        self.auto_compte_rendu(i);
                    }
                }
            }
        }
        self.demarrer_nettoyage();
    }

    /// Au-dessus de la transcription : version affichée (« Brute / Nettoyée »), avancement,
    /// relance, et bandeau quand le nettoyage n'est pas fini.
    pub(super) fn barre_nettoyage(&mut self, ui: &mut Ui, idx: usize) {
        let m = &self.meetings[idx];
        let id = m.id.clone();
        let avancement = self
            .nettoyage_job
            .as_ref()
            .filter(|t| t.id == id)
            .map(|t| (t.avancement.groupe.load(Ordering::Relaxed), t.avancement.groupes.load(Ordering::Relaxed)));
        let en_attente = self.nettoyages_en_attente.iter().any(|(i, _)| *i == id);
        let etat = m.nettoyage.as_ref().map(|n| (n.termine(), n.groupes_faits, n.groupes));
        let par_point = !m.periodes.is_empty();
        let puis_compte_rendu = m.agenda.is_some() && m.summary.is_none();
        let mut action = None;

        ui.horizontal(|ui| {
            ui.label(RichText::new("Transcription").strong());
            if etat.is_some() {
                ui.selectable_value(&mut self.vue_brute, true, "Brute").on_hover_text("Telle que transcrite, jamais modifiée");
                ui.selectable_value(&mut self.vue_brute, false, "Nettoyée")
                    .on_hover_text("Phrases recollées, répétitions et hésitations retirées ; c'est cette version que lit le compte rendu");
            }
            ui.add_space(8.0);
            if let Some((groupe, groupes)) = avancement {
                ui.spinner();
                let texte = if par_point { format!("Nettoyage — point {groupe} sur {groupes}") } else { "Nettoyage de la transcription…".to_string() };
                ui.label(RichText::new(texte).weak());
                if ui.small_button("Arrêter").on_hover_text("Le nettoyage pourra reprendre là où il s'est arrêté").clicked() {
                    action = Some(Action::Arreter);
                }
            } else if en_attente {
                ui.label(RichText::new("Nettoyage en attente de la fin du travail en cours…").weak());
            } else {
                let (libelle, depuis_zero) = match etat {
                    None => ("Nettoyer la transcription", true),
                    Some((false, ..)) => ("Reprendre le nettoyage", false),
                    Some(_) => ("Relancer le nettoyage", true),
                };
                if ui
                    .add_enabled(self.recorder.is_none() && !self.finishing, egui::Button::new(libelle).small())
                    .on_hover_text("Recolle les phrases coupées, retire les répétitions, les hésitations et les simples interjections")
                    .clicked()
                {
                    action = Some(Action::Lancer { depuis_zero });
                }
            }
        });
        let bandeau = match etat {
            Some((false, faits, total)) if avancement.is_none() && !en_attente => {
                Some(format!("Nettoyage partiel ({faits} parties sur {total}) : le compte rendu lit la transcription brute."))
            }
            _ => None,
        };
        if let Some(texte) = bandeau {
            let orange = Color32::from_rgb(0xD9, 0x77, 0x06);
            egui::Frame::new()
                .fill(orange.gamma_multiply(0.18))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(texte).color(orange));
                });
        }

        match action {
            Some(Action::Arreter) => self.arreter_nettoyage(&id),
            Some(Action::Lancer { depuis_zero }) => self.demander_nettoyage(&id, !depuis_zero && puis_compte_rendu, depuis_zero),
            None => {}
        }
    }
}
