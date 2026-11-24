//! Mode découpage dans la fiche (« Découper par points ») : la transcription s'affiche ligne par ligne ; un clic sur
//! une ligne y fait commencer un point de l'ordre du jour, un clic sur l'en-tête d'un point le modifie (changer de
//! point, une ligne plus haut ou plus bas, retirer). « Valider le découpage » range l'audio par point, comme l'écran de
//! vérification (`verification.rs`), qui reste pour la fin d'un import.

use eframe::egui::{self, Color32, CornerRadius, Margin, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2};

use super::TourDeTableApp;
use super::apparence::{Genre, bouton_widget, gras, initiales, palette, pastille};
use super::widgets::{bouton_lecture, couleur_locuteur, lisible};
use crate::audio::Source;
use crate::decoupage::Frontiere;
use crate::storage::{Declenchement, Meeting, Periode, clock};

/// Une ligne de la transcription : une phrase nettoyée, ou un segment brut si le nettoyage n'est pas terminé.
struct LigneTexte {
    debut_ms: u64,
    fin_ms: u64,
    source: Source,
    texte: String,
}

fn lignes_texte(m: &Meeting) -> Vec<LigneTexte> {
    match m.nettoyage.as_ref().filter(|n| n.termine() && !n.phrases.is_empty()) {
        Some(n) => n.phrases.iter().map(|p| LigneTexte { debut_ms: p.debut_ms, fin_ms: p.fin_ms, source: p.source, texte: p.texte.clone() }).collect(),
        None => m.segments.iter().map(|s| LigneTexte { debut_ms: s.start_ms, fin_ms: s.end_ms, source: s.source, texte: s.text.clone() }).collect(),
    }
}

/// Point proposé en premier pour une ligne à `ms` : le premier point sans frontière dont les voisins de l'ordre du jour
/// encadrent cette heure (`places` : point et heure des frontières).
fn suggestion(places: &[(usize, u64)], ms: u64, points: usize) -> Option<usize> {
    (0..points).filter(|p| !places.iter().any(|(q, _)| q == p)).find(|&p| {
        let avant = places.iter().filter(|(q, _)| *q < p).map(|(_, t)| *t).max();
        let apres = places.iter().filter(|(q, _)| *q > p).map(|(_, t)| *t).min();
        avant.is_none_or(|a| a < ms) && apres.is_none_or(|b| ms < b)
    })
}

/// Pour chaque frontière (dans l'ordre du temps), le plus petit point placé plus tard qui la précède dans l'ordre du
/// jour : la frontière est alors « avant le point q ».
fn hors_ordre(points: &[Option<usize>]) -> Vec<Option<usize>> {
    points.iter().enumerate().map(|(i, p)| p.and_then(|p| points[i + 1..].iter().flatten().copied().filter(|&q| q < p).min())).collect()
}

/// Périodes provisoires d'après les frontières triées, pour la bande des points du lecteur.
fn apercu(frontieres: &[&Frontiere], fin_ms: u64) -> Vec<Periode> {
    let mut v = Vec::new();
    for (i, f) in frontieres.iter().enumerate() {
        if f.point.is_some() {
            let fin = frontieres.get(i + 1).map_or(fin_ms, |g| g.debut_ms);
            v.push(Periode { point: f.point, debut_ms: f.debut_ms, fin_ms: Some(fin.max(f.debut_ms)), declenchement: f.declenchement });
        }
    }
    v
}

/// Choix dans la liste des points.
enum Choix {
    Point(Option<usize>),
    Fermer,
}

/// Ce que l'en-tête d'un point demande.
enum ActionEntete {
    Selection,
    Ecouter,
    Changer,
    Monter,
    Descendre,
    Retirer,
}

impl TourDeTableApp {
    /// La fiche en mode découpage. Faux si ce n'est pas le cas (fin d'un import, rencontre absente de l'historique) :
    /// l'écran de vérification s'affiche alors.
    pub(super) fn mode_decoupage(&mut self, ui: &mut Ui) -> bool {
        let Some(v) = self.verification.as_ref().filter(|v| !v.nouvelle) else { return false };
        let id = v.rencontre.id.clone();
        let Some(idx) = self.meetings.iter().position(|m| m.id == id) else { return false };
        if self.ecoute.as_ref().is_some_and(|e| e.id != id) {
            self.ecoute = None;
        }
        let occupe = self.decoupage_job.is_some();
        let p = palette(ui);
        let (mut annuler, mut valider) = (false, false);

        ui.add_space(4.0);
        ui.add(egui::Label::new(gras(&self.meetings[idx].title, 24.0).color(p.texte)).truncate());
        ui.add_space(8.0);
        {
            let v = self.verification.as_ref().expect("vérifié plus haut");
            let total = v.rencontre.agenda.as_ref().map_or(0, |a| a.points.len());
            let places = (0..total).filter(|q| v.lignes.iter().any(|l| l.f.point == Some(*q))).count();
            egui::Frame::new()
                .fill(p.accent_doux)
                .stroke(Stroke::new(1.5, p.accent))
                .corner_radius(CornerRadius::same(14))
                .inner_margin(Margin::symmetric(16, 12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let texte = (ui.available_width() - 470.0).max(200.0);
                        ui.allocate_ui_with_layout(Vec2::new(texte, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.label(gras("Mode découpage", 15.0).color(p.accent_texte));
                            ui.add(
                                egui::Label::new(
                                    RichText::new("Cliquez sur la ligne où commence un point. Les points se rangent d'eux-mêmes selon l'ordre du jour.")
                                        .size(13.0)
                                        .color(p.texte),
                                )
                                .wrap(),
                            );
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if occupe {
                                ui.label(RichText::new("Rangement de l'audio par point…").size(14.0).color(p.texte2));
                                ui.spinner();
                            } else {
                                valider = ui.add(bouton_widget(ui, "Valider le découpage", Genre::Principal, 40.0)).clicked();
                                annuler = ui.add(bouton_widget(ui, "Annuler", Genre::Discret, 40.0)).on_hover_text("Revient à la rencontre sans rien changer").clicked();
                            }
                            let (fond, bord, encre) = if places < total { (p.ambre_doux, p.ambre_bord, p.ambre_texte) } else { (p.vert_doux, p.vert, p.texte) };
                            egui::Frame::new().fill(fond).stroke(Stroke::new(1.0, bord)).corner_radius(CornerRadius::same(255)).inner_margin(Margin::symmetric(12, 5)).show(ui, |ui| {
                                ui.label(gras(format!("{places} points sur {total} placés"), 13.0).color(encre));
                            });
                        });
                    });
                });
        }
        ui.add_space(10.0);

        // Lecteur au bas : sa bande des points montre le découpage en cours, pas celui enregistré.
        if self.recorder.is_none() && crate::audio::blocs::existe(&self.meetings[idx].dir()) {
            let v = self.verification.as_ref().expect("vérifié plus haut");
            let provisoires = apercu(&v.lignes.iter().map(|l| &l.f).collect::<Vec<_>>(), v.duree_ms.max(self.meetings[idx].duration_s * 1000));
            let enregistrees = std::mem::replace(&mut self.meetings[idx].periodes, provisoires);
            egui::Panel::bottom("lecteur")
                .show_separator_line(false)
                .frame(egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.bordure)).corner_radius(CornerRadius::same(14)).inner_margin(Margin::symmetric(16, 10)).outer_margin(Margin { left: 0, right: 0, top: 10, bottom: 0 }))
                .show(ui, |ui| self.lecteur(ui, idx));
            self.meetings[idx].periodes = enregistrees;
        }

        let ecouter = self.contenu_decoupage(ui, idx);
        if let Some(ms) = ecouter {
            self.ecouter_depuis(idx, ms);
        }

        if annuler {
            self.verification = None;
        } else if valider && let Some(v) = self.verification.as_mut() {
            v.choix = None;
            v.choisi = None;
            let frontieres = v.lignes.iter().map(|l| l.f.clone()).collect();
            self.valider_decoupage(self.meetings[idx].clone(), false, frontieres);
        }
        true
    }

    /// Liste des points à droite et transcription ligne par ligne ; rend l'heure à écouter, s'il y a lieu.
    fn contenu_decoupage(&mut self, ui: &mut Ui, idx: usize) -> Option<u64> {
        let p = palette(ui);
        let m = &self.meetings[idx];
        let v = self.verification.as_mut()?;
        let points: Vec<(String, String)> = m.agenda.as_ref().map(|a| a.points.iter().map(|pt| (pt.numero.clone(), pt.titre.clone())).collect()).unwrap_or_default();
        let premiere = |q: usize, lignes: &[super::verification::Ligne]| lignes.iter().find(|l| l.f.point == Some(q)).map(|l| l.f.debut_ms);

        if ui.available_width() >= 1000.0 {
            egui::Panel::right("points-decoupage")
                .exact_size(300.0)
                .resizable(false)
                .show_separator_line(false)
                .frame(egui::Frame::new().inner_margin(Margin { left: 14, right: 0, top: 0, bottom: 0 }))
                .show(ui, |ui| {
                    egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.bordure)).corner_radius(CornerRadius::same(16)).inner_margin(Margin::same(16)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(gras("Points", 15.0).color(p.texte));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label(RichText::new("début").size(12.0).color(p.texte2)));
                        });
                        ui.add_space(4.0);
                        egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
                            for (q, (numero, titre)) in points.iter().enumerate() {
                                let debut = premiere(q, &v.lignes);
                                if ligne_point(ui, numero, titre, debut).clicked()
                                    && let Some(ms) = debut
                                {
                                    v.aller_a = Some(ms);
                                }
                            }
                        });
                    });
                });
        }

        let lignes = lignes_texte(m);
        let debuts: Vec<u64> = lignes.iter().map(|l| l.debut_ms).collect();
        let places: Vec<(usize, u64)> = v.lignes.iter().filter_map(|l| l.f.point.map(|q| (q, l.f.debut_ms))).collect();
        let avis = hors_ordre(&v.lignes.iter().map(|l| l.f.point).collect::<Vec<_>>());
        let nom = |q: Option<usize>| m.intertitre(q);
        let mut ecouter = None;
        let mut placer: Option<(u64, Option<usize>, String)> = None;
        let mut changer: Option<(usize, Option<usize>)> = None;
        let mut action: Option<(usize, ActionEntete)> = None;
        let aller_a = v.aller_a.take();

        egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.bordure)).corner_radius(CornerRadius::same(16)).inner_margin(Margin { left: 8, right: 14, top: 10, bottom: 10 }).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                let mut k = 0;
                let mut voix_precedente = None;
                let mut entete = |ui: &mut Ui, k: usize, v: &super::verification::Verification| {
                    let f = &v.lignes[k].f;
                    let choisi = v.choisi == Some(k);
                    let avis = avis[k].map(|q| format!("avant le point {} dans l'ordre du jour", points.get(q).map_or(String::new(), |pt| pt.0.clone())));
                    let (resp, a) = entete_point(ui, &nom(f.point), f.point.is_some(), f.debut_ms, choisi, avis.as_deref());
                    if aller_a == Some(f.debut_ms) {
                        resp.scroll_to_me(Some(egui::Align::TOP));
                    }
                    if let Some(a) = a {
                        action = Some((k, a));
                    }
                    if choisi && v.changer {
                        match liste_points(ui, &points, &places, None, f.point) {
                            Some(Choix::Point(q)) => changer = Some((k, q)),
                            Some(Choix::Fermer) => action = Some((k, ActionEntete::Changer)),
                            None => {}
                        }
                    }
                };
                for l in &lignes {
                    while k < v.lignes.len() && v.lignes[k].f.debut_ms <= l.debut_ms {
                        entete(ui, k, v);
                        k += 1;
                        voix_precedente = None;
                    }
                    let cle = (l.source, m.voix_entre(l.debut_ms, l.fin_ms));
                    let qui = (voix_precedente != Some(cle)).then(|| (m.locuteur(cle.0, cle.1), couleur_locuteur(cle.0, cle.1)));
                    voix_precedente = Some(cle);
                    let ouvert = v.choix == Some(l.debut_ms);
                    let (clic, joue) = ligne_texte(ui, l, qui.as_ref(), ouvert);
                    if joue {
                        ecouter = Some(l.debut_ms);
                    }
                    if clic {
                        v.choix = if ouvert { None } else { Some(l.debut_ms) };
                        v.choisi = None;
                    }
                    if ouvert {
                        match liste_points(ui, &points, &places, suggestion(&places, l.debut_ms, points.len()), None) {
                            Some(Choix::Point(q)) => placer = Some((l.debut_ms, q, l.texte.clone())),
                            Some(Choix::Fermer) => v.choix = None,
                            None => {}
                        }
                    }
                }
                while k < v.lignes.len() {
                    entete(ui, k, v);
                    k += 1;
                }
                ui.add_space(12.0);
            });
        });

        if let Some((ms, point, extrait)) = placer {
            v.choix = None;
            let k = match v.lignes.iter().position(|l| l.f.debut_ms == ms) {
                Some(k) => {
                    v.lignes[k].f.point = point;
                    k
                }
                None => v.ajouter(Frontiere { point, debut_ms: ms, extrait, raison: String::new(), declenchement: Declenchement::Manuel }),
            };
            v.choisi = Some(k);
        }
        if let Some((k, q)) = changer {
            v.lignes[k].f.point = q;
            v.lignes[k].f.declenchement = Declenchement::Manuel;
            v.changer = false;
        }
        if let Some((k, a)) = action {
            let debut = v.lignes[k].f.debut_ms;
            // Ligne devant laquelle l'en-tête s'affiche : la première qui commence à son heure ou après.
            let i = debuts.partition_point(|&d| d < debut);
            let vers = |cible: Option<&u64>, v: &mut super::verification::Verification| {
                if let Some(&ms) = cible {
                    v.deplacer(k, ms);
                    v.trier();
                    v.choisi = v.lignes.iter().position(|l| l.f.debut_ms == ms);
                }
            };
            match a {
                ActionEntete::Selection => {
                    v.choisi = if v.choisi == Some(k) { None } else { Some(k) };
                    v.changer = false;
                    v.choix = None;
                }
                ActionEntete::Ecouter => ecouter = Some(debut.saturating_sub(2_000)),
                ActionEntete::Changer => v.changer = !v.changer,
                ActionEntete::Monter => vers(i.checked_sub(1).and_then(|j| debuts.get(j)), v),
                ActionEntete::Descendre => vers(debuts.get(i + 1), v),
                ActionEntete::Retirer => {
                    v.lignes.remove(k);
                    v.choisi = None;
                    v.changer = false;
                }
            }
        }
        ecouter
    }
}

/// Une ligne de la liste des points : numéro, titre, heure de début ou « à placer ».
fn ligne_point(ui: &mut Ui, numero: &str, titre: &str, debut: Option<u64>) -> egui::Response {
    let p = palette(ui);
    let fond = ui.painter().add(egui::Shape::Noop);
    let rangee = ui.horizontal(|ui| {
        ui.set_min_height(30.0);
        puce(ui, numero, debut.is_some(), false, 24.0);
        let heure = debut.map_or("à placer".to_string(), clock);
        let largeur = (ui.available_width() - 64.0).max(40.0);
        ui.allocate_ui_with_layout(Vec2::new(largeur, 24.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_min_width(largeur);
            ui.add(egui::Label::new(RichText::new(titre).size(13.0).color(p.texte)).truncate());
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (texte, couleur) = if debut.is_some() { (RichText::new(heure).size(12.0), p.texte2) } else { (gras(heure.as_str(), 12.0), p.ambre_texte) };
            ui.label(texte.color(couleur));
        });
    });
    let rect = rangee.response.rect.expand2(Vec2::new(4.0, 2.0));
    let resp = ui.interact(rect, ui.scope_id().with(("point", numero)), if debut.is_some() { Sense::click() } else { Sense::hover() });
    if debut.is_none() {
        ui.painter().set(fond, egui::Shape::rect_filled(rect, CornerRadius::same(8), p.ambre_doux.gamma_multiply(0.5)));
    } else if resp.hovered() {
        ui.painter().set(fond, egui::Shape::rect_filled(rect, CornerRadius::same(8), p.piste));
    }
    if debut.is_some() { resp.on_hover_text("Aller au début de ce point").on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp.on_hover_text("Ajoutez-le avec « + Point ici » sur sa première ligne") }
}

/// Numéro d'un point dans un carré : bleu s'il est placé, ambre pointillé sinon, bleu plein s'il est choisi.
fn puce(ui: &mut Ui, numero: &str, place: bool, choisi: bool, cote: f32) -> egui::Response {
    let p = palette(ui);
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(cote), Sense::hover());
    let (fond, encre) = match (choisi, place) {
        (true, _) => (p.accent_plein, Color32::WHITE),
        (false, true) => (p.accent_doux, p.accent_texte),
        (false, false) => (p.ambre_doux, p.ambre_texte),
    };
    ui.painter().rect_filled(rect, CornerRadius::same(7), fond);
    if !place && !choisi {
        ui.painter().rect_stroke(rect.shrink(0.5), CornerRadius::same(7), Stroke::new(1.0, p.ambre_bord), egui::StrokeKind::Inside);
    }
    let taille = if numero.chars().count() > 2 { cote * 0.4 } else { cote * 0.48 };
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, numero, super::colonne::police_grasse(taille), encre);
    resp
}

/// En-tête d'un point dans la transcription ; choisi, il montre ses actions.
fn entete_point(ui: &mut Ui, intertitre: &str, point: bool, debut_ms: u64, choisi: bool, avis: Option<&str>) -> (egui::Response, Option<ActionEntete>) {
    let p = palette(ui);
    let (numero, titre) = match point {
        true => intertitre.split_once(". ").map_or((String::new(), intertitre.to_string()), |(n, t)| (n.to_string(), t.to_string())),
        false => ("–".to_string(), intertitre.to_string()),
    };
    let mut action = None;
    ui.add_space(8.0);
    let (fond, contour) = if choisi { (p.accent_doux, Stroke::new(1.5, p.accent)) } else { (Color32::TRANSPARENT, Stroke::NONE) };
    let cadre = egui::Frame::new().fill(fond).stroke(contour).corner_radius(CornerRadius::same(12)).inner_margin(Margin { left: 62, right: 8, top: 6, bottom: 6 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let numero_resp = puce(ui, &numero, true, choisi, 28.0).interact(Sense::click());
            ui.add_space(2.0);
            let largeur_titre = (ui.available_width() - if choisi { 470.0 } else { 120.0 } - if avis.is_some() { 260.0 } else { 0.0 }).max(80.0);
            let titre_resp = ui
                .allocate_ui_with_layout(Vec2::new(largeur_titre, 28.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add(egui::Label::new(gras(&titre, 17.0).color(p.texte)).truncate().sense(Sense::click()))
                })
                .inner;
            ui.label(RichText::new(clock(debut_ms)).size(12.0).color(p.texte2));
            if let Some(a) = avis {
                egui::Frame::new().fill(p.ambre_doux).stroke(Stroke::new(1.0, p.ambre_bord)).corner_radius(CornerRadius::same(255)).inner_margin(Margin::symmetric(10, 3)).show(ui, |ui| {
                    ui.label(gras(a, 12.0).color(p.ambre_texte));
                });
            }
            if (numero_resp.clicked() || titre_resp.clicked()) && action.is_none() {
                action = Some(ActionEntete::Selection);
            }
            titre_resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(if choisi { "Fermer les actions" } else { "Modifier ce point" });
            if choisi {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(bouton_widget(ui, "Retirer", Genre::Danger, 32.0)).clicked() {
                        action = Some(ActionEntete::Retirer);
                    }
                    if ui.add(bouton_widget(ui, "↓", Genre::Secondaire, 32.0)).on_hover_text("Commencer une ligne plus bas").clicked() {
                        action = Some(ActionEntete::Descendre);
                    }
                    if ui.add(bouton_widget(ui, "↑", Genre::Secondaire, 32.0)).on_hover_text("Commencer une ligne plus haut").clicked() {
                        action = Some(ActionEntete::Monter);
                    }
                    if ui.add(bouton_widget(ui, "Changer de point", Genre::Secondaire, 32.0)).clicked() {
                        action = Some(ActionEntete::Changer);
                    }
                    if bouton_lecture(ui, false).on_hover_text("Écouter le début du point").clicked() {
                        action = Some(ActionEntete::Ecouter);
                    }
                });
            }
        });
    });
    (cadre.response, action)
}

/// Une ligne de la transcription : heure, pastille et nom au changement de voix, texte. Au survol (ou liste ouverte),
/// elle s'encadre et montre ▶ et « + Point ici ». Rend (clic pour choisir un point, ▶ cliqué).
fn ligne_texte(ui: &mut Ui, l: &LigneTexte, qui: Option<&(String, Color32)>, ouvert: bool) -> (bool, bool) {
    let p = palette(ui);
    let fond = ui.painter().add(egui::Shape::Noop);
    if qui.is_some() {
        ui.add_space(6.0);
    }
    let reserve = 150.0;
    let rangee = ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.allocate_ui_with_layout(Vec2::new(52.0, 20.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.set_min_width(52.0);
            ui.label(RichText::new(clock(l.debut_ms)).size(12.0).color(p.texte2));
        });
        match qui {
            Some((nom, couleur)) => {
                pastille(ui, &initiales(nom), *couleur, 26.0);
            }
            None => {
                ui.allocate_exact_size(Vec2::splat(26.0), Sense::hover());
            }
        }
        let largeur = (ui.available_width() - reserve).max(120.0);
        ui.allocate_ui_with_layout(Vec2::new(largeur, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.set_width(largeur);
            ui.spacing_mut().item_spacing.y = 1.0;
            if let Some((nom, couleur)) = qui {
                ui.label(gras(nom, 13.5).color(lisible(ui, *couleur)));
            }
            ui.add(egui::Label::new(RichText::new(&l.texte).size(14.5).color(p.texte)).wrap());
        });
    });
    let rect = rangee.response.rect.expand2(Vec2::new(6.0, 3.0));
    let rect = Rect::from_min_max(rect.min, Pos2::new(ui.max_rect().right(), rect.max.y));
    let resp = ui.interact(rect, ui.scope_id().with(("ligne", l.debut_ms)), Sense::click());
    let survol = ui.rect_contains_pointer(rect);
    let (mut clic, mut joue) = (resp.clicked(), false);
    if survol || ouvert {
        ui.painter().set(fond, egui::epaint::RectShape::new(rect, CornerRadius::same(10), p.accent_doux, Stroke::new(1.5, p.accent), egui::StrokeKind::Inside));
        let haut = rect.top() + 3.0;
        let plus = Rect::from_min_size(Pos2::new(rect.right() - 112.0, haut), Vec2::new(106.0, 28.0));
        let ecoute = Rect::from_min_size(Pos2::new(plus.left() - 32.0, haut + 2.0), Vec2::splat(24.0));
        joue = ui.scope_builder(egui::UiBuilder::new().max_rect(ecoute), |ui| bouton_lecture(ui, false).on_hover_text("Écouter à partir d'ici")).inner.clicked();
        clic |= ui.put(plus, bouton_widget(ui, "+ Point ici", Genre::Principal, 28.0)).clicked();
    }
    (clic && !joue, joue)
}

/// Liste « Quel point commence ici ? » sous une ligne ou un en-tête : le point suggéré d'abord mis en évidence, les points
/// à placer en ambre, ceux déjà placés en gris avec leur heure, puis « Hors point ».
fn liste_points(ui: &mut Ui, points: &[(String, String)], places: &[(usize, u64)], suggere: Option<usize>, actuel: Option<usize>) -> Option<Choix> {
    let p = palette(ui);
    let mut choix = None;
    ui.horizontal(|ui| {
        ui.add_space(86.0);
        egui::Frame::new()
            .fill(p.surface)
            .stroke(Stroke::new(1.0, p.bordure_forte))
            .corner_radius(CornerRadius::same(12))
            .inner_margin(Margin::same(8))
            .shadow(egui::epaint::Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(30) })
            .show(ui, |ui| {
                ui.set_width(ui.available_width().min(460.0));
                ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(gras("Quel point commence ici ?", 12.5).color(p.texte2));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(bouton_widget(ui, "Fermer", Genre::Discret, 26.0)).clicked() {
                            choix = Some(Choix::Fermer);
                        }
                    });
                });
                let mut options: Vec<Option<usize>> = (0..points.len()).map(Some).collect();
                options.push(None);
                for q in options {
                    let place = q.and_then(|q| places.iter().find(|(r, _)| *r == q).map(|(_, t)| *t));
                    let (numero, titre) = match q {
                        Some(q) => (points[q].0.clone(), points[q].1.clone()),
                        None => ("–".to_string(), "Hors point (rien à rattacher)".to_string()),
                    };
                    let (note, couleur_note) = match (q, place) {
                        _ if q.is_some() && q == suggere => ("suggéré ici".to_string(), p.accent_texte),
                        _ if q.is_some() && q == actuel => ("actuel".to_string(), p.texte2),
                        (Some(_), Some(t)) => (format!("placé à {}", clock(t)), p.texte2),
                        (Some(_), None) => ("à placer".to_string(), p.ambre_texte),
                        (None, _) => (String::new(), p.texte2),
                    };
                    let fond = ui.painter().add(egui::Shape::Noop);
                    let rangee = ui.horizontal(|ui| {
                        ui.set_min_height(30.0);
                        ui.add_space(4.0);
                        puce(ui, &numero, place.is_some() || q.is_none(), false, 22.0);
                        let largeur = (ui.available_width() - 126.0).max(60.0);
                        let couleur = if place.is_some() && q != suggere { p.texte2 } else { p.texte };
                        ui.allocate_ui_with_layout(Vec2::new(largeur, 22.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.set_min_width(largeur);
                            let t = if q.is_some() && q == suggere { gras(&titre, 13.5) } else { RichText::new(&titre).size(13.5) };
                            ui.add(egui::Label::new(t.color(couleur)).truncate());
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(note).size(11.5).color(couleur_note));
                        });
                    });
                    let rect = rangee.response.rect;
                    let resp = ui.interact(rect, ui.scope_id().with(("choix", q)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
                    if q.is_some() && q == suggere {
                        ui.painter().set(fond, egui::Shape::rect_filled(rect, CornerRadius::same(8), p.accent_doux));
                    } else if resp.hovered() {
                        ui.painter().set(fond, egui::Shape::rect_filled(rect, CornerRadius::same(8), p.piste));
                    }
                    if resp.clicked() {
                        choix = Some(Choix::Point(q));
                    }
                }
                });
            });
    });
    ui.add_space(4.0);
    choix
}

#[cfg(test)]
mod tests {
    use super::{apercu, hors_ordre, suggestion};
    use crate::decoupage::Frontiere;
    use crate::storage::Declenchement;

    #[test]
    fn point_suggere_entre_ses_voisins() {
        // Points 1, 3 et 4 placés (indices 0, 2, 3) ; le point 2 (indice 1) manque.
        let places = [(0, 0), (2, 600_000), (3, 900_000)];
        assert_eq!(suggestion(&places, 300_000, 5), Some(1));
        // Après le point 4 : le point 5 (indice 4).
        assert_eq!(suggestion(&places, 1_000_000, 5), Some(4));
        // Entre le 3 et le 4 : aucun point manquant n'y va.
        assert_eq!(suggestion(&places, 700_000, 5), None);
    }

    #[test]
    fn points_hors_de_l_ordre() {
        // 1, 2, 4, 6, 5 : le 6 est avant le 5 ; hors point ignoré.
        let v = [Some(0), Some(1), Some(3), None, Some(5), Some(4)];
        assert_eq!(hors_ordre(&v), vec![None, None, None, None, Some(4), None]);
    }

    #[test]
    fn periodes_provisoires() {
        let f = |point, debut_ms| Frontiere { point, debut_ms, extrait: String::new(), raison: String::new(), declenchement: Declenchement::Manuel };
        let v = [f(Some(0), 0), f(None, 100), f(Some(1), 200)];
        let p = apercu(&v.iter().collect::<Vec<_>>(), 500);
        assert_eq!(p.len(), 2);
        assert_eq!((p[0].debut_ms, p[0].fin_ms), (0, Some(100)));
        assert_eq!((p[1].debut_ms, p[1].fin_ms), (200, Some(500)));
    }
}
