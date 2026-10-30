//! Interface native (egui) : rendu GPU immédiat, aucun navigateur embarqué.

mod agenda;
mod barre_taches;
mod decoupage;
mod installation;
mod live;
mod nettoyage;
mod past;
mod settings;
mod verification;
mod widgets;

use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use eframe::egui::{self, Align, Color32, CornerRadius, Layout, RichText, Sense, Ui, Vec2};

use crate::audio::{self, DeviceInfo, dsp::Utterance};
use crate::config::{self, Config, Theme};
use crate::models;
use crate::quebec::Glossary;
use crate::recorder::Recorder;
use crate::storage::{self, Agenda, Meeting, clock};
use crate::stt::{Engine, Reply, Request, Settings};
use widgets::{ROUGE, SALLE, draw_logo, style, theme_button};

enum ModelState {
    Missing,
    /// Modèles copiés à l'avance dont l'empreinte est vérifiée avant le premier chargement.
    Verification,
    Loading,
    Ready,
    Failed(String),
}

enum View {
    Live,
    Past(usize),
}

/// Rédaction en cours : (rencontre, résultat, jetons rédigés, début).
type TacheCompteRendu = (String, Receiver<Result<String, String>>, Arc<crate::summary::Avancement>, std::time::Instant);
/// Import d'un fichier audio : nom affiché, résultat (durée, une fois tous les blocs envoyés à
/// la transcription) et avancement.
type TacheImport = (String, Receiver<Result<u64, String>>, Arc<audio::import::Avancement>);

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
    /// Installation des modèles manquants (plein écran tant qu'un téléchargement est nécessaire).
    installation: Option<installation::Installation>,
    /// Rédaction en cours : (rencontre, résultat, jetons rédigés, début).
    summary_job: Option<TacheCompteRendu>,
    barre_taches: barre_taches::BarreTaches,
    notices: Vec<(String, bool)>,
    /// Ordre du jour déposé avant de démarrer : rattaché à la prochaine rencontre.
    draft_agenda: Option<Agenda>,
    /// Fichier audio en cours d'import (nom affiché, résultat : durée, avancement).
    import_job: Option<TacheImport>,
    /// Écran de vérification du découpage par point (fin d'un import, « Découper par points »).
    verification: Option<verification::Verification>,
    /// Rangement de l'audio par point après validation.
    decoupage_job: Option<verification::TacheDecoupage>,
    /// Éditeur des points de l'ordre du jour ouvert (« Modifier les points »).
    edit_points: bool,
    /// Exportation de l'audio en cours : fichiers écrits et avertissements.
    audio_export: Option<Receiver<Result<Vec<String>, String>>>,
    /// Découpage automatique par point de la rencontre en cours.
    detecteur: crate::decoupage::Detecteur,
    /// Point annoncé dont le bouton est mis en évidence, et depuis quand.
    proposition: Option<(usize, std::time::Instant)>,
    bandeau: Option<decoupage::Bandeau>,
    /// Nettoyage de la transcription en cours, et rencontres en attente (id, compte rendu ensuite).
    nettoyage_job: Option<nettoyage::TacheNettoyage>,
    nettoyages_en_attente: Vec<(String, bool)>,
    /// Fiche d'une rencontre : transcription brute plutôt que nettoyée.
    vue_brute: bool,
    ctx: egui::Context,
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
            installation: installation::Installation::au_demarrage(),
            summary_job: None,
            barre_taches: barre_taches::BarreTaches::new(cc),
            notices: Vec::new(),
            draft_agenda: None,
            import_job: None,
            verification: None,
            decoupage_job: None,
            edit_points: false,
            audio_export: None,
            detecteur: Default::default(),
            proposition: None,
            bandeau: None,
            nettoyage_job: None,
            nettoyages_en_attente: Vec::new(),
            vue_brute: false,
            ctx: cc.egui_ctx.clone(),
            cfg,
        };
        app.reprendre_rencontres_interrompues();
        app.reprendre_nettoyages();
        app.appliquer_theme();
        app.push_settings();
        app.load_model();
        if let Some(gio) = models::memoire_insuffisante() {
            app.notify(
                format!(
                    "Cet ordinateur a {} Go de mémoire vive : Tour de table demande au moins 16 Go. \
                     La rédaction du compte rendu risque d'être très lente ou d'échouer.",
                    format!("{gio:.1}").replace('.', ",")
                ),
                true,
            );
        }
        app
    }

    fn appliquer_theme(&self) {
        self.ctx.set_theme(match self.cfg.theme {
            Theme::Systeme => egui::ThemePreference::System,
            Theme::Clair => egui::ThemePreference::Light,
            Theme::Fonce => egui::ThemePreference::Dark,
        });
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
        if self.installation.as_ref().is_some_and(|i| !i.pret(models::RECOMMENDED)) {
            // Chargé dès que l'installation l'a vérifié.
            self.model = ModelState::Verification;
        } else if models::installed(models::RECOMMENDED) {
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

    /// Rencontres interrompues par un plantage ou une coupure de courant : l'audio est réparé
    /// (seuls les blocs ouverts au moment de l'arrêt sont relus) et la période en cours fermée.
    fn reprendre_rencontres_interrompues(&mut self) {
        for i in 0..self.meetings.len() {
            let dir = self.meetings[i].dir();
            // Import (ou découpage) interrompu avant la validation : tout va hors point.
            if crate::audio::import::en_attente(&dir) {
                let m = &mut self.meetings[i];
                let titre = m.title.clone();
                match crate::audio::frontieres::appliquer(&dir, m, &[]).and_then(|()| m.save()) {
                    Ok(()) => self.notify(
                        format!("Le découpage par point de « {titre} » n'a pas été validé : son audio est rangé hors point. Utilisez « Découper par points » dans sa fiche."),
                        false,
                    ),
                    Err(e) => self.notify(format!("Reprise de l'import « {titre} » : {e:#}"), true),
                }
                continue;
            }
            if !crate::audio::blocs::interrompue(&self.meetings[i], &dir) {
                continue;
            }
            let m = &mut self.meetings[i];
            let titre = m.title.clone();
            match crate::audio::blocs::reprendre(m, &dir).and_then(|()| m.save()) {
                Ok(()) => self.notify(format!("La rencontre « {titre} » s'est interrompue brusquement : son audio a été réparé."), false),
                Err(e) => self.notify(format!("Reprise de la rencontre « {titre} » : {e:#}"), true),
            }
        }
    }

    /// Traitement hors affichage : canaux des fils audio, Whisper et tâches.
    fn pump(&mut self) {
        if let Some(rx) = &self.audio_export
            && let Ok(res) = rx.try_recv()
        {
            self.audio_export = None;
            match res {
                Ok(messages) => {
                    for (i, msg) in messages.into_iter().enumerate() {
                        // Le premier message dit où est le fichier ; les avertissements suivent.
                        let avertissement = !msg.starts_with("Audio exporté");
                        if i < 4 || avertissement {
                            self.notify(msg, avertissement);
                        }
                    }
                }
                Err(e) => self.notify(format!("Exportation de l'audio : {e}"), true),
            }
        }
        self.pump_decoupage();
        if let Some((name, rx, _)) = &self.import_job
            && let Ok(result) = rx.try_recv()
        {
            let name = name.clone();
            self.import_job = None;
            match result {
                Ok(duration) => {
                    log::info!("Import « {name} » : {duration} s, tous les blocs envoyés à la transcription");
                    if let Some(m) = self.live.as_mut() {
                        m.duration_s = duration;
                    }
                    self.finishing = true;
                }
                Err(e) => {
                    if let Some(m) = self.live.take() {
                        let _ = std::fs::remove_dir_all(m.dir());
                    }
                    self.notify(format!("Import de « {name} » : {e}"), true);
                    self.signaler();
                }
            }
        }
        while let Ok(u) = self.utt_rx.try_recv() {
            let context = self.live.as_ref().map(|m| m.recent_text()).unwrap_or_default();
            // En chargement : la requête `Load` précède, le fil Whisper les traite dans l'ordre.
            if matches!(self.model, ModelState::Ready | ModelState::Loading) {
                self.in_flight += 1;
                let _ = self.engine.tx.send(Request::Transcribe { utterance: u, context });
            }
        }
        while let Ok(reply) = self.engine.rx.try_recv() {
            match reply {
                Reply::Loaded(Ok(_)) => self.model = ModelState::Ready,
                Reply::Loaded(Err(e)) => {
                    self.notify(format!("Modèle : {e}"), true);
                    self.model = ModelState::Failed(e);
                }
                Reply::Segments { segments, handled } => {
                    self.in_flight = self.in_flight.saturating_sub(handled);
                    self.compter_import(handled);
                    if let Some(m) = self.live.as_mut() {
                        for s in segments.iter().cloned() {
                            m.insert(s);
                        }
                        if let Err(e) = m.save() {
                            log::warn!("Sauvegarde incrémentale : {e}");
                        }
                    }
                    self.detecter(&segments);
                }
                Reply::Error { message, handled } => {
                    self.in_flight = self.in_flight.saturating_sub(handled);
                    self.compter_import(handled);
                    self.notify(message, true);
                }
            }
        }
        if self.finishing && self.in_flight == 0 && self.utt_rx.is_empty() {
            self.finalize();
        }

        if self.installation.as_ref().is_some_and(installation::Installation::terminee) {
            self.installation = None;
        }
        if matches!(self.model, ModelState::Verification) && self.installation.as_ref().is_none_or(|i| i.pret(models::RECOMMENDED)) {
            self.load_model();
        }

        if let Some((id, rx, _, _)) = &self.summary_job
            && let Ok(res) = rx.try_recv()
        {
            let id = id.clone();
            self.summary_job = None;
            match res {
                Ok(text) => {
                    if let Some(i) = self.meetings.iter().position(|m| m.id == id) {
                        let m = &mut self.meetings[i];
                        m.summary = Some(text);
                        let _ = m.save().and_then(|_| m.export_all().map(|_| ()));
                        let titre = m.title.clone();
                        self.notify(format!("Le compte rendu de « {titre} » est prêt : « Visualiser » l'ouvre dans Word."), false);
                    }
                }
                Err(e) => self.notify(e, true),
            }
            self.signaler();
        }
        self.pump_nettoyage();
    }

    /// Avancement montré dans l'icône de la barre des tâches : l'import (bloc par bloc), puis le
    /// compte rendu (point par point) ; les autres travaux longs, sans durée connue.
    fn avancement_global(&self) -> barre_taches::Avancement {
        use barre_taches::Avancement::{Aucun, Fraction, Indetermine};
        use std::sync::atomic::Ordering::Relaxed;
        if let Some((_, _, av)) = &self.import_job {
            let total = av.total.load(Relaxed);
            return if total == 0 { Indetermine } else { Fraction(av.blocs_transcrits() as f32 / total as f32) };
        }
        if let Some((_, _, av, _)) = &self.summary_job {
            // `points + 1` étapes : chaque point, puis l'assemblage (présences, levée).
            let (point, points) = (av.point.load(Relaxed), av.points.load(Relaxed));
            return if points == 0 { Indetermine } else { Fraction(point.saturating_sub(1) as f32 / (points + 1) as f32) };
        }
        if self.finishing || self.decoupage_job.is_some() || self.nettoyage_en_cours() {
            return Indetermine;
        }
        Aucun
    }

    /// Enregistrement ou import qui démarre : Whisper doit être chargé (ou en chargement).
    pub(super) fn whisper_disponible(&mut self) -> bool {
        matches!(self.model, ModelState::Ready | ModelState::Loading)
    }

    /// Énoncés de l'import en cours revenus de Whisper : le fil de l'import envoie le bloc suivant.
    fn compter_import(&self, handled: usize) {
        if let Some((_, _, av)) = &self.import_job {
            av.enonces_transcrits.fetch_add(handled, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Travail long terminé ou qui attend l'utilisateur : l'icône est signalée dans la barre des
    /// tâches (le Dock sous macOS) si l'on travaille dans une autre fenêtre. La fenêtre n'est
    /// jamais réduite ni ramenée au premier plan.
    pub(super) fn signaler(&self) {
        self.ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Informational));
    }

    /// Modèle de rédaction installé et vérifié.
    pub(super) fn redaction_prete(&self) -> bool {
        let file = models::modele_redaction().file;
        self.installation.as_ref().is_none_or(|i| i.pret(file)) && models::installed(file)
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
                if theme_button(ui, self.cfg.theme).clicked() {
                    self.cfg.theme = match self.cfg.theme {
                        Theme::Systeme => Theme::Clair,
                        Theme::Clair => Theme::Fonce,
                        Theme::Fonce => Theme::Systeme,
                    };
                    self.appliquer_theme();
                    self.cfg.save();
                }
                // Le modèle est choisi et installé seul : on ne parle de lui que s'il n'est pas prêt.
                let (txt, color) = match &self.model {
                    ModelState::Ready => return,
                    ModelState::Missing => ("Aucun modèle installé", ROUGE),
                    ModelState::Loading => ("Chargement du modèle…", Color32::from_rgb(0xD9, 0x77, 0x06)),
                    ModelState::Verification => ("Vérification des modèles…", Color32::from_rgb(0xD9, 0x77, 0x06)),
                    ModelState::Failed(_) => ("Modèle en erreur", ROUGE),
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
        // Paramètres au bas de la colonne, comme le compte dans les applications de messagerie.
        egui::Panel::bottom("pied").show_separator_line(false).show(ui, |ui| {
            ui.add_space(4.0);
            if ui.add(egui::Button::new("Paramètres").min_size(Vec2::new(ui.available_width(), 30.0))).clicked() {
                self.show_settings = !self.show_settings;
                if self.show_settings {
                    self.mics = audio::list_microphones();
                    self.system_sources = audio::list_system_sources();
                }
            }
            ui.add_space(6.0);
        });
        ui.add_space(6.0);
        let live_label = if self.recorder.is_some() { "Rencontre en cours" } else { "+ Nouvelle rencontre" };
        let selected = matches!(self.view, View::Live);
        if ui
            .add_sized([ui.available_width(), 32.0], egui::Button::selectable(selected, live_label))
            .clicked()
        {
            self.view = View::Live;
        }
        let idle = self.recorder.is_none() && !self.finishing && self.import_job.is_none() && self.verification.is_none();
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
        let a = self.avancement_global();
        self.barre_taches.afficher(a);
        let ecoute = self.verification.as_ref().is_some_and(verification::Verification::lecture_en_cours);
        if self.recorder.is_some()
            || self.finishing
            || self.installation.is_some()
            || self.summary_job.is_some()
            || self.import_job.is_some()
            || self.decoupage_job.is_some()
            || self.nettoyage_en_cours()
            || ecoute
        {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        if let Some(inst) = self.installation.as_mut().filter(|i| i.visible()) {
            egui::CentralPanel::default_margins().show(ui, |ui| inst.afficher(ui));
            return;
        }
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
        if matches!(self.view, View::Live) && self.verification.is_none() {
            self.raccourcis_points(&ctx);
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
                _ if self.verification.is_some() => self.verification_view(ui),
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
            let echantillons = rec.echantillons.clone();
            rec.stop();
            if let Some(m) = self.live.as_mut() {
                m.fermer_periode(crate::recorder::position_ms(&echantillons));
            }
        }
        if let Some(m) = &self.live
            && !m.segments.is_empty()
        {
            let _ = m.save().and_then(|_| m.export_all().map(|_| ()));
        }
        self.cfg.save();
    }
}
