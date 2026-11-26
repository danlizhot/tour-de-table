//! Écran « Nouvelle rencontre », avant de démarrer : l'ordre du jour (facultatif), ce qu'on enregistre, et le grand
//! bouton « Démarrer l'enregistrement ».

use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, Sense, Stroke, Ui, Vec2};

use super::apparence::{gras, palette};
use super::{ModelState, TourDeTableApp};

/// Largeur de la colonne de l'écran.
const LARGEUR: f32 = 760.0;

impl TourDeTableApp {
    pub(super) fn ecran_nouvelle(&mut self, ui: &mut Ui) {
        let p = palette(ui);
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            let marge = ((ui.available_width() - LARGEUR) / 2.0).max(0.0) as i8;
            egui::Frame::new().inner_margin(Margin { left: marge, right: marge, top: 28, bottom: 24 }).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.label(gras("Nouvelle rencontre", 28.0).color(p.texte));
                ui.label(RichText::new("Tout est traité sur cet ordinateur, rien n'est envoyé en ligne.").size(15.0).color(p.texte2));
                ui.add_space(18.0);

                // 1. Ordre du jour : une zone où le déposer, ou la carte de l'ordre du jour déposé.
                if self.draft_agenda.is_some() {
                    etape(ui, 1, "Ordre du jour", Some("le compte rendu suivra ses points"));
                    ui.add_space(4.0);
                    self.agenda_card(ui, false);
                } else {
                    let mut parcourir = false;
                    let mut saisir = false;
                    carte(ui, |ui| {
                        etape(ui, 1, "Ordre du jour", Some("facultatif, mais le compte rendu suit ses points"));
                        ui.add_space(12.0);
                        (parcourir, saisir) = zone_de_depot(ui);
                    });
                    if parcourir {
                        let choisi = rfd::FileDialog::new()
                            .set_title("Choisir l'ordre du jour")
                            .add_filter("Ordre du jour", crate::agenda::EXTENSIONS)
                            .pick_file();
                        if let Some(chemin) = choisi {
                            self.attach_agenda(&chemin);
                        }
                    }
                    if saisir {
                        self.draft_agenda = Some(crate::storage::Agenda::new("Saisi à la main".into(), String::new()));
                    }
                }
                ui.add_space(14.0);

                // 2. Ce qu'on enregistre.
                carte(ui, |ui| {
                    etape(ui, 2, "Ce qu'on enregistre", None);
                    ui.add_space(12.0);
                    ui.columns(2, |colonnes| {
                        carte_source(&mut colonnes[0], &mut self.cfg.capture_mic, "Micro de la salle", "Les personnes autour de la table");
                        carte_source(&mut colonnes[1], &mut self.cfg.capture_system, "Son de l'ordinateur", "Participants à distance : Teams, Zoom, Meet…");
                    });
                });
                ui.add_space(20.0);

                // Démarrer.
                let pret = matches!(self.model, ModelState::Ready) && !self.finishing && self.import_job.is_none() && self.annulation.is_none() && self.fin_apres_panne.is_none();
                let source = self.cfg.capture_mic || self.cfg.capture_system;
                let resp = bouton_demarrer(ui, pret && source);
                let resp = match (pret, source) {
                    (_, false) => resp.on_hover_text("Choisissez au moins une source à enregistrer"),
                    (false, _) => resp.on_hover_text("Le modèle de reconnaissance vocale n'est pas encore prêt"),
                    _ => resp.on_hover_text("Vous pourrez mettre en pause ou arrêter à tout moment"),
                };
                if resp.clicked() {
                    self.start_recording();
                }
            });
        });
    }
}

/// Carte blanche de l'écran.
fn carte<R>(ui: &mut Ui, contenu: impl FnOnce(&mut Ui) -> R) -> R {
    let p = palette(ui);
    egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.bordure))
        .corner_radius(CornerRadius::same(16))
        .inner_margin(Margin::same(20))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            contenu(ui)
        })
        .inner
}

/// En-tête d'une étape : numéro dans un rond, titre, précision.
fn etape(ui: &mut Ui, numero: u8, titre: &str, precision: Option<&str>) {
    let p = palette(ui);
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::hover());
        ui.painter().circle_filled(r.center(), 13.0, p.accent_doux);
        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, numero.to_string(), super::colonne::police_grasse(13.0), p.accent_texte);
        ui.add_space(2.0);
        ui.label(gras(titre, 16.0).color(p.texte));
        if let Some(t) = precision {
            ui.label(RichText::new(t).size(13.0).color(p.texte2));
        }
    });
}

/// Grande zone en pointillé où glisser l'ordre du jour ; rend (Parcourir, Saisir) cliqués.
fn zone_de_depot(ui: &mut Ui) -> (bool, bool) {
    let p = palette(ui);
    let mut clics = (false, false);
    let reponse = egui::Frame::new()
        .fill(if ui.visuals().dark_mode { p.fond } else { Color32::from_rgb(0xFA, 0xFB, 0xFD) })
        .corner_radius(CornerRadius::same(14))
        .inner_margin(Margin::symmetric(20, 26))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.vertical_centered(|ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                let (r, _) = ui.allocate_exact_size(Vec2::new(30.0, 34.0), Sense::hover());
                icone_document(ui.painter(), r, p.texte2);
                ui.label(gras("Glissez le document ici", 15.0).color(p.texte));
                ui.label(RichText::new("Word, PDF ou texte").size(13.0).color(p.texte2));
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    // Deux boutons centrés : largeur connue d'avance.
                    let largeur = 240.0;
                    ui.add_space(((ui.available_width() - largeur) / 2.0).max(0.0));
                    clics.0 = ui.add(super::apparence::bouton_widget(ui, "Parcourir…", super::apparence::Genre::Secondaire, 36.0)).clicked();
                    clics.1 = ui.add(super::apparence::bouton_widget(ui, "Saisir les points", super::apparence::Genre::Secondaire, 36.0)).clicked();
                });
            });
        });
    // Contour en pointillé.
    let rect = reponse.response.rect;
    let trait_ = Stroke::new(2.0, p.bordure_forte);
    let pointilles = |a: egui::Pos2, b: egui::Pos2| egui::Shape::dashed_line(&[a, b], trait_, 7.0, 5.0);
    let (r, s) = (rect.shrink(1.0), ui.painter());
    for forme in [
        pointilles(r.left_top() + Vec2::new(14.0, 0.0), r.right_top() - Vec2::new(14.0, 0.0)),
        pointilles(r.left_bottom() + Vec2::new(14.0, 0.0), r.right_bottom() - Vec2::new(14.0, 0.0)),
        pointilles(r.left_top() + Vec2::new(0.0, 14.0), r.left_bottom() - Vec2::new(0.0, 14.0)),
        pointilles(r.right_top() + Vec2::new(0.0, 14.0), r.right_bottom() - Vec2::new(0.0, 14.0)),
    ] {
        s.extend(forme);
    }
    clics
}

fn icone_document(p: &egui::Painter, r: egui::Rect, c: Color32) {
    let t = Stroke::new(1.8, c);
    let (g, d, h, b) = (r.left() + 3.0, r.right() - 3.0, r.top() + 1.0, r.bottom() - 1.0);
    let coin = 8.0;
    p.line(vec![egui::pos2(d - coin, h), egui::pos2(g, h), egui::pos2(g, b), egui::pos2(d, b), egui::pos2(d, h + coin), egui::pos2(d - coin, h), egui::pos2(d - coin, h + coin), egui::pos2(d, h + coin)], t);
    let c0 = egui::pos2(r.center().x, b - 7.0);
    p.line_segment([c0, c0 - Vec2::new(0.0, 11.0)], t);
    p.line(vec![c0 + Vec2::new(-4.0, -7.0), c0 - Vec2::new(0.0, 11.0), c0 + Vec2::new(4.0, -7.0)], t);
}

/// Carte d'une source, cochée ou non ; un clic n'importe où la bascule.
fn carte_source(ui: &mut Ui, coche: &mut bool, titre: &str, detail: &str) {
    let p = palette(ui);
    let (fond, contour) = if *coche { (p.accent_doux, Stroke::new(1.5, p.accent)) } else { (p.surface, Stroke::new(1.0, p.bordure_forte)) };
    let reponse = egui::Frame::new()
        .fill(fond)
        .stroke(contour)
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                let (r, _) = ui.allocate_exact_size(Vec2::splat(20.0), Sense::hover());
                let case = r.shrink(1.0);
                if *coche {
                    ui.painter().rect_filled(case, CornerRadius::same(5), p.accent_plein);
                    let q = |x: f32, y: f32| case.center() + Vec2::new(x, y);
                    ui.painter().line(vec![q(-4.5, 0.5), q(-1.5, 3.5), q(4.5, -3.0)], Stroke::new(2.0, Color32::WHITE));
                } else {
                    ui.painter().rect(case, CornerRadius::same(5), p.surface, Stroke::new(1.5, p.bordure_forte), egui::StrokeKind::Inside);
                }
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(gras(titre, 14.5).color(p.texte));
                    ui.label(RichText::new(detail).size(12.5).color(p.texte2));
                });
            });
        })
        .response;
    let clic = ui.interact(reponse.rect, ui.scope_id().with(titre), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
    if clic.clicked() {
        *coche = !*coche;
    }
}

/// Grand bouton rouge « Démarrer l'enregistrement », avec son point blanc.
fn bouton_demarrer(ui: &mut Ui, actif: bool) -> egui::Response {
    let p = palette(ui);
    let texte = gras("Démarrer l'enregistrement", 17.0).color(Color32::WHITE);
    let galley = egui::WidgetText::from(texte).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Button);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(galley.size().x + 74.0, 54.0), if actif { Sense::click() } else { Sense::hover() });
    let resp = if actif { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
    if ui.is_rect_visible(rect) {
        let fond = match (actif, resp.hovered()) {
            (false, _) => p.rouge.gamma_multiply(0.45),
            (true, true) => p.rouge.lerp_to_gamma(Color32::BLACK, 0.12),
            (true, false) => p.rouge,
        };
        ui.painter().rect_filled(rect, CornerRadius::same(14), fond);
        ui.painter().circle_filled(egui::pos2(rect.left() + 30.0, rect.center().y), 7.0, Color32::WHITE);
        ui.painter().galley(egui::pos2(rect.left() + 50.0, rect.center().y - galley.size().y / 2.0), galley, Color32::WHITE);
    }
    if actif { resp } else { resp.on_hover_cursor(egui::CursorIcon::NotAllowed) }
}
