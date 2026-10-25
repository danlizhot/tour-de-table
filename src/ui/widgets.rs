//! Composants réutilisables : couleurs, transcription, Markdown, jauges, logo.

use std::sync::atomic::Ordering;

use eframe::egui::{self, Color32, CornerRadius, Pos2, RichText, Sense, Ui, Vec2};

use super::Download;
use crate::audio::{DeviceInfo, Source};
use crate::models;
use crate::storage::{Meeting, clock};

/// Bleu du drapeau du Québec.
pub const BLEU: Color32 = Color32::from_rgb(0x00, 0x3D, 0xA5);
const BLEU_CLAIR: Color32 = Color32::from_rgb(0x3B, 0x82, 0xF6);
pub(super) const ROUGE: Color32 = Color32::from_rgb(0xDC, 0x26, 0x26);
pub(super) const SALLE: Color32 = Color32::from_rgb(0x0E, 0x9F, 0x6E);
pub(super) const DISTANCE: Color32 = Color32::from_rgb(0x7C, 0x3A, 0xED);

fn source_color(s: Source) -> Color32 {
    match s {
        Source::Salle => SALLE,
        Source::Distance => DISTANCE,
        Source::Mixte => BLEU_CLAIR,
    }
}

pub(super) fn transcript(ui: &mut Ui, m: &Meeting, follow: bool) {
    egui::ScrollArea::vertical().auto_shrink(false).stick_to_bottom(follow).show(ui, |ui| {
        let mut last: Option<Source> = None;
        for s in &m.segments {
            if last != Some(s.source) {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(clock(s.start_ms)).monospace().small().weak());
                    ui.label(RichText::new(s.source.label()).strong().color(source_color(s.source)));
                });
                last = Some(s.source);
            }
            ui.add(egui::Label::new(RichText::new(&s.text).size(15.0)).selectable(true).wrap());
        }
        ui.add_space(12.0);
    });
}

/// Rendu Markdown minimal pour le compte rendu : titres, listes, tableaux, **gras**.
pub(super) fn markdown(ui: &mut Ui, md: &str) {
    for line in md.lines() {
        let t = line.trim_start();
        if let Some(h) = t.strip_prefix("### ") {
            ui.add_space(4.0);
            ui.label(RichText::new(h.replace("**", "")).strong().size(15.0));
        } else if let Some(h) = t.strip_prefix("## ") {
            ui.add_space(6.0);
            ui.label(RichText::new(h.replace("**", "")).strong().size(17.0).color(BLEU_CLAIR));
        } else if let Some(h) = t.strip_prefix("# ") {
            ui.label(RichText::new(h.replace("**", "")).strong().size(20.0));
        } else if let Some(q) = t.strip_prefix("> ") {
            egui::Frame::new()
                .fill(Color32::from_rgb(0xD9, 0x77, 0x06).gamma_multiply(0.18))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(q.replace("**", "")).color(Color32::from_rgb(0xD9, 0x77, 0x06)));
                });
        } else if t.starts_with('|') {
            if !t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) {
                ui.label(RichText::new(t.replace("**", "")).monospace().size(12.5));
            }
        } else if t.is_empty() {
            ui.add_space(2.0);
        } else {
            let (bullet, body) = match t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
                Some(b) => ("•  ", b),
                None => ("", t),
            };
            let indent = (line.len() - t.len()) as f32 * 4.0;
            ui.horizontal_wrapped(|ui| {
                ui.add_space(indent);
                ui.spacing_mut().item_spacing.x = 0.0;
                if !bullet.is_empty() {
                    ui.label(bullet);
                }
                for (i, part) in body.split("**").enumerate() {
                    if !part.is_empty() {
                        let rt = RichText::new(part);
                        ui.label(if i % 2 == 1 { rt.strong() } else { rt });
                    }
                }
            });
        }
    }
}

pub(super) fn device_combo(ui: &mut Ui, id: &str, value: &mut Option<String>, devices: &[DeviceInfo], default: &str) {
    let current = value
        .as_ref()
        .and_then(|v| devices.iter().find(|d| &d.id == v))
        .map_or(default.to_string(), |d| d.name.clone());
    egui::ComboBox::from_id_salt(id).width(260.0).selected_text(truncate(&current, 34)).show_ui(ui, |ui| {
        ui.selectable_value(value, None, default);
        for d in devices {
            ui.selectable_value(value, Some(d.id.clone()), &d.name);
        }
    });
}

pub(super) fn meter(ui: &mut Ui, label: &str, level: f32, color: Color32) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(120.0, 8.0), Sense::hover());
        let p = ui.painter();
        p.rect_filled(rect, CornerRadius::same(4), ui.visuals().extreme_bg_color);
        // Échelle logarithmique : la parole normale remplit la moitié de la jauge.
        let db = 20.0 * level.max(1e-5).log10();
        let frac = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
        let mut fill = rect;
        fill.set_width(rect.width() * frac);
        p.rect_filled(fill, CornerRadius::same(4), color);
        ui.label(RichText::new(label).small());
    });
}

pub(super) fn record_button(ui: &mut Ui, recording: bool, enabled: bool) -> egui::Response {
    let text = if recording { "Arrêter" } else { "Démarrer" };
    let fill = if recording { ROUGE } else { BLEU };
    let btn = egui::Button::new(RichText::new(format!("     {text}")).color(Color32::WHITE).size(16.0))
        .fill(fill)
        .corner_radius(CornerRadius::same(18))
        .min_size(Vec2::new(140.0, 40.0));
    let resp = ui.add_enabled(enabled, btn);
    let c = Pos2::new(resp.rect.left() + 26.0, resp.rect.center().y);
    if recording {
        ui.painter().rect_filled(egui::Rect::from_center_size(c, Vec2::splat(12.0)), CornerRadius::same(2), Color32::WHITE);
    } else {
        ui.painter().circle_filled(c, 7.0, Color32::WHITE);
    }
    if !enabled {
        resp.on_disabled_hover_text("Installez ou chargez d'abord un modèle (Paramètres).")
    } else {
        resp
    }
}

pub(super) fn download_bar(ui: &mut Ui, dl: &Download) {
    let done = dl.progress.done.load(Ordering::Relaxed) as f32;
    let total = dl.progress.total.load(Ordering::Relaxed).max(1) as f32;
    ui.add(
        egui::ProgressBar::new(done / total)
            .text(format!("{} : {:.0} / {:.0} Mo", short_model(&dl.file), done / 1e6, total / 1e6))
            .desired_width(360.0),
    );
}

pub(super) fn short_model(file: &str) -> String {
    models::CATALOG
        .iter()
        .find(|m| m.file == file)
        .map_or_else(|| file.trim_start_matches("ggml-").trim_end_matches(".bin").to_string(), |m| m.label.to_string())
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n).collect::<String>()) }
}

/// Logo vectoriel de Tour de table (même dessin que `assets/logo.svg`) :
/// une table ronde vue de haut, huit participants autour et une onde sonore
/// au centre, sur fond bleu Québec.
pub fn draw_logo(p: &egui::Painter, rect: egui::Rect) {
    let s = rect.width() / 100.0;
    let at = |x: f32, y: f32| Pos2::new(rect.left() + x * s, rect.top() + y * s);
    p.rect_filled(rect, CornerRadius::same((22.0 * s) as u8), BLEU);
    // Participants
    for i in 0..8 {
        let a = (-90.0f32 + 22.5 + 45.0 * i as f32).to_radians();
        p.circle_filled(at(50.0 + 35.0 * a.cos(), 50.0 + 35.0 * a.sin()), 6.2 * s, Color32::WHITE);
    }
    // Table
    p.circle_filled(at(50.0, 50.0), 23.0 * s, Color32::WHITE);
    // Conversation
    for (i, h) in [10.0, 18.0, 26.0, 18.0, 10.0].iter().enumerate() {
        let r = egui::Rect::from_center_size(at(50.0 + (i as f32 - 2.0) * 6.4, 50.0), Vec2::new(4.2 * s, h * s));
        p.rect_filled(r, CornerRadius::same((2.1 * s) as u8), BLEU);
    }
}

pub(super) fn style(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = Vec2::new(8.0, 6.0);
        style.spacing.button_padding = Vec2::new(10.0, 5.0);
        style.visuals.selection.bg_fill = BLEU_CLAIR.gamma_multiply(0.45);
        style.visuals.hyperlink_color = BLEU_CLAIR;
    });
}
