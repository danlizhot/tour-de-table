//! Vérification du découpage par point d'une rencontre déjà transcrite (fin d'un import, ou
//! « Découper par points ») : frontières proposées, écoute autour de chacune, puis validation.

use std::path::PathBuf;

use crossbeam_channel::Receiver;
use eframe::egui::{self, Color32, RichText, Ui};

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

struct Ligne {
    f: Frontiere,
    /// Heure telle que saisie.
    heure: String,
}

pub struct Verification {
    rencontre: Meeting,
    /// Fin d'un import : la rencontre n'est pas encore dans l'historique.
    nouvelle: bool,
    lignes: Vec<Ligne>,
    /// Audio à écouter : `_import/` pendant un import, le dossier de la rencontre sinon.
    audio: PathBuf,
    index: Index,
    duree_ms: u64,
    lecture: Option<(usize, lecture::Lecture)>,
}

impl Verification {
    pub fn lecture_en_cours(&self) -> bool {
        self.lecture.as_ref().is_some_and(|(_, l)| l.en_cours())
    }

    fn trier(&mut self) {
        self.lecture = None;
        self.lignes.sort_by_key(|l| l.f.debut_ms);
    }

    /// Frontière déplacée ou ajoutée à la main : l'extrait suit la nouvelle heure.
    fn deplacer(&mut self, i: usize, ms: u64) {
        let ms = ms.min(self.duree_ms);
        let l = &mut self.lignes[i];
        l.f.debut_ms = ms;
        l.f.declenchement = Declenchement::Manuel;
        l.f.raison.clear();
        l.f.extrait = self.rencontre.segments.iter().find(|s| s.end_ms > ms).map(|s| s.text.clone()).unwrap_or_default();
        l.heure = clock(ms);
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
        self.verification = Some(Verification { rencontre, nouvelle, lignes, audio, index, duree_ms, lecture: None });
    }

    /// Range l'audio par point dans un fil, d'après les frontières validées.
    fn valider_decoupage(&mut self, rencontre: Meeting, nouvelle: bool, frontieres: Vec<Frontiere>) {
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
        if nouvelle {
            self.meetings.insert(0, m);
            self.view = View::Past(0);
            if avec_agenda {
                self.auto_compte_rendu(0);
            }
        } else if let Some(i) = self.meetings.iter().position(|x| x.id == m.id) {
            self.meetings[i] = m;
            self.view = View::Past(i);
            self.notify("Découpage par point enregistré.", false);
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
                let t: String = pt.titre.chars().take(48).collect();
                format!("{}. {t}{}", pt.numero, if pt.titre.chars().count() > 48 { "…" } else { "" })
            }
        };

        ui.heading(format!("Découpage par point — {}", v.rencontre.title));
        ui.label(
            RichText::new(
                "Vérifiez l'heure de début de chaque point. Écoutez 10 s autour d'une frontière pour la confirmer, \
                 déplacez-la, ajoutez-en ou retirez-en, puis validez. Le compte rendu sera rédigé ensuite.",
            )
            .weak(),
        );
        if v.lignes.is_empty() {
            ui.add_space(6.0);
            ui.label("Aucune annonce de point n'a été reconnue : ajoutez les frontières à la main, ou validez pour tout garder hors point.");
        }
        ui.add_space(8.0);

        let mut retirer = None;
        let mut deplacer = None;
        let mut trier = false;
        let mut ecouter = None;
        let en_cours = v.lecture.as_ref().filter(|(_, l)| l.en_cours()).map(|(i, _)| *i);
        egui::ScrollArea::vertical().auto_shrink([false, true]).max_height(ui.available_height() - 60.0).show(ui, |ui| {
            for (i, l) in v.lignes.iter_mut().enumerate() {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt(("point", i)).width(320.0).selected_text(nom(l.f.point)).show_ui(ui, |ui| {
                            ui.selectable_value(&mut l.f.point, None, nom(None));
                            for p in 0..points.len() {
                                ui.selectable_value(&mut l.f.point, Some(p), nom(Some(p)));
                            }
                        });
                        let r = ui.add(egui::TextEdit::singleline(&mut l.heure).desired_width(70.0)).on_hover_text("Heure de début : h:mm:ss ou mm:ss");
                        if r.lost_focus() {
                            match lire_heure(&l.heure) {
                                Some(ms) => deplacer = Some((i, ms)),
                                None => l.heure = clock(l.f.debut_ms),
                            }
                        }
                        if ui.small_button("−5 s").clicked() {
                            deplacer = Some((i, l.f.debut_ms.saturating_sub(PAS_MS)));
                        }
                        if ui.small_button("+5 s").clicked() {
                            deplacer = Some((i, l.f.debut_ms + PAS_MS));
                        }
                        let ecoute = if en_cours == Some(i) { "■ Arrêter" } else { "▶ Écouter" };
                        if ui.small_button(ecoute).on_hover_text("10 s autour de la frontière").clicked() {
                            ecouter = Some(i);
                        }
                        if ui.small_button("Retirer").clicked() {
                            retirer = Some(i);
                        }
                    });
                    if !l.f.extrait.is_empty() {
                        let r = ui.label(RichText::new(format!("« {} »", l.f.extrait)).italics().weak());
                        if !l.f.raison.is_empty() {
                            r.on_hover_text(&l.f.raison);
                        }
                    }
                });
            }
        });
        if let Some((i, ms)) = deplacer {
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

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.add_enabled(!occupe, egui::Button::new("+ Ajouter une frontière")).clicked() {
                // Après la dernière, sur le point qui suit.
                let derniere = v.lignes.last().map(|l| &l.f);
                let ms = derniere.map_or(0, |f| f.debut_ms + 60_000).min(v.duree_ms);
                let point = match derniere.and_then(|f| f.point) {
                    Some(p) if p + 1 < points.len() => Some(p + 1),
                    Some(p) => Some(p),
                    None => (!points.is_empty()).then_some(0),
                };
                let f = Frontiere { point, debut_ms: ms, extrait: String::new(), raison: String::new(), declenchement: Declenchement::Manuel };
                v.lignes.push(Ligne { heure: clock(ms), f });
                let i = v.lignes.len() - 1;
                v.deplacer(i, ms);
                trier = true;
            }
            ui.add_space(12.0);
            if occupe {
                ui.spinner();
                ui.label("Rangement de l'audio par point…");
            } else {
                valider = ui.add(egui::Button::new(RichText::new("Valider le découpage").color(Color32::WHITE)).fill(BLEU)).clicked();
                if !v.nouvelle {
                    annuler = ui.button("Annuler").clicked();
                }
            }
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

#[cfg(test)]
mod tests {
    use super::lire_heure;

    #[test]
    fn heures_saisies() {
        assert_eq!(lire_heure("1:02:03"), Some(3_723_000));
        assert_eq!(lire_heure(" 12:34 "), Some(754_000));
        assert_eq!(lire_heure("45"), Some(45_000));
        assert_eq!(lire_heure("1 h"), None);
    }
}
