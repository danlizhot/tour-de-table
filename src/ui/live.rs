//! Rencontre en cours : enregistrement, import d'un fichier, contrôles et accueil.

use eframe::egui::{self, Color32, RichText, Sense, Ui, Vec2};

use super::widgets::{BLEU, DISTANCE, SALLE, device_combo, download_bar, draw_logo, meter, record_button, transcript};
use super::{ModelState, TourDeTableApp, View};
use crate::audio::{self, Source};
use crate::config::CaptureMode;
use crate::models;
use crate::recorder::{Options, Recorder};
use crate::storage::{Meeting, clock};

impl TourDeTableApp {
    pub(super) fn start_recording(&mut self) {
        let meeting = Meeting::new(&self.cfg.model);
        let wav_dir = self.cfg.save_audio.then(|| meeting.dir());
        if let Some(d) = &wav_dir {
            let _ = std::fs::create_dir_all(d);
        }
        let opts = Options {
            capture_mic: self.cfg.capture_mic,
            capture_system: self.cfg.capture_system,
            mic_device: self.cfg.mic_device.clone(),
            system_device: self.cfg.system_device.clone(),
            mode: self.cfg.mode,
            sensitivity: self.cfg.vad_sensitivity,
            wav_dir,
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
            rec.stop();
            self.finishing = true;
        }
    }

    pub(super) fn finalize(&mut self) {
        self.finishing = false;
        if let Some(m) = self.live.take() {
            if m.segments.is_empty() {
                self.notify("Aucune parole détectée : rien n'a été enregistré.", false);
                let _ = std::fs::remove_dir_all(m.dir());
                return;
            }
            if let Err(e) = m.save().and_then(|_| m.export_all().map(|_| ())) {
                self.notify(format!("Sauvegarde : {e:#}"), true);
            }
            let with_agenda = m.agenda.is_some();
            self.meetings.insert(0, m);
            self.view = View::Past(0);
            if with_agenda && self.cfg.auto_compte_rendu {
                self.auto_compte_rendu(0);
            }
        }
    }

    /// Importe un enregistrement existant (Teams, Zoom, téléphone…) : il devient
    /// une rencontre comme les autres, transcrite par la même chaîne.
    pub(super) fn import_audio(&mut self, path: &std::path::Path) {
        if self.recorder.is_some() || self.finishing || self.import_job.is_some() {
            self.notify("Attendez la fin de la transcription en cours avant d'importer un fichier.", true);
            return;
        }
        if !matches!(self.model, ModelState::Ready(_)) {
            self.notify("Installez ou chargez d'abord un modèle de reconnaissance (Paramètres).", true);
            return;
        }
        let name = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut meeting = Meeting::new(&self.cfg.model);
        meeting.title = name.clone();
        // Date de l'enregistrement plutôt que celle de l'import, si connue.
        if let Ok(modified) = path.metadata().and_then(|m| m.modified()) {
            meeting.started_at = modified.into();
        }
        meeting.agenda = self.draft_agenda.take();
        let _ = meeting.save();
        self.live = Some(meeting);
        self.view = View::Live;
        self.push_settings();

        let (tx, rx) = crossbeam_channel::bounded(1);
        let (path, sensitivity) = (path.to_path_buf(), self.cfg.vad_sensitivity);
        std::thread::spawn(move || {
            let result = audio::import::decode(&path).map(|samples| {
                let duration = samples.len() as u64 / audio::SAMPLE_RATE as u64;
                let mut utterances = Vec::new();
                let mut seg = audio::dsp::Segmenter::new(Source::Mixte, sensitivity);
                seg.push(&samples, &mut utterances);
                seg.flush(&mut utterances);
                (utterances, duration)
            });
            let _ = tx.send(result.map_err(|e| format!("{e:#}")));
        });
        self.import_job = Some((name, rx));
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
            let can_start = matches!(self.model, ModelState::Ready(_)) && !self.finishing;
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
            } else if self.finishing {
                ui.spinner();
                ui.label(format!("Fin de la transcription… ({} restants)", self.in_flight));
            } else {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.cfg.capture_mic, "Micro de la salle");
                        device_combo(ui, "mic", &mut self.cfg.mic_device, &self.mics, "Micro par défaut");
                    });
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.cfg.capture_system, "Son de l'ordinateur (Teams, Zoom…)");
                        device_combo(ui, "sys", &mut self.cfg.system_device, &self.system_sources, "Sortie par défaut");
                    });
                });
                ui.add_space(10.0);
                egui::ComboBox::from_id_salt("mode")
                    .selected_text(match self.cfg.mode {
                        CaptureMode::Separe => "Sources séparées",
                        CaptureMode::Mixe => "Sources mixées",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.cfg.mode, CaptureMode::Separe, "Sources séparées")
                            .on_hover_text("Distingue « Salle » et « À distance ». Idéal avec un casque ou un haut-parleur de conférence.");
                        ui.selectable_value(&mut self.cfg.mode, CaptureMode::Mixe, "Sources mixées")
                            .on_hover_text("À choisir si le son des participants à distance sort des haut-parleurs de la salle (évite les doublons).");
                    });
            }
        });
        ui.add_space(6.0);
    }

    pub(super) fn live_view(&mut self, ui: &mut Ui) {
        if !matches!(self.model, ModelState::Missing | ModelState::Failed(_)) || self.live.is_some() {
            self.agenda_card(ui);
            ui.add_space(6.0);
        }
        match (&self.live, &self.model) {
            (Some(m), _) => {
                if m.segments.is_empty() {
                    ui.centered_and_justified(|ui| {
                        let text = match &self.import_job {
                            Some((name, _)) => format!("Lecture de « {name} »…"),
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

    fn welcome(&mut self, ui: &mut Ui) {
        ui.add_space(30.0);
        ui.vertical_centered(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(96.0), Sense::hover());
            draw_logo(ui.painter(), rect);
            ui.add_space(12.0);
            ui.heading("Bienvenue dans Tour de table");
            ui.label("Pour commencer, installez un modèle de reconnaissance vocale (une seule fois).");
            ui.add_space(12.0);
            let info = &models::CATALOG[0];
            if let Some(dl) = &self.download {
                download_bar(ui, dl);
            } else if ui
                .add(egui::Button::new(
                    RichText::new(format!("Télécharger {} ({} Mo)", info.label, info.size_mb)).color(Color32::WHITE),
                ).fill(BLEU).min_size(Vec2::new(280.0, 36.0)))
                .clicked()
            {
                self.cfg.model = info.file.to_string();
                self.start_download(info.file);
            }
            ui.add_space(6.0);
            ui.label(RichText::new("Autres modèles : Paramètres").small().weak());
        });
    }
}
