//! Colonne de gauche : logo, nouvelle rencontre (ou rencontre en cours), import, historique, puis guide, archives,
//! paramètres, numéro de version et thème.

use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, Margin, Pos2, Rect, Sense, Stroke, Ui, Vec2};

use super::apparence::{Genre, bouton_widget, date_courte, duree_courte, etiquette_section, gras, palette};
use super::widgets::{draw_logo, theme_button};
use super::{ModelState, TourDeTableApp, View};
use crate::audio;
use crate::config::{self, Theme};
use crate::storage::{self, clock};

pub(super) const LARGEUR: f32 = 260.0;

impl TourDeTableApp {
    pub(super) fn colonne(&mut self, ui: &mut Ui) {
        let p = palette(ui);
        egui::Panel::left("history")
            .resizable(false)
            .exact_size(LARGEUR)
            .frame(egui::Frame::new().fill(p.surface).inner_margin(Margin { left: 14, right: 14, top: 18, bottom: 12 }))
            .show(ui, |ui| {
                egui::Panel::bottom("pied").show_separator_line(false).frame(egui::Frame::new()).show(ui, |ui| self.pied(ui));
                self.entete(ui);
                ui.add_space(16.0);
                self.boutons_rencontre(ui);
                ui.add_space(18.0);
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    etiquette_section(ui, "Historique");
                });
                ui.add_space(2.0);
                self.historique(ui);
            });
    }

    fn entete(&mut self, ui: &mut Ui) {
        let p = palette(ui);
        ui.horizontal(|ui| {
            ui.add_space(2.0);
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(32.0), Sense::hover());
            draw_logo(ui.painter(), rect);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.label(gras("Tour de table", 16.5).color(p.texte));
                ui.label(egui::RichText::new("Tout le monde parle, rien ne se perd.").size(10.5).color(p.texte2));
            });
        });
        // Le modèle est choisi et installé seul : on ne parle de lui que s'il n'est pas prêt.
        let etat = match &self.model {
            ModelState::Ready => None,
            ModelState::Missing => Some(("Aucun modèle installé", p.rouge, None)),
            ModelState::Loading => Some(("Chargement du modèle…", p.ambre_texte, None)),
            ModelState::Verification => Some(("Vérification des modèles…", p.ambre_texte, None)),
            ModelState::Failed(e) => Some(("Modèle en erreur", p.rouge, Some(e.clone()))),
        };
        if let Some((texte, couleur, detail)) = etat {
            ui.add_space(10.0);
            let r = ui.horizontal(|ui| {
                ui.add_space(8.0);
                let (r, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                ui.painter().circle_filled(r.center(), 4.0, couleur);
                ui.label(egui::RichText::new(texte).size(12.5).color(couleur));
            });
            if let Some(e) = detail {
                r.response.on_hover_text(e);
            }
        }
    }

    fn boutons_rencontre(&mut self, ui: &mut Ui) {
        let p = palette(ui);
        let largeur = ui.available_width();
        if let Some(rec) = &self.recorder {
            let duree = clock(rec.origine_ms + rec.started.elapsed().as_millis() as u64);
            let detail = if rec.en_pause() { format!("En pause · {duree}") } else { format!("Enregistrement · {duree}") };
            let pause = rec.en_pause();
            let choisi = matches!(self.view, View::Live);
            let resp = carte_cliquable(ui, largeur, 54.0, |ui, rect, survol| {
                let fond = if choisi || survol { p.rouge_doux } else { p.rouge_doux.gamma_multiply(0.7) };
                ui.painter().rect_filled(rect, CornerRadius::same(10), fond);
                ui.painter().rect_filled(Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height())), CornerRadius::same(2), p.rouge);
                let point = Pos2::new(rect.left() + 20.0, rect.center().y);
                if !pause {
                    let t = ui.input(|i| i.time) as f32;
                    let phase = (t % 1.8) / 1.8;
                    ui.painter().circle_filled(point, 5.0 + 6.0 * phase, p.rouge.gamma_multiply(0.45 * (1.0 - phase)));
                }
                ui.painter().circle_filled(point, 5.0, if pause { p.texte2 } else { p.rouge });
                let x = rect.left() + 36.0;
                ui.painter().text(Pos2::new(x, rect.top() + 10.0), egui::Align2::LEFT_TOP, "Rencontre en cours", police_grasse(14.0), p.texte);
                ui.painter().text(Pos2::new(x, rect.top() + 30.0), egui::Align2::LEFT_TOP, detail, FontId::proportional(12.5), p.rouge_texte);
            });
            if resp.clicked() {
                self.view = View::Live;
            }
        } else if self.live.is_some() {
            // Import ou fin d'une transcription : même carte, en ambre, pour revenir à l'écran qui suit le travail.
            let detail = match self.fraction_import() {
                Some(f) => format!("Import · {} %", (f * 100.0).round() as u32),
                None if self.import_job.is_some() => "Import · préparation".to_string(),
                None => format!("{} énoncés restants", self.in_flight),
            };
            let choisi = matches!(self.view, View::Live);
            let resp = carte_cliquable(ui, largeur, 54.0, |ui, rect, survol| {
                let fond = if choisi || survol { p.ambre_doux } else { p.ambre_doux.gamma_multiply(0.7) };
                ui.painter().rect_filled(rect, CornerRadius::same(10), fond);
                ui.painter().rect_stroke(rect, CornerRadius::same(10), Stroke::new(1.0, p.ambre_bord), egui::StrokeKind::Inside);
                ui.painter().rect_filled(Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height())), CornerRadius::same(2), p.ambre);
                // Arc qui tourne : le travail avance.
                let centre = Pos2::new(rect.left() + 20.0, rect.center().y);
                let t = ui.input(|i| i.time) as f32;
                let debut = t * 4.0;
                let points: Vec<Pos2> = (0..=20).map(|i| centre + 6.0 * Vec2::angled(debut + i as f32 / 20.0 * 4.2)).collect();
                ui.painter().add(egui::Shape::line(points, Stroke::new(2.0, p.ambre)));
                let x = rect.left() + 36.0;
                ui.painter().text(Pos2::new(x, rect.top() + 10.0), egui::Align2::LEFT_TOP, "Transcription en cours", police_grasse(14.0), p.texte);
                ui.painter().text(Pos2::new(x, rect.top() + 30.0), egui::Align2::LEFT_TOP, detail, FontId::proportional(12.5), p.ambre_texte);
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
            });
            if resp.on_hover_text("Revenir à la transcription en cours").clicked() {
                self.view = View::Live;
            }
        } else {
            let b = egui::Button::new(gras("+  Nouvelle rencontre", 15.0).color(Color32::WHITE))
                .fill(p.accent_plein)
                .stroke(Stroke::NONE)
                .corner_radius(CornerRadius::same(10))
                .min_size(Vec2::new(largeur, 40.0));
            if ui.add(b).clicked() {
                self.view = View::Live;
            }
        }
        ui.add_space(8.0);
        let libre = self.recorder.is_none() && !self.finishing && self.import_job.is_none() && self.verification.is_none();
        let b = bouton_widget(ui, "Importer un enregistrement…", Genre::Secondaire, 38.0).min_size(Vec2::new(largeur, 38.0));
        if ui
            .add_enabled(libre, b)
            .on_hover_text("MP3, M4A, MP4 (Teams), WAV, FLAC ou OGG. Vous pouvez aussi glisser le fichier dans la fenêtre.")
            .clicked()
        {
            self.pick_audio();
        }
    }

    fn historique(&mut self, ui: &mut Ui) {
        let p = palette(ui);
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            let mut choix = None;
            for (i, m) in self.meetings.iter().enumerate() {
                let choisi = matches!(self.view, View::Past(j) if j == i);
                let detail = format!("{} · {}", date_courte(&m.started_at), duree_courte(m.duration_s));
                let resp = carte_cliquable(ui, ui.available_width(), 52.0, |ui, rect, survol| {
                    if choisi {
                        ui.painter().rect_filled(rect, CornerRadius::same(10), p.accent_doux);
                        ui.painter().rect_filled(Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height())), CornerRadius::same(2), p.accent);
                    } else if survol {
                        ui.painter().rect_filled(rect, CornerRadius::same(10), p.piste);
                    }
                    let largeur = rect.width() - 24.0;
                    texte_tronque(ui, Pos2::new(rect.left() + 12.0, rect.top() + 8.0), &m.title, police_grasse(14.0), p.texte, largeur);
                    texte_tronque(ui, Pos2::new(rect.left() + 12.0, rect.top() + 28.0), &detail, FontId::proportional(12.5), p.texte2, largeur);
                });
                if resp.on_hover_text(&m.title).clicked() {
                    choix = Some(i);
                }
            }
            if let Some(i) = choix {
                self.view = View::Past(i);
            }
            if self.meetings.is_empty() {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("Aucune rencontre pour l'instant.").size(13.0).color(p.texte2));
                });
            }
        });
    }

    fn pied(&mut self, ui: &mut Ui) {
        let p = palette(ui);
        let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 9.0), Sense::hover());
        ui.painter().hline(r.x_range(), r.top(), Stroke::new(1.0, p.bordure));
        if ligne_pied(ui, "Guide d'utilisation", icone_livre, false)
            .on_hover_text("Ouvre le guide d'utilisation (PDF), livré avec l'application")
            .clicked()
            && let Err(e) = storage::ouvrir_guide()
        {
            log::error!("Guide d'utilisation : {e:#}");
            self.notify(format!("{e:#}"), true);
        }
        if ligne_pied(ui, "Archives", icone_archive, false).on_hover_text("Ouvre le dossier des rencontres archivées (fichiers .zip)").clicked() {
            let dir = config::archives_dir();
            match std::fs::create_dir_all(&dir) {
                Ok(()) => storage::reveal(&dir),
                Err(e) => self.notify(format!("Dossier des archives : {e}"), true),
            }
        }
        if ligne_pied(ui, "Paramètres", icone_engrenage, self.show_settings).clicked() {
            self.show_settings = !self.show_settings;
            if self.show_settings {
                self.mics = audio::list_microphones();
                self.system_sources = audio::list_system_sources();
            }
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(egui::RichText::new(concat!("Version ", env!("CARGO_PKG_VERSION"))).size(12.0).color(p.texte2));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme_button(ui, self.cfg.theme).clicked() {
                    self.cfg.theme = match self.cfg.theme {
                        Theme::Systeme => Theme::Clair,
                        Theme::Clair => Theme::Fonce,
                        Theme::Fonce => Theme::Systeme,
                    };
                    self.appliquer_theme();
                    self.cfg.save();
                }
            });
        });
    }
}

/// Police grasse de la taille donnée, pour le dessin direct.
pub(super) fn police_grasse(taille: f32) -> FontId {
    FontId::new(taille, FontFamily::Name("gras".into()))
}

/// Zone cliquable de `largeur` × `hauteur`, dessinée par `dessin(ui, rect, survolée)`.
pub(super) fn carte_cliquable(ui: &mut Ui, largeur: f32, hauteur: f32, dessin: impl FnOnce(&mut Ui, Rect, bool)) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(largeur, hauteur), Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    if ui.is_rect_visible(rect) {
        dessin(ui, rect, resp.hovered());
    }
    resp
}

/// Texte sur une ligne, coupé par « … » s'il dépasse `largeur`.
pub(super) fn texte_tronque(ui: &Ui, pos: Pos2, texte: &str, police: FontId, couleur: Color32, largeur: f32) {
    let mut job = egui::text::LayoutJob::simple_singleline(texte.to_string(), police, couleur);
    job.wrap = egui::text::TextWrapping { max_width: largeur, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    ui.painter().galley(pos, galley, couleur);
}

pub(super) type Icone = fn(&egui::Painter, Rect, Color32);

/// Bouton carré à icône, contour gris (Dossier, Archiver…).
pub(super) fn bouton_icone(ui: &mut Ui, icone: Icone, actif: bool) -> egui::Response {
    let p = palette(ui);
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(38.0), if actif { Sense::click() } else { Sense::hover() });
    let resp = if actif { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
    if ui.is_rect_visible(rect) {
        let fond = if actif && resp.hovered() { p.piste } else { p.surface };
        ui.painter().rect(rect, CornerRadius::same(10), fond, Stroke::new(1.0, p.bordure_forte), egui::StrokeKind::Inside);
        let couleur = if actif { p.texte } else { p.texte2.gamma_multiply(0.6) };
        icone(ui.painter(), Rect::from_center_size(rect.center(), Vec2::splat(17.0)), couleur);
    }
    if actif { resp } else { resp.on_hover_cursor(egui::CursorIcon::NotAllowed) }
}

pub(super) fn icone_dossier(p: &egui::Painter, r: Rect, c: Color32) {
    let t = Stroke::new(1.6, c);
    let corps = Rect::from_min_max(Pos2::new(r.min.x, r.min.y + 4.0), Pos2::new(r.max.x, r.max.y - 1.0));
    p.rect_stroke(corps, CornerRadius::same(2), t, egui::StrokeKind::Middle);
    p.line_segment([Pos2::new(r.min.x + 1.0, r.min.y + 4.0), Pos2::new(r.min.x + 2.0, r.min.y + 1.5)], t);
    p.line_segment([Pos2::new(r.min.x + 2.0, r.min.y + 1.5), Pos2::new(r.min.x + 7.0, r.min.y + 1.5)], t);
    p.line_segment([Pos2::new(r.min.x + 7.0, r.min.y + 1.5), Pos2::new(r.min.x + 8.5, r.min.y + 4.0)], t);
}

/// Ligne du pied de la colonne : icône et libellé, fond au survol.
fn ligne_pied(ui: &mut Ui, texte: &str, icone: Icone, actif: bool) -> egui::Response {
    let p = palette(ui);
    carte_cliquable(ui, ui.available_width(), 36.0, |ui, rect, survol| {
        if survol || actif {
            ui.painter().rect_filled(rect, CornerRadius::same(8), p.piste);
        }
        let couleur = if actif { p.texte } else { p.texte.gamma_multiply(0.85) };
        icone(ui.painter(), Rect::from_center_size(Pos2::new(rect.left() + 18.0, rect.center().y), Vec2::splat(16.0)), couleur);
        let police = if actif { police_grasse(14.0) } else { FontId::proportional(14.0) };
        ui.painter().text(Pos2::new(rect.left() + 36.0, rect.center().y), egui::Align2::LEFT_CENTER, texte, police, couleur);
    })
}

fn icone_livre(p: &egui::Painter, r: Rect, c: Color32) {
    let t = Stroke::new(1.6, c);
    let (g, d) = (Rect::from_min_max(r.min + Vec2::new(0.0, 2.0), Pos2::new(r.center().x, r.max.y - 1.0)), Rect::from_min_max(Pos2::new(r.center().x, r.min.y + 2.0), r.max - Vec2::new(0.0, 1.0)));
    p.rect_stroke(g, CornerRadius::same(2), t, egui::StrokeKind::Middle);
    p.rect_stroke(d, CornerRadius::same(2), t, egui::StrokeKind::Middle);
}

pub(super) fn icone_archive(p: &egui::Painter, r: Rect, c: Color32) {
    let t = Stroke::new(1.6, c);
    let haut = Rect::from_min_max(r.min + Vec2::new(0.0, 1.0), Pos2::new(r.max.x, r.min.y + 6.0));
    let bas = Rect::from_min_max(Pos2::new(r.min.x + 1.5, r.min.y + 6.0), r.max - Vec2::new(1.5, 1.0));
    p.rect_stroke(haut, CornerRadius::same(1), t, egui::StrokeKind::Middle);
    p.rect_stroke(bas, CornerRadius::same(1), t, egui::StrokeKind::Middle);
    p.line_segment([Pos2::new(r.center().x - 2.5, r.min.y + 9.5), Pos2::new(r.center().x + 2.5, r.min.y + 9.5)], t);
}

fn icone_engrenage(p: &egui::Painter, r: Rect, c: Color32) {
    let t = Stroke::new(1.6, c);
    let centre = r.center();
    p.circle_stroke(centre, 2.6, t);
    p.circle_stroke(centre, 5.6, t);
    for i in 0..8 {
        let a = i as f32 * std::f32::consts::FRAC_PI_4;
        let d = Vec2::new(a.cos(), a.sin());
        p.line_segment([centre + d * 5.6, centre + d * 8.0], Stroke::new(2.2, c));
    }
}
