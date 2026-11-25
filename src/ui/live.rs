//! Rencontre en cours : enregistrement, import d'un fichier, contrôles et accueil.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use eframe::egui::{self, Color32, CornerRadius, RichText, Sense, Ui, Vec2};

use super::apparence::{Genre, bouton_widget, gras, palette};
use super::widgets::{draw_logo, transcript};
use super::{ModelState, TourDeTableApp, View};
use crate::audio::{self, Source, import};
use crate::config;
use crate::models;
use crate::recorder::{Options, Recorder};
use crate::storage::{Meeting, clock};

impl TourDeTableApp {
    pub(super) fn start_recording(&mut self) {
        self.suspendre_nettoyage();
        if !self.whisper_disponible() {
            return;
        }
        let mut meeting = Meeting::new(models::RECOMMENDED);
        meeting.nommer_d_apres(self.draft_agenda.as_ref());
        meeting.agenda = self.draft_agenda.take();
        if let Err(echec) = self.demarrer(meeting, None) {
            let (meeting, e) = *echec;
            let _ = std::fs::remove_dir_all(meeting.dir());
            self.draft_agenda = meeting.agenda;
            self.notify(format!("Impossible de démarrer : {e:#}"), true);
        }
    }

    /// « Poursuivre la rencontre » : une rencontre arrêtée par une panne reprend dans son dossier,
    /// au point où l'on était rendu, à la suite de son audio et de sa transcription.
    pub(super) fn poursuivre(&mut self, idx: usize) {
        if self.recorder.is_some() || self.live.is_some() || self.finishing || self.import_job.is_some() || self.fin_apres_panne.is_some() {
            self.notify("Attendez la fin de la transcription en cours avant de poursuivre la rencontre.", true);
            return;
        }
        if self.fins_a_transcrire.contains(&self.meetings[idx].id) {
            self.notify("La fin de la rencontre, interrompue par la panne, va d'abord être transcrite : réessayez dans un instant.", false);
            return;
        }
        self.suspendre_nettoyage();
        if !self.whisper_disponible() {
            return;
        }
        let dir = self.meetings[idx].dir();
        let index = match crate::audio::blocs::lire_index(&dir) {
            Ok(i) => i,
            Err(e) => {
                self.notify(format!("Impossible de poursuivre la rencontre : {e:#}"), true);
                return;
            }
        };
        let mut meeting = self.meetings.remove(idx);
        let fin_texte = meeting.segments.iter().map(|s| s.end_ms).max().unwrap_or(0) * audio::SAMPLE_RATE as u64 / 1000;
        let origine = crate::audio::blocs::fin(&index).max(fin_texte);
        let origine_ms = origine * 1000 / audio::SAMPLE_RATE as u64;
        let point = meeting.periodes.last().and_then(|p| p.point);
        meeting.debuter_point(point, origine_ms, crate::storage::Declenchement::Manuel);
        meeting.interrompue = false;
        log::info!("Rencontre « {} » poursuivie à {}", meeting.title, clock(origine_ms));
        let reprise = crate::recorder::Reprise { index, origine, point };
        self.poursuivie = true;
        if let Err(echec) = self.demarrer(meeting, Some(reprise)) {
            self.poursuivie = false;
            let (mut meeting, e) = *echec;
            meeting.fermer_periode(origine_ms);
            meeting.interrompue = true;
            let _ = meeting.save();
            self.meetings.insert(idx, meeting);
            self.notify(format!("Impossible de poursuivre la rencontre : {e:#}"), true);
        }
    }

    /// Ouvre les sources et commence à enregistrer `meeting` ; en cas d'échec, la rend avec l'erreur.
    fn demarrer(&mut self, meeting: Meeting, reprise: Option<crate::recorder::Reprise>) -> Result<(), Box<(Meeting, anyhow::Error)>> {
        // L'audio est toujours conservé : en blocs rangés par point de l'ordre du jour pendant la
        // rencontre, puis en un seul fichier par source.
        let dossier = meeting.dir();
        let _ = std::fs::create_dir_all(&dossier);
        let opts = Options {
            capture_mic: self.cfg.capture_mic,
            capture_system: self.cfg.capture_system,
            mic_device: self.cfg.mic_device.clone(),
            system_device: self.cfg.system_device.clone(),
            mode: self.cfg.mode,
            sensitivity: config::VAD_SENSITIVITY,
            dossier,
            points: titres_points(meeting.agenda.as_ref()),
            date: meeting.started_at.date_naive(),
            reprise,
        };
        match Recorder::start(opts, self.utt_tx.clone()) {
            Ok(rec) => {
                for w in rec.warnings.clone() {
                    self.notify(w, true);
                }
                meeting.save().ok();
                // Une rencontre poursuivie garde ses voix jusqu'au calcul complet de la fin.
                self.lancer_voix_direct(meeting.voix.is_none());
                self.live = Some(meeting);
                self.push_settings();
                self.recorder = Some(rec);
                self.detecteur = Default::default();
                self.proposition = None;
                self.bandeau = None;
                self.view = View::Live;
                self.notices.retain(|a| a.erreur);
                Ok(())
            }
            Err(e) => Err(Box::new((meeting, e))),
        }
    }

    pub(super) fn stop_recording(&mut self) {
        if let Some(rec) = self.recorder.take() {
            if let Some(m) = self.live.as_mut() {
                m.duration_s = rec.origine_ms / 1000 + rec.started.elapsed().as_secs();
            }
            let echantillons = rec.echantillons.clone();
            rec.stop();
            self.voix_direct = None;
            // Fin de la période en cours : après le dernier échantillon transmis.
            if let Some(m) = self.live.as_mut() {
                let fin = crate::recorder::position_ms(&echantillons);
                m.fermer_periode(fin);
                m.fermer_pause(fin);
                let _ = m.save();
            }
            self.edit_points = false;
            self.proposition = None;
            self.bandeau = None;
            self.finishing = true;
        }
    }

    /// Met l'enregistrement en pause ou le reprend : la pause est notée dans la rencontre.
    pub(super) fn basculer_pause(&mut self) {
        let (Some(rec), Some(m)) = (&self.recorder, self.live.as_mut()) else { return };
        let pause = !rec.en_pause();
        let ms = rec.mettre_en_pause(pause);
        if pause {
            m.debuter_pause(ms);
        } else {
            m.fermer_pause(ms);
        }
        log::info!("Enregistrement {} à {}", if pause { "en pause" } else { "repris" }, clock(ms));
        let _ = m.save();
    }

    pub(super) fn finalize(&mut self) {
        self.finishing = false;
        let poursuivie = std::mem::take(&mut self.poursuivie);
        if let Some(m) = self.live.take() {
            if m.segments.is_empty() && !poursuivie {
                self.notify("Aucune parole détectée : rien n'a été enregistré.", false);
                self.signaler();
                let _ = std::fs::remove_dir_all(m.dir());
                return;
            }
            // Import : l'audio est rangé par point avant tout le reste (compte rendu compris).
            if import::en_attente(&m.dir()) {
                if let Err(e) = m.save() {
                    self.notify(format!("Sauvegarde : {e:#}"), true);
                }
                self.decouper_import(m);
                return;
            }
            if let Err(e) = m.save().and_then(|_| m.export_all().map(|_| ())) {
                self.notify(format!("Sauvegarde : {e:#}"), true);
            }
            // Transcription → nettoyage → compte rendu (avec un ordre du jour).
            let (id, with_agenda) = (m.id.clone(), m.agenda.is_some());
            self.meetings.insert(0, m);
            self.view = View::Past(0);
            self.demander_nettoyage(&id, with_agenda, true);
            self.demander_voix(&id);
        }
    }

    /// Importe un enregistrement existant (Teams, Zoom, téléphone…) : il devient
    /// une rencontre comme les autres, transcrite par la même chaîne.
    pub(super) fn import_audio(&mut self, path: &std::path::Path) {
        if self.recorder.is_some() || self.finishing || self.import_job.is_some() {
            self.notify("Attendez la fin de la transcription en cours avant d'importer un fichier.", true);
            return;
        }
        if self.verification.is_some() || self.decoupage_job.is_some() {
            self.notify("Attendez la fin du découpage par point en cours avant d'importer un autre fichier.", true);
            return;
        }
        if !self.whisper_disponible() {
            self.notify("Le modèle de reconnaissance vocale n'est pas encore prêt : réessayez dans un instant.", true);
            return;
        }
        self.suspendre_nettoyage();
        let name = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        // Date de l'enregistrement plutôt que celle de l'import, si connue.
        let started_at = path.metadata().and_then(|m| m.modified()).map_or_else(|_| chrono::Local::now(), Into::into);
        let mut meeting = Meeting::nouvelle(models::RECOMMENDED, Some(name.clone()), started_at);
        meeting.nommer_d_apres(self.draft_agenda.as_ref());
        meeting.agenda = self.draft_agenda.take();
        let _ = meeting.save();
        self.live = Some(meeting);
        self.view = View::Live;
        self.push_settings();

        self.lancer_import(name.clone(), path.to_path_buf(), None);
        self.notify(format!("« {name} » importé : la transcription commence."), false);
    }

    /// Fil de l'import de la rencontre en cours (`self.live`) : copie du fichier (sauf en reprise), blocs, puis
    /// transcription bloc par bloc. En reprise d'un import interrompu (`reprise` : fin du texte déjà transcrit, en
    /// ms), le fichier déjà copié est redécodé et seuls les énoncés qui commencent après sont transcrits.
    pub(super) fn lancer_import(&mut self, name: String, path: std::path::PathBuf, reprise: Option<u64>) {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let avancement = Arc::new(import::Avancement::default());
        let (dir, av, utt_tx) = (self.live.as_ref().map(Meeting::dir).unwrap_or_default(), avancement.clone(), self.utt_tx.clone());
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<u64> {
                let travail = dir.join(import::TRAVAIL);
                let source = match reprise {
                    None => import::copier_source(&path, &dir)?,
                    Some(_) => {
                        // Blocs à moitié écrits par l'import interrompu : récrits d'après la copie.
                        let _ = std::fs::remove_dir_all(&travail);
                        path
                    }
                };
                let deja = reprise.map_or(0, |ms| ms * audio::SAMPLE_RATE as u64 / 1000);
                let index = import::ecrire_blocs(&source, &travail, &av)?;
                import::transcrire_blocs(&travail, &index, |k, enonces| {
                    // Un bloc d'avance au plus : la file de Whisper reste courte, la mémoire aussi.
                    while av.enonces_transcrits.load(Ordering::Relaxed) < av.envoyes_avant(k.saturating_sub(1)) {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    // Reprise : un énoncé est transcrit en entier ou pas du tout ; ceux d'avant la fin du texte le sont.
                    let enonces: Vec<_> = enonces.into_iter().filter(|u| reprise.is_none() || u.start >= deja).collect();
                    av.bloc_envoye(enonces.len());
                    for u in enonces {
                        utt_tx.send(u).map_err(|_| anyhow::anyhow!("Application fermée"))?;
                    }
                    Ok(())
                })?;
                Ok(index.blocs.iter().map(|b| b.decalage + b.echantillons).max().unwrap_or(0) / audio::SAMPLE_RATE as u64)
            })();
            let _ = tx.send(result.map_err(|e| format!("{e:#}")));
        });
        self.import_job = Some((name, rx, avancement));
    }

    /// Import interrompu (application fermée pendant sa transcription) : il reprend dès que la transcription est
    /// libre, là où il était rendu. Appelé à chaque image depuis `pump`.
    pub(super) fn reprendre_import(&mut self) {
        let libre = matches!(self.model, ModelState::Ready)
            && self.recorder.is_none()
            && self.live.is_none()
            && !self.finishing
            && self.import_job.is_none()
            && self.fin_apres_panne.is_none()
            && self.verification.is_none()
            && self.decoupage_job.is_none();
        if !libre || self.imports_a_reprendre.is_empty() {
            return;
        }
        let id = self.imports_a_reprendre.remove(0);
        let Some(i) = self.meetings.iter().position(|m| m.id == id) else { return };
        let Some(source) = import::source_copiee(&self.meetings[i].dir()) else { return };
        let m = self.meetings.remove(i);
        let fin = m.segments.iter().map(|s| s.end_ms).max().unwrap_or(0);
        let name = source.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        log::info!("Reprise de l'import « {} » à {}", m.title, clock(fin));
        self.suspendre_nettoyage();
        self.live = Some(m);
        // L'écran suit le travail ; l'historique a perdu une ligne, une fiche ouverte n'est plus la bonne.
        if matches!(self.view, View::Past(_)) {
            self.view = View::Live;
        }
        self.lancer_import(name, source, Some(fin));
    }

    /// Avancement de l'import (0 à 1), s'il est connu.
    pub(super) fn fraction_import(&self) -> Option<f32> {
        let (_, _, av) = self.import_job.as_ref()?;
        let total = av.total.load(Ordering::Relaxed);
        (total > 0).then(|| av.blocs_transcrits() as f32 / total as f32)
    }

    /// Carte en haut de l'écran pendant un import ou la fin d'une transcription : la rencontre, puis les étapes qui
    /// suivent, la première en cours.
    pub(super) fn carte_traitement(&mut self, ui: &mut Ui) {
        let p = palette(ui);
        let Some(m) = &self.live else { return };
        let titre = m.title.clone();
        let avec_ordre_du_jour = m.agenda.as_ref().is_some_and(|a| !a.points.is_empty());
        let (detail, fraction) = match self.etat_import() {
            Some(etat) => (etat, self.fraction_import()),
            None => (format!("Fin de la transcription · {} énoncés restants", self.in_flight), None),
        };
        let mut etapes: Vec<(&str, String)> = vec![("Transcription", detail)];
        if avec_ordre_du_jour {
            etapes.push(("Découpage par points", "Vous vérifierez où commence chaque point".into()));
        }
        etapes.push(("Nettoyage", "Quelques secondes".into()));
        if avec_ordre_du_jour && crate::models::redaction_locale() {
            etapes.push(("Compte rendu", "Rédigé d'après l'ordre du jour".into()));
        }
        etapes.push(("Voix", "Quand l'ordinateur est libre".into()));
        ui.horizontal(|ui| {
            ui.label(gras(&titre, 20.0).color(p.texte));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new("Vous pouvez consulter d'autres rencontres : le travail continue.").size(12.5).color(p.texte2));
            });
        });
        ui.add_space(10.0);
        ui.columns(etapes.len(), |colonnes| {
            for (k, (nom, detail)) in etapes.iter().enumerate() {
                // Texte aligné à gauche : les colonnes d'egui le justifieraient, mots écartés.
                colonnes[k].with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                let en_cours = k == 0;
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::hover());
                    if en_cours {
                        ui.painter().circle_filled(r.center(), 12.0, p.accent_plein);
                        let angle = ui.input(|i| i.time) as f32 * 5.0;
                        let points: Vec<egui::Pos2> = (0..=18).map(|j| {
                            let a = angle + j as f32 / 18.0 * 4.4;
                            r.center() + Vec2::new(a.cos(), a.sin()) * 6.0
                        }).collect();
                        ui.painter().add(egui::Shape::line(points, egui::Stroke::new(2.0, Color32::WHITE)));
                    } else {
                        ui.painter().circle_filled(r.center(), 12.0, p.piste);
                        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, (k + 1).to_string(), super::colonne::police_grasse(12.0), p.texte);
                    }
                    let titre = if en_cours { gras(*nom, 14.0).color(p.texte) } else { RichText::new(*nom).size(14.0).color(p.texte2) };
                    ui.label(titre);
                });
                let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width() - 10.0, 6.0), Sense::hover());
                ui.painter().rect_filled(r, CornerRadius::same(3), p.piste);
                if en_cours {
                    match fraction {
                        Some(f) => {
                            let mut plein = r;
                            plein.set_width(r.width() * f.clamp(0.02, 1.0));
                            ui.painter().rect_filled(plein, CornerRadius::same(3), p.accent);
                        }
                        // Avancement inconnu : un segment qui va et vient.
                        None => {
                            let t = (ui.input(|i| i.time) as f32 * 0.6).fract();
                            let x = r.left() + (r.width() * 0.7) * (1.0 - (2.0 * t - 1.0).abs());
                            ui.painter().rect_filled(egui::Rect::from_min_size(egui::pos2(x, r.top()), Vec2::new(r.width() * 0.3, r.height())), CornerRadius::same(3), p.accent);
                        }
                    }
                }
                ui.label(RichText::new(detail).size(12.5).color(p.texte2));
                });
            }
        });
    }

    /// Où en est l'import : décodage en blocs, puis transcription bloc par bloc.
    fn etat_import(&self) -> Option<String> {
        let (name, _, av) = self.import_job.as_ref()?;
        let total = av.total.load(Ordering::Relaxed);
        Some(if total == 0 {
            match av.ecrits.load(Ordering::Relaxed) {
                0 => format!("Lecture de « {name} »…"),
                n => format!("Lecture de « {name} » : {n} blocs de 2 minutes écrits…"),
            }
        } else {
            format!("Transcription de « {name} » : bloc {} sur {total}", (av.blocs_transcrits() + 1).min(total))
        })
    }

    pub(super) fn pick_audio(&mut self) {
        let picked = rfd::FileDialog::new()
            .set_title("Importer un enregistrement")
            .add_filter("Enregistrement audio ou vidéo", audio::import::EXTENSIONS)
            .pick_file();
        if let Some(path) = picked {
            self.import_audio(&path);
        }
    }

    /// Bandeau en haut de l'écran pendant l'enregistrement : témoin rouge, chronomètre, niveau de chaque source, Pause
    /// et Arrêter.
    pub(super) fn bandeau_enregistrement(&mut self, ui: &mut Ui) {
        let p = palette(ui);
        let Some(rec) = &self.recorder else { return };
        let pause = rec.en_pause();
        let duree = clock(rec.origine_ms + rec.started.elapsed().as_millis() as u64);
        // Les deux sources, toujours : une source qui n'est pas enregistrée le dit au lieu de disparaître.
        let niveaux: Vec<(&str, Option<f32>, Color32)> = [
            (Source::Salle, "Salle", rec.mic_level.get(), p.accent),
            (Source::Distance, "À distance", rec.sys_level.get(), p.vert),
        ]
        .into_iter()
        .map(|(s, n, l, c)| (n, rec.active.contains(&s).then_some(l), c))
        .collect();
        let en_attente = self.in_flight;
        let mut arreter = false;
        let mut basculer = false;
        ui.horizontal(|ui| {
            // Témoin qui pulse, puis l'état et le chronomètre.
            let (r, _) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::hover());
            if !pause {
                let phase = (ui.input(|i| i.time) as f32 % 1.8) / 1.8;
                ui.painter().circle_filled(r.center(), 6.0 + 7.0 * phase, p.rouge.gamma_multiply(0.45 * (1.0 - phase)));
            }
            ui.painter().circle_filled(r.center(), 6.0, if pause { p.texte2 } else { p.rouge });
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let etat = if pause { "EN PAUSE" } else { "ENREGISTREMENT" };
                ui.label(gras(etat, 11.5).color(if pause { p.texte2 } else { p.rouge_texte }).extra_letter_spacing(0.7));
                ui.label(gras(&duree, 30.0).color(p.texte));
            });
            ui.add_space(18.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                if pause {
                    ui.label(RichText::new("En pause : rien n'est capté, ni transcrit, ni conservé dans l'audio").size(13.0).color(p.texte2));
                } else {
                    let largeur = (ui.available_width() - 260.0).clamp(160.0, 420.0);
                    for (nom, niveau, couleur) in &niveaux {
                        ui.horizontal(|ui| {
                            ui.allocate_ui_with_layout(Vec2::new(78.0, 14.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                ui.set_min_width(78.0);
                                ui.label(RichText::new(*nom).size(13.0).color(p.texte));
                            });
                            match niveau {
                                Some(n) => jauge_segments(ui, *n, *couleur, largeur),
                                None => {
                                    ui.label(RichText::new("non enregistré").size(12.5).color(p.texte2));
                                }
                            }
                        });
                    }
                }
                if en_attente > 1 {
                    ui.label(RichText::new(format!("{en_attente} énoncés en attente de transcription")).size(12.0).color(p.texte2));
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(bouton_widget(ui, "Arrêter", Genre::Danger, 44.0).min_size(Vec2::new(116.0, 44.0))).on_hover_text("Termine la rencontre ; la transcription se termine, puis le nettoyage et le compte rendu suivent").clicked() {
                    arreter = true;
                }
                let texte = if pause { "Reprendre" } else { "Pause" };
                let aide = if pause { "Reprendre la transcription" } else { "Rien n'est capté pendant la pause : ni transcrit, ni conservé dans l'audio" };
                if ui.add(bouton_widget(ui, texte, Genre::Secondaire, 44.0).min_size(Vec2::new(116.0, 44.0))).on_hover_text(aide).clicked() {
                    basculer = true;
                }
            });
        });
        if arreter {
            self.stop_recording();
        } else if basculer {
            self.basculer_pause();
        }
    }

    pub(super) fn live_view(&mut self, ui: &mut Ui) {
        self.bandeau(ui);
        // Avant la rencontre : l'écran « Nouvelle rencontre ».
        if self.live.is_none() && self.recorder.is_none() && !matches!(self.model, ModelState::Missing | ModelState::Failed(_)) {
            self.ecran_nouvelle(ui);
            return;
        }
        let pret = !matches!(self.model, ModelState::Missing | ModelState::Failed(_)) || self.live.is_some();
        // Ordre du jour déposé, avant ou pendant la rencontre : deux onglets pleine hauteur, les mêmes du début à la
        // fin. Sinon, la carte où déposer l'ordre du jour reste au-dessus de la transcription.
        let avec_ordre_du_jour = match &self.live {
            Some(m) => m.agenda.is_some(),
            None => self.draft_agenda.is_some(),
        };
        if pret && avec_ordre_du_jour {
            self.volets_direct(ui, Self::transcription_directe);
            return;
        }
        if pret {
            self.agenda_card(ui, false);
            ui.add_space(6.0);
        }
        self.transcription_directe(ui);
    }

    /// Transcription de la rencontre en cours, ou ce qu'on attend.
    fn transcription_directe(&mut self, ui: &mut Ui) {
        match (&self.live, &self.model) {
            (Some(m), _) => {
                if m.segments.is_empty() {
                    ui.centered_and_justified(|ui| {
                        let text = match self.etat_import() {
                            Some(etat) => etat,
                            None if self.finishing => "Transcription de l'enregistrement…".to_string(),
                            None if m.en_pause() => "En pause : rien n'est capté.".to_string(),
                            None => "À l'écoute… la transcription apparaît au fil de la rencontre.".to_string(),
                        };
                        ui.spinner();
                        ui.label(RichText::new(text).weak());
                    });
                } else {
                    // Voix en direct dans une carte à droite quand l'écran est assez large, comme dans la fiche.
                    if m.voix.is_some() && ui.available_width() >= 900.0 {
                        egui::Panel::right("volet-voix-direct")
                            .exact_size(300.0)
                            .resizable(false)
                            .show_separator_line(false)
                            .frame(egui::Frame::new().inner_margin(egui::Margin { left: 16, right: 0, top: 0, bottom: 0 }))
                            .show(ui, |ui| self.carte_voix_direct(ui));
                    }
                    if let Some(m) = &self.live {
                        transcript(ui, m, true, Default::default());
                    }
                }
            }
            (None, ModelState::Missing | ModelState::Failed(_)) => self.welcome(ui),
            (None, _) => {
                ui.centered_and_justified(|ui| {
                    ui.label(
                        RichText::new("Choisissez vos sources ci-dessous puis cliquez sur « Démarrer ».\nTout est traité sur cet ordinateur, rien n'est envoyé en ligne.")
                            .weak(),
                    );
                });
            }
        }
    }

    /// Modèle de reconnaissance vocale introuvable (effacé depuis le démarrage : l'installation le
    /// remet en place au prochain démarrage) ou en erreur.
    fn welcome(&mut self, ui: &mut Ui) {
        ui.add_space(30.0);
        ui.vertical_centered(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(96.0), Sense::hover());
            draw_logo(ui.painter(), rect);
            ui.add_space(12.0);
            ui.heading("Bienvenue dans Tour de table");
            ui.label(match &self.model {
                ModelState::Failed(_) => "Le modèle de reconnaissance vocale n'a pas pu être chargé : la raison est indiquée en haut à droite et dans le journal.",
                _ => "Le modèle de reconnaissance vocale est introuvable. Redémarrez Tour de table pour le réinstaller.",
            });
        });
    }
}

/// Jauge de niveau en 24 segments ; les derniers passent à l'ambre quand le son est fort.
fn jauge_segments(ui: &mut Ui, niveau: f32, couleur: Color32, largeur: f32) {
    let p = palette(ui);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(largeur, 12.0), Sense::hover());
    // Échelle logarithmique : la parole normale remplit la moitié de la jauge.
    let db = 20.0 * niveau.max(1e-5).log10();
    let allumes = (((db + 60.0) / 60.0).clamp(0.0, 1.0) * 24.0).round() as usize;
    let pas = rect.width() / 24.0;
    for k in 0..24 {
        let r = egui::Rect::from_min_size(egui::pos2(rect.left() + k as f32 * pas, rect.top()), Vec2::new(pas - 3.0, rect.height()));
        let c = if k >= allumes { p.piste } else if k >= 20 { p.ambre_texte } else { couleur };
        ui.painter().rect_filled(r, CornerRadius::same(2), c);
    }
}

/// Titres des points, pour nommer les dossiers des blocs audio.
pub(super) fn titres_points(agenda: Option<&crate::storage::Agenda>) -> Vec<String> {
    agenda.map(|a| a.points.iter().map(|p| p.titre.clone()).collect()).unwrap_or_default()
}
