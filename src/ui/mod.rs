//! Interface native (egui) : rendu GPU immédiat, aucun navigateur embarqué.

mod agenda;
mod arriere_plan;
mod apparence;
mod barre_taches;
mod colonne;
mod decoupage;
mod ecoute;
mod installation;
mod live;
mod nettoyage;
mod nouvelle;
mod onglets;
mod past;
mod puce;
mod voix;
mod settings;
mod suppression;
mod mode_decoupage;
mod verification;
mod widgets;

use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use eframe::egui::{self, Align, Color32, CornerRadius, Layout, RichText, Ui};

use crate::audio::{self, DeviceInfo, dsp::Utterance};
use crate::config::{self, Config, Theme};
use crate::models;
use crate::quebec::Glossary;
use crate::recorder::Recorder;
use crate::storage::{self, Agenda, Meeting};
use crate::stt::{Engine, Reply, Request, Settings};
use apparence::style;
use widgets::{ROUGE, SALLE};

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

/// Transcription, au démarrage, de la fin d'une rencontre interrompue par une panne : l'audio qui
/// attendait Whisper au moment de l'arrêt.
struct FinApresPanne {
    id: String,
    titre: String,
    /// Nombre d'énoncés envoyés à Whisper, une fois tous lus.
    resultat: Receiver<Result<usize, String>>,
    envoyes: Option<usize>,
}

/// Rencontre archivée et chemin de son archive, ou l'erreur.
type FinArchivage = (Meeting, Result<std::path::PathBuf, String>);

/// Titre de la rencontre supprimée (vide pour un import annulé), et le résultat.
type FinSuppression = (String, Result<(), String>);

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
    notices: Vec<Avis>,
    /// Ordre du jour déposé avant de démarrer : rattaché à la prochaine rencontre.
    draft_agenda: Option<Agenda>,
    /// Fichier audio en cours d'import (nom affiché, résultat : durée, avancement).
    import_job: Option<TacheImport>,
    /// Rencontres interrompues dont la fin reste à transcrire, et celle en cours.
    fins_a_transcrire: Vec<String>,
    /// Imports interrompus pendant leur transcription, à reprendre (`live::reprendre_import`).
    imports_a_reprendre: Vec<String>,
    fin_apres_panne: Option<FinApresPanne>,
    /// La rencontre en cours a été poursuivie après une panne : elle a déjà son audio et sa
    /// transcription, et n'est jamais supprimée, même sans parole dans la nouvelle partie.
    poursuivie: bool,
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
    /// Fiche d'une rencontre : onglets affichés.
    onglets: onglets::Onglets,
    /// Rencontre en cours avec un ordre du jour : onglets affichés.
    onglets_direct: onglets::Onglets,
    /// Étalonnage de la puce graphique en cours, et début de la période sans tâche.
    etalonnage: Option<crate::gpu::Etalonnage>,
    inoccupe_depuis: Instant,
    /// Reconnaissance des voix en cours (processus à part), rencontres en attente, et fiche dont
    /// les noms des voix sont ouverts.
    voix_job: Option<crate::voix::Calcul>,
    voix_en_attente: Vec<String>,
    /// Voix en direct pendant l'enregistrement ou l'import (`voix_direct`).
    voix_direct: Option<crate::voix_direct::Direct>,
    /// Rencontre importée pour laquelle les voix en direct ont été lancées (une seule fois par import).
    voix_direct_import: Option<String>,
    noms_des_voix: Option<String>,
    /// Archivages en cours (compression dans un fil) : la rencontre revient dans l'historique si l'archivage échoue.
    archivage: (Sender<FinArchivage>, Receiver<FinArchivage>),
    archivages_en_cours: usize,
    /// Arrière-plan (Windows) : fenêtre fermée pendant un travail, cachée jusqu'à la fin (`arriere_plan`).
    arriere_plan: arriere_plan::ArrierePlan,
    /// Début de la transcription d'un import, ou de la fin d'un enregistrement (après « Arrêter ») : son temps est
    /// noté dans la rencontre à la fin (`Meeting::temps`).
    debut_transcription: Option<Instant>,
    /// Dossier d'un import annulé : retiré une fois le fil de l'import arrêté et les derniers énoncés revenus de
    /// Whisper, qui ne doivent jamais aller dans une autre rencontre. Rien d'autre n'est transcrit d'ici là.
    annulation: Option<std::path::PathBuf>,
    /// Suppressions définitives en cours (titre, résultat), et rencontre dont on demande confirmation.
    suppression: (Sender<FinSuppression>, Receiver<FinSuppression>),
    confirmer_suppression: Option<String>,
    /// « Annuler l'import » cliqué une première fois : la carte demande confirmation.
    confirmer_annulation: bool,
    /// Écoute d'un passage ou d'un point de la fiche ; transcription à faire défiler jusqu'à une heure (rencontre, ms).
    ecoute: Option<ecoute::Ecoute>,
    aller_a: Option<(String, u64)>,
    /// Heure où l'écoute vient de sauter par le lecteur (rencontre, ms) : la transcription y défile.
    saut_ecoute: Option<(String, u64)>,
    /// Plages surlignées dans la transcription (point choisi dans le compte rendu), et onglet à afficher.
    surligne: Option<(String, Vec<(u64, u64)>)>,
    transcription_demandee: bool,
    /// Version du Microsoft Store : réponse attendue du Store, puis mise à jour offerte (bannière).
    verification_mise_a_jour: Receiver<bool>,
    mise_a_jour_offerte: bool,
    /// Mise à jour lancée depuis l'application : avancement envoyé par le fil du Store, dernier état reçu.
    installation_maj: Option<(Receiver<crate::mise_a_jour::Etat>, crate::mise_a_jour::Etat)>,
    /// Fenêtre de l'application (Windows), à qui appartient la fenêtre de mise à jour du Store.
    fenetre: Option<isize>,
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
            fins_a_transcrire: Vec::new(),
            imports_a_reprendre: Vec::new(),
            fin_apres_panne: None,
            poursuivie: false,
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
            onglets: onglets::Onglets::default(),
            onglets_direct: onglets::Onglets::direct(),
            etalonnage: None,
            inoccupe_depuis: Instant::now(),
            voix_job: None,
            voix_en_attente: Vec::new(),
            voix_direct: None,
            voix_direct_import: None,
            noms_des_voix: None,
            archivage: crossbeam_channel::unbounded(),
            archivages_en_cours: 0,
            arriere_plan: arriere_plan::ArrierePlan::new(&cc.egui_ctx),
            debut_transcription: None,
            annulation: None,
            suppression: crossbeam_channel::unbounded(),
            confirmer_suppression: None,
            confirmer_annulation: false,
            ecoute: None,
            aller_a: None,
            saut_ecoute: None,
            surligne: None,
            transcription_demandee: false,
            verification_mise_a_jour: {
                let ctx = cc.egui_ctx.clone();
                crate::mise_a_jour::verifier(move || ctx.request_repaint())
            },
            mise_a_jour_offerte: false,
            installation_maj: None,
            fenetre: crate::mise_a_jour::fenetre(cc),
            ctx: cc.egui_ctx.clone(),
            cfg,
        };
        app.reprendre_rencontres_interrompues();
        app.reprendre_nettoyages();
        app.reprendre_voix_provisoires();
        app.appliquer_theme();
        app.push_settings();
        app.load_model();
        if let Some(gio) = models::memoire_insuffisante() {
            app.notify(
                format!(
                    "Cet ordinateur a {} Go de mémoire vive : il en faut au moins 16 pour rédiger les comptes rendus sur place. \
                     La transcription fonctionne ; pour le compte rendu, « Copier pour un assistant en ligne » (onglet Compte rendu) \
                     prépare une demande à coller dans Copilot ou ChatGPT.",
                    format!("{gio:.1}").replace('.', ",")
                ),
                true,
            );
        }
        app
    }

    /// Bannière « Une nouvelle version est disponible » (version du Microsoft Store). La mise à jour
    /// ferme l'application : elle attend la fin de la rencontre et des travaux en cours. Une fois lancée, la bannière
    /// montre son avancement ; Windows ferme puis relance Tour de table pour l'installer.
    fn bandeau_mise_a_jour(&mut self, ui: &mut Ui) {
        if let Some((_, etat)) = &self.installation_maj {
            use crate::mise_a_jour::Etat;
            let p = apparence::palette(ui);
            let (texte, part) = match etat {
                Etat::Telechargement(f) => (format!("Téléchargement de la nouvelle version : {} %", (f * 100.0).round()), Some(*f)),
                Etat::Installation => ("Installation : Tour de table va se fermer, puis se rouvrir tout seul.".to_string(), None),
                _ => ("Préparation de la mise à jour…".to_string(), None),
            };
            egui::Frame::new().fill(p.accent_doux).corner_radius(CornerRadius::same(12)).inner_margin(egui::Margin { left: 16, right: 16, top: 8, bottom: 8 }).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 34.0), Layout::left_to_right(Align::Center), |ui| {
                    ui.label(apparence::gras(texte, 14.0).color(p.accent_texte));
                    if let Some(f) = part {
                        ui.add_space(12.0);
                        ui.add(egui::ProgressBar::new(f).desired_width(ui.available_width().min(260.0)));
                    } else {
                        ui.add_space(8.0);
                        ui.spinner();
                    }
                });
            });
            return;
        }
        let occupe = self.recorder.is_some()
            || self.finishing
            || self.import_job.is_some()
            || self.summary_job.is_some()
            || self.decoupage_job.is_some()
            || self.verification.is_some();
        let p = apparence::palette(ui);
        egui::Frame::new().fill(p.accent_doux).corner_radius(CornerRadius::same(12)).inner_margin(egui::Margin { left: 16, right: 10, top: 8, bottom: 8 }).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 34.0), Layout::left_to_right(Align::Center), |ui| {
                ui.label(apparence::gras("Une nouvelle version de Tour de table est disponible.", 14.0).color(p.accent_texte));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let bouton = ui
                        .add_enabled(!occupe, apparence::bouton_widget(ui, "Mettre à jour", apparence::Genre::Principal, 34.0))
                        .on_hover_text("Télécharge et installe la nouvelle version par le Microsoft Store ; Tour de table se ferme, puis se rouvre")
                        .on_disabled_hover_text("Après la rencontre et les travaux en cours : la mise à jour ferme Tour de table");
                    if bouton.clicked() {
                        let ctx = self.ctx.clone();
                        let rx = crate::mise_a_jour::installer(self.fenetre, move || ctx.request_repaint());
                        self.installation_maj = Some((rx, crate::mise_a_jour::Etat::Attente));
                        self.mise_a_jour_offerte = false;
                    }
                    if ui.add(apparence::bouton_widget(ui, "Plus tard", apparence::Genre::Discret, 34.0)).clicked() {
                        self.mise_a_jour_offerte = false;
                    }
                });
            });
        });
    }

    /// Avancement de la mise à jour lancée depuis l'application. Refusée : l'avis revient ; échec : la page du Store
    /// s'est ouverte à la place.
    fn pump_mise_a_jour(&mut self) {
        use crate::mise_a_jour::Etat;
        let Some((rx, etat)) = &mut self.installation_maj else { return };
        let mut fin = None;
        while let Ok(e) = rx.try_recv() {
            match e {
                Etat::Annulee | Etat::Echec(_) => fin = Some(e),
                autre => *etat = autre,
            }
        }
        match fin {
            Some(Etat::Annulee) => {
                self.installation_maj = None;
                self.mise_a_jour_offerte = true;
            }
            Some(_) => {
                self.installation_maj = None;
                self.notify("La mise à jour n'a pas pu se faire d'ici : la page du Microsoft Store s'est ouverte pour la terminer.", false);
            }
            None => {}
        }
    }

    /// Une mise à jour est lancée : Windows fermera bientôt Tour de table. Rien de neuf ne démarre d'ici là.
    pub(super) fn mise_a_jour_en_cours(&mut self) -> bool {
        if self.installation_maj.is_none() {
            return false;
        }
        self.notify("Une mise à jour est en cours : Tour de table va se fermer pour l'installer, puis se rouvrir. Attendez avant de commencer.", true);
        true
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
            let _ = self.engine.tx.send(Request::Load { path: config::model_path(), use_gpu: crate::gpu::whisper() });
        } else {
            self.model = ModelState::Missing;
        }
    }

    pub(super) fn notify(&mut self, msg: impl Into<String>, error: bool) {
        let msg = msg.into();
        if error {
            log::error!("{msg}");
        }
        self.notices.retain(|a| a.texte != msg);
        self.notices.push(Avis { texte: msg, erreur: error, depuis: Instant::now() });
        if self.notices.len() > 4 {
            self.notices.remove(0);
        }
    }

    /// Rencontres interrompues par un plantage ou une coupure de courant : l'audio est réparé
    /// (seuls les blocs ouverts au moment de l'arrêt sont relus) et la période en cours fermée.
    fn reprendre_rencontres_interrompues(&mut self) {
        for i in 0..self.meetings.len() {
            let dir = self.meetings[i].dir();
            // Import interrompu : sa transcription reprend là où elle était rendue (puis le découpage par points).
            if crate::audio::import::en_attente(&dir) && crate::audio::import::source_copiee(&dir).is_some() {
                let titre = self.meetings[i].title.clone();
                self.imports_a_reprendre.push(self.meetings[i].id.clone());
                self.notify(format!("L'import de « {titre} » s'est interrompu : sa transcription reprend là où elle était rendue."), false);
                continue;
            }
            // Import (ou découpage) interrompu, sans copie du fichier : tout va hors point.
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
                // Application fermée pendant la rencontre : audio intact, mais la fin attendait Whisper.
                if self.meetings[i].interrompue {
                    self.fins_a_transcrire.push(self.meetings[i].id.clone());
                }
                continue;
            }
            let m = &mut self.meetings[i];
            let titre = m.title.clone();
            m.interrompue = true;
            let id = m.id.clone();
            match crate::audio::blocs::reprendre(m, &dir).and_then(|()| m.save()) {
                Ok(()) => {
                    self.notify(
                        format!("La rencontre « {titre} » s'est interrompue brusquement : son audio a été réparé. « Poursuivre la rencontre », dans sa fiche, reprend l'enregistrement."),
                        false,
                    );
                    self.fins_a_transcrire.push(id);
                }
                Err(e) => self.notify(format!("Reprise de la rencontre « {titre} » : {e:#}"), true),
            }
        }
    }

    /// Fin d'une rencontre interrompue : ses énoncés non transcrits passent par Whisper, une
    /// rencontre à la fois, quand rien d'autre n'utilise la transcription.
    fn pump_fin_apres_panne(&mut self) {
        if let Some(f) = self.fin_apres_panne.as_mut() {
            if f.envoyes.is_none() {
                match f.resultat.try_recv() {
                    Ok(Ok(n)) => f.envoyes = Some(n),
                    Ok(Err(e)) => {
                        let titre = f.titre.clone();
                        self.fin_apres_panne = None;
                        self.notify(format!("Transcription de la fin de « {titre} » : {e}"), true);
                        return;
                    }
                    Err(_) => return,
                }
            }
            if self.in_flight > 0 || !self.utt_rx.is_empty() {
                return;
            }
            let Some(f) = self.fin_apres_panne.take() else { return };
            if f.envoyes.unwrap_or(0) > 0
                && let Some(m) = self.meetings.iter().find(|m| m.id == f.id)
            {
                if let Err(e) = m.save().and_then(|_| m.export_all().map(|_| ())) {
                    self.notify(format!("Sauvegarde : {e:#}"), true);
                }
                self.notify(format!("La fin de « {} », interrompue par la panne, est maintenant transcrite.", f.titre), false);
            }
            return;
        }
        let libre = matches!(self.model, ModelState::Ready)
            && self.recorder.is_none()
            && self.live.is_none()
            && !self.finishing
            && self.import_job.is_none()
            && self.annulation.is_none();
        if !libre || self.fins_a_transcrire.is_empty() {
            return;
        }
        let id = self.fins_a_transcrire.remove(0);
        let Some(m) = self.meetings.iter().find(|m| m.id == id).cloned() else { return };
        let (tx, rx) = crossbeam_channel::bounded(1);
        let utt_tx = self.utt_tx.clone();
        std::thread::spawn(move || {
            let r = (|| -> anyhow::Result<usize> {
                let enonces = crate::audio::blocs::enonces_non_transcrits(&m, &m.dir(), crate::config::VAD_SENSITIVITY)?;
                let n = enonces.len();
                for u in enonces {
                    utt_tx.send(u).map_err(|_| anyhow::anyhow!("Application fermée"))?;
                }
                Ok(n)
            })();
            if let Ok(n) = &r {
                log::info!("Fin de « {} » après une panne : {n} énoncés à transcrire", m.title);
            }
            let _ = tx.send(r.map_err(|e| format!("{e:#}")));
        });
        let titre = self.meetings.iter().find(|m| m.id == id).map(|m| m.title.clone()).unwrap_or_default();
        self.fin_apres_panne = Some(FinApresPanne { id, titre, resultat: rx, envoyes: None });
    }

    /// Traitement hors affichage : canaux des fils audio, Whisper et tâches.
    fn pump(&mut self) {
        self.surveiller_puce();
        self.pump_archivage();
        self.surveiller_ecoute();
        if let Ok(oui) = self.verification_mise_a_jour.try_recv() {
            self.mise_a_jour_offerte = oui;
        }
        self.pump_mise_a_jour();
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
                // Import annulé : son dossier est retiré par `pump_suppression`, une fois Whisper vidé.
                _ if self.annulation.is_some() => log::info!("Import « {name} » annulé : fil arrêté"),
                Ok(duration) => {
                    log::info!("Import « {name} » : {duration} s, tous les blocs envoyés à la transcription");
                    if let Some(m) = self.live.as_mut() {
                        m.duration_s = duration;
                    }
                    self.finishing = true;
                }
                Err(e) => {
                    self.voix_direct = None;
                    self.voix_direct_import = None;
                    self.debut_transcription = None;
                    if let Some(m) = self.live.take() {
                        let _ = std::fs::remove_dir_all(m.dir());
                    }
                    self.notify(format!("Import de « {name} » : {e}"), true);
                    self.signaler();
                }
            }
        }
        while let Ok(u) = self.utt_rx.try_recv() {
            // Énoncés d'un import annulé, envoyés avant l'arrêt de son fil : jetés.
            if self.annulation.is_some() {
                continue;
            }
            self.voix_direct_pour_import();
            if (self.recorder.is_some() || self.import_job.is_some())
                && let Some(d) = &self.voix_direct
            {
                d.envoyer(&u);
            }
            let context = match (&self.live, &self.fin_apres_panne) {
                (Some(m), _) => m.recent_text(),
                (None, Some(f)) => self.meetings.iter().find(|m| m.id == f.id).map(|m| m.recent_text()).unwrap_or_default(),
                (None, None) => String::new(),
            };
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
                    let id_fin = self.fin_apres_panne.as_ref().map(|f| f.id.clone());
                    let cible = match self.live.as_mut() {
                        Some(m) => Some(m),
                        None => self.meetings.iter_mut().find(|m| Some(&m.id) == id_fin.as_ref()),
                    };
                    if let Some(m) = cible {
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
        self.recevoir_voix_direct();
        self.pump_suppression();
        if self.finishing && self.in_flight == 0 && self.utt_rx.is_empty() {
            self.finalize();
        }
        self.pump_fin_apres_panne();
        self.reprendre_import();

        if self.installation.as_ref().is_some_and(installation::Installation::terminee) {
            self.installation = None;
        }
        if matches!(self.model, ModelState::Verification) && self.installation.as_ref().is_none_or(|i| i.pret(models::RECOMMENDED)) {
            self.load_model();
        }

        if let Some((id, rx, _, debut)) = &self.summary_job
            && let Ok(res) = rx.try_recv()
        {
            let (id, duree) = (id.clone(), debut.elapsed());
            self.summary_job = None;
            match res {
                Ok(text) => {
                    if let Some(i) = self.meetings.iter().position(|m| m.id == id) {
                        let m = &mut self.meetings[i];
                        m.summary = Some(text);
                        let (titre, avec_ordre_du_jour) = (m.title.clone(), m.agenda.is_some());
                        self.arriere_plan.compte_rendu_pret(&titre, avec_ordre_du_jour);
                        let m = &mut self.meetings[i];
                        crate::storage::Temps::ajouter(&mut m.temps.compte_rendu_s, duree);
                        // Compte rendu rédigé : la rencontre est terminée, on ne la poursuit plus.
                        m.interrompue = false;
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
        self.surveiller_voix();
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
        if self.finishing || self.decoupage_job.is_some() || self.nettoyage_en_cours() || self.archivages_en_cours > 0 {
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

}

impl eframe::App for TourDeTableApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump();
        self.gerer_arriere_plan(ctx);
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
        self.colonne(ui);
        if self.mise_a_jour_offerte || self.installation_maj.is_some() {
            egui::Panel::top("mise_a_jour")
                .show_separator_line(false)
                .frame(egui::Frame::new().fill(apparence::palette(ui).fond).inner_margin(egui::Margin { left: 24, right: 24, top: 16, bottom: 0 }))
                .show(ui, |ui| self.bandeau_mise_a_jour(ui));
        }
        if matches!(self.view, View::Live) && self.verification.is_none() {
            self.raccourcis_points(&ctx);
            if self.recorder.is_some() {
                let p = apparence::palette(ui);
                // La marge autour de la carte prend le fond de la fenêtre (sinon elle reste noire).
                egui::Panel::top("enregistrement")
                    .show_separator_line(false)
                    .frame(egui::Frame::new().fill(p.fond).inner_margin(egui::Margin { left: 8, right: 8, top: 10, bottom: 2 }))
                    .show(ui, |ui| {
                        egui::Frame::new()
                            .fill(p.surface)
                            .stroke(egui::Stroke::new(1.0, p.bordure))
                            .corner_radius(CornerRadius::same(16))
                            .inner_margin(egui::Margin::symmetric(18, 12))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                self.bandeau_enregistrement(ui);
                            });
                    });
            } else if self.live.is_some() || self.rencontre_en_finition().is_some() {
                // Import ou fin de transcription : les étapes du traitement, en haut de l'écran.
                let p = apparence::palette(ui);
                egui::Panel::top("traitement")
                    .show_separator_line(false)
                    .frame(egui::Frame::new().fill(p.fond).inner_margin(egui::Margin { left: 8, right: 8, top: 10, bottom: 2 }))
                    .show(ui, |ui| {
                        egui::Frame::new()
                            .fill(p.surface)
                            .stroke(egui::Stroke::new(1.0, p.bordure))
                            .corner_radius(CornerRadius::same(16))
                            .inner_margin(egui::Margin::symmetric(18, 14))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                self.carte_traitement(ui);
                            });
                    });
            }
        }
        egui::CentralPanel::default_margins().show(ui, |ui| {
            match self.view {
                // « Découper par points » se fait dans la fiche ; l'écran de vérification reste pour la fin d'un import.
                _ if self.verification.is_some() => {
                    if !self.mode_decoupage(ui) {
                        self.verification_view(ui);
                    }
                }
                View::Live => self.live_view(ui),
                View::Past(i) if i < self.meetings.len() => self.past_view(ui, i),
                View::Past(_) => self.view = View::Live,
            }
        });
        if self.show_settings {
            self.settings_window(&ctx);
        }
        self.fenetre_suppression(&ctx);
        self.afficher_avis(&ctx);
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

/// Avis affiché en bannière : une information disparaît seule, une erreur reste jusqu'à ce qu'on
/// la ferme.
pub(super) struct Avis {
    texte: String,
    erreur: bool,
    depuis: Instant,
}

/// Durée d'affichage d'une information (le survol de la souris la prolonge).
const DUREE_AVIS: Duration = Duration::from_secs(6);

impl TourDeTableApp {
    /// Bannières dans le coin en bas à droite (au bout de la barre « Démarrer »), par-dessus le
    /// contenu ; plusieurs s'empilent vers le haut.
    fn afficher_avis(&mut self, ctx: &egui::Context) {
        self.notices.retain(|a| a.erreur || a.depuis.elapsed() < DUREE_AVIS);
        if self.notices.is_empty() {
            return;
        }
        let mut fermer = None;
        let zone = egui::Area::new(egui::Id::unique("avis"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::RIGHT_BOTTOM, [-16.0, -12.0])
            .show(ctx, |ui| {
                ui.set_max_width(440.0);
                for (i, a) in self.notices.iter().enumerate() {
                    let couleur = if a.erreur { ROUGE } else { SALLE };
                    egui::Frame::popup(ui.style()).stroke(egui::Stroke::new(1.5, couleur)).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.add(egui::Label::new(RichText::new(&a.texte).color(couleur)).wrap());
                            if ui.small_button("×").on_hover_text("Fermer").clicked() {
                                fermer = Some(i);
                            }
                        });
                    });
                    ui.add_space(4.0);
                }
            });
        if let Some(i) = fermer {
            self.notices.remove(i);
        }
        // Lu sous la souris : le compte à rebours recommence.
        if zone.response.contains_pointer() {
            for a in &mut self.notices {
                a.depuis = Instant::now();
            }
        }
        // Réveil à la disparition de la prochaine information (une erreur ne fait pas redessiner).
        if let Some(reste) = self.notices.iter().filter(|a| !a.erreur).map(|a| DUREE_AVIS.saturating_sub(a.depuis.elapsed())).min() {
            ctx.request_repaint_after(reste);
        }
    }
}

impl Drop for TourDeTableApp {
    fn drop(&mut self) {
        // Fermeture pendant une rencontre : on garde ce qui a été transcrit.
        if let Some(rec) = self.recorder.take() {
            if let Some(m) = self.live.as_mut() {
                m.duration_s = rec.origine_ms / 1000 + rec.started.elapsed().as_secs();
                // Les derniers énoncés n'auront pas le temps d'être transcrits : ils le seront au
                // prochain démarrage, et la rencontre pourra être poursuivie.
                m.interrompue = true;
            }
            let echantillons = rec.echantillons.clone();
            rec.stop();
            if let Some(m) = self.live.as_mut() {
                let fin = crate::recorder::position_ms(&echantillons);
                m.fermer_periode(fin);
                m.fermer_pause(fin);
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
