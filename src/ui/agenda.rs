//! Ordre du jour : dépôt, saisie et modification, avant ou après la rencontre ; liste des points
//! et découpage de la rencontre par point.

use eframe::egui::{self, Align, Color32, CornerRadius, Key, Layout, Modifiers, RichText, Ui};

use super::widgets::{BLEU, SALLE};
use super::{ModelState, TourDeTableApp, View};
use crate::agenda::{Point, renumeroter};
use crate::storage::{Agenda, Declenchement, Meeting, clock};

/// Modification de la liste des points dans l'éditeur.
enum Edition {
    Ajouter,
    Supprimer(usize),
    /// Échange le point avec le précédent.
    Monter(usize),
    /// Relit les points dans le texte de l'ordre du jour.
    Analyser,
}

impl TourDeTableApp {
    /// Ordre du jour visé par un dépôt : la rencontre affichée, sinon la prochaine.
    fn agenda_slot(&mut self) -> &mut Option<Agenda> {
        match self.view {
            View::Past(i) if i < self.meetings.len() => &mut self.meetings[i].agenda,
            _ => match self.live.as_mut() {
                Some(m) => &mut m.agenda,
                None => &mut self.draft_agenda,
            },
        }
    }

    /// Rencontre à laquelle appartient l'ordre du jour affiché (aucune avant le démarrage).
    fn agenda_meeting(&self) -> Option<&Meeting> {
        match self.view {
            View::Past(i) => self.meetings.get(i),
            View::Live => self.live.as_ref(),
        }
    }

    fn agenda_meeting_mut(&mut self) -> Option<&mut Meeting> {
        match self.view {
            View::Past(i) => self.meetings.get_mut(i),
            View::Live => self.live.as_mut(),
        }
    }

    pub(super) fn attach_agenda(&mut self, path: &std::path::Path) {
        match crate::agenda::read(path) {
            Ok(text) => {
                let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                // Les périodes d'un ordre du jour remplacé ne désignent plus les mêmes points.
                if let Some(m) = self.agenda_meeting_mut() {
                    m.renumeroter_periodes(|_| None);
                }
                *self.agenda_slot() = Some(Agenda::new(file.clone(), text));
                self.agenda_changed();
                self.notify(format!("Ordre du jour « {file} » ajouté."), false);
            }
            Err(e) => self.notify(format!("Ordre du jour : {e:#}"), true),
        }
    }

    /// Sauvegarde et, pendant une rencontre, guide Whisper avec les nouveaux sujets.
    fn agenda_changed(&mut self) {
        match self.view {
            View::Past(i) if i < self.meetings.len() => {
                let _ = self.meetings[i].save();
            }
            _ => {
                if let Some(m) = &self.live {
                    let _ = m.save();
                    if let Some(rec) = &self.recorder {
                        rec.nommer_points(super::live::titres_points(m.agenda.as_ref()));
                    }
                    self.push_settings();
                }
            }
        }
    }

    fn peut_demarrer(&self) -> bool {
        matches!(self.model, ModelState::Ready(_)) && !self.finishing && self.import_job.is_none()
    }

    /// Débute un point (ou, pour `None`, ferme la période en cours) à la position courante de
    /// l'enregistrement. Si l'enregistrement n'est pas démarré, il démarre sur ce point.
    pub(super) fn debuter_point(&mut self, point: Option<usize>) {
        self.abandonner_propositions();
        if self.recorder.is_none() {
            if point.is_none() || self.live.is_some() || !self.peut_demarrer() {
                return;
            }
            self.start_recording();
        }
        let (Some(rec), Some(m)) = (&self.recorder, self.live.as_mut()) else { return };
        // Déjà sur ce point (ou déjà hors point) : pas de coupe. Une période ouverte dont le point a
        // été supprimé (point `None`) se ferme encore.
        let ouverte = m.periodes.last().is_some_and(|p| p.fin_ms.is_none());
        if point == m.point_actif() && (point.is_some() || !ouverte) {
            return;
        }
        // Le fil audio coupe les blocs et donne l'échantillon exact de la coupe.
        let ms = rec.changer_point(point);
        m.debuter_point(point, ms, Declenchement::Manuel);
        if let Err(e) = m.save() {
            log::warn!("Sauvegarde après changement de point : {e}");
        }
    }

    /// Point principal voisin (`suivant` ou précédent) du point actif ; hors point, on repart
    /// du dernier point traité (le précédent est alors ce point lui-même).
    fn point_voisin(&self, suivant: bool) -> Option<usize> {
        let m = self.live.as_ref()?;
        let principaux: Vec<usize> =
            m.agenda.as_ref()?.points.iter().enumerate().filter(|(_, p)| p.niveau == 1).map(|(i, _)| i).collect();
        let actif = m.point_actif();
        let repere = actif.or_else(|| m.periodes.iter().rev().find_map(|p| p.point));
        let pos = repere.and_then(|r| principaux.iter().rposition(|&i| i <= r));
        let cible = match (pos, suivant) {
            (None, true) => Some(0),
            (None, false) => None,
            (Some(p), true) => Some(p + 1),
            (Some(p), false) if actif.is_none() => Some(p),
            (Some(p), false) => p.checked_sub(1),
        };
        cible.and_then(|c| principaux.get(c).copied())
    }

    /// Ctrl+→ point suivant, Ctrl+← point précédent, Ctrl+H hors point (Cmd sur macOS).
    pub(super) fn raccourcis_points(&mut self, ctx: &egui::Context) {
        if self.recorder.is_none() || ctx.egui_wants_keyboard_input() {
            return;
        }
        let (suivant, precedent, hors) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::COMMAND, Key::ArrowRight),
                i.consume_key(Modifiers::COMMAND, Key::ArrowLeft),
                i.consume_key(Modifiers::COMMAND, Key::H),
            )
        });
        if hors {
            self.debuter_point(None);
        } else if (suivant || precedent)
            && let Some(p) = self.point_voisin(suivant)
        {
            self.debuter_point(Some(p));
        }
    }

    /// Applique une modification de la liste des points, en gardant les périodes de la rencontre
    /// rattachées aux mêmes points.
    fn editer_points(&mut self, edition: Edition) {
        // L'éditeur reste ouvert même si la liste n'est plus vide.
        self.edit_points |= matches!(edition, Edition::Ajouter);
        let Some(a) = self.agenda_slot().as_mut() else { return };
        let n = a.points.len();
        let correspondance: Box<dyn Fn(usize) -> Option<usize>> = match edition {
            Edition::Ajouter => {
                a.points.push(Point { numero: String::new(), titre: "Nouveau point".into(), niveau: 1, detail: String::new() });
                Box::new(Some)
            }
            Edition::Supprimer(i) if i < n => {
                a.points.remove(i);
                Box::new(move |j| match j.cmp(&i) {
                    std::cmp::Ordering::Less => Some(j),
                    std::cmp::Ordering::Equal => None,
                    std::cmp::Ordering::Greater => Some(j - 1),
                })
            }
            Edition::Monter(i) if i > 0 && i < n => {
                a.points.swap(i - 1, i);
                Box::new(move |j| Some(if j == i { i - 1 } else if j == i - 1 { i } else { j }))
            }
            Edition::Analyser => {
                a.points = crate::agenda::points(&a.text);
                Box::new(|_| None)
            }
            _ => return,
        };
        renumeroter(&mut a.points);
        if let Some(m) = self.agenda_meeting_mut() {
            m.renumeroter_periodes(correspondance);
        }
        self.agenda_changed();
    }

    pub(super) fn agenda_card(&mut self, ui: &mut Ui) {
        let mut browse = false;
        let mut remove = false;
        let mut edited = false;
        let mut manual = false;
        let mut debuter: Option<Option<usize>> = None;
        let mut exporter: Option<usize> = None;
        let mut edition = None;
        let mut renomme = false;
        let slot = self.agenda_slot().clone();

        // État du découpage par point : point actif, temps passé sur chaque point.
        let enregistre = self.recorder.is_some();
        let passee = matches!(self.view, View::Past(_));
        let maintenant = self.recorder.as_ref().map(|r| r.position_ms());
        let (actif, durees, a_des_periodes) = match (self.agenda_meeting(), &slot) {
            (Some(m), Some(a)) => {
                let fin = maintenant.unwrap_or(m.duration_s * 1000);
                let durees: Vec<Option<u64>> = (0..a.points.len())
                    .map(|i| m.periodes.iter().any(|p| p.point == Some(i)).then(|| m.duree_point(i, fin)))
                    .collect();
                (m.point_actif().filter(|_| enregistre), durees, !m.periodes.is_empty())
            }
            _ => (None, Vec::new(), false),
        };
        // « Débuter le point » démarre l'enregistrement s'il ne l'est pas encore.
        let peut_debuter = !passee && (enregistre || (self.live.is_none() && self.peut_demarrer()));
        let editer = slot.as_ref().is_some_and(|a| a.points.is_empty()) || self.edit_points;
        // Rencontre passée enregistrée en blocs : l'audio de chaque point peut être exporté.
        let audio_par_point = passee && self.agenda_meeting().is_some_and(|m| crate::audio::blocs::existe(&m.dir()));
        let propose = if enregistre { self.point_propose() } else { None };

        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new("Ordre du jour").strong());
                match &slot {
                    None => {
                        ui.label(RichText::new("optionnel : glissez un document Word, PDF ou texte ici").weak());
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            manual = ui.small_button("Saisir").clicked();
                            browse = ui.small_button("Parcourir…").clicked();
                        });
                    }
                    Some(a) => {
                        ui.label(RichText::new(&a.file).weak());
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            remove = ui.small_button("Retirer").clicked();
                            browse = ui.small_button("Remplacer…").clicked();
                            if !editer && ui.small_button("Modifier les points").clicked() {
                                self.edit_points = true;
                            }
                            if actif.is_some()
                                && ui.small_button("Hors point").on_hover_text(RACCOURCIS).clicked()
                            {
                                debuter = Some(None);
                            }
                        });
                    }
                }
            });
            let Some(a) = &slot else { return };
            // Un long ordre du jour défile plutôt que de cacher la transcription.
            let hauteur = ui.ctx().content_rect().height() * 0.4;
            if editer && a.points.is_empty() {
                ui.label(RichText::new("Aucun point reconnu dans l'ordre du jour : ajoutez-les ici pour découper la rencontre par point.").weak());
            }
            egui::ScrollArea::vertical().id_salt("points").max_height(hauteur).auto_shrink([false, true]).show(ui, |ui| {
                if editer {
                    ui.add_space(4.0);
                    if let Some(a) = self.agenda_slot().as_mut() {
                        (edition, renomme) = editeur(ui, a);
                    }
                } else {
                    ui.add_space(2.0);
                    for (i, p) in a.points.iter().enumerate() {
                        let duree = durees.get(i).copied().flatten();
                        match ligne_point(ui, i, p, actif == Some(i), propose == Some(i), duree, peut_debuter, audio_par_point) {
                            Some(ClicPoint::Debuter(i)) => debuter = Some(Some(i)),
                            Some(ClicPoint::Exporter(i)) => exporter = Some(i),
                            None => {}
                        }
                    }
                }
            });
            // Hors de la zone qui défile : toujours visibles.
            if editer {
                ui.horizontal(|ui| {
                    if ui.small_button("Ajouter un point").clicked() {
                        edition = Some(Edition::Ajouter);
                        ui.data_mut(|d| d.insert_temp(egui::Id::unique(NOUVEAU_POINT), true));
                    }
                    if !a_des_periodes
                        && !a.text.trim().is_empty()
                        && ui.small_button("Relire le texte").on_hover_text("Reconnaît à nouveau les points dans le texte de l'ordre du jour").clicked()
                    {
                        edition = Some(Edition::Analyser);
                    }
                    if !a.points.is_empty() && ui.small_button("Terminé").clicked() {
                        self.edit_points = false;
                    }
                });
            }
            egui::CollapsingHeader::new("Voir et modifier le texte").id_salt("agenda_text").show(ui, |ui| {
                if let Some(a) = self.agenda_slot().as_mut() {
                    edited = ui
                        .add(egui::TextEdit::multiline(&mut a.text).desired_rows(6).desired_width(f32::INFINITY))
                        .lost_focus();
                }
            });
        });
        if manual {
            *self.agenda_slot() = Some(Agenda::new("Saisi à la main".into(), String::new()));
        }
        if remove {
            *self.agenda_slot() = None;
            if let Some(m) = self.agenda_meeting_mut() {
                m.renumeroter_periodes(|_| None);
            }
            self.agenda_changed();
        }
        if edited {
            // Points jamais retouchés et rencontre pas encore découpée : on les relit dans le texte.
            let ancien = slot.as_ref().map(|a| crate::agenda::points(&a.text));
            if !a_des_periodes
                && ancien.as_ref() == slot.as_ref().map(|a| &a.points)
                && let Some(a) = self.agenda_slot().as_mut()
            {
                a.points = crate::agenda::points(&a.text);
            }
            self.agenda_changed();
        }
        if renomme {
            self.agenda_changed();
        }
        if let Some(e) = edition {
            self.editer_points(e);
        }
        if let Some(point) = debuter {
            self.debuter_point(point);
        }
        if let (Some(point), &View::Past(idx)) = (exporter, &self.view) {
            self.exporter_audio(idx, crate::audio::blocs::Portee::Point(point));
        }
        if browse {
            let picked = rfd::FileDialog::new()
                .set_title("Choisir l'ordre du jour")
                .add_filter("Ordre du jour", crate::agenda::EXTENSIONS)
                .pick_file();
            if let Some(path) = picked {
                self.attach_agenda(&path);
            }
        }
    }
}

/// Clé (mémoire egui) du point qui vient d'être ajouté dans l'éditeur.
const NOUVEAU_POINT: &str = "nouveau_point";

/// Les polices intégrées n'ont pas de flèches.
const RACCOURCIS: &str = if cfg!(target_os = "macos") {
    "Cmd + flèche droite ou gauche : point suivant ou précédent\nCmd + H : hors point"
} else {
    "Ctrl + flèche droite ou gauche : point suivant ou précédent\nCtrl + H : hors point"
};

/// Coche d'un point traité, dessinée : les polices intégrées n'ont pas « ✓ ».
fn coche(ui: &mut Ui) {
    let (r, _) = ui.allocate_exact_size(egui::Vec2::splat(12.0), egui::Sense::hover());
    let p = |x: f32, y: f32| r.left_top() + egui::vec2(x, y) * r.width();
    ui.painter().line(vec![p(0.1, 0.55), p(0.4, 0.85), p(0.9, 0.2)], egui::Stroke::new(2.0, SALLE));
}

enum ClicPoint {
    Debuter(usize),
    Exporter(usize),
}

/// Une ligne de la liste des points ; retourne l'action demandée sur clic.
#[allow(clippy::too_many_arguments)]
fn ligne_point(ui: &mut Ui, i: usize, p: &Point, actif: bool, propose: bool, duree: Option<u64>, peut_debuter: bool, exporter: bool) -> Option<ClicPoint> {
    let mut clic = None;
    let retrait = (p.niveau.saturating_sub(1)) as f32 * 22.0;
    let fond = if actif { BLEU.gamma_multiply(0.22) } else { Color32::TRANSPARENT };
    egui::Frame::new().fill(fond).corner_radius(CornerRadius::same(6)).inner_margin(egui::Margin::symmetric(6, 2)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.add_space(retrait);
            ui.label(RichText::new(&p.numero).monospace().weak());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if p.niveau == 1 {
                    if actif {
                        ui.label(RichText::new(clock(duree.unwrap_or(0))).monospace().strong());
                    } else if peut_debuter {
                        let texte = if duree.is_some() { "Revenir au point" } else { "Débuter le point" };
                        // Point dont l'annonce vient d'être entendue : bouton mis en évidence.
                        let bouton = if propose {
                            egui::Button::new(RichText::new(texte).small().strong().color(Color32::WHITE)).fill(BLEU)
                        } else {
                            egui::Button::new(RichText::new(texte).small())
                        };
                        let aide = if propose { format!("Changement de point entendu : cliquez pour débuter ce point.\n{RACCOURCIS}") } else { RACCOURCIS.to_string() };
                        if ui.add(bouton).on_hover_text(aide).clicked() {
                            clic = Some(ClicPoint::Debuter(i));
                        }
                    }
                }
                if exporter && duree.is_some() && ui.small_button("Exporter l'audio de ce point").on_hover_text("Fichier WAV : tous les passages sur ce point, bout à bout").clicked() {
                    clic = Some(ClicPoint::Exporter(i));
                }
                if let Some(d) = duree.filter(|_| !actif) {
                    ui.label(RichText::new(clock(d)).small().weak());
                    coche(ui);
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    let titre = RichText::new(&p.titre);
                    ui.add(egui::Label::new(if actif { titre.strong() } else { titre }).truncate());
                });
            });
        });
        if !p.detail.is_empty() {
            egui::Frame::new().inner_margin(egui::Margin { left: retrait as i8 + 24, ..Default::default() }).show(ui, |ui| {
                egui::CollapsingHeader::new(RichText::new("Détail").small().weak()).id_salt(("point_detail", i)).show(ui, |ui| {
                    ui.label(RichText::new(&p.detail).small());
                });
            });
        }
    });
    clic
}

/// Lignes de l'éditeur des points : renommer, supprimer, réordonner. Retourne la modification
/// demandée et si un titre a été renommé (perte du focus).
fn editeur(ui: &mut Ui, a: &mut Agenda) -> (Option<Edition>, bool) {
    let mut edition = None;
    let mut renomme = false;
    // Point qui vient d'être ajouté : on y place le curseur.
    let nouveau = ui.data_mut(|d| d.remove_temp::<bool>(egui::Id::unique(NOUVEAU_POINT))).unwrap_or(false);
    let n = a.points.len();
    for (i, p) in a.points.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.add_space((p.niveau.saturating_sub(1)) as f32 * 22.0);
            ui.label(RichText::new(&p.numero).monospace().weak());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.small_button("Supprimer").clicked() {
                    edition = Some(Edition::Supprimer(i));
                }
                if ui.add_enabled(i + 1 < n, egui::Button::new("Descendre").small()).clicked() {
                    edition = Some(Edition::Monter(i + 1));
                }
                if ui.add_enabled(i > 0, egui::Button::new("Monter").small()).clicked() {
                    edition = Some(Edition::Monter(i));
                }
                let champ = ui.add(egui::TextEdit::singleline(&mut p.titre).desired_width(f32::INFINITY));
                if nouveau && i + 1 == n {
                    champ.request_focus();
                    champ.scroll_to_me(None);
                }
                renomme |= champ.lost_focus();
            });
        });
    }
    (edition, renomme)
}
