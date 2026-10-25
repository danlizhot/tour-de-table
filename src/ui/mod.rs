//! Interface native (egui) : rendu GPU immédiat, aucun navigateur embarqué.

mod agenda;
mod live;
mod past;
mod settings;
mod widgets;

use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use eframe::egui::{self, Align, Color32, CornerRadius, Layout, RichText, Sense, Ui, Vec2};

use crate::audio::{self, DeviceInfo, dsp::Utterance};
use crate::config::{self, Config};
use crate::models::{self, Progress};
use crate::quebec::Glossary;
use crate::recorder::Recorder;
use crate::storage::{self, Agenda, Meeting, clock};
use crate::stt::{Engine, Reply, Request, Settings};
use widgets::{ROUGE, SALLE, draw_logo, short_model, style};

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

    pub(super) fn push_settings(&self) {
        let _ = self.engine.tx.send(Request::Configure(Settings {
            threads: config::threads(),
            beam_size: config::BEAM_SIZE,
            glossary: Glossary::parse(&self.cfg.glossary),
            agenda_hint: self
                .live
                .as_ref()
                .and_then(|m| m.agenda.as_ref())
                .map(|a| crate::agenda::hint(&a.text))
                .unwrap_or_default(),
        }));
    }

    pub(super) fn load_model(&mut self) {
        if models::installed(models::RECOMMENDED) {
            self.model = ModelState::Loading;
            let _ = self.engine.tx.send(Request::Load { path: config::model_path(), use_gpu: config::USE_GPU });
        } else {
            self.model = ModelState::Missing;
        }
    }

    pub(super) fn notify(&mut self, msg: impl Into<String>, error: bool) {
        let msg = msg.into();
        if error {
            log::error!("{msg}");
        }
        self.notices.push((msg, error));
        if self.notices.len() > 4 {
            self.notices.remove(0);
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
                        if file == models::RECOMMENDED {
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

    pub(super) fn start_download(&mut self, file: &str) {
        let progress = Arc::new(Progress::default());
        let (tx, rx) = crossbeam_channel::bounded(1);
        let (f, p) = (file.to_string(), progress.clone());
        std::thread::spawn(move || {
            let _ = tx.send(models::download(&f, p).map_err(|e| format!("{e:#}")));
        });
        self.download = Some(Download { file: file.to_string(), progress, done: rx });
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
