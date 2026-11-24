//! Vérification du découpage par point d'une rencontre déjà transcrite (fin d'un import, ou
//! « Découper par points ») : frontières proposées, écoute autour de chacune, puis validation.

use std::path::PathBuf;

use crossbeam_channel::Receiver;
use eframe::egui::{self, Color32, CornerRadius, RichText, Ui};

use super::apparence::{Genre, bouton_widget, gras, palette};
use super::widgets::BLEU;
use super::{TourDeTableApp, View};
use crate::audio::blocs::{self, Index};
use crate::audio::{SAMPLE_RATE, frontieres, import, lecture};
use crate::decoupage::{self, Frontiere};
use crate::storage::{Declenchement, Meeting, clock};

/// Durée écoutée autour d'une frontière.
const ECOUTE_MS: u64 = 10_000;
/// Pas des boutons « −5 s » et « +5 s ».
const PAS_MS: u64 = 5_000;

/// Rangement par point en cours : import (rencontre pas encore dans l'historique), copie de la
/// rencontre (si le rangement échoue) et résultat.
pub type TacheDecoupage = (bool, Meeting, Receiver<Result<Meeting, String>>);

pub(super) struct Ligne {
    pub(super) f: Frontiere,
    /// Heure telle que saisie.
    heure: String,
}

pub struct Verification {
    pub(super) rencontre: Meeting,
    /// Fin d'un import : la rencontre n'est pas encore dans l'historique.
    pub(super) nouvelle: bool,
    pub(super) lignes: Vec<Ligne>,
    /// Audio à écouter : `_import/` pendant un import, le dossier de la rencontre sinon.
    audio: PathBuf,
    index: Index,
    pub(super) duree_ms: u64,
    lecture: Option<(usize, lecture::Lecture)>,
    /// Mode découpage dans la fiche (`mode_decoupage.rs`) : ligne dont on choisit le point (rang dans la transcription,
    /// pas son heure : deux lignes peuvent commencer à la même seconde).
    pub(super) choix: Option<usize>,
    /// Frontière dont l'en-tête est sélectionné, et sa liste « Changer de point » ouverte.
    pub(super) choisi: Option<usize>,
    pub(super) changer: bool,
    /// Heure d'un en-tête vers lequel faire défiler la transcription.
    pub(super) aller_a: Option<u64>,
}

impl Verification {
    pub fn lecture_en_cours(&self) -> bool {
        self.lecture.as_ref().is_some_and(|(_, l)| l.en_cours())
    }

    pub(super) fn trier(&mut self) {
        self.lecture = None;
        self.lignes.sort_by_key(|l| l.f.debut_ms);
    }

    /// Frontière ajoutée à la main (mode découpage) ; la liste reste triée. Rend son indice.
    pub(super) fn ajouter(&mut self, f: Frontiere) -> usize {
        let ms = f.debut_ms;
        self.lignes.push(Ligne { heure: clock(ms), f });
        self.trier();
        self.lignes.iter().position(|l| l.f.debut_ms == ms).unwrap_or(0)
    }

    /// Frontière déplacée ou ajoutée à la main : l'extrait suit la nouvelle heure.
    pub(super) fn deplacer(&mut self, i: usize, ms: u64) {
        let ms = ms.min(self.duree_ms);
        let l = &mut self.lignes[i];
        l.f.debut_ms = ms;
        l.f.declenchement = Declenchement::Manuel;
        l.f.raison.clear();
        l.f.extrait = self.rencontre.segments.iter().find(|s| s.end_ms > ms).map(|s| s.text.clone()).unwrap_or_default();
        l.heure = clock(ms);
    }
}

/// Heure où placer une frontière du point `p` pour qu'elle tombe entre celles des points voisins de l'ordre du jour
/// (`autres` : point et heure des autres frontières) : `None` si l'heure `actuelle` y est déjà, ou si les voisins
/// sont eux-mêmes dans le désordre. Sans point suivant : une minute après le précédent.
fn heure_dans_l_ordre(autres: &[(usize, u64)], p: usize, actuelle: Option<u64>, duree_ms: u64) -> Option<u64> {
    let avant = autres.iter().filter(|(q, _)| *q < p).map(|(_, ms)| *ms).max();
    let apres = autres.iter().filter(|(q, _)| *q > p).map(|(_, ms)| *ms).min();
    let (lo, hi) = (avant.unwrap_or(0), apres.unwrap_or(duree_ms));
    if lo > hi {
        return None;
    }
    if let Some(t) = actuelle
        && avant.is_none_or(|a| t > a)
        && apres.is_none_or(|b| t < b)
    {
        return None;
    }
    Some(match (avant, apres) {
        (Some(a), None) => (a + 60_000).min((a + duree_ms) / 2),
        _ => (lo + hi) / 2,
    })
}

impl Verification {
    /// Point et heure des frontières autres que la `i`-ième.
    pub(super) fn autres(&self, i: Option<usize>) -> Vec<(usize, u64)> {
        let lignes = self.lignes.iter().enumerate().filter(|(k, _)| Some(*k) != i);
        lignes.filter_map(|(_, l)| l.f.point.map(|p| (p, l.f.debut_ms))).collect()
    }
}

/// « 1:02:03 », « 12:34 » ou « 45 » (secondes).
fn lire_heure(texte: &str) -> Option<u64> {
    let mut s = 0u64;
    for partie in texte.trim().split(':') {
        s = s * 60 + partie.trim().parse::<u64>().ok()?;
    }
    Some(s * 1000)
}

impl TourDeTableApp {
    /// Fin d'un import : les frontières détectées sont appliquées sans écran de vérification
    /// (« Découper par points » de la fiche permet de les corriger plus tard).
    pub(super) fn decouper_import(&mut self, rencontre: Meeting) {
        let points = rencontre.agenda.as_ref().map(|a| a.points.clone()).unwrap_or_default();
        let frontieres = decoupage::detecter_hors_ligne(&rencontre.segments, &points);
        let dir = rencontre.dir();
        for f in &frontieres {
            decoupage::journaliser(&dir, &format!("hors ligne | {} | {} | « {} » | {}", clock(f.debut_ms), rencontre.intertitre(f.point), f.extrait, f.raison));
        }
        self.valider_decoupage(rencontre, true, frontieres);
    }

    /// Ouvre l'écran de vérification ; sans point à l'ordre du jour, tout va hors point.
    pub(super) fn ouvrir_verification(&mut self, rencontre: Meeting, nouvelle: bool) {
        let points = rencontre.agenda.as_ref().map(|a| a.points.clone()).unwrap_or_default();
        if points.is_empty() {
            self.valider_decoupage(rencontre, nouvelle, Vec::new());
            return;
        }
        let dir = rencontre.dir();
        let audio = if nouvelle { dir.join(import::TRAVAIL) } else { dir.clone() };
        let index = match blocs::lire_index(&audio) {
            Ok(i) => i,
            Err(e) => {
                self.notify(format!("Découpage par point de « {} » : {e:#}", rencontre.title), true);
                if nouvelle {
                    self.meetings.insert(0, rencontre);
                    self.view = View::Past(0);
                }
                return;
            }
        };
        let proposees = if !nouvelle && !rencontre.periodes.is_empty() {
            frontieres::frontieres_de(&rencontre)
        } else {
            let v = decoupage::detecter_hors_ligne(&rencontre.segments, &points);
            for f in &v {
                decoupage::journaliser(&dir, &format!("hors ligne | {} | {} | « {} » | {}", clock(f.debut_ms), rencontre.intertitre(f.point), f.extrait, f.raison));
            }
            v
        };
        let duree_ms = index.blocs.iter().map(|b| blocs::ms(b.decalage + b.echantillons)).max().unwrap_or(0);
        let lignes = proposees.into_iter().map(|f| Ligne { heure: clock(f.debut_ms), f }).collect();
        self.verification =
            Some(Verification { rencontre, nouvelle, lignes, audio, index, duree_ms, lecture: None, choix: None, choisi: None, changer: false, aller_a: None });
    }

    /// Range l'audio par point dans un fil, d'après les frontières validées.
    pub(super) fn valider_decoupage(&mut self, rencontre: Meeting, nouvelle: bool, frontieres: Vec<Frontiere>) {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let copie = rencontre.clone();
        std::thread::spawn(move || {
            let mut m = rencontre;
            let dir = m.dir();
            let r = if nouvelle { frontieres::appliquer(&dir, &mut m, &frontieres) } else { frontieres::redecouper(&dir, &mut m, &frontieres) };
            let r = r.and_then(|()| m.save()).and_then(|()| m.export_all().map(|_| ()));
            let _ = tx.send(r.map(|()| m).map_err(|e| format!("{e:#}")));
        });
        self.decoupage_job = Some((nouvelle, copie, rx));
    }

    /// Fin du rangement : la rencontre (re)prend sa place dans l'historique, puis le compte rendu.
    pub(super) fn pump_decoupage(&mut self) {
        let Some((nouvelle, copie, rx)) = &self.decoupage_job else { return };
        let Ok(res) = rx.try_recv() else { return };
        let (nouvelle, copie) = (*nouvelle, copie.clone());
        self.decoupage_job = None;
        let m = match res {
            Ok(m) => m,
            Err(e) => {
                self.notify(format!("Découpage par point de « {} » : {e}", copie.title), true);
                // Sans écran ouvert (pas d'ordre du jour), la rencontre ne doit pas disparaître.
                if nouvelle && self.verification.is_none() {
                    self.meetings.insert(0, copie);
                    self.view = View::Past(0);
                }
                return;
            }
        };
        self.verification = None;
        let avec_agenda = m.agenda.as_ref().is_some_and(|a| !a.points.is_empty());
        let (id, deja_nettoyee) = (m.id.clone(), m.nettoyage.is_some());
        if nouvelle {
            self.meetings.insert(0, m);
            self.view = View::Past(0);
            // Transcription → nettoyage → compte rendu (avec un ordre du jour).
            self.demander_nettoyage(&id, avec_agenda, true);
            self.demander_voix(&id);
        } else if let Some(i) = self.meetings.iter().position(|x| x.id == m.id) {
            self.meetings[i] = m;
            self.view = View::Past(i);
            self.notify("Découpage par point enregistré.", false);
            // Les phrases nettoyées sont rattachées aux anciens points : on recommence.
            if deja_nettoyee {
                self.demander_nettoyage(&id, false, true);
            }
        }
    }

    pub(super) fn verification_view(&mut self, ui: &mut Ui) {
        let occupe = self.decoupage_job.is_some();
        let mut erreur = None;
        let mut valider = false;
        let mut annuler = false;
        let Some(v) = self.verification.as_mut() else { return };
        if let Some((_, l)) = &v.lecture
            && let Ok(e) = l.erreur.try_recv()
        {
            erreur = Some(e);
        }
        let points = v.rencontre.agenda.as_ref().map(|a| a.points.clone()).unwrap_or_default();
        let nom = |p: Option<usize>| match p.and_then(|i| points.get(i)) {
            None => "Hors point".to_string(),
            Some(pt) => {
                let t: String = pt.titre.chars().take(36).collect();
                format!("{}. {t}{}", pt.numero, if pt.titre.chars().count() > 36 { "…" } else { "" })
            }
        };

        let pal = palette(ui);
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            // Retour à la fiche, comme « Annuler » ; pas pendant un import, que la rencontre attend pour exister.
            if !v.nouvelle
                && !occupe
                && ui.add(bouton_widget(ui, "←  Retour", Genre::Discret, 28.0)).on_hover_text("Revient à la rencontre sans rien changer").clicked()
            {
                annuler = true;
            }
            ui.label(RichText::new(&v.rencontre.title).size(13.5).color(pal.texte2));
        });
        ui.label(gras("Découper par points", 24.0).color(pal.texte));
        ui.label(
            RichText::new(
                "Vérifiez où commence chaque point. Écoutez 10 secondes autour d'une frontière pour la confirmer, \
                 déplacez-la, ajoutez-en ou retirez-en, puis validez : le compte rendu sera rédigé ensuite.",
            )
            .size(14.0)
            .color(pal.texte),
        );
        if v.lignes.is_empty() {
            ui.add_space(6.0);
            ui.label(
                RichText::new("Aucune annonce de point n'a été reconnue : ajoutez les frontières à la main, ou validez pour tout garder hors point.")
                    .size(14.0)
                    .color(pal.ambre_texte),
            );
        }
        ui.add_space(10.0);
        // Vue d'ensemble : la rencontre découpée selon les frontières.
        {
            let en_ecoute = v.lecture.as_ref().filter(|(_, l)| l.en_cours()).map(|(i, _)| *i);
            egui::Frame::new()
                .fill(pal.surface)
                .stroke(egui::Stroke::new(1.0, pal.bordure))
                .corner_radius(CornerRadius::same(16))
                .inner_margin(egui::Margin::symmetric(18, 12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("0:00").size(12.0).color(pal.texte2));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(clock(v.duree_ms)).size(12.0).color(pal.texte2));
                        });
                    });
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), egui::Sense::hover());
                    let duree = v.duree_ms.max(1) as f32;
                    let x = |ms: u64| rect.left() + rect.width() * (ms as f32 / duree).clamp(0.0, 1.0);
                    let mut debut = 0;
                    let mut morceaux: Vec<(u64, u64, Option<usize>, Option<usize>)> = Vec::new();
                    for (k, l) in v.lignes.iter().enumerate() {
                        if l.f.debut_ms > debut || k == 0 {
                            if let Some(dernier) = morceaux.last_mut() {
                                dernier.1 = l.f.debut_ms;
                            } else if l.f.debut_ms > 0 {
                                morceaux.push((0, l.f.debut_ms, None, None));
                            }
                        }
                        morceaux.push((l.f.debut_ms, v.duree_ms, l.f.point, Some(k)));
                        debut = l.f.debut_ms;
                    }
                    if morceaux.is_empty() {
                        morceaux.push((0, v.duree_ms, None, None));
                    }
                    for (d, f, point, ligne) in morceaux {
                        let r = egui::Rect::from_x_y_ranges(x(d) + 1.0..=x(f) - 1.0, rect.y_range());
                        let actif = ligne.is_some() && ligne == en_ecoute;
                        let fond = match point {
                            _ if actif => pal.accent_plein,
                            Some(i) => (if ui.visuals().dark_mode { pal.piste } else { Color32::WHITE }).lerp_to_gamma(pal.accent, [0.18, 0.32, 0.46, 0.60][i % 4]),
                            None => pal.piste,
                        };
                        ui.painter().rect_filled(r, CornerRadius::same(5), fond);
                        if let Some(i) = point
                            && r.width() > 18.0
                        {
                            let clair = actif || i % 4 >= 2 || ui.visuals().dark_mode;
                            let numero = points.get(i).map_or(String::new(), |p| p.numero.clone());
                            ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, numero, super::colonne::police_grasse(12.0), if clair { Color32::WHITE } else { pal.texte });
                        }
                    }
                });
        }
        ui.add_space(10.0);

        let mut retirer = None;
        let mut deplacer = None;
        let mut trier = false;
        let mut ecouter = None;
        let mut repointe = None;
        let en_cours = v.lecture.as_ref().filter(|(_, l)| l.en_cours()).map(|(i, _)| *i);
        egui::ScrollArea::vertical().auto_shrink([false, true]).max_height(ui.available_height() - 60.0).show(ui, |ui| {
            for (i, l) in v.lignes.iter_mut().enumerate() {
                let joue = en_cours == Some(i);
                let (contour, fond) = if joue { (egui::Stroke::new(1.5, pal.accent), pal.surface) } else { (egui::Stroke::new(1.0, pal.bordure), pal.surface) };
                egui::Frame::new().fill(fond).stroke(contour).corner_radius(CornerRadius::same(14)).inner_margin(egui::Margin::symmetric(14, 10)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let numero = l.f.point.and_then(|p| points.get(p)).map_or("–".to_string(), |p| p.numero.clone());
                        let (r, _) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::hover());
                        let (bg, encre) = if joue { (pal.accent_plein, Color32::WHITE) } else if l.f.point.is_some() { (pal.accent_doux, pal.accent_texte) } else { (pal.piste, pal.texte2) };
                        ui.painter().rect_filled(r, CornerRadius::same(8), bg);
                        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, numero, super::colonne::police_grasse(12.0), encre);
                        let avant = l.f.point;
                        egui::ComboBox::from_id_salt(("point", i)).width(340.0).height(400.0).selected_text(nom(l.f.point)).show_ui(ui, |ui| {
                            ui.selectable_value(&mut l.f.point, None, nom(None));
                            for p in 0..points.len() {
                                ui.selectable_value(&mut l.f.point, Some(p), nom(Some(p)));
                            }
                        });
                        if l.f.point != avant {
                            repointe = Some(i);
                        }
                        let r = ui
                            .add(egui::TextEdit::singleline(&mut l.heure).desired_width(76.0).margin(egui::Margin::symmetric(10, 6)))
                            .on_hover_text("Heure de début : h:mm:ss ou mm:ss");
                        if r.lost_focus() {
                            match lire_heure(&l.heure) {
                                Some(ms) => deplacer = Some((i, ms)),
                                None => l.heure = clock(l.f.debut_ms),
                            }
                        }
                        if ui.add(bouton_widget(ui, "−5 s", Genre::Secondaire, 32.0)).clicked() {
                            deplacer = Some((i, l.f.debut_ms.saturating_sub(PAS_MS)));
                        }
                        if ui.add(bouton_widget(ui, "+5 s", Genre::Secondaire, 32.0)).clicked() {
                            deplacer = Some((i, l.f.debut_ms + PAS_MS));
                        }
                        let (texte, genre) = if joue { ("Arrêter", Genre::Principal) } else { ("Écouter", Genre::Secondaire) };
                        if ui.add(bouton_widget(ui, texte, genre, 32.0)).on_hover_text("10 s autour de la frontière").clicked() {
                            ecouter = Some(i);
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add(bouton_widget(ui, "Retirer", Genre::Discret, 32.0)).on_hover_text("Retire cette frontière").clicked() {
                                retirer = Some(i);
                            }
                        });
                    });
                    if joue && let Some((_, lecture)) = &v.lecture {
                        ui.horizontal(|ui| {
                            ui.add_space(36.0);
                            barre_frontiere(ui, l.f.debut_ms, v.duree_ms, lecture.avancement_ms());
                        });
                    }
                    if !l.f.extrait.is_empty() {
                        ui.horizontal(|ui| {
                            ui.add_space(36.0);
                            // Une seule ligne : un long extrait (une boucle de Whisper, par exemple) élargirait la carte et
                            // pousserait « Retirer » et « Valider » hors de la fenêtre. Le texte entier s'affiche au survol.
                            let texte = RichText::new(format!("« {} »", l.f.extrait)).italics().size(14.0).color(pal.texte2);
                            let r = ui.add(egui::Label::new(texte).truncate());
                            if !l.f.raison.is_empty() {
                                r.on_hover_text(&l.f.raison);
                            }
                        });
                    }
                });
                ui.add_space(4.0);
            }
        });
        if let Some((i, ms)) = deplacer {
            v.deplacer(i, ms);
            trier = true;
        }
        // Point changé : la frontière va se ranger entre celles des points voisins de l'ordre du jour.
        if let Some(i) = repointe
            && let Some(p) = v.lignes[i].f.point
            && let Some(ms) = heure_dans_l_ordre(&v.autres(Some(i)), p, Some(v.lignes[i].f.debut_ms), v.duree_ms)
        {
            v.deplacer(i, ms);
            trier = true;
        }
        if let Some(i) = retirer {
            v.lignes.remove(i);
            v.lecture = None;
        }
        if let Some(i) = ecouter {
            if en_cours == Some(i) {
                v.lecture = None;
            } else {
                let t = v.lignes[i].f.debut_ms;
                let ech = |ms: u64| ms * SAMPLE_RATE as u64 / 1000;
                let l = lecture::jouer(v.audio.clone(), v.index.clone(), ech(t.saturating_sub(ECOUTE_MS / 2)), ech(t + ECOUTE_MS / 2));
                v.lecture = Some((i, l));
            }
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            // Avec un ordre du jour, on choisit le point qui commence ; la frontière se range entre ses voisins.
            let mut ajout: Option<Option<usize>> = None;
            if points.is_empty() || occupe {
                if ui.add_enabled(!occupe, bouton_widget(ui, "+ Ajouter une frontière", Genre::Secondaire, 40.0)).clicked() {
                    ajout = Some(None);
                }
            } else {
                let bouton = bouton_widget(ui, "+ Ajouter une frontière      ", Genre::Secondaire, 40.0);
                let (resp, _) = egui::containers::menu::MenuButton::from_button(bouton).ui(ui, |ui| {
                    ui.set_min_width(340.0);
                    ui.label(RichText::new("Quel point commence ?").size(13.0).color(pal.texte2));
                    for p in 0..points.len() {
                        let place = v.lignes.iter().any(|l| l.f.point == Some(p));
                        let texte = if place {
                            RichText::new(format!("{}  (déjà placé)", nom(Some(p)))).color(pal.texte2)
                        } else {
                            RichText::new(nom(Some(p))).color(pal.texte)
                        };
                        if ui.button(texte).clicked() {
                            ajout = Some(Some(p));
                        }
                    }
                });
                super::apparence::chevron(ui, &resp, pal.texte);
            }
            if let Some(point) = ajout {
                // Entre les frontières des points voisins ; sinon, une minute après la dernière.
                let ms = point.and_then(|p| heure_dans_l_ordre(&v.autres(None), p, None, v.duree_ms));
                let ms = ms.unwrap_or_else(|| v.lignes.last().map_or(0, |l| l.f.debut_ms + 60_000).min(v.duree_ms));
                let f = Frontiere { point, debut_ms: ms, extrait: String::new(), raison: String::new(), declenchement: Declenchement::Manuel };
                v.lignes.push(Ligne { heure: clock(ms), f });
                let i = v.lignes.len() - 1;
                v.deplacer(i, ms);
                trier = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if occupe {
                    ui.label(RichText::new("Rangement de l'audio par point…").size(14.0).color(pal.texte2));
                    ui.spinner();
                } else {
                    valider = ui.add(bouton_widget(ui, "Valider le découpage", Genre::Principal, 40.0)).clicked();
                    if !v.nouvelle {
                        annuler = ui.add(bouton_widget(ui, "Annuler", Genre::Discret, 40.0)).clicked();
                    }
                }
            });
        });
        if trier {
            v.trier();
        }

        if let Some(e) = erreur {
            self.notify(e, true);
        }
        if annuler {
            self.verification = None;
        } else if valider && let Some(v) = self.verification.as_mut() {
            v.lecture = None;
            let frontieres = v.lignes.iter().map(|l| l.f.clone()).collect();
            let (m, nouvelle) = (v.rencontre.clone(), v.nouvelle);
            self.valider_decoupage(m, nouvelle, frontieres);
        }
    }
}

/// Petit lecteur d'une frontière en cours d'écoute : avancement des 10 s, avec un repère à l'heure de la frontière
/// pour savoir si ce qu'on entend tombe avant ou après.
fn barre_frontiere(ui: &mut Ui, t: u64, duree_ms: u64, (fait, total): (u64, u64)) {
    let debut = t.saturating_sub(ECOUTE_MS / 2);
    let fin = (t + ECOUTE_MS / 2).min(duree_ms.max(t));
    let etendue = fin.saturating_sub(debut).max(1) as f32;
    let secondes = |ms: u64| format!("{} s", (ms as f32 / 1000.0).round() as u64);
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("−{}", secondes(t - debut))).weak());
        let (rect, _) = ui.allocate_exact_size(egui::vec2(260.0, 14.0), egui::Sense::hover());
        let p = ui.painter();
        let rail = egui::Rect::from_center_size(rect.center(), egui::vec2(rect.width(), 6.0));
        p.rect_filled(rail, 3.0, ui.visuals().widgets.inactive.bg_fill);
        let fraction = if total == 0 { 0.0 } else { (fait as f32 / total as f32).clamp(0.0, 1.0) };
        let mut lu = rail;
        lu.set_right(rail.left() + rail.width() * fraction);
        p.rect_filled(lu, 3.0, BLEU);
        let x = rect.left() + rect.width() * (t - debut) as f32 / etendue;
        p.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], egui::Stroke::new(2.0, ui.visuals().strong_text_color()));
        ui.label(RichText::new(format!("+{}", secondes(fin.saturating_sub(t)))).weak());
        let ecart = (debut + fait) as i64 - t as i64;
        let ou = if ecart < 0 { "avant la frontière" } else { "après la frontière" };
        ui.label(RichText::new(format!("{} {ou}", secondes(ecart.unsigned_abs()))));
    })
    .response
    .on_hover_text("Le trait marque l'heure de début du point");
}

#[cfg(test)]
mod tests {
    use super::{heure_dans_l_ordre, lire_heure};

    #[test]
    fn frontiere_rangee_entre_ses_voisins() {
        let autres = [(0, 60_000), (2, 600_000), (3, 900_000)];
        // Point 2 ajouté (index 1) : entre le point 1 et le point 3.
        assert_eq!(heure_dans_l_ordre(&autres, 1, None, 3_600_000), Some(330_000));
        // Déjà au bon endroit : on n'y touche pas.
        assert_eq!(heure_dans_l_ordre(&autres, 1, Some(200_000), 3_600_000), None);
        // Placé après le point 4 : ramené entre le 1 et le 3.
        assert_eq!(heure_dans_l_ordre(&autres, 1, Some(1_000_000), 3_600_000), Some(330_000));
        // Dernier point : une minute après le précédent.
        assert_eq!(heure_dans_l_ordre(&autres, 4, None, 3_600_000), Some(960_000));
        // Premier point : avant le suivant.
        assert_eq!(heure_dans_l_ordre(&autres[1..], 0, None, 3_600_000), Some(300_000));
        // Voisins dans le désordre : rien à faire.
        assert_eq!(heure_dans_l_ordre(&[(0, 500_000), (2, 100_000)], 1, None, 3_600_000), None);
    }

    #[test]
    fn heures_saisies() {
        assert_eq!(lire_heure("1:02:03"), Some(3_723_000));
        assert_eq!(lire_heure(" 12:34 "), Some(754_000));
        assert_eq!(lire_heure("45"), Some(45_000));
        assert_eq!(lire_heure("1 h"), None);
    }
}
