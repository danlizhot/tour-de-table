//! Onglets de la fiche d'une rencontre : ordre du jour, transcription, compte rendu, un à la fois ou deux côte à côte.

use std::sync::atomic::Ordering;

use eframe::egui::{self, Color32, CornerRadius, RichText, Sense, Ui, Vec2};

use super::TourDeTableApp;
use super::widgets::{BLEU_CLAIR, markdown, transcript, transcript_nettoye};
use crate::config;
use crate::storage::clock;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Onglet {
    OrdreDuJour,
    Transcription,
    CompteRendu,
}

const TOUS: [Onglet; 3] = [Onglet::OrdreDuJour, Onglet::Transcription, Onglet::CompteRendu];

/// Largeur de la fiche sous laquelle deux volets seraient trop étroits pour être lus.
const LARGEUR_COTE_A_COTE: f32 = 760.0;
/// La même limite pour la fenêtre entière, historique compris.
const LARGEUR_FENETRE_COTE_A_COTE: f32 = 1040.0;

/// Ce que montre la fiche : un volet, ou deux côte à côte. Jamais deux fois le même onglet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Onglets {
    pub gauche: Onglet,
    pub droite: Option<Onglet>,
    /// Rencontre pour laquelle l'onglet de départ a été choisi.
    rencontre: Option<u64>,
}

impl Default for Onglets {
    fn default() -> Self {
        Self { gauche: Onglet::Transcription, droite: None, rencontre: None }
    }
}

impl Onglets {
    /// Clic sur un onglet d'un volet. Choisir ce que montre l'autre volet échange les deux.
    fn choisir(&mut self, a_droite: bool, onglet: Onglet) {
        match (a_droite, self.droite) {
            (false, Some(d)) if d == onglet => self.droite = Some(std::mem::replace(&mut self.gauche, onglet)),
            (false, _) => self.gauche = onglet,
            (true, Some(d)) if self.gauche == onglet => self.gauche = self.droite.replace(onglet).unwrap_or(d),
            (true, _) => self.droite = Some(onglet),
        }
    }

    /// Ouvre ou ferme le second volet. À l'ouverture, l'ordre du jour passe à gauche quand il n'est pas déjà affiché :
    /// c'est lui qu'on suit en lisant le reste.
    fn basculer(&mut self) {
        self.droite = match self.droite {
            Some(_) => None,
            None if self.gauche == Onglet::OrdreDuJour => Some(Onglet::Transcription),
            None => Some(std::mem::replace(&mut self.gauche, Onglet::OrdreDuJour)),
        };
    }

    /// Enregistrement ou import en cours : on reste sur l'ordre du jour tant que l'utilisateur ne choisit pas autre
    /// chose, et chaque nouvelle rencontre y revient.
    pub(super) fn direct() -> Self {
        Self { gauche: Onglet::OrdreDuJour, droite: None, rencontre: None }
    }

    fn ouvrir_direct(&mut self, rencontre: u64) {
        if self.rencontre != Some(rencontre) {
            *self = Self { rencontre: Some(rencontre), droite: self.droite, ..Self::direct() };
        }
    }

    /// Onglet de départ d'une rencontre qu'on vient d'ouvrir : le compte rendu s'il existe, sinon la transcription.
    fn ouvrir(&mut self, rencontre: u64, a_un_compte_rendu: bool) {
        if self.rencontre == Some(rencontre) {
            return;
        }
        self.rencontre = Some(rencontre);
        let depart = if a_un_compte_rendu { Onglet::CompteRendu } else { Onglet::Transcription };
        match self.droite {
            None => self.gauche = depart,
            Some(_) if self.gauche != depart => self.droite = Some(depart),
            Some(_) => {}
        }
    }
}

/// Empreinte de l'identifiant d'une rencontre.
fn empreinte(id: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut h);
    h.finish()
}

/// Bouton d'onglet : grand, souligné quand il est choisi, avec un indicateur quand un travail y est en cours.
fn bouton_onglet(ui: &mut Ui, texte: &str, choisi: bool, occupe: bool) -> egui::Response {
    let couleur = if choisi { ui.visuals().strong_text_color() } else { ui.visuals().text_color().gamma_multiply(0.75) };
    let galley = egui::WidgetText::from(RichText::new(texte).size(16.0).strong()).into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        egui::TextStyle::Button,
    );
    let indicateur = if occupe { 20.0 } else { 0.0 };
    let taille = Vec2::new(galley.size().x + 28.0 + indicateur, galley.size().y + 16.0);
    let (rect, resp) = ui.allocate_exact_size(taille, Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    if ui.is_rect_visible(rect) {
        let haut = CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 };
        let fond = match (choisi, resp.hovered()) {
            (true, _) => BLEU_CLAIR.gamma_multiply(0.22),
            (false, true) => ui.visuals().widgets.hovered.weak_bg_fill,
            (false, false) => Color32::TRANSPARENT,
        };
        ui.painter().rect_filled(rect, haut, fond);
        if choisi {
            let trait_bas = egui::Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - 3.0), rect.right_bottom());
            ui.painter().rect_filled(trait_bas, 0, BLEU_CLAIR);
        }
        let pos = egui::pos2(rect.left() + 14.0, rect.center().y - galley.size().y / 2.0);
        let fin_texte = pos.x + galley.size().x;
        ui.painter().galley(pos, galley, couleur);
        if occupe {
            let r = egui::Rect::from_center_size(egui::pos2(fin_texte + 13.0, rect.center().y), Vec2::splat(13.0));
            egui::Spinner::new().paint_at(ui, r);
        }
    }
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, true, choisi, texte));
    resp
}

/// Bouton « Côte à côte », précédé d'un pictogramme de deux volets.
fn bouton_cote_a_cote(ui: &mut Ui, actif: bool) -> egui::Response {
    let galley =
        egui::WidgetText::from(RichText::new("Côte à côte")).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Button);
    let taille = Vec2::new(galley.size().x + 46.0, galley.size().y + 12.0);
    let (rect, resp) = ui.allocate_exact_size(taille, Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    if ui.is_rect_visible(rect) {
        let visuel = ui.style().interact_selectable(&resp, actif);
        ui.painter().rect(rect, CornerRadius::same(6), visuel.weak_bg_fill, visuel.bg_stroke, egui::StrokeKind::Inside);
        let couleur = visuel.text_color();
        // Deux petits volets.
        let icone = egui::Rect::from_min_size(egui::pos2(rect.left() + 10.0, rect.center().y - 6.5), Vec2::new(18.0, 13.0));
        let moitie = (icone.width() - 2.0) / 2.0;
        for x in [icone.left(), icone.left() + moitie + 2.0] {
            let volet = egui::Rect::from_min_size(egui::pos2(x, icone.top()), Vec2::new(moitie, icone.height()));
            ui.painter().rect_stroke(volet, CornerRadius::same(2), egui::Stroke::new(1.5, couleur), egui::StrokeKind::Inside);
        }
        ui.painter().galley(egui::pos2(icone.right() + 8.0, rect.center().y - galley.size().y / 2.0), galley, couleur);
    }
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, actif, "Côte à côte"));
    resp
}

/// Trait sous la barre, collé aux onglets.
fn trait_sous_la_barre(ui: &mut Ui) {
    let (trait_bas, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(trait_bas.translate(Vec2::new(0.0, -ui.spacing().item_spacing.y)), 0, ui.visuals().widgets.noninteractive.bg_stroke.color);
    ui.add_space(2.0);
}

impl TourDeTableApp {
    /// Bouton « Côte à côte » de la fiche, toujours au même endroit ; absent quand la fenêtre est trop étroite.
    pub(super) fn bascule_cote_a_cote(&mut self, ui: &mut Ui) {
        if ui.ctx().content_rect().width() < LARGEUR_FENETRE_COTE_A_COTE {
            return;
        }
        let actif = self.onglets.droite.is_some();
        let aide = if actif {
            "Revenir à un seul volet"
        } else {
            "Affiche deux volets en même temps, par exemple l'ordre du jour à côté de la transcription"
        };
        if bouton_cote_a_cote(ui, actif).on_hover_text(aide).clicked() {
            self.onglets.basculer();
        }
    }

    /// Rencontre en cours avec un ordre du jour : l'ordre du jour et la transcription en deux onglets, ou côte à
    /// côte. `transcription` dessine la transcription en cours.
    pub(super) fn volets_direct(&mut self, ui: &mut Ui, transcription: impl Fn(&mut Self, &mut Ui)) {
        let mut onglets = self.onglets_direct;
        onglets.ouvrir_direct(empreinte(self.live.as_ref().map_or("", |m| m.id.as_str())));
        let assez_large = ui.available_width() >= LARGEUR_COTE_A_COTE;
        let cote_a_cote = onglets.droite.is_some() && assez_large;
        let aide = if cote_a_cote { "Revenir à un seul volet" } else { "Affiche l'ordre du jour à côté de la transcription" };
        let ordre_du_jour = |app: &mut Self, ui: &mut Ui| {
            egui::ScrollArea::vertical().id_salt("direct_oj").auto_shrink(false).show(ui, |ui| app.agenda_card(ui, true));
        };
        if cote_a_cote {
            ui.columns(2, |colonnes| {
                colonnes[0].push_id("gauche", |ui| {
                    ui.horizontal(|ui| bouton_onglet(ui, "Ordre du jour", true, false));
                    trait_sous_la_barre(ui);
                    ordre_du_jour(self, ui);
                });
                colonnes[1].push_id("droite", |ui| {
                    ui.horizontal(|ui| {
                        bouton_onglet(ui, "Transcription", true, false);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if bouton_cote_a_cote(ui, true).on_hover_text(aide).clicked() {
                                onglets.droite = None;
                            }
                        });
                    });
                    trait_sous_la_barre(ui);
                    transcription(self, ui);
                });
            });
        } else {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (onglet, texte) in [(Onglet::OrdreDuJour, "Ordre du jour"), (Onglet::Transcription, "Transcription")] {
                    if bouton_onglet(ui, texte, onglets.gauche == onglet, false).clicked() {
                        onglets.gauche = onglet;
                    }
                }
                if assez_large {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if bouton_cote_a_cote(ui, false).on_hover_text(aide).clicked() {
                            onglets.droite = Some(Onglet::Transcription);
                        }
                    });
                }
            });
            trait_sous_la_barre(ui);
            match onglets.gauche {
                Onglet::OrdreDuJour => ordre_du_jour(self, ui),
                _ => transcription(self, ui),
            }
        }
        self.onglets_direct = onglets;
    }

    /// Corps de la fiche : la barre d'onglets et le volet choisi, ou deux volets côte à côte. Rend l'action demandée.
    pub(super) fn volets(&mut self, ui: &mut Ui, idx: usize) -> Option<&'static str> {
        let m = &self.meetings[idx];
        let mut onglets = self.onglets;
        onglets.ouvrir(empreinte(&m.id), m.summary.is_some());
        // Fenêtre trop étroite : un seul volet, sans perdre le choix pour quand elle s'élargira.
        let assez_large = ui.available_width() >= LARGEUR_COTE_A_COTE;
        let droite = onglets.droite.filter(|_| assez_large);
        let mut action = None;
        match droite {
            None => {
                let gauche = onglets.gauche;
                self.barre_onglets(ui, idx, &mut onglets, false);
                action = self.volet(ui, idx, gauche);
            }
            Some(d) => {
                let gauche = onglets.gauche;
                ui.columns(2, |colonnes| {
                    colonnes[0].push_id("gauche", |ui| {
                        self.barre_onglets(ui, idx, &mut onglets, false);
                        action = self.volet(ui, idx, gauche).or(action);
                    });
                    colonnes[1].push_id("droite", |ui| {
                        self.barre_onglets(ui, idx, &mut onglets, true);
                        action = self.volet(ui, idx, d).or(action);
                    });
                });
            }
        }
        self.onglets = onglets;
        action
    }

    /// Les trois onglets d'un volet.
    fn barre_onglets(&mut self, ui: &mut Ui, idx: usize, onglets: &mut Onglets, a_droite: bool) {
        let m = &self.meetings[idx];
        let choisi = if a_droite { onglets.droite.unwrap_or(onglets.gauche) } else { onglets.gauche };
        let redige = self.summary_job.as_ref().is_some_and(|(id, ..)| *id == m.id);
        let nettoie = self.nettoyage_job.as_ref().is_some_and(|t| t.id == m.id);
        let compte_rendu = if m.agenda.is_some() { "Compte rendu" } else { "Résumé" };
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for onglet in TOUS {
                let (texte, occupe) = match onglet {
                    Onglet::OrdreDuJour => ("Ordre du jour", false),
                    Onglet::Transcription => ("Transcription", nettoie),
                    Onglet::CompteRendu => (compte_rendu, redige),
                };
                if bouton_onglet(ui, texte, onglet == choisi, occupe).clicked() {
                    onglets.choisir(a_droite, onglet);
                }
            }
        });
        trait_sous_la_barre(ui);
    }

    fn volet(&mut self, ui: &mut Ui, idx: usize, onglet: Onglet) -> Option<&'static str> {
        match onglet {
            Onglet::OrdreDuJour => {
                egui::ScrollArea::vertical().id_salt("volet_oj").auto_shrink(false).show(ui, |ui| self.agenda_card(ui, true));
                None
            }
            Onglet::Transcription => {
                self.barre_nettoyage(ui, idx);
                let m = &self.meetings[idx];
                match m.nettoyage.as_ref().filter(|_| !self.vue_brute) {
                    Some(n) => transcript_nettoye(ui, m, n),
                    None => transcript(ui, m, false),
                }
                None
            }
            Onglet::CompteRendu => self.volet_compte_rendu(ui, idx),
        }
    }

    fn volet_compte_rendu(&mut self, ui: &mut Ui, idx: usize) -> Option<&'static str> {
        let mut action = None;
        let busy = self.summary_job.as_ref().filter(|(id, ..)| *id == self.meetings[idx].id).map(|(_, _, a, t0)| {
            let (point, points) = (a.point.load(Ordering::Relaxed), a.points.load(Ordering::Relaxed));
            let etape = match (point, points) {
                (_, 0) => String::new(),
                (p, n) if p > n => "Assemblage · ".to_string(),
                (p, n) => format!("Point {p} sur {n} · "),
            };
            (etape, a.jetons.load(Ordering::Relaxed), t0.elapsed().as_secs())
        });
        let redaction_prete = self.redaction_prete();
        let m = &self.meetings[idx];
        let has_agenda = m.agenda.is_some();
        let occupe = busy.is_some();
        ui.horizontal_wrapped(|ui| {
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
                let label = match (m.summary.is_some(), has_agenda) {
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
                    .add_enabled(!self.nettoyage_en_cours(), egui::Button::new(label))
                    .on_hover_text(format!("{hover}\nModèle local : {}", config::moteur().nom()))
                    .on_disabled_hover_text("Attendez la fin du nettoyage de la transcription")
                    .clicked()
                {
                    action = Some("summary");
                }
                if m.summary.is_some() && ui.button("Visualiser").on_hover_text("Ouvre le compte rendu dans Word (compte-rendu.docx)").clicked() {
                    action = Some("visualiser");
                }
                if let Some(sum) = &m.summary
                    && ui.button("Copier").clicked()
                {
                    ui.ctx().copy_text(sum.clone());
                }
            }
        });
        ui.add_space(4.0);
        match &m.summary {
            Some(s) => {
                egui::ScrollArea::vertical().id_salt("cr").auto_shrink(false).show(ui, |ui| {
                    markdown(ui, s);
                    ui.add_space(12.0);
                });
            }
            None => {
                let texte = match (occupe, has_agenda) {
                    (true, true) => "Le compte rendu apparaîtra ici dès qu'il sera rédigé.",
                    (true, false) => "Le résumé apparaîtra ici dès qu'il sera rédigé.",
                    (false, true) => "Aucun compte rendu pour l'instant.",
                    (false, false) => "Aucun résumé pour l'instant. Avec un ordre du jour, vous obtenez un compte rendu point par point.",
                };
                ui.add_space(24.0);
                ui.vertical_centered(|ui| ui.label(RichText::new(texte).weak()));
            }
        }
        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn un_seul_volet() {
        let mut o = Onglets::default();
        o.choisir(false, Onglet::CompteRendu);
        assert_eq!((o.gauche, o.droite), (Onglet::CompteRendu, None));
    }

    #[test]
    fn cote_a_cote_met_l_ordre_du_jour_a_gauche() {
        let mut o = Onglets::default();
        o.basculer();
        assert_eq!((o.gauche, o.droite), (Onglet::OrdreDuJour, Some(Onglet::Transcription)));
        o.basculer();
        assert_eq!((o.gauche, o.droite), (Onglet::OrdreDuJour, None));
        o.basculer();
        assert_eq!((o.gauche, o.droite), (Onglet::OrdreDuJour, Some(Onglet::Transcription)));
    }

    #[test]
    fn jamais_deux_fois_le_meme_onglet() {
        for a_droite in [false, true] {
            for onglet in TOUS {
                let mut o = Onglets { gauche: Onglet::OrdreDuJour, droite: Some(Onglet::Transcription), rencontre: None };
                o.choisir(a_droite, onglet);
                assert_ne!(Some(o.gauche), o.droite, "{a_droite} {onglet:?}");
                let choisi = if a_droite { o.droite } else { Some(o.gauche) };
                assert_eq!(choisi, Some(onglet));
            }
        }
    }

    #[test]
    fn choisir_l_onglet_de_l_autre_volet_echange_les_deux() {
        let mut o = Onglets { gauche: Onglet::OrdreDuJour, droite: Some(Onglet::Transcription), rencontre: None };
        o.choisir(false, Onglet::Transcription);
        assert_eq!((o.gauche, o.droite), (Onglet::Transcription, Some(Onglet::OrdreDuJour)));
        o.choisir(true, Onglet::Transcription);
        assert_eq!((o.gauche, o.droite), (Onglet::OrdreDuJour, Some(Onglet::Transcription)));
    }

    #[test]
    fn en_direct_on_reste_sur_l_ordre_du_jour() {
        let mut o = Onglets::direct();
        o.ouvrir_direct(1);
        assert_eq!(o.gauche, Onglet::OrdreDuJour);
        // Le choix de l'utilisateur tient pour toute la rencontre.
        o.gauche = Onglet::Transcription;
        o.ouvrir_direct(1);
        assert_eq!(o.gauche, Onglet::Transcription);
        // Nouvelle rencontre : retour à l'ordre du jour, côte à côte gardé.
        o.droite = Some(Onglet::Transcription);
        o.ouvrir_direct(2);
        assert_eq!((o.gauche, o.droite), (Onglet::OrdreDuJour, Some(Onglet::Transcription)));
    }

    #[test]
    fn onglet_de_depart_d_une_rencontre() {
        let mut o = Onglets::default();
        o.ouvrir(1, true);
        assert_eq!(o.gauche, Onglet::CompteRendu);
        // Même rencontre : le choix de l'utilisateur est gardé.
        o.choisir(false, Onglet::OrdreDuJour);
        o.ouvrir(1, true);
        assert_eq!(o.gauche, Onglet::OrdreDuJour);
        o.ouvrir(2, false);
        assert_eq!(o.gauche, Onglet::Transcription);
        // Côte à côte : le volet de gauche est gardé, celui de droite montre l'onglet de départ.
        let mut o = Onglets { gauche: Onglet::OrdreDuJour, droite: Some(Onglet::Transcription), rencontre: None };
        o.ouvrir(3, true);
        assert_eq!((o.gauche, o.droite), (Onglet::OrdreDuJour, Some(Onglet::CompteRendu)));
    }
}
