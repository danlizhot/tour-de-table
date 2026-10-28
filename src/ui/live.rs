//! Rencontre en cours : enregistrement, import d'un fichier, contrôles et accueil.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use eframe::egui::{RichText, Sense, Ui, Vec2};

use super::widgets::{DISTANCE, SALLE, draw_logo, meter, record_button, transcript};
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
        let meeting = Meeting::new(models::RECOMMENDED);
        // L'audio est toujours conservé, en blocs rangés par point de l'ordre du jour.
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
            points: titres_points(self.draft_agenda.as_ref()),
        };
        match Recorder::start(opts, self.utt_tx.clone()) {
            Ok(rec) => {
                for w in rec.warnings.clone() {
                    self.notify(w, true);
                }
                let mut meeting = meeting;
                meeting.agenda = self.draft_agenda.take();
                meeting.save().ok();
                self.live = Some(meeting);
                self.push_settings();
                self.recorder = Some(rec);
                self.detecteur = Default::default();
                self.proposition = None;
                self.bandeau = None;
                self.view = View::Live;
                self.notices.retain(|(_, e)| *e);
            }
            Err(e) => {
                let _ = std::fs::remove_dir_all(meeting.dir());
                self.notify(format!("Impossible de démarrer : {e:#}"), true);
            }
        }
    }

    pub(super) fn stop_recording(&mut self) {
        if let Some(rec) = self.recorder.take() {
            if let Some(m) = self.live.as_mut() {
                m.duration_s = rec.started.elapsed().as_secs();
            }
            let echantillons = rec.echantillons.clone();
            rec.stop();
            // Fin de la période en cours : après le dernier échantillon transmis.
            if let Some(m) = self.live.as_mut() {
                m.fermer_periode(crate::recorder::position_ms(&echantillons));
                let _ = m.save();
            }
            self.edit_points = false;
            self.proposition = None;
            self.bandeau = None;
            self.finishing = true;
        }
    }

    pub(super) fn finalize(&mut self) {
        self.finishing = false;
        if let Some(m) = self.live.take() {
            if m.segments.is_empty() {
                self.notify("Aucune parole détectée : rien n'a été enregistré.", false);
                self.signaler();
                let _ = std::fs::remove_dir_all(m.dir());
                return;
            }
            // Import : le découpage par point est vérifié avant tout le reste (compte rendu compris).
            if import::en_attente(&m.dir()) {
                if let Err(e) = m.save() {
                    self.notify(format!("Sauvegarde : {e:#}"), true);
                }
                self.signaler();
                self.ouvrir_verification(m, true);
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
            self.notify("Validez d'abord le découpage par point affiché avant d'importer un autre fichier.", true);
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
        meeting.agenda = self.draft_agenda.take();
        let _ = meeting.save();
        self.live = Some(meeting);
        self.view = View::Live;
        self.push_settings();

        let (tx, rx) = crossbeam_channel::bounded(1);
        let avancement = Arc::new(import::Avancement::default());
        let (path, dir, av, utt_tx) = (path.to_path_buf(), self.live.as_ref().map(Meeting::dir).unwrap_or_default(), avancement.clone(), self.utt_tx.clone());
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<u64> {
                let source = import::copier_source(&path, &dir)?;
                let travail = dir.join(import::TRAVAIL);
                let index = import::ecrire_blocs(&source, &travail, &av)?;
                import::transcrire_blocs(&travail, &index, |k, enonces| {
                    // Un bloc d'avance au plus : la file de Whisper reste courte, la mémoire aussi.
                    while av.enonces_transcrits.load(Ordering::Relaxed) < av.envoyes_avant(k.saturating_sub(1)) {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
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

    pub(super) fn controls(&mut self, ui: &mut Ui) {
        ui.add_space(6.0);
        let recording = self.recorder.is_some();
        ui.horizontal(|ui| {
            let can_start = matches!(self.model, ModelState::Ready) && !self.finishing;
            let resp = record_button(ui, recording, recording || can_start);
            if resp.clicked() {
                if recording {
                    self.stop_recording();
                } else {
                    self.start_recording();
                }
            }
            ui.add_space(10.0);
            if let Some(rec) = &self.recorder {
                ui.label(RichText::new(clock(rec.started.elapsed().as_millis() as u64)).size(22.0).monospace());
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    if rec.active.contains(&Source::Salle) {
                        meter(ui, "Salle", rec.mic_level.get(), SALLE);
                    }
                    if rec.active.contains(&Source::Distance) {
                        meter(ui, "À distance", rec.sys_level.get(), DISTANCE);
                    }
                });
                if self.in_flight > 1 {
                    ui.label(RichText::new(format!("{} énoncés en attente", self.in_flight)).small().weak());
                }
            } else if let Some(etat) = self.etat_import() {
                ui.spinner();
                ui.label(etat);
            } else if self.finishing {
                ui.spinner();
                ui.label(format!("Fin de la transcription… ({} restants)", self.in_flight));
            } else {
                // Le choix des périphériques et du mode est dans les Paramètres.
                ui.vertical(|ui| {
                    ui.checkbox(&mut self.cfg.capture_mic, "Micro de la salle");
                    ui.checkbox(&mut self.cfg.capture_system, "Son de l'ordinateur (Teams, Zoom…)");
                });
            }
        });
        ui.add_space(6.0);
    }

    pub(super) fn live_view(&mut self, ui: &mut Ui) {
        self.bandeau(ui);
        if !matches!(self.model, ModelState::Missing | ModelState::Failed(_)) || self.live.is_some() {
            self.agenda_card(ui);
            ui.add_space(6.0);
        }
        match (&self.live, &self.model) {
            (Some(m), _) => {
                if m.segments.is_empty() {
                    ui.centered_and_justified(|ui| {
                        let text = match self.etat_import() {
                            Some(etat) => etat,
                            None if self.finishing => "Transcription de l'enregistrement…".to_string(),
                            None => "À l'écoute… la transcription apparaît au fil de la rencontre.".to_string(),
                        };
                        ui.spinner();
                        ui.label(RichText::new(text).weak());
                    });
                } else {
                    transcript(ui, m, true);
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

/// Titres des points, pour nommer les dossiers des blocs audio.
pub(super) fn titres_points(agenda: Option<&crate::storage::Agenda>) -> Vec<String> {
    agenda.map(|a| a.points.iter().map(|p| p.titre.clone()).collect()).unwrap_or_default()
}
