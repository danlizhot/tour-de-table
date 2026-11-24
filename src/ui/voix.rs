//! Reconnaissance des voix : file d'attente, processus de calcul et barre de la transcription.
//!
//! Une rencontre qui vient de finir (enregistrée ou importée) est mise en file. Le calcul tourne
//! quand l'application n'a rien d'autre à faire : un enregistrement, un import, un nettoyage ou un
//! compte rendu qui démarre l'arrête, et il reprend ensuite depuis le début.

use std::time::{Duration, Instant};

use eframe::egui::{self, CornerRadius, RichText, Sense, Ui, Vec2};

use super::TourDeTableApp;
use super::apparence::{self, gras, initiales, palette, pastille};
use super::widgets::couleur_locuteur;
use crate::voix::{self, Calcul};

/// Attente après la fin d'une tâche : le nettoyage puis le compte rendu s'enchaînent sans que le
/// calcul démarre entre les deux.
const DELAI: Duration = Duration::from_secs(5);

impl TourDeTableApp {
    pub(super) fn demander_voix(&mut self, id: &str) {
        if !self.voix_en_attente.iter().any(|i| i == id) && self.voix_job.as_ref().is_none_or(|c| c.id != id) {
            self.voix_en_attente.push(id.to_string());
        }
    }

    /// Appelé à chaque image depuis `pump`.
    pub(super) fn surveiller_voix(&mut self) {
        if self.occupe() || self.etalonnage.is_some() {
            if let Some(mut c) = self.voix_job.take() {
                log::info!("Reconnaissance des voix interrompue : elle reprendra plus tard");
                c.interrompre();
                self.voix_en_attente.insert(0, c.id.clone());
            }
            return;
        }
        if let Some(c) = &mut self.voix_job {
            let Ok(resultat) = c.fin.try_recv() else {
                self.ctx.request_repaint_after(Duration::from_secs(2));
                return;
            };
            c.conclure();
            let id = c.id.clone();
            self.voix_job = None;
            self.inoccupe_depuis = Instant::now();
            let Some(m) = self.meetings.iter_mut().find(|m| m.id == id) else { return };
            match resultat {
                Ok(mut v) => {
                    // Une relance garde les noms déjà donnés.
                    if let Some(ancien) = &m.voix {
                        v.noms = ancien.noms.clone();
                    }
                    log::info!("« {} » : {} voix reconnues", m.title, v.nombre());
                    m.voix = Some(v);
                    if let Err(e) = m.save().and_then(|_| m.export_all().map(|_| ())) {
                        self.notify(format!("Sauvegarde des voix : {e:#}"), true);
                    }
                }
                Err(e) => {
                    log::error!("Reconnaissance des voix de « {} » : {e}", m.title);
                    self.notify(format!("Reconnaissance des voix impossible : {e}"), true);
                }
            }
            return;
        }
        if self.voix_en_attente.is_empty() || !voix::modeles_installes() {
            return;
        }
        if let Some(reste) = DELAI.checked_sub(self.inoccupe_depuis.elapsed()) {
            self.ctx.request_repaint_after(reste);
            return;
        }
        let id = self.voix_en_attente.remove(0);
        let Some(m) = self.meetings.iter().find(|m| m.id == id) else { return };
        match Calcul::lancer(id, &m.dir()) {
            Ok(c) => {
                log::info!("Reconnaissance des voix de « {} »", m.title);
                self.voix_job = Some(c);
            }
            Err(e) => self.notify(format!("{e:#}"), true),
        }
        self.ctx.request_repaint_after(Duration::from_secs(2));
    }

    /// Noms des voix, au-dessus des boutons quand « Nommer les voix » est ouvert.
    pub(super) fn noms_ouverts(&mut self, ui: &mut Ui, idx: usize) {
        if self.noms_des_voix.as_deref() == Some(self.meetings[idx].id.as_str()) {
            self.noms_voix(ui, idx);
            ui.add_space(4.0);
        }
    }

    /// Boutons des voix, dans la ligne de boutons de la transcription : avancement, relance, noms.
    pub(super) fn barre_voix(&mut self, ui: &mut Ui, idx: usize, avec_carte: bool) {
        let m = &self.meetings[idx];
        let id = m.id.clone();
        let en_cours = self.voix_job.as_ref().is_some_and(|c| c.id == id);
        let en_attente = self.voix_en_attente.contains(&id);
        let audio = crate::audio::blocs::existe(&m.dir());
        if !audio && m.voix.is_none() {
            return;
        }
        let nombre = m.voix.as_ref().map(voix::Voix::nombre);
        let minutes = (m.duration_s / 300).max(1);
        let mut relancer = false;
        let ouvert = self.noms_des_voix.as_deref() == Some(id.as_str());
        let mut basculer = false;
        {

            if en_cours {
                ui.spinner();
                ui.label(RichText::new(format!("Reconnaissance des voix… (environ {minutes} min)")).weak());
            } else if en_attente {
                ui.label(RichText::new("Reconnaissance des voix en attente de la fin du travail en cours…").weak());
            } else if avec_carte && nombre.is_some() {
                // Les noms et la relance sont dans la carte « Voix reconnues ».
            } else if let Some(n) = nombre {
                ui.label(RichText::new(format!("{n} voix reconnues")).weak());
                if ui.button(if ouvert { "Fermer les noms" } else { "Nommer les voix" }).clicked() {
                    basculer = true;
                }
                if audio
                    && ui
                        .add_enabled(self.recorder.is_none() && !self.finishing, egui::Button::new("Relancer les voix"))
                        .on_hover_text("Reconnaît les voix de nouveau ; les noms donnés sont gardés")
                        .clicked()
                {
                    relancer = true;
                }
            } else if ui
                .add_enabled(self.recorder.is_none() && !self.finishing && voix::modeles_installes(), egui::Button::new("Reconnaître les voix"))
                .on_hover_text(format!("Distingue les personnes qui parlent ; environ {minutes} min, quand l'application est libre"))
                .clicked()
            {
                relancer = true;
            }
        }
        if basculer {
            self.noms_des_voix = if ouvert { None } else { Some(id.clone()) };
        }
        if relancer {
            self.demander_voix(&id);
        }
    }

    /// Carte « Voix reconnues », à droite de la transcription : un nom par voix, son temps de parole en barre, et la
    /// relance. Les noms sont enregistrés quand on quitte le champ.
    pub(super) fn carte_voix(&mut self, ui: &mut Ui, idx: usize) {
        let p = palette(ui);
        let peut_relancer = crate::audio::blocs::existe(&self.meetings[idx].dir()) && self.recorder.is_none() && !self.finishing;
        let en_cours = self.voix_job.as_ref().is_some_and(|c| c.id == self.meetings[idx].id) || self.voix_en_attente.contains(&self.meetings[idx].id);
        let mut relancer = false;
        let mut modifie = false;
        apparence::carte(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(gras("Voix reconnues", 15.0).color(p.texte));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new("temps de parole").size(12.0).color(p.texte2));
                });
            });
            ui.add_space(6.0);
            let m = &mut self.meetings[idx];
            let Some(v) = &mut m.voix else { return };
            let parole = v.parole();
            v.noms.resize(parole.len().max(v.noms.len()), String::new());
            let max = parole.iter().copied().max().unwrap_or(1).max(1);
            for (i, ms) in parole.iter().enumerate() {
                let couleur = couleur_locuteur(crate::audio::Source::Mixte, Some(i as u16));
                let lettres = if v.noms[i].trim().is_empty() { format!("V{}", i + 1) } else { initiales(&v.noms[i]) };
                ui.horizontal(|ui| {
                    pastille(ui, &lettres, couleur, 30.0);
                    let minutes = format!("{} min", (ms + 30_000) / 60_000);
                    let largeur = ui.available_width() - 56.0;
                    let champ = ui.add_sized(
                        [largeur, 32.0],
                        egui::TextEdit::singleline(&mut v.noms[i]).hint_text(format!("Voix {} : nom ou rôle", i + 1)).align(egui::Align2::LEFT_CENTER),
                    );
                    modifie |= champ.lost_focus();
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(minutes).size(12.0).color(p.texte2));
                    });
                });
                let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 6.0), Sense::hover());
                let piste = egui::Rect::from_min_max(egui::pos2(rect.left() + 38.0, rect.top()), rect.max);
                ui.painter().rect_filled(piste, CornerRadius::same(3), p.piste);
                let mut plein = piste;
                plein.set_width(piste.width() * (*ms as f32 / max as f32));
                ui.painter().rect_filled(plein, CornerRadius::same(3), couleur);
                ui.add_space(6.0);
            }
            ui.label(RichText::new("Le nom remplace « Voix 1 », « Voix 2 »… dans la transcription et les exports.").size(12.0).color(p.texte2));
            ui.add_space(4.0);
            if en_cours {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Reconnaissance en cours…").size(13.0).color(p.texte2));
                });
            } else if ui
                .add_enabled(peut_relancer, apparence::bouton_widget(ui, "Relancer la reconnaissance", apparence::Genre::Secondaire, 34.0).min_size(Vec2::new(ui.available_width(), 34.0)))
                .on_hover_text("Reconnaît les voix de nouveau ; les noms donnés sont gardés")
                .clicked()
            {
                relancer = true;
            }
        });
        let m = &mut self.meetings[idx];
        if modifie && let Err(e) = m.save().and_then(|_| m.export_all().map(|_| ())) {
            self.notify(format!("Sauvegarde des noms : {e:#}"), true);
        }
        if relancer {
            let id = self.meetings[idx].id.clone();
            self.demander_voix(&id);
        }
    }

    /// Un nom par voix, avec son temps de parole ; enregistré quand on quitte le champ.
    fn noms_voix(&mut self, ui: &mut Ui, idx: usize) {
        let m = &mut self.meetings[idx];
        let Some(v) = &mut m.voix else { return };
        let parole = v.parole();
        v.noms.resize(parole.len().max(v.noms.len()), String::new());
        let mut modifie = false;
        egui::Grid::new("noms-des-voix").num_columns(3).spacing([12.0, 6.0]).show(ui, |ui| {
            for (i, ms) in parole.iter().enumerate() {
                ui.label(RichText::new(format!("Voix {}", i + 1)).strong().color(couleur_locuteur(crate::audio::Source::Mixte, Some(i as u16))));
                // Taille imposée : dans une grille, le champ garderait la largeur de la colonne (quelques lettres).
                let hauteur = ui.spacing().interact_size.y;
                let champ = ui.add_sized([240.0, hauteur], egui::TextEdit::singleline(&mut v.noms[i]).hint_text("Nom ou rôle"));
                modifie |= champ.lost_focus();
                ui.label(RichText::new(format!("{} min de parole", (ms + 30_000) / 60_000)).weak());
                ui.end_row();
            }
        });
        ui.label(RichText::new("Le nom remplace « Voix 1 », « Voix 2 »… dans la transcription et les exports.").weak().small());
        if modifie && let Err(e) = m.save().and_then(|_| m.export_all().map(|_| ())) {
            self.notify(format!("Sauvegarde des noms : {e:#}"), true);
        }
    }
}
