//! Composants réutilisables : couleurs, transcription, Markdown, jauges, logo.

use eframe::egui::{self, Color32, CornerRadius, Pos2, RichText, Sense, Ui, Vec2};

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

/// Écoute dans la transcription : boutons affichés, paragraphe en cours de lecture (son début), et heure où faire
/// défiler la transcription (« Voir dans la transcription » d'un point du compte rendu).
#[derive(Default, Clone, Copy)]
pub(super) struct EtatEcoute<'a> {
    pub boutons: bool,
    pub en_lecture: Option<u64>,
    pub aller_a: Option<u64>,
    /// Plages (ms) dont les paragraphes sont surlignés : le point choisi dans le compte rendu, ou écouté.
    pub surligne: &'a [(u64, u64)],
}

/// Clic sur le bouton d'un paragraphe de la transcription.
pub(super) enum ClicEcoute {
    Ecouter { debut_ms: u64, fin_ms: u64, libelle: String },
    Arreter,
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
        for (k, &(intertitre, cle, d, f)) in paragraphes.iter().enumerate() {
            let mut reperes = Vec::new();
            if let Some(point) = intertitre {
                ui.add_space(12.0);
                reperes.push(ui.label(RichText::new(m.intertitre(point)).strong().size(16.5).color(if point.is_some() { BLEU_CLAIR } else { Color32::GRAY })));
                ui.separator();
            }
            let (debut_ms, fin_ms) = (phrases[d].debut_ms, phrases[f].fin_ms);
            let joue = ecoute.en_lecture == Some(debut_ms);
            let surligne = joue || ecoute.surligne.iter().any(|&(d, f)| debut_ms >= d && debut_ms < f);
            ui.add_space(8.0);
            let fond = if surligne { BLEU_CLAIR.gamma_multiply(0.14) } else { Color32::TRANSPARENT };
            egui::Frame::new().fill(fond).corner_radius(CornerRadius::same(6)).inner_margin(egui::Margin::symmetric(4, 2)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let entete = ui.horizontal(|ui| {
                    ui.label(RichText::new(clock(debut_ms)).monospace().small().weak());
                    let locuteur = m.locuteur(cle.0, cle.1);
                    ui.label(RichText::new(&locuteur).strong().color(couleur_locuteur(cle.0, cle.1)));
                    if ecoute.boutons {
                        let aide = if joue { "Arrêter l'écoute" } else { "Entendre ce passage" };
                        if bouton_lecture(ui, joue).on_hover_text(aide).clicked() {
                            clic = Some(if joue {
                                ClicEcoute::Arreter
                            } else {
                                ClicEcoute::Ecouter { debut_ms, fin_ms, libelle: format!("{locuteur} · {}", clock(debut_ms)) }
                            });
                        }
                    }
                });
                reperes.push(entete.response);
                for p in &phrases[d..=f] {
                    ui.add(egui::Label::new(RichText::new(p.texte).size(15.0)).selectable(true).wrap());
                }
            });
            if cible == Some(k) {
                reperes[0].scroll_to_me(Some(egui::Align::TOP));
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

/// Petit bouton rond : triangle « écouter », ou carré « arrêter » pendant la lecture (les polices n'ont pas
/// toujours ces symboles).
pub(super) fn bouton_lecture(ui: &mut Ui, joue: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    if ui.is_rect_visible(rect) {
        let visuel = ui.style().interact(&resp);
        let fond = if resp.hovered() || joue { BLEU_CLAIR.gamma_multiply(0.25) } else { visuel.weak_bg_fill };
        ui.painter().circle_filled(rect.center(), 10.0, fond);
        let c = rect.center();
        let couleur = if joue { BLEU_CLAIR } else { visuel.text_color() };
        if joue {
            ui.painter().rect_filled(egui::Rect::from_center_size(c, Vec2::splat(8.0)), 1, couleur);
        } else {
            let pts = vec![c + Vec2::new(-3.0, -5.0), c + Vec2::new(5.0, 0.0), c + Vec2::new(-3.0, 5.0)];
            ui.painter().add(egui::Shape::convex_polygon(pts, couleur, egui::Stroke::NONE));
        }
    }
    resp
}

/// Rendu Markdown minimal pour le compte rendu : titres, listes, tableaux, **gras**.
/// `titre` ajoute des boutons au bout de chaque titre de section (« ## »), qu'il reçoit.
pub(super) fn markdown(ui: &mut Ui, md: &str, titre: &mut dyn FnMut(&mut Ui, &str)) {
    for line in md.lines() {
        let t = line.trim_start();
        if let Some(h) = t.strip_prefix("### ") {
            ui.add_space(4.0);
            ui.label(RichText::new(h.replace("**", "")).strong().size(15.0));
        } else if let Some(h) = t.strip_prefix("## ") {
            ui.add_space(6.0);
            let h = h.replace("**", "");
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(&h).strong().size(17.0).color(BLEU_CLAIR));
                titre(ui, &h);
            });
        } else if let Some(h) = t.strip_prefix("# ") {
            ui.label(RichText::new(h.replace("**", "")).strong().size(20.0));
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
        resp.on_disabled_hover_text("Le modèle de reconnaissance vocale n'est pas encore prêt.")
    } else {
        resp
    }
}

/// Pause ou reprise de l'enregistrement, à côté de « Arrêter » ; icône dessinée comme celle
/// de `record_button` : deux barres, ou le triangle de lecture pendant la pause.
pub(super) fn pause_button(ui: &mut Ui, en_pause: bool) -> egui::Response {
    let text = if en_pause { "Reprendre" } else { "Pause" };
    let btn = egui::Button::new(RichText::new(format!("     {text}")).size(16.0))
        .corner_radius(CornerRadius::same(18))
        .min_size(Vec2::new(130.0, 40.0));
    let resp = ui.add(btn);
    let couleur = ui.style().interact(&resp).fg_stroke.color;
    let c = Pos2::new(resp.rect.left() + 26.0, resp.rect.center().y);
    if en_pause {
        let points = vec![c + Vec2::new(-5.0, -7.0), c + Vec2::new(7.0, 0.0), c + Vec2::new(-5.0, 7.0)];
        ui.painter().add(egui::Shape::convex_polygon(points, couleur, egui::Stroke::NONE));
    } else {
        for dx in [-3.5, 3.5] {
            let barre = egui::Rect::from_center_size(c + Vec2::new(dx, 0.0), Vec2::new(4.0, 14.0));
            ui.painter().rect_filled(barre, CornerRadius::same(1), couleur);
        }
    }
    resp.on_hover_text(if en_pause {
        "Reprendre la transcription"
    } else {
        "Rien n'est capté pendant la pause : ni transcrit, ni conservé dans l'audio"
    })
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

pub(super) fn style(ctx: &egui::Context) {
    police_du_systeme(ctx);
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = Vec2::new(8.0, 6.0);
        style.spacing.button_padding = Vec2::new(10.0, 5.0);
        style.visuals.selection.bg_fill = BLEU_CLAIR.gamma_multiply(0.45);
        style.visuals.hyperlink_color = BLEU_CLAIR;
    });
    // Thème clair : texte bleu foncé, et texte secondaire assez contrasté pour être lu (le gris par défaut,
    // atténué, se lisait mal).
    ctx.style_mut_of(egui::Theme::Light, |style| {
        let v = &mut style.visuals;
        v.widgets.noninteractive.fg_stroke.color = BLEU_FONCE;
        v.widgets.inactive.fg_stroke.color = BLEU_FONCE;
        v.widgets.active.fg_stroke.color = BLEU_NUIT;
        v.weak_text_color = Some(BLEU_GRIS);
    });
}

/// Texte du thème clair.
const BLEU_FONCE: Color32 = Color32::from_rgb(0x1B, 0x2A, 0x4A);
/// Texte en évidence du thème clair.
const BLEU_NUIT: Color32 = Color32::from_rgb(0x0B, 0x16, 0x30);
/// Texte secondaire du thème clair (contraste d'environ 6,5:1 sur blanc).
const BLEU_GRIS: Color32 = Color32::from_rgb(0x4A, 0x58, 0x75);

/// Police d'interface du système (Segoe UI sous Windows), plus lisible que la police intégrée d'egui, très mince
/// (Ubuntu Light). Rien n'est livré ni téléchargé : sans police trouvée, la police intégrée reste.
fn police_du_systeme(ctx: &egui::Context) {
    let windows = std::env::var_os("WINDIR").map(|w| std::path::PathBuf::from(w).join("Fonts/segoeui.ttf"));
    let candidates = windows.into_iter().chain(
        [
            "/System/Library/Fonts/Supplemental/Arial.ttf",
            "/Library/Fonts/Arial.ttf",
            "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        ]
        .map(std::path::PathBuf::from),
    );
    let Some((chemin, octets)) = candidates.filter_map(|c| std::fs::read(&c).ok().map(|o| (c, o))).next() else {
        log::info!("Aucune police du système trouvée : police intégrée");
        return;
    };
    log::info!("Police : {}", chemin.display());
    let mut polices = egui::FontDefinitions::default();
    polices.font_data.insert("systeme".into(), std::sync::Arc::new(egui::FontData::from_owned(octets)));
    if let Some(famille) = polices.families.get_mut(&egui::FontFamily::Proportional) {
        famille.insert(0, "systeme".into());
    }
    ctx.set_fonts(polices);
}
