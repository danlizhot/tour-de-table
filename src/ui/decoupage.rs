//! Découpage automatique par point pendant la rencontre : détection dans chaque segment final,
//! changement automatique (avec bandeau « Annuler ») ou mise en évidence du bouton du point.

use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText, Ui};

use super::TourDeTableApp;
use super::widgets::BLEU;
use crate::decoupage::{self, Contexte, Detection};
use crate::storage::{Declenchement, Periode, clock};
use crate::stt::Segment;

/// Durée du bandeau « Passé au point… [Annuler] ».
pub const DUREE_BANDEAU: Duration = Duration::from_secs(10);
/// Durée de la mise en évidence d'un bouton « Débuter le point ».
pub const DUREE_PROPOSITION: Duration = Duration::from_secs(60);
/// Écart minimal entre le début du point actif et la frontière d'un changement automatique.
const ECART_MINIMAL_MS: u64 = 1000;

/// Changement automatique qu'on peut encore annuler.
pub struct Bandeau {
    texte: String,
    expire: Instant,
    /// Périodes de la rencontre avant le changement.
    periodes: Vec<Periode>,
}

impl TourDeTableApp {
    /// Cherche une annonce de changement de point dans les segments qui viennent d'arriver.
    pub(super) fn detecter(&mut self, segments: &[Segment]) {
        if self.recorder.is_none() {
            return;
        }
        for s in segments {
            let Some(m) = self.live.as_ref() else { return };
            let Some(points) = m.agenda.as_ref().map(|a| &a.points[..]).filter(|p| !p.is_empty()) else { return };
            let ctx = Contexte {
                points,
                actif: m.point_actif(),
                dernier: m.periodes.iter().rev().find_map(|p| p.point),
                debut_actif_ms: m.periodes.last().filter(|p| p.fin_ms.is_none()).map(|p| p.debut_ms),
            };
            let Some(d) = self.detecteur.analyser(s.start_ms, s.end_ms, &s.text, &ctx) else { continue };
            let (dir, intertitre) = (m.dir(), m.intertitre(Some(d.point)));
            let decision = self.decider(&d);
            decoupage::journaliser(
                &dir,
                &format!("{} | score {} | {intertitre} | « {} » | {} | {decision}", clock(d.frontiere_ms), d.score, s.text, d.raison),
            );
        }
    }

    /// Donne suite à une détection ; retourne la décision, pour le journal.
    fn decider(&mut self, d: &Detection) -> String {
        if d.score < decoupage::SEUIL {
            return format!("ignorée (seuil {})", decoupage::SEUIL);
        }
        if !self.cfg.passage_auto {
            self.proposition = Some((d.point, Instant::now()));
            return "bouton mis en évidence".into();
        }
        let (Some(rec), Some(m)) = (&self.recorder, self.live.as_mut()) else { return "ignorée (pas d'enregistrement)".into() };
        // La frontière doit tomber après le début du point actif (ou la fin du dernier point) et
        // avant maintenant ; sinon, on se contente de proposer.
        let plancher = m.periodes.last().map_or(0, |p| p.fin_ms.unwrap_or(p.debut_ms));
        if d.frontiere_ms < plancher + ECART_MINIMAL_MS || d.frontiere_ms > rec.position_ms() {
            self.proposition = Some((d.point, Instant::now()));
            return "frontière hors du point actif : bouton mis en évidence".into();
        }
        let periodes = m.periodes.clone();
        rec.changer_point_a_posteriori(Some(d.point), d.frontiere_ms);
        m.debuter_point(Some(d.point), d.frontiere_ms, Declenchement::Auto);
        if let Err(e) = m.save() {
            log::warn!("Sauvegarde après changement de point automatique : {e}");
        }
        let texte = match m.agenda.as_ref().and_then(|a| a.points.get(d.point)) {
            Some(p) => format!("Passé au point {} — {}", p.numero, p.titre),
            None => format!("Passé au point {}", d.point + 1),
        };
        self.proposition = None;
        self.bandeau = Some(Bandeau { texte, expire: Instant::now() + DUREE_BANDEAU, periodes });
        format!("changement automatique à {}", clock(d.frontiere_ms))
    }

    /// Un clic manuel l'emporte : propositions et bandeau disparaissent.
    pub(super) fn abandonner_propositions(&mut self) {
        self.proposition = None;
        if self.bandeau.take().is_some()
            && let Some(rec) = &self.recorder
        {
            rec.confirmer_changement();
        }
    }

    /// Point dont le bouton « Débuter le point » est mis en évidence.
    pub(super) fn point_propose(&self) -> Option<usize> {
        let actif = self.live.as_ref().and_then(|m| m.point_actif());
        self.proposition.filter(|&(p, t)| t.elapsed() < DUREE_PROPOSITION && Some(p) != actif).map(|(p, _)| p)
    }

    /// Bandeau du dernier changement automatique, avec « Annuler ».
    pub(super) fn bandeau(&mut self, ui: &mut Ui) {
        let Some(b) = &self.bandeau else { return };
        if Instant::now() >= b.expire {
            self.bandeau = None;
            if let Some(rec) = &self.recorder {
                rec.confirmer_changement();
            }
            return;
        }
        let mut annuler = false;
        egui::Frame::new().fill(BLEU).corner_radius(6).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(&b.texte).color(Color32::WHITE).strong());
                annuler = ui.button("Annuler").clicked();
            });
        });
        ui.add_space(6.0);
        if annuler {
            self.annuler_passage();
        }
    }

    /// Remet exactement l'état d'avant le changement automatique : périodes, blocs audio, index.
    fn annuler_passage(&mut self) {
        let Some(b) = self.bandeau.take() else { return };
        let (Some(rec), Some(m)) = (&self.recorder, self.live.as_mut()) else { return };
        if !rec.annuler_changement() {
            self.notify("Le changement de point n'a pas pu être annulé.", true);
            return;
        }
        m.periodes = b.periodes;
        if let Err(e) = m.save() {
            log::warn!("Sauvegarde après annulation du changement de point : {e}");
        }
        decoupage::journaliser(&m.dir(), &format!("annulation par l'utilisateur : {}", b.texte));
    }
}
