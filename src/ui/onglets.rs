//! Onglets de la fiche d'une rencontre : ordre du jour, transcription, compte rendu, un à la fois ou deux côte à côte.

use std::sync::atomic::Ordering;

use eframe::egui::{self, Color32, CornerRadius, RichText, Sense, Ui, Vec2};

use super::TourDeTableApp;
use super::apparence::{gras, palette};
use super::widgets::{ClicEcoute, EtatEcoute, markdown, transcript, transcript_nettoye};
use crate::{config, models};
use crate::storage::clock;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Onglet {
    OrdreDuJour,
    Transcription,
    CompteRendu,
}

/// Dans un volet, le texte reste aligné à gauche : les colonnes d'egui le justifieraient, mots écartés.
fn aligne_a_gauche() -> egui::Layout {
    egui::Layout::top_down(egui::Align::Min)
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

    /// La transcription devient visible à côté du compte rendu, pour comparer : dans le volet qui ne le montre pas,
    /// en passant côte à côte s'il n'y a qu'un volet. Fenêtre trop étroite pour deux volets : à sa place.
    fn afficher_transcription(&mut self, assez_large: bool) {
        if self.gauche == Onglet::Transcription || self.droite == Some(Onglet::Transcription) && assez_large {
            return;
        }
        if !assez_large {
            self.gauche = Onglet::Transcription;
        } else if self.gauche == Onglet::CompteRendu {
            self.droite = Some(Onglet::Transcription);
        } else {
            self.gauche = Onglet::Transcription;
        }
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

/// Rangée d'onglets : fond gris arrondi, l'onglet choisi en blanc.
fn barre_segmentee<R>(ui: &mut Ui, contenu: impl FnOnce(&mut Ui) -> R) -> R {
    let p = palette(ui);
    egui::Frame::new()
        .fill(p.piste)
        .corner_radius(CornerRadius::same(12))
        .inner_margin(egui::Margin::same(4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                contenu(ui)
            })
            .inner
        })
        .inner
}

/// Bouton d'onglet dans une `barre_segmentee`, avec un indicateur quand un travail y est en cours.
fn bouton_onglet(ui: &mut Ui, texte: &str, choisi: bool, occupe: bool) -> egui::Response {
    let p = palette(ui);
    let (texte_riche, couleur) = if choisi { (gras(texte, 14.5), p.texte) } else { (RichText::new(texte).size(14.5), p.texte2) };
    let galley = egui::WidgetText::from(texte_riche).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Button);
    let indicateur = if occupe { 20.0 } else { 0.0 };
    let taille = Vec2::new(galley.size().x + 36.0 + indicateur, 36.0);
    let (rect, resp) = ui.allocate_exact_size(taille, Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    if ui.is_rect_visible(rect) {
        let fond = match (choisi, resp.hovered()) {
            (true, _) => p.surface,
            (false, true) => p.surface.gamma_multiply(0.5),
            (false, false) => Color32::TRANSPARENT,
        };
        ui.painter().rect_filled(rect, CornerRadius::same(9), fond);
        let pos = egui::pos2(rect.left() + 18.0, rect.center().y - galley.size().y / 2.0);
        let fin_texte = pos.x + galley.size().x;
        ui.painter().galley(pos, galley, couleur);
        if occupe {
            let r = egui::Rect::from_center_size(egui::pos2(fin_texte + 13.0, rect.center().y), Vec2::splat(13.0));
            egui::Spinner::new().color(p.accent).paint_at(ui, r);
        }
    }
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, true, choisi, texte));
    resp
}

/// Bouton « Côte à côte », précédé d'un pictogramme de deux volets.
fn bouton_cote_a_cote(ui: &mut Ui, actif: bool) -> egui::Response {
    let galley =
        egui::WidgetText::from(RichText::new("Côte à côte")).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Button);
    let taille = Vec2::new(galley.size().x + 46.0, 36.0);
    let (rect, resp) = ui.allocate_exact_size(taille, Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    if ui.is_rect_visible(rect) {
        let p = palette(ui);
        let (fond, contour, couleur) = match (actif, resp.hovered()) {
            (true, _) => (p.accent_doux, p.accent_doux, p.accent_texte),
            (false, true) => (p.piste, p.bordure_forte, p.texte),
            (false, false) => (p.surface, p.bordure_forte, p.texte),
        };
        ui.painter().rect(rect, CornerRadius::same(10), fond, egui::Stroke::new(1.0, contour), egui::StrokeKind::Inside);
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

/// Espace sous la rangée d'onglets.
fn trait_sous_la_barre(ui: &mut Ui) {
    ui.add_space(8.0);
}

/// Pastille « Écouter » (ou « En écoute », en bleu) au bout du titre d'un point du compte rendu.
fn bouton_ecouter_point(ui: &mut Ui, joue: bool) -> egui::Response {
    let p = palette(ui);
    let (texte, fond, encre) = if joue { ("En écoute", p.accent_plein, Color32::WHITE) } else { ("Écouter", p.piste, p.texte) };
    let galley = egui::WidgetText::from(gras(texte, 13.0)).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Button);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(galley.size().x + 40.0, 30.0), Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    if ui.is_rect_visible(rect) {
        let fond = if resp.hovered() && !joue { p.bordure_forte } else { fond };
        ui.painter().rect_filled(rect, CornerRadius::same(15), fond);
        let c = egui::pos2(rect.left() + 17.0, rect.center().y);
        if joue {
            for dx in [-2.5, 2.5] {
                ui.painter().rect_filled(egui::Rect::from_center_size(c + Vec2::new(dx, 0.0), Vec2::new(2.6, 10.0)), 1, encre);
            }
        } else {
            let pts = vec![c + Vec2::new(-3.0, -5.0), c + Vec2::new(5.0, 0.0), c + Vec2::new(-3.0, 5.0)];
            ui.painter().add(egui::Shape::convex_polygon(pts, encre, egui::Stroke::NONE));
        }
        ui.painter().galley(egui::pos2(rect.left() + 28.0, rect.center().y - galley.size().y / 2.0), galley, encre);
    }
    resp
}

/// Clic sur un bouton d'un point du compte rendu.
enum ClicPoint {
    Ecouter(usize),
    Arreter,
    Voir(usize),
}

/// Cadre autour du texte d'un volet, comme celui de l'ordre du jour ; il occupe toute la place laissée par le pied.
/// Deux colonnes dans l'espace encore libre : `ui.columns` les étire jusqu'au bas de la fiche, par-dessus le lecteur
/// déjà placé au bas.
fn colonnes(ui: &mut Ui, contenu: impl FnOnce(&mut [Ui])) {
    let libre = ui.available_rect_before_wrap();
    ui.scope_builder(egui::UiBuilder::new().max_rect(libre), |ui| ui.columns(2, contenu));
}

pub(super) fn cadre<R>(ui: &mut Ui, contenu: impl FnOnce(&mut Ui) -> R) -> R {
    let p = palette(ui);
    egui::Frame::new()
        .fill(p.surface)
        .stroke(egui::Stroke::new(1.0, p.bordure))
        .corner_radius(CornerRadius::same(16))
        .inner_margin(egui::Margin::symmetric(22, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(ui.available_height());
            contenu(ui)
        })
        .inner
}

/// Pied d'un volet : ses boutons, toujours visibles sous le texte qui défile. Au moins la hauteur d'un bouton, même
/// vide : les cadres de tous les volets s'arrêtent à la même hauteur.
pub(super) fn pied<R>(ui: &mut Ui, id: &str, contenu: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Panel::bottom(id)
        .frame(egui::Frame::NONE)
        .show_separator_line(false)
        .show(ui, |ui| {
            let bouton = ui.text_style_height(&egui::TextStyle::Button) + 2.0 * ui.spacing().button_padding.y;
            ui.set_min_height(bouton.max(ui.spacing().interact_size.y) + 4.0);
            ui.add_space(2.0);
            let r = contenu(ui);
            ui.add_space(2.0);
            r
        })
        .inner
}

impl TourDeTableApp {
    /// Ordre du jour déposé, avant ou pendant la rencontre : l'ordre du jour et la transcription (ou ce qu'on
    /// attend) en deux onglets, ou côte à côte. `transcription` dessine la transcription en cours.
    pub(super) fn volets_direct(&mut self, ui: &mut Ui, transcription: impl Fn(&mut Self, &mut Ui)) {
        let mut onglets = self.onglets_direct;
        onglets.ouvrir_direct(empreinte(self.live.as_ref().map_or("", |m| m.id.as_str())));
        let assez_large = ui.available_width() >= LARGEUR_COTE_A_COTE;
        let cote_a_cote = onglets.droite.is_some() && assez_large;
        let aide = if cote_a_cote { "Revenir à un seul volet" } else { "Affiche l'ordre du jour à côté de la transcription" };
        let ordre_du_jour = |app: &mut Self, ui: &mut Ui| app.agenda_card(ui, true);
        if cote_a_cote {
            colonnes(ui, |colonnes| {
                colonnes[0].with_layout(aligne_a_gauche(), |ui| {
                    ui.horizontal(|ui| barre_segmentee(ui, |ui| bouton_onglet(ui, "Ordre du jour", true, false)));
                    trait_sous_la_barre(ui);
                    ordre_du_jour(self, ui);
                });
                colonnes[1].with_layout(aligne_a_gauche(), |ui| {
                    ui.horizontal(|ui| {
                        barre_segmentee(ui, |ui| bouton_onglet(ui, "Transcription", true, false));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if bouton_cote_a_cote(ui, true).on_hover_text(aide).clicked() {
                                onglets.droite = None;
                            }
                        });
                    });
                    trait_sous_la_barre(ui);
                    pied(ui, "pied-direct", |_| ());
                    cadre(ui, |ui| transcription(self, ui));
                });
            });
        } else {
            ui.horizontal(|ui| {
                barre_segmentee(ui, |ui| {
                    for (onglet, texte) in [(Onglet::OrdreDuJour, "Ordre du jour"), (Onglet::Transcription, "Transcription")] {
                        if bouton_onglet(ui, texte, onglets.gauche == onglet, false).clicked() {
                            onglets.gauche = onglet;
                        }
                    }
                });
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
                _ => {
                    pied(ui, "pied-direct", |_| ());
                    cadre(ui, |ui| transcription(self, ui));
                }
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
                self.barre_onglets(ui, idx, &mut onglets, false, true);
                action = self.volet(ui, idx, gauche);
            }
            Some(d) => {
                let gauche = onglets.gauche;
                colonnes(ui, |colonnes| {
                    colonnes[0].with_layout(aligne_a_gauche(), |ui| {
                        self.barre_onglets(ui, idx, &mut onglets, false, false);
                        action = self.volet(ui, idx, gauche).or(action);
                    });
                    colonnes[1].with_layout(aligne_a_gauche(), |ui| {
                        self.barre_onglets(ui, idx, &mut onglets, true, true);
                        action = self.volet(ui, idx, d).or(action);
                    });
                });
            }
        }
        // « Voir dans la transcription » pendant cette image : la copie locale des onglets fait foi.
        if std::mem::take(&mut self.transcription_demandee) {
            onglets.afficher_transcription(assez_large);
        }
        self.onglets = onglets;
        action
    }

    /// Les trois onglets d'un volet.
    fn barre_onglets(&mut self, ui: &mut Ui, idx: usize, onglets: &mut Onglets, a_droite: bool, bascule: bool) {
        let m = &self.meetings[idx];
        let choisi = if a_droite { onglets.droite.unwrap_or(onglets.gauche) } else { onglets.gauche };
        let redige = self.summary_job.as_ref().is_some_and(|(id, ..)| *id == m.id);
        let nettoie = self.nettoyage_job.as_ref().is_some_and(|t| t.id == m.id);
        let compte_rendu = if m.agenda.is_some() { "Compte rendu" } else { "Résumé" };
        ui.horizontal(|ui| {
            barre_segmentee(ui, |ui| {
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
            // « Côte à côte », toujours au bout de la dernière rangée d'onglets ; absent quand la fenêtre est trop étroite.
            if bascule && ui.ctx().content_rect().width() >= LARGEUR_FENETRE_COTE_A_COTE {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let actif = onglets.droite.is_some();
                    let aide = if actif {
                        "Revenir à un seul volet"
                    } else {
                        "Affiche deux volets en même temps, par exemple l'ordre du jour à côté de la transcription"
                    };
                    if bouton_cote_a_cote(ui, actif).on_hover_text(aide).clicked() {
                        onglets.basculer();
                    }
                });
            }
        });
        trait_sous_la_barre(ui);
    }

    fn volet(&mut self, ui: &mut Ui, idx: usize, onglet: Onglet) -> Option<&'static str> {
        match onglet {
            Onglet::OrdreDuJour => {
                self.agenda_card(ui, true);
                None
            }
            Onglet::Transcription => {
                // Voix reconnues dans une carte à droite quand la fiche est assez large ; sinon, sous le texte.
                let avec_carte = self.meetings[idx].voix.is_some() && ui.available_width() >= 900.0;
                if avec_carte {
                    egui::Panel::right("volet-voix")
                        .exact_size(310.0)
                        .resizable(false)
                        .show_separator_line(false)
                        .frame(egui::Frame::new().inner_margin(egui::Margin { left: 16, right: 0, top: 0, bottom: 0 }))
                        .show(ui, |ui| self.carte_voix(ui, idx));
                }
                // Boutons sous le texte, comme dans l'ordre du jour.
                pied(ui, "pied-transcription", |ui| {
                    if !avec_carte {
                        self.noms_ouverts(ui, idx);
                    }
                    ui.horizontal_wrapped(|ui| {
                        self.barre_nettoyage(ui, idx);
                        ui.add_space(12.0);
                        self.barre_voix(ui, idx, avec_carte);
                    });
                });
                self.bandeau_nettoyage(ui, idx);
                let m = &self.meetings[idx];
                let ecoute = EtatEcoute {
                    boutons: self.recorder.is_none() && crate::audio::blocs::existe(&m.dir()),
                    position: self.position_ecoute(&m.id),
                    suivre: self.ecoute_active(&m.id),
                    aller_a: self.aller_a.as_ref().filter(|(id, _)| *id == m.id).map(|&(_, ms)| ms),
                    saut: self.saut_ecoute.as_ref().filter(|(id, _)| *id == m.id).map(|&(_, ms)| ms),
                    surligne: self.surligne.as_ref().filter(|(id, _)| *id == m.id).map_or(&[], |(_, p)| p.as_slice()),
                };
                let clic = cadre(ui, |ui| match m.nettoyage.as_ref().filter(|_| !self.vue_brute) {
                    Some(n) => transcript_nettoye(ui, m, n, ecoute),
                    None => transcript(ui, m, false, ecoute),
                });
                if ecoute.aller_a.is_some() {
                    self.aller_a = None;
                }
                match clic {
                    Some(ClicEcoute::Ecouter { debut_ms }) => {
                        self.surligne = None;
                        self.ecouter_depuis(idx, debut_ms);
                    }
                    Some(ClicEcoute::Pause) => self.basculer_ecoute(),
                    None => {}
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
        pied(ui, "pied-compte-rendu", |ui| ui.horizontal_wrapped(|ui| {
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
                let texte = if !models::redaction_locale() {
                    "Moins de 16 Go de mémoire vive : pas de rédaction sur cet ordinateur."
                } else if self.installation.is_some() {
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
            if !occupe
                && !m.segments.is_empty()
                && ui
                    .button("Copier pour un assistant en ligne")
                    .on_hover_text(
                        "Copie une demande prête à coller dans Copilot, ChatGPT… : consignes de rédaction, ordre du jour et \
                         transcription.\nTour de table n'envoie rien, mais le texte collé quitte l'ordinateur : vérifiez que votre \
                         organisme le permet.",
                    )
                    .clicked()
            {
                action = Some("assistant");
            }
        }));
        let m = &self.meetings[idx];
        // Points découpés (titre tel qu'écrit dans le compte rendu, périodes) : on peut les écouter ou les retrouver
        // dans la transcription.
        let audio = self.recorder.is_none() && crate::audio::blocs::existe(&m.dir());
        let points: Vec<(String, Vec<(u64, u64)>)> = m
            .agenda
            .as_ref()
            .map(|a| {
                (0..a.points.len())
                    .map(|i| {
                        let plages = m
                            .periodes
                            .iter()
                            .filter(|p| p.point == Some(i))
                            .map(|p| (p.debut_ms, p.fin_ms.unwrap_or(m.duration_s * 1000)))
                            .collect();
                        (m.intertitre(Some(i)), plages)
                    })
                    .collect()
            })
            .unwrap_or_default();
        // Point entendu : celui dont une plage contient l'heure de la lecture, si elle avance.
        let position = self.position_ecoute(&m.id).filter(|_| self.ecoute_active(&m.id));
        let point_a = |ms: u64| points.iter().position(|(_, plages)| plages.iter().any(|&(d, f)| d <= ms && ms < f));
        let en_ecoute = position.and_then(point_a);
        // L'écoute vient de sauter (barre du lecteur, −5 s, +5 s) : le compte rendu défile jusqu'au point entendu.
        let saut = self.saut_ecoute.as_ref().filter(|(id, _)| *id == m.id).and_then(|&(_, ms)| point_a(ms));
        let mut clic_point = None;
        cadre(ui, |ui| match &m.summary {
            Some(s) => {
                egui::ScrollArea::vertical().id_salt("cr").auto_shrink(false).show(ui, |ui| {
                    // Le compte rendu se lit comme une page : une colonne d'au plus 780 px, centrée.
                    let marge = ((ui.available_width() - 780.0) / 2.0).max(0.0) as i8;
                    egui::Frame::new().inner_margin(egui::Margin { left: marge, right: marge, top: 14, bottom: 14 }).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        markdown(ui, s, &mut |ui, titre| {
                            let Some(i) = points.iter().position(|(t, plages)| t == titre.trim() && !plages.is_empty()) else { return };
                            if saut == Some(i) {
                                ui.scroll_to_rect(ui.min_rect(), Some(egui::Align::TOP));
                            }
                            if audio {
                                let joue = en_ecoute == Some(i);
                                if bouton_ecouter_point(ui, joue).on_hover_text(if joue { "Pause" } else { "Écouter à partir de ce point" }).clicked() {
                                    clic_point = Some(if joue { ClicPoint::Arreter } else { ClicPoint::Ecouter(i) });
                                }
                            }
                            let lien = egui::Button::new(RichText::new("Voir dans la transcription").size(13.0).color(palette(ui).accent)).frame(false);
                            if ui.add(lien).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                clic_point = Some(ClicPoint::Voir(i));
                            }
                        });
                    });
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
        });
        match clic_point {
            Some(ClicPoint::Ecouter(i)) => {
                if let Some(&(debut, _)) = points[i].1.first() {
                    self.ecouter_depuis(idx, debut);
                }
            }
            Some(ClicPoint::Arreter) => self.basculer_ecoute(),
            Some(ClicPoint::Voir(i)) => {
                self.aller_a = points[i].1.first().map(|&(d, _)| (self.meetings[idx].id.clone(), d));
                self.surligne = Some((self.meetings[idx].id.clone(), points[i].1.clone()));
                self.transcription_demandee = true;
            }
            None => {}
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
    fn voir_dans_la_transcription_garde_le_compte_rendu() {
        let o = |gauche, droite| Onglets { gauche, droite, rencontre: None };
        let voir = |mut x: Onglets, large| {
            x.afficher_transcription(large);
            (x.gauche, x.droite)
        };
        // Un seul volet : côte à côte, le compte rendu reste à gauche.
        assert_eq!(voir(o(Onglet::CompteRendu, None), true), (Onglet::CompteRendu, Some(Onglet::Transcription)));
        // Côte à côte : la transcription prend le volet qui ne montre pas le compte rendu.
        assert_eq!(
            voir(o(Onglet::CompteRendu, Some(Onglet::OrdreDuJour)), true),
            (Onglet::CompteRendu, Some(Onglet::Transcription))
        );
        assert_eq!(
            voir(o(Onglet::OrdreDuJour, Some(Onglet::CompteRendu)), true),
            (Onglet::Transcription, Some(Onglet::CompteRendu))
        );
        // Fenêtre trop étroite pour deux volets : à la place du compte rendu.
        assert_eq!(voir(o(Onglet::CompteRendu, None), false), (Onglet::Transcription, None));
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
