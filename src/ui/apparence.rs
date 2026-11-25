//! Apparence : palette des thèmes clair et foncé, police Inter, style d'egui et composants de base (boutons, cartes,
//! choix segmentés, pastilles). Les écrans n'emploient que ces couleurs, pour que les deux thèmes restent lisibles.

use eframe::egui::{self, Color32, CornerRadius, FontFamily, Margin, RichText, Sense, Stroke, Ui, Vec2};

/// Couleurs d'un thème.
pub(super) struct Palette {
    /// Fond de la fenêtre, derrière les cartes.
    pub fond: Color32,
    /// Cartes, colonne de gauche, fenêtres.
    pub surface: Color32,
    pub bordure: Color32,
    /// Contour des boutons et des champs.
    pub bordure_forte: Color32,
    /// Fond des choix segmentés, des pistes de jauge, du survol.
    pub piste: Color32,
    pub texte: Color32,
    /// Texte secondaire (contraste d'au moins 4,5:1 sur le fond et sur les cartes).
    pub texte2: Color32,
    pub accent: Color32,
    /// Fond des boutons principaux (texte blanc lisible dans les deux thèmes).
    pub accent_plein: Color32,
    /// Fond d'un élément choisi ou en cours.
    pub accent_doux: Color32,
    /// Texte sur `accent_doux`.
    pub accent_texte: Color32,
    pub rouge: Color32,
    pub rouge_doux: Color32,
    pub rouge_texte: Color32,
    pub vert: Color32,
    pub vert_doux: Color32,
    /// Ambre franc : travail en cours (transcription d'un import, fin d'une transcription).
    pub ambre: Color32,
    pub ambre_doux: Color32,
    pub ambre_bord: Color32,
    pub ambre_texte: Color32,
}

const fn rgb(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

pub(super) const CLAIR: Palette = Palette {
    fond: rgb(0xF4F6F9),
    surface: rgb(0xFFFFFF),
    bordure: rgb(0xE3E7EE),
    bordure_forte: rgb(0xD5DBE5),
    piste: rgb(0xEAEEF3),
    texte: rgb(0x1B2230),
    texte2: rgb(0x5B6474),
    accent: rgb(0x2453C9),
    accent_plein: rgb(0x2453C9),
    accent_doux: rgb(0xEAF0FF),
    accent_texte: rgb(0x1A3E9C),
    rouge: rgb(0xC81E2D),
    rouge_doux: rgb(0xFDECEE),
    rouge_texte: rgb(0x8A1C26),
    vert: rgb(0x0F7A4E),
    vert_doux: rgb(0xE6F5EE),
    ambre: rgb(0xD98A00),
    ambre_doux: rgb(0xFFF7E8),
    ambre_bord: rgb(0xF3D9A4),
    ambre_texte: rgb(0x6B3F00),
};

pub(super) const FONCE: Palette = Palette {
    fond: rgb(0x10151F),
    surface: rgb(0x19202C),
    bordure: rgb(0x29323F),
    bordure_forte: rgb(0x3A4556),
    piste: rgb(0x242C3A),
    texte: rgb(0xE6EAF2),
    texte2: rgb(0x9EA9BB),
    accent: rgb(0x5B8CFF),
    accent_plein: rgb(0x3563D6),
    accent_doux: rgb(0x1E2B48),
    accent_texte: rgb(0xB4C8FF),
    rouge: rgb(0xE5484D),
    rouge_doux: rgb(0x3B1D23),
    rouge_texte: rgb(0xFFB3B6),
    vert: rgb(0x3CCB87),
    vert_doux: rgb(0x16302A),
    ambre: rgb(0xF0B44C),
    ambre_doux: rgb(0x33280F),
    ambre_bord: rgb(0x5C4718),
    ambre_texte: rgb(0xF5CD7A),
};

/// Palette du thème affiché.
pub(super) fn palette(ui: &Ui) -> &'static Palette {
    palette_de(ui.ctx())
}

pub(super) fn palette_de(ctx: &egui::Context) -> &'static Palette {
    if ctx.theme() == egui::Theme::Dark { &FONCE } else { &CLAIR }
}

/// Famille des titres et des libellés en évidence (police grasse du système, sinon la police ordinaire).
const GRAS: &str = "gras";

/// Texte gras de la taille donnée.
pub(super) fn gras(texte: impl Into<String>, taille: f32) -> RichText {
    RichText::new(texte).family(FontFamily::Name(GRAS.into())).size(taille)
}

pub(super) fn style(ctx: &egui::Context) {
    polices(ctx);
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = Vec2::new(8.0, 6.0);
        style.spacing.button_padding = Vec2::new(12.0, 6.0);
        style.spacing.interact_size.y = 26.0;
    });
    for (theme, p) in [(egui::Theme::Light, &CLAIR), (egui::Theme::Dark, &FONCE)] {
        ctx.style_mut_of(theme, |style| {
            let v = &mut style.visuals;
            v.panel_fill = p.fond;
            v.window_fill = p.surface;
            v.window_stroke = Stroke::new(1.0, p.bordure);
            v.window_corner_radius = CornerRadius::same(16);
            v.menu_corner_radius = CornerRadius::same(10);
            v.extreme_bg_color = if v.dark_mode { rgb(0x141A25) } else { rgb(0xFAFBFC) };
            v.faint_bg_color = p.piste;
            v.code_bg_color = p.piste;
            v.weak_text_color = Some(p.texte2);
            v.hyperlink_color = p.accent;
            v.selection.bg_fill = p.accent_doux;
            v.selection.stroke = Stroke::new(1.0, p.accent_texte);
            let w = &mut v.widgets;
            w.noninteractive.bg_stroke = Stroke::new(1.0, p.bordure);
            w.noninteractive.fg_stroke = Stroke::new(1.0, p.texte);
            w.noninteractive.corner_radius = CornerRadius::same(10);
            for (etat, fond) in [(&mut w.inactive, p.surface), (&mut w.hovered, p.piste), (&mut w.active, p.bordure_forte), (&mut w.open, p.piste)] {
                etat.weak_bg_fill = fond;
                etat.bg_fill = p.piste;
                etat.bg_stroke = Stroke::new(1.0, p.bordure_forte);
                etat.fg_stroke = Stroke::new(1.0, p.texte);
                etat.corner_radius = CornerRadius::same(10);
                etat.expansion = 0.0;
            }
        });
    }
}

/// Police Inter (licence OFL, `assets/polices`), intégrée à l'exécutable : même allure sur tous les systèmes. Les
/// polices intégrées d'egui restent derrière pour les caractères qu'Inter n'a pas.
fn polices(ctx: &egui::Context) {
    let mut defs = egui::FontDefinitions::default();
    defs.font_data.insert("inter".into(), std::sync::Arc::new(egui::FontData::from_static(include_bytes!("../../assets/polices/Inter-Regular.ttf"))));
    defs.font_data.insert("inter-gras".into(), std::sync::Arc::new(egui::FontData::from_static(include_bytes!("../../assets/polices/Inter-SemiBold.ttf"))));
    let mut gras = defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    if let Some(f) = defs.families.get_mut(&FontFamily::Proportional) {
        f.insert(0, "inter".into());
    }
    gras.insert(0, "inter-gras".into());
    defs.families.insert(FontFamily::Name(GRAS.into()), gras);
    ctx.set_fonts(defs);
}

/// Genre de bouton.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Genre {
    /// L'action attendue de l'écran : fond bleu.
    Principal,
    /// Contour gris sur fond de carte.
    Secondaire,
    /// Arrêter, supprimer : fond rouge.
    Danger,
    /// Sans fond ni contour, pour les actions rares.
    Discret,
}

/// Bouton de hauteur `hauteur` (36 px d'ordinaire), à ajouter avec `ui.add`.
pub(super) fn bouton_widget<'a>(ui: &Ui, texte: impl Into<String>, genre: Genre, hauteur: f32) -> egui::Button<'a> {
    let p = palette(ui);
    let taille = if hauteur >= 40.0 { 15.0 } else { 14.0 };
    let texte = texte.into();
    let (contenu, fond, trait_) = match genre {
        Genre::Principal => (gras(texte, taille).color(Color32::WHITE), p.accent_plein, Stroke::NONE),
        Genre::Danger => (gras(texte, taille).color(Color32::WHITE), p.rouge, Stroke::NONE),
        Genre::Secondaire => (RichText::new(texte).size(taille).color(p.texte), p.surface, Stroke::new(1.0, p.bordure_forte)),
        Genre::Discret => (RichText::new(texte).size(taille).color(p.texte), Color32::TRANSPARENT, Stroke::NONE),
    };
    egui::Button::new(contenu).fill(fond).stroke(trait_).corner_radius(CornerRadius::same(10)).min_size(Vec2::new(0.0, hauteur))
}

/// Cadre d'une carte : fond de surface, contour fin, coins arrondis.
pub(super) fn carte(ui: &Ui) -> egui::Frame {
    let p = palette(ui);
    egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.bordure))
        .corner_radius(CornerRadius::same(16))
        .inner_margin(Margin::same(18))
}

/// Choix segmenté (onglets, Brute / Nettoyée…) : vrai si la valeur a changé.
pub(super) fn segmente<T: PartialEq + Copy>(ui: &mut Ui, valeur: &mut T, choix: &[(T, &str)], hauteur: f32) -> bool {
    let p = palette(ui);
    let mut change = false;
    egui::Frame::new().fill(p.piste).corner_radius(CornerRadius::same(12)).inner_margin(Margin::same(4)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for &(v, nom) in choix {
                let choisi = *valeur == v;
                let texte = if choisi { gras(nom, 14.0).color(p.texte) } else { RichText::new(nom).size(14.0).color(p.texte2) };
                let b = egui::Button::new(texte)
                    .fill(if choisi { p.surface } else { Color32::TRANSPARENT })
                    .stroke(Stroke::NONE)
                    .corner_radius(CornerRadius::same(9))
                    .min_size(Vec2::new(0.0, hauteur));
                if ui.add(b).clicked() && !choisi {
                    *valeur = v;
                    change = true;
                }
            }
        });
    });
    change
}

/// Pastille ronde aux initiales, dans la couleur de la voix.
pub(super) fn pastille(ui: &mut Ui, initiales: &str, couleur: Color32, diametre: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(diametre), Sense::hover());
    if ui.is_rect_visible(rect) {
        let fond = if ui.visuals().dark_mode { couleur.gamma_multiply(0.28) } else { couleur.gamma_multiply(0.14) };
        let painter = ui.painter();
        painter.circle_filled(rect.center(), diametre / 2.0, fond);
        let texte = if ui.visuals().dark_mode { couleur.lerp_to_gamma(Color32::WHITE, 0.35) } else { couleur };
        painter.text(rect.center(), egui::Align2::CENTER_CENTER, initiales, egui::FontId::new(diametre * 0.38, FontFamily::Name(GRAS.into())), texte);
    }
    resp
}

/// Initiales d'un nom : « Hélène Brassard » → « HB », « Marc-André Pelletier » → « MP », « Voix 3 » → « V3 ».
pub(super) fn initiales(nom: &str) -> String {
    let nom = nom.split('(').next().unwrap_or(nom);
    let mots: Vec<&str> = nom.split_whitespace().collect();
    match mots.as_slice() {
        [] => "?".into(),
        [seul] => seul.chars().take(2).collect::<String>().to_uppercase(),
        [premier, .., dernier] => {
            let a = premier.chars().next().unwrap_or('?');
            let b = if dernier.chars().all(|c| c.is_ascii_digit()) { dernier.to_string() } else { dernier.chars().take(1).collect() };
            format!("{a}{b}").to_uppercase()
        }
    }
}

/// Libellé de section en petites capitales : « HISTORIQUE ».
pub(super) fn etiquette_section(ui: &mut Ui, texte: &str) {
    ui.label(gras(texte.to_uppercase(), 11.5).color(palette(ui).texte2).extra_letter_spacing(0.7));
}

const MOIS_COURTS: [&str; 12] = ["janv.", "févr.", "mars", "avr.", "mai", "juin", "juill.", "août", "sept.", "oct.", "nov.", "déc."];

/// « 14 oct. 2026 ».
pub(super) fn date_courte(d: &chrono::DateTime<chrono::Local>) -> String {
    use chrono::Datelike;
    format!("{} {} {}", d.day(), MOIS_COURTS[d.month0() as usize], d.year())
}

/// « 1 h 13 min », « 45 min ».
pub(super) fn duree_longue(secondes: u64) -> String {
    let minutes = (secondes + 30) / 60;
    if minutes < 60 { duree_courte(secondes) } else { format!("{} h {} min", minutes / 60, minutes % 60) }
}

/// Petit chevron vers le bas au bout d'un bouton de menu.
pub(super) fn chevron(ui: &Ui, bouton: &egui::Response, couleur: Color32) {
    let c = egui::pos2(bouton.rect.right() - 18.0, bouton.rect.center().y + 1.0);
    let t = Stroke::new(1.8, couleur);
    ui.painter().line_segment([c + Vec2::new(-4.0, -2.5), c], t);
    ui.painter().line_segment([c, c + Vec2::new(4.0, -2.5)], t);
}

/// « 1 h 12 », « 45 min », « moins d'une minute ».
pub(super) fn duree_courte(secondes: u64) -> String {
    let minutes = (secondes + 30) / 60;
    match minutes {
        0 => "moins d'une minute".into(),
        1..60 => format!("{minutes} min"),
        _ => format!("{} h {:02}", minutes / 60, minutes % 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initiales_des_noms() {
        assert_eq!(initiales("Hélène Brassard"), "HB");
        assert_eq!(initiales("Marc-André Pelletier"), "MP");
        assert_eq!(initiales("Nadine Roy (à distance)"), "NR");
        assert_eq!(initiales("Voix 3"), "V3");
        assert_eq!(initiales("Salle"), "SA");
        assert_eq!(initiales(""), "?");
    }

    #[test]
    fn durees_courtes() {
        assert_eq!(duree_courte(20), "moins d'une minute");
        assert_eq!(duree_courte(45 * 60 + 10), "45 min");
        assert_eq!(duree_courte(72 * 60 + 46), "1 h 13");
        assert_eq!(duree_courte(3600), "1 h 00");
    }
}
