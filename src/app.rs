//! Interface native (egui) : rendu GPU immédiat, aucun navigateur embarqué.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use eframe::egui::{
    self, Align, Color32, CornerRadius, Layout, Pos2, RichText, Sense, Ui, Vec2,
};

use crate::audio::{self, DeviceInfo, Source, dsp::Utterance};
use crate::config::{CaptureMode, Config, LlmBackend};
use crate::models::{self, Progress};
use crate::quebec::Glossary;
use crate::recorder::{Options, Recorder};
use crate::storage::{self, Agenda, Meeting, clock};
use crate::stt::{Engine, Reply, Request, Settings};

/// Bleu du drapeau du Québec.
pub const BLEU: Color32 = Color32::from_rgb(0x00, 0x3D, 0xA5);
const BLEU_CLAIR: Color32 = Color32::from_rgb(0x3B, 0x82, 0xF6);
const ROUGE: Color32 = Color32::from_rgb(0xDC, 0x26, 0x26);
const SALLE: Color32 = Color32::from_rgb(0x0E, 0x9F, 0x6E);
const DISTANCE: Color32 = Color32::from_rgb(0x7C, 0x3A, 0xED);

fn source_color(s: Source) -> Color32 {
    match s {
        Source::Salle => SALLE,
        Source::Distance => DISTANCE,
        Source::Mixte => BLEU_CLAIR,
    }
}

enum ModelState {
    Missing,
    Loading,
    Ready(String),
    Failed(String),
}

enum View {
    Live,
    Past(usize),
}

struct Download {
    file: String,
    progress: Arc<Progress>,
    done: Receiver<Result<(), String>>,
}

pub struct TourDeTableApp {
    cfg: Config,
    engine: Engine,
    model: ModelState,
    mics: Vec<DeviceInfo>,
    system_sources: Vec<DeviceInfo>,

    recorder: Option<Recorder>,
    live: Option<Meeting>,
    utt_tx: Sender<Utterance>,
    utt_rx: Receiver<Utterance>,
    /// Énoncés envoyés à Whisper et pas encore revenus.
    in_flight: usize,
    finishing: bool,

    meetings: Vec<Meeting>,
    view: View,
    show_settings: bool,
    download: Option<Download>,
    /// Rédaction en cours : (rencontre, résultat, jetons rédigés, début).
    summary_job: Option<(String, Receiver<Result<String, String>>, Arc<std::sync::atomic::AtomicUsize>, std::time::Instant)>,
    notices: Vec<(String, bool)>,
    /// Ordre du jour déposé avant de démarrer : rattaché à la prochaine rencontre.
    draft_agenda: Option<Agenda>,
    /// Fichier audio en cours de décodage (nom affiché, résultat : énoncés et durée).
    import_job: Option<(String, Receiver<Result<(Vec<Utterance>, u64), String>>)>,
}

impl TourDeTableApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        style(&cc.egui_ctx);
        let ctx = cc.egui_ctx.clone();
        let engine = Engine::spawn(move || ctx.request_repaint());
        let cfg = Config::load();
        let (utt_tx, utt_rx) = crossbeam_channel::unbounded();
        let mut app = Self {
            engine,
            model: ModelState::Missing,
            mics: audio::list_microphones(),
            system_sources: audio::list_system_sources(),
            recorder: None,
            live: None,
            utt_tx,
            utt_rx,
            in_flight: 0,
            finishing: false,
            meetings: storage::list(),
            view: View::Live,
            show_settings: false,
            download: None,
            summary_job: None,
            notices: Vec::new(),
            draft_agenda: None,
            import_job: None,
            cfg,
        };
        app.push_settings();
        app.load_model();
        app
    }

    fn push_settings(&self) {
        let _ = self.engine.tx.send(Request::Configure(Settings {
            threads: self.cfg.threads,
            beam_size: self.cfg.beam_size,
            glossary: Glossary::parse(&self.cfg.glossary),
            agenda_hint: self
                .live
                .as_ref()
                .and_then(|m| m.agenda.as_ref())
                .map(|a| crate::agenda::hint(&a.text))
                .unwrap_or_default(),
        }));
    }

    fn load_model(&mut self) {
        if models::installed(&self.cfg.model) {
            self.model = ModelState::Loading;
            let _ = self.engine.tx.send(Request::Load { path: self.cfg.model_path(), use_gpu: self.cfg.use_gpu });
        } else {
            self.model = ModelState::Missing;
        }
    }

    fn notify(&mut self, msg: impl Into<String>, error: bool) {
        let msg = msg.into();
        if error {
            log::error!("{msg}");
        }
        self.notices.push((msg, error));
        if self.notices.len() > 4 {
            self.notices.remove(0);
        }
    }

    fn start_recording(&mut self) {
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

    fn stop_recording(&mut self) {
        if let Some(rec) = self.recorder.take() {
            if let Some(m) = self.live.as_mut() {
                m.duration_s = rec.started.elapsed().as_secs();
            }
            rec.stop();
            self.finishing = true;
        }
    }

    fn finalize(&mut self) {
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
            self.meetings.insert(0, m);
            self.view = View::Past(0);
        }
    }

    /// Importe un enregistrement existant (Teams, Zoom, téléphone…) : il devient
    /// une rencontre comme les autres, transcrite par la même chaîne.
    fn import_audio(&mut self, path: &std::path::Path) {
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

    fn pick_audio(&mut self) {
        let picked = rfd::FileDialog::new()
            .set_title("Importer un enregistrement")
            .add_filter("Enregistrement audio ou vidéo", audio::import::EXTENSIONS)
            .pick_file();
        if let Some(path) = picked {
            self.import_audio(&path);
        }
    }

    /// Traitement hors affichage : canaux des fils audio, Whisper et tâches.
    fn pump(&mut self) {
        if let Some((name, rx)) = &self.import_job {
            if let Ok(result) = rx.try_recv() {
                let name = name.clone();
                self.import_job = None;
                match result {
                    Ok((utterances, duration)) => {
                        log::info!("Import « {name} » : {duration} s, {} énoncés", utterances.len());
                        if let Some(m) = self.live.as_mut() {
                            m.duration_s = duration;
                        }
                        for u in utterances {
                            let _ = self.utt_tx.send(u);
                        }
                        self.finishing = true;
                    }
                    Err(e) => {
                        if let Some(m) = self.live.take() {
                            let _ = std::fs::remove_dir_all(m.dir());
                        }
                        self.notify(format!("Import de « {name} » : {e}"), true);
                    }
                }
            }
        }
        while let Ok(u) = self.utt_rx.try_recv() {
            let context = self.live.as_ref().map(|m| m.recent_text()).unwrap_or_default();
            if matches!(self.model, ModelState::Ready(_)) {
                self.in_flight += 1;
                let _ = self.engine.tx.send(Request::Transcribe { utterance: u, context });
            }
        }
        while let Ok(reply) = self.engine.rx.try_recv() {
            match reply {
                Reply::Loaded(Ok(name)) => self.model = ModelState::Ready(name),
                Reply::Loaded(Err(e)) => {
                    self.notify(format!("Modèle : {e}"), true);
                    self.model = ModelState::Failed(e);
                }
                Reply::Segments { segments, handled } => {
                    self.in_flight = self.in_flight.saturating_sub(handled);
                    if let Some(m) = self.live.as_mut() {
                        for s in segments {
                            m.insert(s);
                        }
                        if let Err(e) = m.save() {
                            log::warn!("Sauvegarde incrémentale : {e}");
                        }
                    }
                }
                Reply::Error { message, handled } => {
                    self.in_flight = self.in_flight.saturating_sub(handled);
                    self.notify(message, true);
                }
            }
        }
        if self.finishing && self.in_flight == 0 && self.utt_rx.is_empty() {
            self.finalize();
        }

        if let Some(dl) = &self.download {
            if let Ok(res) = dl.done.try_recv() {
                let file = dl.file.clone();
                self.download = None;
                match res {
                    Ok(()) => {
                        self.notify(format!("Modèle {file} installé."), false);
                        if self.cfg.model == file {
                            self.load_model();
                        }
                    }
                    Err(e) => self.notify(format!("Téléchargement : {e}"), true),
                }
            }
        }

        if let Some((id, rx, _, _)) = &self.summary_job {
            if let Ok(res) = rx.try_recv() {
                let id = id.clone();
                self.summary_job = None;
                match res {
                    Ok(text) => {
                        if let Some(m) = self.meetings.iter_mut().find(|m| m.id == id) {
                            m.summary = Some(text);
                            let _ = m.save().and_then(|_| m.export_all().map(|_| ()));
                        }
                    }
                    Err(e) => self.notify(e, true),
                }
            }
        }
    }

    fn start_download(&mut self, file: &str) {
        let progress = Arc::new(Progress::default());
        let (tx, rx) = crossbeam_channel::bounded(1);
        let (f, p) = (file.to_string(), progress.clone());
        std::thread::spawn(move || {
            let _ = tx.send(models::download(&f, p).map_err(|e| format!("{e:#}")));
        });
        self.download = Some(Download { file: file.to_string(), progress, done: rx });
    }

    fn summarize(&mut self, idx: usize) {
        let m = &self.meetings[idx];
        let (tx, rx) = crossbeam_channel::bounded(1);
        let moteur = self.cfg.moteur();
        let progress = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let progress_thread = progress.clone();
        let title = m.title.clone();
        let date = m.started_at.format("%Y-%m-%d à %H h %M").to_string();
        let duration = clock(m.duration_s * 1000);
        let agenda = m.agenda.as_ref().map(|a| a.text.clone());
        let transcript = m.plain_text();
        std::thread::spawn(move || {
            let req = crate::summary::Request {
                moteur: &moteur,
                progress: Some(&progress_thread),
                title: &title,
                date: &date,
                duration: &duration,
                agenda: agenda.as_deref(),
                transcript: &transcript,
            };
            let _ = tx.send(crate::summary::compte_rendu(&req).map_err(|e| format!("{e:#}")));
        });
        self.summary_job = Some((m.id.clone(), rx, progress, std::time::Instant::now()));
    }

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

    fn attach_agenda(&mut self, path: &std::path::Path) {
        match crate::agenda::read(path) {
            Ok(text) => {
                let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                *self.agenda_slot() = Some(Agenda { file: file.clone(), text });
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
                    self.push_settings();
                }
            }
        }
    }

    fn agenda_card(&mut self, ui: &mut Ui) {
        let mut browse = false;
        let mut remove = false;
        let mut edited = false;
        let mut manual = false;
        let slot = self.agenda_slot().clone();
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
                        });
                    }
                }
            });
            if slot.is_some() {
                egui::CollapsingHeader::new("Voir et modifier").id_salt("agenda_text").show(ui, |ui| {
                    if let Some(a) = self.agenda_slot().as_mut() {
                        edited = ui
                            .add(egui::TextEdit::multiline(&mut a.text).desired_rows(6).desired_width(f32::INFINITY))
                            .lost_focus();
                    }
                });
            }
        });
        if manual {
            *self.agenda_slot() = Some(Agenda { file: "Saisi à la main".into(), text: String::new() });
        }
        if remove {
            *self.agenda_slot() = None;
            self.agenda_changed();
        }
        if edited {
            self.agenda_changed();
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

    // ───────────────────────── Affichage ─────────────────────────

    fn top_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(34.0), Sense::hover());
            draw_logo(ui.painter(), rect);
            ui.vertical(|ui| {
                ui.label(RichText::new("Tour de table").size(20.0).strong());
                ui.label(RichText::new("Tout le monde parle, rien ne se perd.").small().weak());
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Paramètres").clicked() {
                    self.show_settings = !self.show_settings;
                    if self.show_settings {
                        self.mics = audio::list_microphones();
                        self.system_sources = audio::list_system_sources();
                    }
                }
                let (txt, color) = match &self.model {
                    ModelState::Missing => ("Aucun modèle installé".to_string(), ROUGE),
                    ModelState::Loading => ("Chargement du modèle…".to_string(), Color32::from_rgb(0xD9, 0x77, 0x06)),
                    ModelState::Ready(n) => (format!("Modèle : {}", short_model(n)), SALLE),
                    ModelState::Failed(_) => ("Modèle en erreur".to_string(), ROUGE),
                };
                let chip = ui.label(RichText::new(txt).color(color).small());
                if let ModelState::Failed(e) = &self.model {
                    chip.on_hover_text(e);
                }
                let (r, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                ui.painter().circle_filled(r.center(), 4.0, color);
            });
        });
    }

    fn side_list(&mut self, ui: &mut Ui) {
        ui.add_space(6.0);
        let live_label = if self.recorder.is_some() { "Rencontre en cours" } else { "+ Nouvelle rencontre" };
        let selected = matches!(self.view, View::Live);
        if ui
            .add_sized([ui.available_width(), 32.0], egui::Button::selectable(selected, live_label))
            .clicked()
        {
            self.view = View::Live;
        }
        let idle = self.recorder.is_none() && !self.finishing && self.import_job.is_none();
        if ui
            .add_enabled(idle, egui::Button::new("Importer un enregistrement…").min_size(Vec2::new(ui.available_width(), 28.0)))
            .on_hover_text("MP3, M4A, MP4 (Teams), WAV, FLAC ou OGG. Vous pouvez aussi glisser le fichier dans la fenêtre.")
            .clicked()
        {
            self.pick_audio();
        }
        ui.add_space(8.0);
        ui.label(RichText::new("Historique").small().weak());
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            for (i, m) in self.meetings.iter().enumerate() {
                let sel = matches!(self.view, View::Past(j) if j == i);
                let text = format!(
                    "{}\n{} · {}",
                    m.title,
                    m.started_at.format("%Y-%m-%d"),
                    clock(m.duration_s * 1000)
                );
                if ui.add_sized([ui.available_width(), 40.0], egui::Button::selectable(sel, text)).clicked() {
                    self.view = View::Past(i);
                }
            }
            if self.meetings.is_empty() {
                ui.label(RichText::new("Aucune rencontre pour l'instant.").weak());
            }
        });
    }

    fn controls(&mut self, ui: &mut Ui) {
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

    fn live_view(&mut self, ui: &mut Ui) {
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

    fn past_view(&mut self, ui: &mut Ui, idx: usize) {
        let mut action = None;
        {
            let m = &mut self.meetings[idx];
            ui.horizontal(|ui| {
                let resp = ui.add(egui::TextEdit::singleline(&mut m.title).font(egui::TextStyle::Heading).desired_width(ui.available_width() - 330.0));
                if resp.lost_focus() {
                    let _ = m.save();
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Supprimer").clicked() {
                        action = Some("delete");
                    }
                    if ui.button("Dossier").clicked() {
                        action = Some("reveal");
                    }
                    if ui.button("Exporter").on_hover_text("Markdown, texte et sous-titres SRT").clicked() {
                        action = Some("export");
                    }
                    if ui.button("Copier").clicked() {
                        ui.ctx().copy_text(m.plain_text());
                    }
                });
            });
            ui.label(
                RichText::new(format!(
                    "{} · {} · {} segments · {}",
                    m.started_at.format("%Y-%m-%d %H h %M"),
                    clock(m.duration_s * 1000),
                    m.segments.len(),
                    short_model(&m.model)
                ))
                .small()
                .weak(),
            );
        }
        ui.add_space(8.0);

        self.agenda_card(ui);
        ui.add_space(6.0);

        let busy = self
            .summary_job
            .as_ref()
            .filter(|(id, ..)| *id == self.meetings[idx].id)
            .map(|(_, _, p, t0)| (p.load(Ordering::Relaxed), t0.elapsed().as_secs()));
        // Modèle de rédaction intégré pas encore téléchargé : on propose de l'installer.
        let llm_missing = (self.cfg.llm_backend == LlmBackend::Integre && !models::installed(&self.cfg.llm_model))
            .then(|| models::llm_info(&self.cfg.llm_model))
            .flatten();
        let has_agenda = self.meetings[idx].agenda.is_some();
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(if has_agenda { "Compte rendu" } else { "Résumé" }).strong());
                if let Some((jetons, secs)) = busy {
                    ui.spinner();
                    let etat = if jetons == 0 {
                        format!("Lecture de la transcription par le modèle local… {}", clock(secs * 1000))
                    } else {
                        // ≈ 0,75 mot par jeton en français
                        format!("Rédaction en cours : environ {} mots · {}", jetons * 3 / 4, clock(secs * 1000))
                    };
                    ui.label(RichText::new(etat).weak());
                } else if let Some(info) = llm_missing {
                    if let Some(dl) = self.download.as_ref().filter(|d| d.file == info.file) {
                        download_bar(ui, dl);
                    } else if ui
                        .add_enabled(self.download.is_none(), egui::Button::new(format!(
                            "Installer le modèle de rédaction ({:.1} Go, une seule fois)",
                            info.size_mb as f32 / 1000.0
                        )).small())
                        .on_hover_text(format!("{} · {}", info.label, info.note))
                        .clicked()
                    {
                        action = Some("install-llm");
                    }
                } else {
                    let label = match (self.meetings[idx].summary.is_some(), has_agenda) {
                        (true, _) => "Régénérer",
                        (false, true) => "Rédiger le compte rendu",
                        (false, false) => "Rédiger un résumé",
                    };
                    let hover = if has_agenda {
                        "Suit l'ordre du jour, point par point, à partir de la transcription"
                    } else {
                        "Ajoutez un ordre du jour pour obtenir un compte rendu point par point"
                    };
                    if ui
                        .small_button(label)
                        .on_hover_text(format!("{hover}\nModèle local : {}", self.cfg.moteur().nom()))
                        .clicked()
                    {
                        action = Some("summary");
                    }
                    if let Some(sum) = &self.meetings[idx].summary {
                        if ui.small_button("Copier").clicked() {
                            ui.ctx().copy_text(sum.clone());
                        }
                    }
                }
            });
            if let Some(s) = &self.meetings[idx].summary {
                egui::ScrollArea::vertical().id_salt("cr").max_height(360.0).show(ui, |ui| markdown(ui, s));
            }
        });
        ui.add_space(8.0);
        transcript(ui, &self.meetings[idx], false);

        match action {
            Some("delete") => {
                let m = self.meetings.remove(idx);
                match storage::delete(&m) {
                    Ok(()) => self.notify("Rencontre déplacée dans la corbeille de Tour de table.", false),
                    Err(e) => self.notify(format!("{e:#}"), true),
                }
                self.view = View::Live;
            }
            Some("reveal") => storage::reveal(&self.meetings[idx].dir()),
            Some("export") => match self.meetings[idx].export_all() {
                Ok(dir) => storage::reveal(&dir),
                Err(e) => self.notify(format!("Export : {e:#}"), true),
            },
            Some("summary") => self.summarize(idx),
            Some("install-llm") => {
                let file = self.cfg.llm_model.clone();
                self.start_download(&file);
            }
            _ => {}
        }
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_settings;
        let before = (self.cfg.model.clone(), self.cfg.use_gpu);
        let mut changed = false;
        egui::Window::new("Paramètres")
            .open(&mut open)
            .default_width(520.0)
            .collapsible(false)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().max_height(560.0).show(ui, |ui| {
                    ui.heading("Modèle de reconnaissance");
                    for info in models::CATALOG {
                        ui.horizontal(|ui| {
                            let installed = models::installed(info.file);
                            ui.add_enabled_ui(installed, |ui| {
                                ui.radio_value(&mut self.cfg.model, info.file.to_string(), info.label);
                            });
                            ui.label(RichText::new(format!("{} · ~{:.1} Go de RAM", info.note, info.ram_mb as f32 / 1000.0)).small().weak());
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if installed {
                                    ui.label(RichText::new("Installé").small().color(SALLE));
                                } else if self.download.as_ref().is_some_and(|d| d.file == info.file) {
                                    if ui.small_button("Annuler").clicked() {
                                        self.download.as_ref().unwrap().progress.cancel.store(true, Ordering::Relaxed);
                                    }
                                } else if ui
                                    .add_enabled(self.download.is_none(), egui::Button::new(format!("Télécharger · {} Mo", info.size_mb)).small())
                                    .clicked()
                                {
                                    self.start_download(info.file);
                                }
                            });
                        });
                    }
                    if let Some(dl) = &self.download {
                        download_bar(ui, dl);
                    }
                    let extra: Vec<String> = models::local_models()
                        .into_iter()
                        .filter(|f| !models::CATALOG.iter().any(|c| c.file == f))
                        .collect();
                    for f in extra {
                        ui.radio_value(&mut self.cfg.model, f.clone(), format!("{f} (personnalisé)"));
                    }
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Modèle affiné maison? Déposez le fichier ggml .bin dans le dossier des modèles.").small().weak());
                        if ui.small_button("Ouvrir").clicked() {
                            let dir = crate::config::models_dir();
                            let _ = std::fs::create_dir_all(&dir);
                            storage::reveal(&dir);
                        }
                    });

                    ui.separator();
                    ui.heading("Performance");
                    let max = std::thread::available_parallelism().map_or(8, |n| n.get());
                    changed |= ui.add(egui::Slider::new(&mut self.cfg.threads, 1..=max).text("fils de calcul")).changed();
                    changed |= ui
                        .add(egui::Slider::new(&mut self.cfg.beam_size, 1..=8).text("faisceau (précision ↔ vitesse)"))
                        .changed();
                    ui.add_enabled_ui(cfg!(any(feature = "metal", feature = "vulkan", feature = "cuda")), |ui| {
                        ui.checkbox(&mut self.cfg.use_gpu, "Accélération GPU");
                    });

                    ui.separator();
                    ui.heading("Détection de la parole");
                    ui.add(egui::Slider::new(&mut self.cfg.vad_sensitivity, 0.0..=1.0).text("sensibilité"));
                    ui.checkbox(&mut self.cfg.save_audio, "Conserver l'audio (WAV) avec la transcription");

                    ui.separator();
                    ui.heading("Lexique québécois");
                    ui.label(
                        RichText::new(
                            "Un terme par ligne (noms, organismes, acronymes) pour guider la reconnaissance,\nou une correction « mauvais => bon ». Ex. : hydro québec => Hydro-Québec",
                        )
                        .small()
                        .weak(),
                    );
                    changed |= ui
                        .add(egui::TextEdit::multiline(&mut self.cfg.glossary).desired_rows(6).desired_width(f32::INFINITY))
                        .changed();

                    ui.separator();
                    ui.heading("Rédaction des comptes rendus");
                    ui.radio_value(&mut self.cfg.llm_backend, LlmBackend::Integre, "Modèle intégré (recommandé : rien à installer)");
                    ui.radio_value(&mut self.cfg.llm_backend, LlmBackend::Ollama, "Ollama (avancé, si vous l'utilisez déjà)");
                    ui.add_space(4.0);
                    if self.cfg.llm_backend == LlmBackend::Integre {
                        for info in models::LLM_CATALOG {
                            ui.horizontal(|ui| {
                                let installed = models::installed(info.file);
                                ui.radio_value(&mut self.cfg.llm_model, info.file.to_string(), info.label);
                                ui.label(
                                    RichText::new(format!("{} · {:.1} Go · ~{:.1} Go de RAM", info.note, info.size_mb as f32 / 1000.0, info.ram_mb as f32 / 1000.0))
                                        .small()
                                        .weak(),
                                );
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if installed {
                                        ui.label(RichText::new("Installé").small().color(SALLE));
                                    } else if self.download.as_ref().is_some_and(|d| d.file == info.file) {
                                        if ui.small_button("Annuler").clicked() {
                                            self.download.as_ref().unwrap().progress.cancel.store(true, Ordering::Relaxed);
                                        }
                                    } else if ui
                                        .add_enabled(self.download.is_none(), egui::Button::new("Télécharger").small())
                                        .clicked()
                                    {
                                        self.start_download(info.file);
                                    }
                                });
                            });
                        }
                        if let Some(dl) = self.download.as_ref().filter(|d| models::llm_info(&d.file).is_some()) {
                            download_bar(ui, dl);
                        }
                    } else {
                        ui.horizontal(|ui| {
                            ui.label("Adresse");
                            ui.text_edit_singleline(&mut self.cfg.ollama_url);
                        });
                        ui.horizontal(|ui| {
                            ui.label("Modèle");
                            ui.text_edit_singleline(&mut self.cfg.ollama_model);
                        });
                    }
                });
            });
        if changed {
            self.push_settings();
        }
        if (self.cfg.model.clone(), self.cfg.use_gpu) != before && self.recorder.is_none() {
            self.load_model();
        }
        if self.show_settings && !open {
            self.cfg.save();
        }
        self.show_settings = open;
    }
}

impl eframe::App for TourDeTableApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump();
        if self.recorder.is_some() || self.finishing || self.download.is_some() || self.summary_job.is_some() || self.import_job.is_some() {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let (hovering, dropped) = ctx.input(|i| (!i.raw.hovered_files.is_empty(), i.raw.dropped_files.clone()));
        for f in dropped {
            let path = f.path().to_path_buf();
            if path.as_os_str().is_empty() {
                continue;
            }
            if audio::import::is_audio(&path) {
                self.import_audio(&path);
            } else {
                self.attach_agenda(&path);
            }
        }
        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(6.0);
            self.top_bar(ui);
            ui.add_space(4.0);
        });
        egui::Panel::left("history").resizable(true).default_size(240.0).show(ui, |ui| self.side_list(ui));
        if matches!(self.view, View::Live) {
            egui::Panel::bottom("controls").show(ui, |ui| self.controls(ui));
        }
        egui::CentralPanel::default_margins().show(ui, |ui| {
            for (msg, err) in self.notices.clone() {
                let color = if err { ROUGE } else { SALLE };
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&msg).color(color));
                    if ui.small_button("×").clicked() {
                        self.notices.retain(|(m, _)| *m != msg);
                    }
                });
            }
            match self.view {
                View::Live => self.live_view(ui),
                View::Past(i) if i < self.meetings.len() => self.past_view(ui, i),
                View::Past(_) => self.view = View::Live,
            }
        });
        if self.show_settings {
            self.settings_window(&ctx);
        }
        if hovering {
            let screen = ctx.content_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("depot")));
            painter.rect_filled(screen, CornerRadius::ZERO, Color32::from_black_alpha(170));
            painter.text(
                screen.center(),
                egui::Align2::CENTER_CENTER,
                "Déposez un ordre du jour ou un enregistrement",
                egui::FontId::proportional(26.0),
                Color32::WHITE,
            );
        }
    }
}

impl Drop for TourDeTableApp {
    fn drop(&mut self) {
        // Fermeture pendant une rencontre : on garde ce qui a été transcrit.
        if let Some(rec) = self.recorder.take() {
            if let Some(m) = self.live.as_mut() {
                m.duration_s = rec.started.elapsed().as_secs();
            }
            rec.stop();
        }
        if let Some(m) = &self.live {
            if !m.segments.is_empty() {
                let _ = m.save().and_then(|_| m.export_all().map(|_| ()));
            }
        }
        self.cfg.save();
    }
}

// ───────────────────────── Composants ─────────────────────────

fn transcript(ui: &mut Ui, m: &Meeting, follow: bool) {
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
fn markdown(ui: &mut Ui, md: &str) {
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

fn device_combo(ui: &mut Ui, id: &str, value: &mut Option<String>, devices: &[DeviceInfo], default: &str) {
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

fn meter(ui: &mut Ui, label: &str, level: f32, color: Color32) {
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

fn record_button(ui: &mut Ui, recording: bool, enabled: bool) -> egui::Response {
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

fn download_bar(ui: &mut Ui, dl: &Download) {
    let done = dl.progress.done.load(Ordering::Relaxed) as f32;
    let total = dl.progress.total.load(Ordering::Relaxed).max(1) as f32;
    ui.add(
        egui::ProgressBar::new(done / total)
            .text(format!("{} : {:.0} / {:.0} Mo", short_model(&dl.file), done / 1e6, total / 1e6))
            .desired_width(360.0),
    );
}

fn short_model(file: &str) -> String {
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

fn style(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = Vec2::new(8.0, 6.0);
        style.spacing.button_padding = Vec2::new(10.0, 5.0);
        style.visuals.selection.bg_fill = BLEU_CLAIR.gamma_multiply(0.45);
        style.visuals.hyperlink_color = BLEU_CLAIR;
    });
}
