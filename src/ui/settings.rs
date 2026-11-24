//! Fenêtre des Paramètres : seulement ce que l'application ne peut pas décider
//! seule. Modèles, fils de calcul, GPU, détection de voix… sont automatiques.

use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, Stroke, Ui};

use super::apparence::{etiquette_section, gras, palette, palette_de, segmente};

use super::TourDeTableApp;
use super::widgets::device_combo;
use crate::config::CaptureMode;

impl TourDeTableApp {
    /// Fenêtre des Paramètres, par-dessus l'application assombrie ; se ferme par la croix, Échap ou un clic à côté.
    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
        let p = palette_de(ctx);
        let mut changed = false;
        let mut fermer = false;
        let cadre = egui::Frame::new().fill(p.surface).corner_radius(CornerRadius::same(20)).inner_margin(Margin::same(0)).shadow(egui::Shadow {
            offset: [0, 12],
            blur: 40,
            spread: 0,
            color: Color32::from_black_alpha(70),
        });
        let reponse = egui::Modal::new(egui::Id::unique("parametres")).frame(cadre).backdrop_color(Color32::from_rgba_unmultiplied(20, 28, 45, 96)).show(ctx, |ui| {
            ui.set_width(560.0);
            egui::Frame::new().inner_margin(Margin { left: 24, right: 18, top: 18, bottom: 8 }).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(gras("Paramètres", 20.0).color(p.texte));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if bouton_fermer(ui).on_hover_text("Fermer (Échap)").clicked() {
                            fermer = true;
                        }
                    });
                });
            });
            let hauteur = ctx.content_rect().height() - 160.0;
            egui::ScrollArea::vertical().max_height(hauteur).min_scrolled_height(hauteur.min(640.0)).show(ui, |ui| {
                section(ui, "Son", true, |ui| {
                    ui.horizontal(|ui| {
                        ui.allocate_ui_with_layout(egui::vec2(190.0, 30.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.set_min_width(190.0);
                            ui.label(RichText::new("Micro de la salle").size(14.0).color(p.texte));
                        });
                        device_combo(ui, "mic", &mut self.cfg.mic_device, &self.mics, "Par défaut");
                    });
                    ui.horizontal(|ui| {
                        ui.allocate_ui_with_layout(egui::vec2(190.0, 30.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.set_min_width(190.0);
                            ui.label(RichText::new("Son de l'ordinateur").size(14.0).color(p.texte));
                        });
                        device_combo(ui, "sys", &mut self.cfg.system_device, &self.system_sources, "Par défaut");
                    });
                    ui.add_space(4.0);
                    ui.label(RichText::new("Les participants à distance sortent-ils dans les haut-parleurs de la salle?").size(14.0).color(p.texte));
                    segmente(ui, &mut self.cfg.mode, &[(CaptureMode::Separe, "Non"), (CaptureMode::Mixe, "Oui")], 30.0);
                    ui.label(
                        RichText::new(match self.cfg.mode {
                            CaptureMode::Separe => "Casque ou haut-parleur de conférence : la transcription distingue « Salle » et « À distance ».",
                            CaptureMode::Mixe => "Le micro capte aussi les participants à distance : les deux sources sont réunies pour éviter les doublons.",
                        })
                        .size(12.5)
                        .color(p.texte2),
                    );
                });
                section(ui, "Ordre du jour", false, |ui| {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.set_width(ui.available_width() - 60.0);
                            ui.label(RichText::new("Passer automatiquement au point annoncé").size(14.0).color(p.texte));
                            ui.label(
                                RichText::new("Quand on entend « on passe au point 4 », la rencontre passe seule à ce point (annulable 10 secondes). Sinon, Tour de table propose le point et vous confirmez.")
                                    .size(12.5)
                                    .color(p.texte2),
                            );
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            interrupteur(ui, &mut self.cfg.passage_auto);
                        });
                    });
                });
                section(ui, "Lexique", false, |ui| {
                    ui.label(
                        RichText::new("Un terme par ligne (noms propres, organismes, sigles) pour guider la reconnaissance, ou une correction « mauvais => bon ». Ex. : hydro québec => Hydro-Québec")
                            .size(13.0)
                            .color(p.texte2),
                    );
                    changed |= ui
                        .add(egui::TextEdit::multiline(&mut self.cfg.glossary).desired_rows(6).desired_width(f32::INFINITY).margin(Margin::symmetric(12, 10)))
                        .changed();
                });
                ui.add_space(14.0);
            });
        });
        if changed {
            self.push_settings();
        }
        if fermer || reponse.should_close() {
            self.cfg.save();
            self.show_settings = false;
        }
    }
}

/// Section de la fenêtre : titre en petites capitales, trait au-dessus sauf pour la première.
fn section(ui: &mut Ui, titre: &str, premiere: bool, contenu: impl FnOnce(&mut Ui)) {
    let p = palette(ui);
    if !premiere {
        let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
        ui.painter().hline(r.x_range(), r.center().y, Stroke::new(1.0, p.bordure));
    }
    egui::Frame::new().inner_margin(Margin { left: 24, right: 24, top: 14, bottom: 16 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 8.0;
        etiquette_section(ui, titre);
        contenu(ui);
    });
}

/// Interrupteur (glissière) : bascule `valeur` au clic.
fn interrupteur(ui: &mut Ui, valeur: &mut bool) -> egui::Response {
    let p = palette(ui);
    let (rect, mut resp) = ui.allocate_exact_size(egui::vec2(44.0, 24.0), egui::Sense::click());
    if resp.clicked() {
        *valeur = !*valeur;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(resp.id, *valeur);
    let fond = p.bordure_forte.lerp_to_gamma(p.accent_plein, t);
    ui.painter().rect_filled(rect, CornerRadius::same(12), fond);
    let x = egui::lerp((rect.left() + 12.0)..=(rect.right() - 12.0), t);
    ui.painter().circle_filled(egui::pos2(x, rect.center().y), 9.0, Color32::WHITE);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Bouton rond de fermeture (croix dessinée).
fn bouton_fermer(ui: &mut Ui) -> egui::Response {
    let p = palette(ui);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(34.0, 34.0), egui::Sense::click());
    let fond = if resp.hovered() { p.bordure_forte } else { p.piste };
    ui.painter().circle_filled(rect.center(), 17.0, fond);
    let c = rect.center();
    let t = Stroke::new(1.8, p.texte);
    ui.painter().line_segment([c + egui::vec2(-5.0, -5.0), c + egui::vec2(5.0, 5.0)], t);
    ui.painter().line_segment([c + egui::vec2(5.0, -5.0), c + egui::vec2(-5.0, 5.0)], t);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}
