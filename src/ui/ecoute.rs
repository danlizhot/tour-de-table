//! Écoute dans la fiche d'une rencontre : lecteur fixe au bas de la fiche, qui lit la rencontre à partir d'un endroit
//! (un paragraphe de la transcription, un point du compte rendu, un clic sur les bandes) jusqu'à la fin.
//!
//! Une seule écoute à la fois ; elle s'arrête quand on change de rencontre, qu'on archive celle-ci ou qu'un
//! enregistrement démarre (le son de l'ordinateur serait capté dans la rencontre).

use std::time::Duration;

use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2};

use super::TourDeTableApp;
use super::apparence::palette;
use super::widgets::couleur_locuteur;
use crate::audio::{SAMPLE_RATE, blocs, lecture};
use crate::storage::{Meeting, clock};

/// Pas des boutons −5 s et +5 s.
const PAS_MS: u64 = 5_000;

pub(super) struct Ecoute {
    pub id: String,
    /// Heure de la rencontre où la lecture a commencé.
    debut_ms: u64,
    lecture: lecture::Lecture,
}

fn echantillons(ms: u64) -> u64 {
    ms * SAMPLE_RATE as u64 / 1000
}

/// Fin de l'audio de la rencontre, en ms.
fn fin_ms(m: &Meeting) -> u64 {
    m.duration_s * 1000
}

impl TourDeTableApp {
    /// Lit la rencontre `idx` à partir de `debut_ms`, jusqu'à la fin.
    pub(super) fn ecouter_depuis(&mut self, idx: usize, debut_ms: u64) {
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
        let fin = fin_ms(m);
        let debut_ms = debut_ms.min(fin.saturating_sub(1));
        let lecture = lecture::jouer(dir, index, echantillons(debut_ms), echantillons(fin));
        self.ecoute = Some(Ecoute { id: m.id.clone(), debut_ms, lecture });
    }

    /// Heure de la rencontre `id` qu'on entend, si elle est en écoute.
    pub(super) fn position_ecoute(&self, id: &str) -> Option<u64> {
        self.ecoute.as_ref().filter(|e| e.id == id).map(|e| e.debut_ms + e.lecture.avancement_ms().0)
    }

    /// Vrai si la rencontre `id` est en écoute et pas en pause.
    pub(super) fn ecoute_active(&self, id: &str) -> bool {
        self.ecoute.as_ref().is_some_and(|e| e.id == id && !e.lecture.en_pause())
    }

    /// Met en pause ou reprend l'écoute en cours.
    pub(super) fn basculer_ecoute(&mut self) {
        if let Some(e) = &self.ecoute {
            e.lecture.basculer_pause();
        }
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
            self.ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    /// Lecteur au bas de la fiche de la rencontre `idx` : −5 s, lecture, +5 s, heure, bandes des points et des voix.
    pub(super) fn lecteur(&mut self, ui: &mut Ui, idx: usize) {
        let p = palette(ui);
        let m = &self.meetings[idx];
        let fin = fin_ms(m).max(1);
        let position = self.position_ecoute(&m.id);
        let active = self.ecoute_active(&m.id);
        let mut saut: Option<u64> = None;
        let mut basculer = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let ici = position.unwrap_or(0);
            if bouton_pas(ui, "−5 s").on_hover_text("Reculer de 5 secondes").clicked() {
                saut = Some(ici.saturating_sub(PAS_MS));
            }
            if bouton_lecture_principal(ui, active).on_hover_text(if active { "Pause" } else { "Écouter" }).clicked() {
                basculer = true;
            }
            if bouton_pas(ui, "+5 s").on_hover_text("Avancer de 5 secondes").clicked() {
                saut = Some((ici + PAS_MS).min(fin - 1));
            }
            ui.add_space(6.0);
            ui.label(RichText::new(format!("{} / {}", clock(ici), clock(fin))).size(13.5).color(p.texte));
            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                // Une largeur minimale par minute : les prises de parole restent visibles. Plus large que la place
                // disponible (fenêtre réduite ou longue rencontre), la ligne du temps défile, avec sa barre.
                let largeur = ui.available_width();
                let contenu = largeur.max(fin as f32 / 60_000.0 * PX_PAR_MINUTE);
                if contenu > largeur + 1.0 {
                    ui.scope(|ui| {
                        ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
                        egui::ScrollArea::horizontal()
                            .id_salt(("ligne-du-temps", &m.id))
                            .auto_shrink([false, true])
                            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                            .show(ui, |ui| {
                                if let Some(ms) = bandes(ui, m, contenu, position, active) {
                                    saut = Some(ms);
                                }
                            });
                    });
                } else if let Some(ms) = bandes(ui, m, largeur, position, false) {
                    saut = Some(ms);
                }
                // Légende à droite ; l'étiquette prend le reste et se termine par « … » si la place manque.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let legende = if m.voix.is_some() && !m.periodes.is_empty() { "Haut : points · Bas : qui parle" } else { "" };
                    ui.label(RichText::new(legende).size(12.0).color(p.texte2));
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(egui::Label::new(RichText::new(etiquette_position(m, position)).size(12.0).color(p.texte2)).truncate());
                    });
                });
            });
        });
        if let Some(ms) = saut {
            let pause = self.ecoute.as_ref().is_some_and(|e| e.lecture.en_pause());
            self.ecouter_depuis(idx, ms);
            if pause {
                self.basculer_ecoute();
            }
        } else if basculer {
            match position {
                Some(_) => self.basculer_ecoute(),
                None => self.ecouter_depuis(idx, 0),
            }
        }
    }
}

/// « Point 4 · Rapport financier du trimestre · Luc Fortin » à l'heure entendue.
fn etiquette_position(m: &Meeting, position: Option<u64>) -> String {
    let Some(ms) = position else {
        return "Cliquez sur ▶ d'un paragraphe, d'un point ou sur la ligne du temps pour écouter".into();
    };
    let mut morceaux = Vec::new();
    if !m.periodes.is_empty() {
        let point = m.point_de(ms);
        morceaux.push(m.intertitre(point));
    }
    if let Some(v) = m.voix.as_ref().and_then(|v| v.tours.iter().find(|t| t.debut_ms <= ms && ms < t.fin_ms).map(|t| v.nom(t.voix))) {
        morceaux.push(v);
    }
    if morceaux.is_empty() { "Écoute de la rencontre".into() } else { morceaux.join(" · ") }
}

/// Teintes des points de l'ordre du jour, en alternance pour distinguer deux points voisins.
fn teinte_point(ui: &Ui, i: usize) -> Color32 {
    let p = palette(ui);
    let fortes = [0.18, 0.32, 0.46, 0.60];
    let base = if ui.visuals().dark_mode { p.piste.lerp_to_gamma(Color32::WHITE, 0.08) } else { Color32::WHITE };
    base.lerp_to_gamma(p.accent, fortes[i % fortes.len()])
}

/// Largeur minimale de la ligne du temps, par minute de rencontre.
const PX_PAR_MINUTE: f32 = 36.0;

/// Bandes de la ligne du temps (points en haut, voix en bas) et marqueur de la position ; rend l'heure cliquée.
/// `suivre` : dans une ligne du temps qui défile, garder le marqueur au centre pendant la lecture. Glisser sur les
/// bandes déplace un marqueur fantôme ; au relâchement, l'écoute reprend à cet endroit.
fn bandes(ui: &mut Ui, m: &Meeting, largeur: f32, position: Option<u64>, suivre: bool) -> Option<u64> {
    let p = palette(ui);
    let fin = fin_ms(m).max(1) as f32;
    let avec_points = !m.periodes.is_empty();
    let avec_voix = m.voix.is_some();
    let haut_points = if avec_points { 22.0 } else { 0.0 };
    let haut_voix = if avec_voix { 12.0 } else if avec_points { 0.0 } else { 10.0 };
    let hauteur = haut_points + haut_voix + if avec_points && avec_voix { 3.0 } else { 0.0 };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(largeur, hauteur), Sense::click_and_drag());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let x = |ms: u64| rect.left() + rect.width() * (ms as f32 / fin).clamp(0.0, 1.0);
    let painter = ui.painter_at(rect.expand(6.0));
    let mut infobulle = None;
    if avec_points {
        let bande = Rect::from_min_size(rect.min, Vec2::new(rect.width(), haut_points));
        for per in &m.periodes {
            let (d, f) = (per.debut_ms, per.fin_ms.unwrap_or(fin as u64));
            let r = Rect::from_x_y_ranges(x(d) + 1.0..=x(f) - 1.0, bande.y_range());
            let actif = position.is_some_and(|ms| d <= ms && ms < f);
            let fond = match per.point {
                Some(_) if actif => p.accent_plein,
                Some(i) => teinte_point(ui, i),
                None => p.piste,
            };
            painter.rect_filled(r, CornerRadius::same(4), fond);
            let numero = per.point.map(|i| m.intertitre(Some(i)).split_once(". ").map_or(format!("{}", i + 1), |(n, _)| n.to_string()));
            if let Some(n) = &numero
                && r.width() > 18.0
            {
                let clair = actif || per.point.is_some_and(|i| i % 4 >= 2) || ui.visuals().dark_mode;
                painter.text(r.center(), egui::Align2::CENTER_CENTER, n, super::colonne::police_grasse(11.0), if clair { Color32::WHITE } else { p.texte });
            }
            if let Some(h) = resp.hover_pos()
                && r.contains(h)
            {
                infobulle = Some(format!("{} ({})", m.intertitre(per.point), clock(d)));
            }
        }
    }
    if let Some(v) = &m.voix {
        let haut = rect.top() + haut_points + if avec_points { 3.0 } else { 0.0 };
        let bande = Rect::from_min_size(Pos2::new(rect.left(), haut), Vec2::new(rect.width(), haut_voix));
        painter.rect_filled(bande, CornerRadius::same(3), p.piste);
        for t in &v.tours {
            let r = Rect::from_x_y_ranges(x(t.debut_ms)..=x(t.fin_ms).max(x(t.debut_ms) + 1.0), bande.y_range());
            painter.rect_filled(r, CornerRadius::ZERO, couleur_locuteur(crate::audio::Source::Mixte, Some(t.voix)));
            if infobulle.is_none()
                && let Some(h) = resp.hover_pos()
                && r.contains(h)
            {
                infobulle = Some(v.nom(t.voix));
            }
        }
    } else if !avec_points {
        painter.rect_filled(rect, CornerRadius::same(5), p.piste);
    }
    if let Some(ms) = position {
        let r = Rect::from_center_size(Pos2::new(x(ms), rect.center().y), Vec2::new(3.0, rect.height() + 8.0));
        painter.rect_filled(r, CornerRadius::same(2), p.texte);
        if suivre && !resp.dragged() {
            ui.scroll_to_rect(r, Some(egui::Align::Center));
        }
    }
    // Glisser : marqueur fantôme sous le pointeur, gardé en vue (la ligne du temps défile quand on s'approche du bord).
    let pointeur = ui.input(|i| i.pointer.latest_pos());
    let fraction = |h: Pos2| ((h.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
    if resp.dragged()
        && let Some(h) = pointeur
    {
        let gx = rect.left() + fraction(h) * rect.width();
        let r = Rect::from_center_size(Pos2::new(gx, rect.center().y), Vec2::new(3.0, rect.height() + 8.0));
        painter.rect_filled(r, CornerRadius::same(2), p.accent);
        ui.scroll_to_rect(r.expand2(Vec2::new(40.0, 0.0)), None);
        resp.show_tooltip_text(clock((fraction(h) * fin) as u64));
    }
    if resp.drag_stopped()
        && let Some(h) = pointeur
    {
        return Some((fraction(h) * fin) as u64);
    }
    let survol = resp.hover_pos().map(|h| ((h.x - rect.left()) / rect.width()).clamp(0.0, 1.0));
    let resp = match (infobulle, survol) {
        (Some(t), Some(f)) => resp.on_hover_text(format!("{t}\nAller à {}", clock((f * fin) as u64))),
        (None, Some(f)) => resp.on_hover_text(format!("Aller à {}", clock((f * fin) as u64))),
        _ => resp,
    };
    if resp.clicked()
        && let Some(h) = resp.interact_pointer_pos()
    {
        return Some((((h.x - rect.left()) / rect.width()).clamp(0.0, 1.0) * fin) as u64);
    }
    None
}

/// Bouton −5 s / +5 s.
fn bouton_pas(ui: &mut Ui, texte: &str) -> egui::Response {
    let p = palette(ui);
    let b = egui::Button::new(super::apparence::gras(texte, 13.0).color(p.texte))
        .fill(p.piste)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(9))
        .min_size(Vec2::new(46.0, 34.0));
    ui.add(b)
}

/// Grand bouton rond lecture / pause.
fn bouton_lecture_principal(ui: &mut Ui, active: bool) -> egui::Response {
    let p = palette(ui);
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(44.0), Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    if ui.is_rect_visible(rect) {
        let fond = if resp.hovered() { p.accent_plein.lerp_to_gamma(Color32::BLACK, 0.12) } else { p.accent_plein };
        ui.painter().circle_filled(rect.center(), 22.0, fond);
        let c = rect.center();
        if active {
            for dx in [-4.0, 4.0] {
                ui.painter().rect_filled(Rect::from_center_size(c + Vec2::new(dx, 0.0), Vec2::new(4.0, 14.0)), CornerRadius::same(1), Color32::WHITE);
            }
        } else {
            let pts = vec![c + Vec2::new(-4.0, -7.0), c + Vec2::new(8.0, 0.0), c + Vec2::new(-4.0, 7.0)];
            ui.painter().add(egui::Shape::convex_polygon(pts, Color32::WHITE, Stroke::NONE));
        }
    }
    resp
}
