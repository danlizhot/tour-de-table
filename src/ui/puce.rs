//! Étalonnage de la puce graphique en arrière-plan, quand l'application est inoccupée.

use std::time::{Duration, Instant};

use super::{ModelState, TourDeTableApp};
use crate::gpu::{self, Etalonnage};
use crate::models;
use crate::stt::Request;

/// Attente après le démarrage (ou la fin d'une tâche) avant de mesurer : une rencontre lancée
/// aussitôt n'est pas ralentie.
const DELAI: Duration = Duration::from_secs(60);

impl TourDeTableApp {
    fn occupe(&self) -> bool {
        self.recorder.is_some()
            || self.finishing
            || self.import_job.is_some()
            || self.summary_job.is_some()
            || self.nettoyage_job.is_some()
            || self.decoupage_job.is_some()
            || self.installation.is_some()
            || !matches!(self.model, ModelState::Ready)
    }

    /// Appelé à chaque image depuis `pump`.
    pub(super) fn surveiller_puce(&mut self) {
        if self.occupe() {
            if let Some(mut e) = self.etalonnage.take() {
                log::info!("Étalonnage de la puce graphique interrompu : il sera repris plus tard");
                e.interrompre();
            }
            self.inoccupe_depuis = Instant::now();
            return;
        }
        if let Some(e) = &self.etalonnage {
            if e.termine() {
                self.etalonnage = None;
                // Whisper retenu sur la puce : rechargé tout de suite.
                if gpu::whisper() {
                    self.model = ModelState::Loading;
                    let _ = self.engine.tx.send(Request::Load { path: crate::config::model_path(), use_gpu: true });
                }
            } else {
                self.ctx.request_repaint_after(Duration::from_secs(2));
            }
            return;
        }
        if !cfg!(feature = "vulkan") {
            return;
        }
        // Sans événement, l'interface ne se redessine pas : on demande d'être rappelé.
        if let Some(reste) = DELAI.checked_sub(self.inoccupe_depuis.elapsed()) {
            self.ctx.request_repaint_after(reste);
            return;
        }
        if !gpu::a_etalonner() || !models::installed(models::modele_redaction().file) {
            return;
        }
        match Etalonnage::lancer() {
            Ok(e) => {
                log::info!("Étalonnage de la puce graphique en arrière-plan");
                self.etalonnage = Some(e);
            }
            Err(e) => {
                log::warn!("{e:#}");
                // Pas de nouvel essai avant le prochain démarrage.
                self.inoccupe_depuis = Instant::now() + Duration::from_secs(365 * 24 * 3600);
            }
        }
        self.ctx.request_repaint_after(Duration::from_secs(2));
    }
}
