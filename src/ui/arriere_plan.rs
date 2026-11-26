//! Fermer la fenêtre pendant un travail (Windows) : Tour de table le termine en arrière-plan. Une notification
//! l'annonce (les premières fois) et une icône près de l'horloge montre l'avancement, avec « Ouvrir » et « Quitter ».
//! À la fin, une notification dit ce qui est prêt et que Tour de table s'est fermé, avec un bouton « Ouvrir » : la
//! fenêtre ne revient jamais d'elle-même (partage d'écran, travail en cours ailleurs). Fermer quand rien ne travaille,
//! ou pendant un enregistrement, ferme l'application comme avant. Ailleurs que sous Windows (ni icône ni
//! notification), la fermeture reste celle d'avant.
//!
//! eframe continue d'appeler `App::logic` (donc `pump`) quand la fenêtre est cachée, environ dix fois par seconde, même
//! sous Windows où une fenêtre cachée ne reçoit plus d'ordre de dessin : la chaîne (transcription, découpage, nettoyage,
//! compte rendu, voix) avance sans écran. Essai sous Linux, 26 novembre 2026 : import de 5 min, fenêtre fermée au
//! départ, tout terminé en 4 min 46 s.
//!
//! Relancer Tour de table (menu Démarrer, « Ouvrir » d'une notification) ramène la fenêtre (`instance`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use eframe::egui;

use super::TourDeTableApp;

/// Fois où l'explication du passage en arrière-plan est montrée.
const AVIS_ARRIERE_PLAN: u32 = 3;

pub(super) struct ArrierePlan {
    /// Fenêtre cachée, travail en cours.
    pub(super) actif: bool,
    /// « Quitter » demandé : la prochaine fermeture n'est plus interceptée.
    quitter: bool,
    /// Un autre exemplaire demande la fenêtre (`instance::surveiller`).
    demande_fenetre: Arc<AtomicBool>,
    /// Dernière rencontre travaillée, et ce qui en est prêt (« Le compte rendu »…), pour la notification de fin.
    rencontre: Option<String>,
    pret: Option<&'static str>,
    maj: Instant,
    #[cfg(windows)]
    icone: Option<(crate::zone_notification::Icone, std::sync::mpsc::Receiver<crate::zone_notification::Demande>)>,
}

impl ArrierePlan {
    pub(super) fn new(ctx: &egui::Context) -> Self {
        let c = ctx.clone();
        Self {
            actif: false,
            quitter: false,
            demande_fenetre: crate::instance::surveiller(move || c.request_repaint()),
            rencontre: None,
            pret: None,
            maj: Instant::now(),
            #[cfg(windows)]
            icone: None,
        }
    }

    /// Un compte rendu (ou résumé) vient d'être rédigé : la notification de fin le dira.
    pub(super) fn compte_rendu_pret(&mut self, titre: &str, avec_ordre_du_jour: bool) {
        if self.actif {
            self.rencontre = Some(titre.to_string());
            self.pret = Some(if avec_ordre_du_jour { "Le compte rendu" } else { "Le résumé" });
        }
    }
}

impl TourDeTableApp {
    /// Travail que la fermeture de la fenêtre interromprait : il peut se terminer en arrière-plan. Les voix en attente
    /// comptent seulement si leurs modèles sont installés (sinon elles attendraient toujours).
    pub(super) fn travail_a_terminer(&self) -> bool {
        self.import_job.is_some()
            || self.finishing
            || self.annulation.is_some()
            || self.decoupage_job.is_some()
            || self.summary_job.is_some()
            || self.nettoyage_job.is_some()
            || !self.nettoyages_en_attente.is_empty()
            || self.voix_job.is_some()
            || (!self.voix_en_attente.is_empty() && crate::voix::modeles_installes())
            || !self.imports_a_reprendre.is_empty()
            || !self.fins_a_transcrire.is_empty()
            || self.fin_apres_panne.is_some()
            || self.archivages_en_cours > 0
    }

    /// Ce que fait Tour de table, en quelques mots (infobulle et menu de l'icône), et la rencontre concernée.
    fn travail_en_cours(&self) -> (String, Option<String>) {
        let titre = |id: &str| self.meetings.iter().find(|m| m.id == id).map(|m| m.title.clone());
        if self.import_job.is_some() || self.finishing {
            let etat = match self.fraction_import() {
                Some(f) => format!("Transcription : {} %", (f * 100.0).round() as u32),
                None => "Transcription".to_string(),
            };
            return (etat, self.live.as_ref().map(|m| m.title.clone()));
        }
        if let Some((_, m, _)) = &self.decoupage_job {
            return ("Rangement de l'audio".into(), Some(m.title.clone()));
        }
        if let Some((id, _, av, _)) = &self.summary_job {
            let (point, points) = (av.point.load(Ordering::Relaxed), av.points.load(Ordering::Relaxed));
            let etat = if points == 0 {
                "Compte rendu".to_string()
            } else if point > points {
                "Compte rendu : dernière étape".to_string()
            } else {
                format!("Compte rendu : point {} sur {points}", point.max(1))
            };
            return (etat, titre(id));
        }
        if let Some(t) = &self.nettoyage_job {
            return ("Nettoyage de la transcription".into(), titre(&t.id));
        }
        if let Some(c) = &self.voix_job {
            return ("Reconnaissance des voix".into(), titre(&c.id));
        }
        ("En attente du travail suivant".into(), None)
    }

    /// Appelé à chaque passage de `logic`, après `pump`.
    pub(super) fn gerer_arriere_plan(&mut self, ctx: &egui::Context) {
        // Relancé ailleurs (menu Démarrer, notification) : la fenêtre revient à l'avant-plan.
        if self.arriere_plan.demande_fenetre.swap(false, Ordering::Relaxed) {
            self.ramener_fenetre(ctx);
        }
        #[cfg(windows)]
        if let Some(d) = self.arriere_plan.icone.as_ref().and_then(|(_, rx)| rx.try_recv().ok()) {
            match d {
                crate::zone_notification::Demande::Ouvrir => self.ramener_fenetre(ctx),
                crate::zone_notification::Demande::Quitter => {
                    log::info!("« Quitter » depuis l'icône près de l'horloge");
                    self.arriere_plan.quitter = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        if ctx.input(|i| i.viewport().close_requested()) {
            let intercepter = cfg!(windows)
                && !self.arriere_plan.quitter
                && self.recorder.is_none()
                && self.verification.is_none()
                && self.travail_a_terminer();
            if intercepter {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                if !self.arriere_plan.actif {
                    self.passer_en_arriere_plan(ctx);
                }
            }
            return;
        }
        if !self.arriere_plan.actif {
            return;
        }
        if self.travail_a_terminer() {
            // Les voix attendent que l'ordinateur soit libre : un passage au moins chaque demi-seconde.
            ctx.request_repaint_after(Duration::from_millis(500));
            if self.arriere_plan.maj.elapsed() >= Duration::from_secs(1) {
                self.arriere_plan.maj = Instant::now();
                let (etat, rencontre) = self.travail_en_cours();
                if rencontre.is_some() {
                    self.arriere_plan.rencontre = rencontre;
                }
                #[cfg(windows)]
                if let Some((icone, _)) = &self.arriere_plan.icone {
                    icone.avancement(&etat, true);
                }
                #[cfg(not(windows))]
                let _ = etat;
            }
            return;
        }
        // Tout est terminé : une notification le dit, avec « Ouvrir », et Tour de table se ferme.
        let texte = match (self.arriere_plan.pret, &self.arriere_plan.rencontre) {
            (Some(quoi), Some(titre)) => format!("{quoi} de « {titre} » est prêt."),
            (None, Some(titre)) => format!("La transcription de « {titre} » est prête."),
            _ => "Le travail en arrière-plan est terminé.".to_string(),
        };
        log::info!("Travail terminé en arrière-plan : Tour de table se ferme");
        // Attendue avant de quitter : sinon elle pourrait ne jamais partir.
        #[cfg(windows)]
        let _ = crate::notification::afficher(
            "Tour de table a terminé",
            &format!("{texte} Tour de table s'est fermé : « Ouvrir » le relance."),
            true,
        )
        .join();
        #[cfg(not(windows))]
        let _ = texte;
        self.arriere_plan.quitter = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// Fenêtre fermée pendant un travail : cachée, icône près de l'horloge, explication les premières fois.
    fn passer_en_arriere_plan(&mut self, ctx: &egui::Context) {
        let (etat, rencontre) = self.travail_en_cours();
        log::info!("Fenêtre fermée pendant un travail : Tour de table continue en arrière-plan ({etat})");
        self.arriere_plan.actif = true;
        self.arriere_plan.rencontre = rencontre.clone();
        self.arriere_plan.pret = None;
        self.ecoute = None;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        #[cfg(windows)]
        {
            let c = ctx.clone();
            self.arriere_plan.icone = crate::zone_notification::Icone::montrer(&etat, move || c.request_repaint());
            if self.cfg.avis_arriere_plan < AVIS_ARRIERE_PLAN {
                self.cfg.avis_arriere_plan += 1;
                self.cfg.save();
                let quoi = rencontre.map_or_else(|| "son travail".to_string(), |t| format!("le travail sur « {t} »"));
                let _ = crate::notification::afficher(
                    "Tour de table continue en arrière-plan",
                    &format!("Il termine {quoi}. L'icône près de l'horloge montre l'avancement ; il vous préviendra à la fin."),
                    false,
                );
            }
        }
        #[cfg(not(windows))]
        let _ = (etat, rencontre, AVIS_ARRIERE_PLAN);
    }

    /// La fenêtre revient à l'avant-plan (relance, icône) ; l'icône près de l'horloge disparaît.
    fn ramener_fenetre(&mut self, ctx: &egui::Context) {
        if self.arriere_plan.actif {
            log::info!("Fenêtre ramenée pendant le travail en arrière-plan");
        }
        self.arriere_plan.actif = false;
        #[cfg(windows)]
        {
            self.arriere_plan.icone = None;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }
}
