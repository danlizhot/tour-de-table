//! Composants réutilisables : couleurs, transcription, Markdown, jauges, logo.

use eframe::egui::{self, Color32, CornerRadius, Pos2, RichText, Sense, Stroke, Ui, Vec2};

use super::apparence::{gras, initiales, palette, pastille, pastille_provisoire};

use crate::audio::{DeviceInfo, Source};
use crate::config::Theme;
use crate::models;
use crate::storage::{Meeting, clock};

/// Bleu du drapeau du Québec.
pub const BLEU: Color32 = Color32::from_rgb(0x00, 0x3D, 0xA5);
pub(super) const BLEU_CLAIR: Color32 = Color32::from_rgb(0x3B, 0x82, 0xF6);
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

/// Couleur d'une voix reconnue (tons moyens, lisibles en thème clair comme sombre), sinon de la source.
pub(super) fn couleur_locuteur(source: Source, voix: Option<u16>) -> Color32 {
    const VOIX: [Color32; 8] = [
        Color32::from_rgb(0x25, 0x63, 0xEB),
        Color32::from_rgb(0xD9, 0x77, 0x06),
        Color32::from_rgb(0x05, 0x96, 0x69),
        Color32::from_rgb(0xC0, 0x26, 0xD3),
        Color32::from_rgb(0xDC, 0x26, 0x26),
        Color32::from_rgb(0x08, 0x91, 0xB2),
        Color32::from_rgb(0x65, 0xA3, 0x0D),
        Color32::from_rgb(0x9D, 0x6B, 0x53),
    ];
    match voix {
        Some(v) => VOIX[v as usize % VOIX.len()],
        None => source_color(source),
    }
}

/// Écoute dans la transcription : boutons affichés, heure entendue (le paragraphe qui la contient est surligné et suivi
/// quand la lecture avance), et heure où faire défiler la transcription (« Voir dans la transcription »).
#[derive(Default, Clone, Copy)]
pub(super) struct EtatEcoute<'a> {
    pub boutons: bool,
    pub position: Option<u64>,
    /// La lecture avance (pas en pause) : la transcription suit le paragraphe entendu.
    pub suivre: bool,
    pub aller_a: Option<u64>,
    /// Heure où l'écoute vient de sauter (barre du lecteur, −5 s, +5 s) : son paragraphe vient au centre.
    pub saut: Option<u64>,
    /// Plages (ms) dont les paragraphes sont surlignés : le point choisi dans le compte rendu.
    pub surligne: &'a [(u64, u64)],
}

/// Clic sur le bouton d'un paragraphe de la transcription.
pub(super) enum ClicEcoute {
    Ecouter { debut_ms: u64 },
    Pause,
}

/// Une phrase de la transcription, brute ou nettoyée.
struct Phrase<'a> {
    debut_ms: u64,
    fin_ms: u64,
    source: Source,
    point: Option<usize>,
    texte: &'a str,
}

pub(super) fn transcript(ui: &mut Ui, m: &Meeting, follow: bool, ecoute: EtatEcoute<'_>) -> Option<ClicEcoute> {
    let decoupee = !m.periodes.is_empty();
    let phrases = m.segments.iter().map(|s| Phrase {
        debut_ms: s.start_ms,
        fin_ms: s.end_ms,
        source: s.source,
        point: if decoupee { m.point_de(s.start_ms) } else { None },
        texte: &s.text,
    });
    afficher_transcription(ui, m, phrases.collect(), follow, ecoute, None)
}

/// Transcription nettoyée, même présentation que `transcript`.
pub(super) fn transcript_nettoye(ui: &mut Ui, m: &Meeting, n: &crate::nettoyage::Nettoyage, ecoute: EtatEcoute<'_>) -> Option<ClicEcoute> {
    let phrases = n.phrases.iter().map(|p| Phrase { debut_ms: p.debut_ms, fin_ms: p.fin_ms, source: p.source, point: p.point, texte: &p.texte });
    let suite = (!n.termine()).then_some("La suite de la rencontre n'est pas encore nettoyée : voir la version brute.");
    afficher_transcription(ui, m, phrases.collect(), false, ecoute, suite)
}

/// Intertitre avant le paragraphe (changement de point), clé (source, voix), première et dernière phrase.
type Paragraphe = (Option<Option<usize>>, (Source, Option<u16>), usize, usize);

/// Paragraphes (même source et même voix), avec un intertitre à chaque changement de point une fois la rencontre
/// découpée.
fn afficher_transcription(ui: &mut Ui, m: &Meeting, phrases: Vec<Phrase>, follow: bool, ecoute: EtatEcoute<'_>, suite: Option<&str>) -> Option<ClicEcoute> {
    let mut paragraphes: Vec<Paragraphe> = Vec::new();
    for (i, p) in phrases.iter().enumerate() {
        let cle = (p.source, m.voix_entre(p.debut_ms, p.fin_ms));
        let intertitre = (!m.periodes.is_empty() && (i == 0 || p.point != phrases[i - 1].point)).then_some(p.point);
        match paragraphes.last_mut() {
            Some((_, c, _, fin)) if intertitre.is_none() && *c == cle => *fin = i,
            _ => paragraphes.push((intertitre, cle, i, i)),
        }
    }
    // Paragraphe vers lequel défiler : le premier qui finit après l'heure demandée.
    let cible = ecoute.aller_a.and_then(|ms| paragraphes.iter().position(|&(_, _, _, f)| phrases[f].fin_ms > ms));
    let mut clic = None;
    egui::ScrollArea::vertical().auto_shrink(false).stick_to_bottom(follow).show(ui, |ui| {
        let pal = palette(ui);
        for (k, &(intertitre, cle, d, f)) in paragraphes.iter().enumerate() {
            let mut reperes = Vec::new();
            if let Some(point) = intertitre {
                ui.add_space(if k == 0 { 8.0 } else { 20.0 });
                reperes.push(intertitre_point(ui, m, point, phrases[d].debut_ms));
                ui.add_space(4.0);
            }
            let (debut_ms, fin_ms) = (phrases[d].debut_ms, phrases[f].fin_ms);
            let fin_suivante = paragraphes.get(k + 1).map_or(u64::MAX, |&(_, _, d2, _)| phrases[d2].debut_ms);
            let entendu = ecoute.position.is_some_and(|ms| debut_ms <= ms && ms < fin_suivante.max(fin_ms));
            let joue = entendu && ecoute.suivre;
            let surligne = entendu || ecoute.surligne.iter().any(|&(d, f)| debut_ms >= d && debut_ms < f);
            let (fond, contour) = if surligne { (pal.accent_doux, Stroke::new(1.0, pal.accent.gamma_multiply(0.35))) } else { (Color32::TRANSPARENT, Stroke::NONE) };
            egui::Frame::new()
                .fill(fond)
                .stroke(contour)
                .corner_radius(CornerRadius::same(12))
                .inner_margin(egui::Margin { left: 10, right: 12, top: 9, bottom: 9 })
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_top(|ui| {
                        let locuteur = m.locuteur(cle.0, cle.1);
                        let couleur = lisible(ui, couleur_locuteur(cle.0, cle.1));
                        let lettres = match (cle.1, cle.0) {
                            (Some(_), _) => initiales(&locuteur),
                            (None, Source::Salle) => "S".into(),
                            (None, Source::Distance) => "D".into(),
                            (None, Source::Mixte) => "·".into(),
                        };
                        if cle.1.is_some() && m.voix.as_ref().is_some_and(|v| v.provisoire) {
                            pastille_provisoire(ui, &lettres, couleur_locuteur(cle.0, cle.1), 34.0);
                        } else {
                            pastille(ui, &lettres, couleur_locuteur(cle.0, cle.1), 34.0);
                        }
                        ui.add_space(4.0);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 3.0;
                            let entete = ui.horizontal(|ui| {
                                ui.label(gras(&locuteur, 14.0).color(couleur));
                                ui.label(RichText::new(clock(debut_ms)).size(12.0).color(pal.texte2));
                                if ecoute.boutons {
                                    let aide = if joue { "Pause" } else { "Écouter à partir d'ici" };
                                    if bouton_lecture(ui, joue).on_hover_text(aide).clicked() {
                                        clic = Some(if joue { ClicEcoute::Pause } else { ClicEcoute::Ecouter { debut_ms } });
                                    }
                                }
                            });
                            reperes.push(entete.response);
                            for p in &phrases[d..=f] {
                                ui.add(egui::Label::new(RichText::new(p.texte).size(15.0).color(pal.texte)).selectable(true).wrap());
                            }
                        });
                    });
                });
            if cible == Some(k) {
                reperes[0].scroll_to_me(Some(egui::Align::TOP));
            }
            if ecoute.saut.is_some_and(|ms| debut_ms <= ms && ms < fin_suivante.max(fin_ms)) {
                ui.data_mut(|d| d.insert_temp(egui::Id::unique(("suivi", &m.id)), debut_ms));
                reperes[0].scroll_to_me(Some(egui::Align::Center));
            } else if joue {
                // Le paragraphe entendu change pendant la lecture : la transcription le suit.
                let id = egui::Id::unique(("suivi", &m.id));
                if ui.data(|d| d.get_temp::<u64>(id)) != Some(debut_ms) {
                    ui.data_mut(|d| d.insert_temp(id, debut_ms));
                    reperes[0].scroll_to_me(Some(egui::Align::Center));
                }
            }
        }
        if let Some(texte) = suite {
            ui.add_space(8.0);
            ui.label(RichText::new(texte).weak().italics());
        }
        ui.add_space(12.0);
    });
    clic
}

/// Intertitre d'un point dans la transcription : numéro dans une pastille carrée, titre, heure de début.
fn intertitre_point(ui: &mut Ui, m: &Meeting, point: Option<usize>, debut_ms: u64) -> egui::Response {
    let pal = palette(ui);
    let texte = m.intertitre(point);
    let (numero, titre) = match point {
        Some(_) => texte.split_once(". ").map_or((String::new(), texte.clone()), |(n, t)| (n.to_string(), t.to_string())),
        None => ("–".to_string(), texte.clone()),
    };
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::hover());
        let (fond, encre) = if point.is_some() { (pal.accent_doux, pal.accent_texte) } else { (pal.piste, pal.texte2) };
        ui.painter().rect_filled(rect, CornerRadius::same(8), fond);
        let police = egui::FontId::new(if numero.chars().count() > 2 { 11.0 } else { 13.0 }, egui::FontFamily::Name("gras".into()));
        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, &numero, police, encre);
        ui.add_space(4.0);
        ui.label(gras(titre, 17.0).color(pal.texte));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(clock(debut_ms)).size(12.0).color(pal.texte2));
        });
    })
    .response
}

/// Couleur d'une voix assez claire pour être lue sur le fond foncé.
pub(super) fn lisible(ui: &Ui, couleur: Color32) -> Color32 {
    if ui.visuals().dark_mode { couleur.lerp_to_gamma(Color32::WHITE, 0.3) } else { couleur }
}

/// Petit bouton rond : triangle « écouter », ou carré « arrêter » pendant la lecture (les polices n'ont pas
/// toujours ces symboles).
pub(super) fn bouton_lecture(ui: &mut Ui, joue: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    if ui.is_rect_visible(rect) {
        let pal = palette(ui);
        let (fond, couleur) = match (joue, resp.hovered()) {
            (true, _) => (pal.accent_plein, Color32::WHITE),
            (false, true) => (pal.bordure_forte, pal.texte),
            (false, false) => (pal.piste, pal.texte),
        };
        ui.painter().circle_filled(rect.center(), 12.0, fond);
        let c = rect.center();
        if joue {
            for dx in [-2.5, 2.5] {
                ui.painter().rect_filled(egui::Rect::from_center_size(c + Vec2::new(dx, 0.0), Vec2::new(2.6, 9.0)), 1, couleur);
            }
        } else {
            let pts = vec![c + Vec2::new(-2.5, -5.0), c + Vec2::new(5.5, 0.0), c + Vec2::new(-2.5, 5.0)];
            ui.painter().add(egui::Shape::convex_polygon(pts, couleur, egui::Stroke::NONE));
        }
    }
    resp
}

/// Rendu Markdown minimal pour le compte rendu : titres, listes, tableaux, **gras**.
/// `titre` ajoute des boutons au bout de chaque titre de section (« ## »), qu'il reçoit.
pub(super) fn markdown(ui: &mut Ui, md: &str, titre: &mut dyn FnMut(&mut Ui, &str)) {
    let pal = palette(ui);
    ui.style_mut().override_text_style = None;
    ui.visuals_mut().override_text_color = Some(pal.texte);
    for line in md.lines() {
        let t = line.trim_start();
        if let Some(h) = t.strip_prefix("### ") {
            ui.add_space(4.0);
            ui.label(gras(h.replace("**", ""), 15.0).color(pal.texte));
        } else if let Some(h) = t.strip_prefix("## ") {
            ui.add_space(14.0);
            let h = h.replace("**", "");
            ui.horizontal_wrapped(|ui| {
                // « 4. Rapport financier » : le numéro dans une pastille carrée, comme dans la transcription.
                match h.split_once(". ").filter(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit() || c == '.')) {
                    Some((n, titre)) => {
                        let (rect, _) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::hover());
                        ui.painter().rect_filled(rect, CornerRadius::same(8), pal.accent_doux);
                        let police = egui::FontId::new(if n.chars().count() > 2 { 11.0 } else { 13.0 }, egui::FontFamily::Name("gras".into()));
                        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, n, police, pal.accent_texte);
                        ui.add_space(2.0);
                        ui.label(gras(titre, 17.0).color(pal.texte));
                    }
                    None => {
                        ui.label(gras(&h, 17.0).color(pal.texte));
                    }
                }
                ui.add_space(6.0);
                titre(ui, &h);
            });
            ui.add_space(2.0);
        } else if let Some(h) = t.strip_prefix("# ") {
            ui.label(gras(h.replace("**", ""), 22.0).color(pal.texte));
        } else if let Some(q) = t.strip_prefix("> ") {
            egui::Frame::new()
                .fill(Color32::from_rgb(0xD9, 0x77, 0x06).gamma_multiply(0.18))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    // « > - Point 5 : 1 340,02 » : une puce de l'avertissement « À vérifier ».
                    let q = q.strip_prefix("- ").map_or_else(|| q.to_string(), |r| format!("• {r}"));
                    ui.label(RichText::new(q.replace("**", "")).color(Color32::from_rgb(0xD9, 0x77, 0x06)));
                });
        } else if t.starts_with('|') {
            if !t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) {
                ui.label(RichText::new(t.replace("**", "")).monospace().size(12.5));
            }
        } else if let Some(note) = t.strip_prefix('*').and_then(|n| n.strip_suffix('*')).filter(|n| !n.starts_with('*') && !n.is_empty()) {
            ui.label(RichText::new(note).italics().weak());
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

/// Bouton carré du thème, avec son icône dessinée (la police par défaut d'egui n'a pas ces
/// symboles) : soleil, lune ou écran pour le système.
pub(super) fn theme_button(ui: &mut Ui, theme: Theme) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::click());
    let v = ui.style().interact(&resp);
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(7), v.weak_bg_fill);
    let c = rect.center();
    let trait_ = egui::Stroke::new(1.5, v.fg_stroke.color);
    match theme {
        Theme::Clair => {
            p.circle_stroke(c, 4.0, trait_);
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::FRAC_PI_4;
                let d = Vec2::new(a.cos(), a.sin());
                p.line_segment([c + d * 6.5, c + d * 9.0], trait_);
            }
        }
        Theme::Fonce => {
            // Croissant : un disque, entamé par un disque de la couleur du bouton.
            p.circle_filled(c, 7.5, v.fg_stroke.color);
            p.circle_filled(c + Vec2::new(4.0, -3.5), 6.5, v.weak_bg_fill);
        }
        Theme::Systeme => {
            let ecran = egui::Rect::from_center_size(c - Vec2::new(0.0, 2.0), Vec2::new(16.0, 11.0));
            p.rect_stroke(ecran, CornerRadius::same(2), trait_, egui::StrokeKind::Middle);
            p.line_segment([c + Vec2::new(0.0, 3.5), c + Vec2::new(0.0, 6.5)], trait_);
            p.line_segment([c + Vec2::new(-4.0, 7.0), c + Vec2::new(4.0, 7.0)], trait_);
        }
    }
    let nom = match theme {
        Theme::Clair => "clair",
        Theme::Fonce => "foncé",
        Theme::Systeme => "celui du système",
    };
    resp.on_hover_text(format!("Thème : {nom}. Cliquer pour changer."))
}

pub(super) fn short_model(file: &str) -> String {
    Some(&models::WHISPER)
        .filter(|m| m.file == file)
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
