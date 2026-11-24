//! Installation des modèles, sans aucun choix ni clic : au premier démarrage, et ensuite pour
//! ce qui manque seulement (modèle effacé, modèle remplacé par une nouvelle version). Plein écran tant que
//! quelque chose doit être téléchargé ; les modèles le sont l'un après l'autre, dans un fil, et
//! une fermeture de l'application en cours de route reprend au démarrage suivant (`.part`).
//!
//! Un modèle déjà présent mais jamais vérifié (copié à l'avance par le service informatique) est
//! vérifié sans afficher l'écran : celui-ci n'apparaît que si l'empreinte est mauvaise.

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, CornerRadius, RichText, Sense, Ui, Vec2};

use super::apparence::{gras, palette};
use super::widgets::{BLEU, ROUGE, draw_logo};
use crate::models::{self, Corrompu, Etat, Progress, Requis, Reseau};

/// Attentes avant les nouvelles tentatives automatiques après une erreur réseau.
const REESSAIS: [u64; 3] = [5, 15, 45];
/// Fenêtre du débit moyen pour estimer le temps restant.
const FENETRE: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, PartialEq)]
enum EtatLigne {
    EnAttente,
    Verification,
    EnCours,
    /// Nouvelle tentative automatique à cet instant.
    Reessai(Instant),
    Termine,
    Erreur,
}

struct Ligne {
    requis: Requis,
    /// `AVerifier` (présent, jamais vérifié) ou `ATelecharger`.
    action: Etat,
    etat: EtatLigne,
}

enum Probleme {
    Espace { besoin: u64, libre: u64 },
    Reseau(String),
    Corrompu(&'static str),
    Autre(String),
}

struct Partage {
    lignes: Vec<Ligne>,
    probleme: Option<Probleme>,
    fini: bool,
}

pub(super) struct Installation {
    partage: Arc<Mutex<Partage>>,
    /// Octets du fichier en cours de téléchargement.
    progres: Arc<Progress>,
    /// (instant, octets téléchargés en tout) des 10 dernières secondes.
    echantillons: VecDeque<(Instant, u64)>,
}

impl Installation {
    /// Modèles manquants ou à vérifier ; `None` si tout est prêt. Les anciens modèles de
    /// rédaction ne sont supprimés qu'une fois le modèle actuel installé et vérifié.
    pub(super) fn au_demarrage() -> Option<Self> {
        let lignes: Vec<Ligne> = models::requis()
            .into_iter()
            .map(|requis| (requis, models::etat(requis.file)))
            .filter(|(_, etat)| *etat != Etat::Pret)
            .map(|(requis, action)| Ligne { requis, action, etat: EtatLigne::EnAttente })
            .collect();
        if lignes.is_empty() {
            supprimer_obsoletes();
            return None;
        }
        for l in &lignes {
            log::info!("Installation : {} ({}) {:?}", l.requis.libelle, l.requis.file, l.action);
        }
        let installation = Self {
            partage: Arc::new(Mutex::new(Partage { lignes, probleme: None, fini: false })),
            progres: Arc::new(Progress::default()),
            echantillons: VecDeque::new(),
        };
        installation.lancer();
        Some(installation)
    }

    fn lancer(&self) {
        let (partage, progres) = (self.partage.clone(), self.progres.clone());
        std::thread::spawn(move || installer(&partage, &progres));
    }

    /// Plein écran : quelque chose doit être téléchargé, ou l'installation est bloquée.
    pub(super) fn visible(&self) -> bool {
        let p = self.partage.lock().unwrap();
        p.probleme.is_some() || p.lignes.iter().any(|l| l.action == Etat::ATelecharger)
    }

    /// Modèle utilisable : rien à faire pour lui, ou installé et vérifié.
    pub(super) fn pret(&self, file: &str) -> bool {
        self.partage.lock().unwrap().lignes.iter().all(|l| l.requis.file != file || l.etat == EtatLigne::Termine)
    }

    pub(super) fn terminee(&self) -> bool {
        self.partage.lock().unwrap().fini
    }

    fn reessayer(&mut self) {
        {
            let mut p = self.partage.lock().unwrap();
            p.probleme = None;
            for l in p.lignes.iter_mut().filter(|l| l.etat == EtatLigne::Erreur) {
                l.etat = EtatLigne::EnAttente;
            }
        }
        self.echantillons.clear();
        self.lancer();
    }

    pub(super) fn afficher(&mut self, ui: &mut Ui) {
        let (fait, total) = self.octets();
        let maintenant = Instant::now();
        self.echantillons.push_back((maintenant, fait));
        while self.echantillons.front().is_some_and(|(t, _)| maintenant - *t > FENETRE) {
            self.echantillons.pop_front();
        }
        let mut reessayer = false;
        let pal = palette(ui);
        let largeur = ui.available_width().min(620.0);
        ui.add_space((ui.available_height() * 0.10).min(70.0));
        ui.vertical_centered(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(88.0), Sense::hover());
            draw_logo(ui.painter(), rect);
            ui.add_space(18.0);
            ui.label(gras("Bienvenue dans Tour de table", 28.0).color(pal.texte));
            ui.add_space(4.0);
            ui.label(
                RichText::new("Tour de table prépare ses outils de reconnaissance. Cette étape n'a lieu qu'une fois ; ensuite, tout fonctionne sans connexion.")
                    .size(15.0)
                    .color(pal.texte2),
            );
            ui.add_space(22.0);
            ui.allocate_ui(Vec2::new(largeur, 0.0), |ui| {
                egui::Frame::new()
                    .fill(pal.surface)
                    .stroke(egui::Stroke::new(1.0, pal.bordure))
                    .corner_radius(egui::CornerRadius::same(18))
                    .inner_margin(egui::Margin::symmetric(24, 20))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let p = self.partage.lock().unwrap();
                        for l in &p.lignes {
                            self.ligne(ui, l, maintenant);
                            ui.add_space(12.0);
                        }
                        let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 9.0), Sense::hover());
                        ui.painter().hline(r.x_range(), r.center().y, egui::Stroke::new(1.0, pal.bordure));
                        let reste = match (&p.probleme, self.restant(total - fait)) {
                            (Some(_), _) => String::new(),
                            (None, Some(reste)) => reste,
                            (None, None) => "estimation du temps restant…".to_string(),
                        };
                        ui.horizontal(|ui| {
                            ui.label(gras(format!("{} sur {}", taille(fait), taille(total)), 13.5).color(pal.texte));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(RichText::new(reste).size(13.5).color(pal.texte));
                            });
                        });
                        if let Some(pb) = &p.probleme {
                            ui.add_space(16.0);
                            reessayer = probleme(ui, pb);
                        }
                    });
                // Moins de 16 Go : pas de modèle de rédaction ; on dit pourquoi et ce qui reste possible.
                if let Some(gio) = crate::models::memoire_insuffisante() {
                    ui.add_space(14.0);
                    egui::Frame::new()
                        .fill(pal.ambre_doux)
                        .stroke(egui::Stroke::new(1.0, pal.ambre_bord))
                        .corner_radius(egui::CornerRadius::same(14))
                        .inner_margin(egui::Margin::symmetric(16, 12))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(gras(format!("Cet ordinateur a {} Go de mémoire vive", format!("{gio:.1}").replace('.', ",")), 14.0).color(pal.ambre_texte));
                            ui.label(
                                RichText::new("Il en faut au moins 16 pour rédiger les comptes rendus sur place. La transcription fonctionne ; pour le compte rendu, « Copier pour un assistant en ligne » prépare une demande à coller dans Copilot ou ChatGPT.")
                                    .size(13.0)
                                    .color(pal.ambre_texte),
                            );
                        });
                }
            });
        });
        if reessayer {
            self.reessayer();
        }
    }

    fn ligne(&self, ui: &mut Ui, l: &Ligne, maintenant: Instant) {
        let pal = palette(ui);
        let (fait, total) = (self.progres.done.load(Ordering::Relaxed), self.progres.total.load(Ordering::Relaxed));
        let (etat, couleur, fraction) = match l.etat {
            EtatLigne::EnAttente => ("En attente".to_string(), pal.texte2, 0.0),
            EtatLigne::Verification => ("Vérification…".to_string(), pal.accent, 1.0),
            // L'empreinte est calculée dès le dernier octet reçu.
            EtatLigne::EnCours if total > 0 && fait >= total => ("Vérification…".to_string(), pal.accent, 1.0),
            EtatLigne::EnCours => (format!("{} sur {}", taille(fait), taille(l.requis.octets)), pal.accent, fait as f32 / l.requis.octets as f32),
            EtatLigne::Reessai(t) => (
                format!("Nouvelle tentative dans {} s…", t.saturating_duration_since(maintenant).as_secs() + 1),
                pal.ambre_texte,
                fait as f32 / l.requis.octets as f32,
            ),
            EtatLigne::Termine => ("Prêt".to_string(), pal.vert, 1.0),
            EtatLigne::Erreur => ("Erreur".to_string(), pal.rouge, 0.0),
        };
        let termine = l.etat == EtatLigne::Termine;
        ui.horizontal(|ui| {
            // Rond plein coché quand c'est prêt, anneau bleu sinon.
            let (r, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
            if termine {
                ui.painter().circle_filled(r.center(), 11.0, pal.vert);
                let q = |x: f32, y: f32| r.center() + egui::vec2(x, y);
                ui.painter().line(vec![q(-4.5, 0.5), q(-1.5, 3.5), q(4.5, -3.0)], egui::Stroke::new(2.2, egui::Color32::WHITE));
            } else {
                ui.painter().circle_filled(r.center(), 11.0, pal.accent_doux);
                ui.painter().circle_stroke(r.center(), 9.5, egui::Stroke::new(2.0, if l.etat == EtatLigne::EnAttente { pal.bordure_forte } else { pal.accent }));
            }
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.label(gras(l.requis.libelle, 14.0).color(pal.texte));
                ui.label(RichText::new(taille(l.requis.octets)).size(12.0).color(pal.texte2));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(gras(etat, 13.0).color(couleur));
            });
        });
        let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 6.0), Sense::hover());
        let piste = egui::Rect::from_min_max(egui::pos2(r.left() + 34.0, r.top()), r.max);
        ui.painter().rect_filled(piste, egui::CornerRadius::same(3), pal.piste);
        let mut plein = piste;
        plein.set_width(piste.width() * fraction.clamp(0.0, 1.0));
        ui.painter().rect_filled(plein, egui::CornerRadius::same(3), if termine { pal.vert } else { pal.accent });
    }

    /// Octets téléchargés et à télécharger en tout (les vérifications seules ne comptent pas).
    fn octets(&self) -> (u64, u64) {
        let p = self.partage.lock().unwrap();
        let fichier = self.progres.done.load(Ordering::Relaxed);
        p.lignes.iter().filter(|l| l.action == Etat::ATelecharger).fold((0, 0), |(fait, total), l| {
            let f = match l.etat {
                EtatLigne::Termine => l.requis.octets,
                EtatLigne::EnCours | EtatLigne::Reessai(_) => fichier.min(l.requis.octets),
                _ => 0,
            };
            (fait + f, total + l.requis.octets)
        })
    }

    /// Temps restant d'après le débit moyen des 10 dernières secondes (au moins 2 s de mesure).
    fn restant(&self, octets: u64) -> Option<String> {
        let (&(t0, o0), &(t1, o1)) = (self.echantillons.front()?, self.echantillons.back()?);
        let duree = (t1 - t0).as_secs_f64();
        let debit = o1.saturating_sub(o0) as f64 / duree;
        if duree < 2.0 || debit < 1.0 {
            return None;
        }
        Some(duree_restante((octets as f64 / debit) as u64))
    }
}

impl Drop for Installation {
    fn drop(&mut self) {
        // Fermeture en cours de téléchargement : la partie reçue est écrite pour être reprise.
        self.progres.cancel.store(true, Ordering::Relaxed);
    }
}

/// Fil de l'installation : vérifications, espace disque, puis téléchargements un par un.
fn installer(partage: &Mutex<Partage>, progres: &Arc<Progress>) {
    let modifier = |i: usize, etat: EtatLigne| partage.lock().unwrap().lignes[i].etat = etat;
    let bloquer = |i: usize, pb: Probleme| {
        let mut p = partage.lock().unwrap();
        p.lignes[i].etat = EtatLigne::Erreur;
        p.probleme = Some(pb);
    };
    let n = partage.lock().unwrap().lignes.len();

    for i in 0..n {
        let (requis, action, etat) = {
            let p = partage.lock().unwrap();
            (p.lignes[i].requis, p.lignes[i].action, p.lignes[i].etat)
        };
        if action != Etat::AVerifier || etat == EtatLigne::Termine {
            continue;
        }
        modifier(i, EtatLigne::Verification);
        match models::verifier_installe(requis.file) {
            Ok(()) => {
                log::info!("Modèle {} déjà présent, empreinte vérifiée", requis.file);
                modifier(i, EtatLigne::Termine);
            }
            Err(e) => {
                log::error!("Modèle {} présent mais invalide, supprimé : {e:#}", requis.file);
                let mut p = partage.lock().unwrap();
                p.lignes[i].action = Etat::ATelecharger;
                p.lignes[i].etat = EtatLigne::EnAttente;
            }
        }
    }

    let a_telecharger: Vec<(usize, Requis)> = {
        let p = partage.lock().unwrap();
        p.lignes
            .iter()
            .enumerate()
            .filter(|(_, l)| l.action == Etat::ATelecharger && l.etat != EtatLigne::Termine)
            .map(|(i, l)| (i, l.requis))
            .collect()
    };
    let restant: u64 = a_telecharger.iter().map(|(_, r)| models::octets_restants(r)).sum();
    let besoin = restant + restant / 10;
    let dossier = crate::config::models_dir();
    let libre = models::espace_libre(&dossier);
    log::info!("Installation : {restant} octets à télécharger, {libre:?} octets libres dans {}", dossier.display());
    if let Some(libre) = libre
        && libre < besoin
    {
        log::error!("Espace disque insuffisant dans {} : {libre} octets libres, {besoin} nécessaires", dossier.display());
        let mut p = partage.lock().unwrap();
        p.probleme = Some(Probleme::Espace { besoin, libre });
        return;
    }

    for (i, requis) in a_telecharger {
        let (mut corrompus, mut reessais) = (0, 0);
        loop {
            modifier(i, EtatLigne::EnCours);
            let avant = requis.octets - models::octets_restants(&requis);
            progres.total.store(requis.octets, Ordering::Relaxed);
            progres.done.store(avant, Ordering::Relaxed);
            let Err(e) = models::download(requis.file, progres.clone()) else {
                log::info!("Modèle {} téléchargé et vérifié", requis.file);
                modifier(i, EtatLigne::Termine);
                break;
            };
            if progres.cancel.load(Ordering::Relaxed) {
                return;
            }
            log::error!("Téléchargement de {} : {e:#}", requis.file);
            if e.downcast_ref::<Corrompu>().is_some() {
                corrompus += 1;
                if corrompus < 2 {
                    continue;
                }
                return bloquer(i, Probleme::Corrompu(requis.libelle));
            }
            if e.downcast_ref::<Reseau>().is_none() {
                return bloquer(i, Probleme::Autre(format!("{e:#}")));
            }
            // Une tentative qui a progressé remet le compte à zéro : seul un serveur
            // injoignable de façon répétée bloque l'installation.
            if progres.done.load(Ordering::Relaxed) > avant {
                reessais = 0;
            }
            let Some(&attente) = REESSAIS.get(reessais) else {
                return bloquer(i, Probleme::Reseau(format!("{e:#}")));
            };
            reessais += 1;
            modifier(i, EtatLigne::Reessai(Instant::now() + Duration::from_secs(attente)));
            std::thread::sleep(Duration::from_secs(attente));
        }
    }
    supprimer_obsoletes();
    partage.lock().unwrap().fini = true;
}

fn supprimer_obsoletes() {
    for p in models::modeles_redaction_obsoletes() {
        match std::fs::remove_file(&p) {
            Ok(()) => log::info!("Ancien modèle de rédaction supprimé : {}", p.display()),
            Err(e) => log::warn!("Suppression de {} : {e}", p.display()),
        }
    }
}

/// Encadré d'erreur avec « Réessayer » ; vrai si le bouton est cliqué.
fn probleme(ui: &mut Ui, pb: &Probleme) -> bool {
    let mut clic = false;
    egui::Frame::new()
        .fill(ROUGE.gamma_multiply(0.12))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            match pb {
                Probleme::Espace { besoin, libre } => {
                    ui.label(RichText::new("Espace disque insuffisant").strong().color(ROUGE));
                    ui.label(format!(
                        "Il faut {} d'espace libre pour installer les modèles, et il en reste {}. Libérez au moins {} sur ce disque, puis cliquez sur « Réessayer ».",
                        taille(*besoin),
                        taille(*libre),
                        taille(besoin - libre)
                    ));
                    ui.label(RichText::new(format!("Dossier des modèles : {}", crate::config::models_dir().display())).small().weak());
                }
                Probleme::Reseau(detail) => {
                    ui.label(RichText::new("Impossible de joindre le serveur des modèles").strong().color(ROUGE));
                    ui.label("Vérifiez la connexion Internet, puis cliquez sur « Réessayer ».");
                    ui.label(
                        "Sur un réseau d'organisation, demandez au service informatique d'autoriser huggingface.co \
                         et les adresses qui se terminent par .hf.co (serveurs de téléchargement de Hugging Face).",
                    );
                    ui.label(RichText::new(detail).small().weak());
                }
                Probleme::Corrompu(libelle) => {
                    ui.label(RichText::new("Téléchargement endommagé").strong().color(ROUGE));
                    ui.label(format!(
                        "Le fichier « {libelle} » est arrivé endommagé deux fois de suite. Vérifiez la connexion (un proxy peut altérer les téléchargements), puis cliquez sur « Réessayer »."
                    ));
                }
                Probleme::Autre(detail) => {
                    ui.label(RichText::new("L'installation a échoué").strong().color(ROUGE));
                    ui.label(detail);
                }
            }
            ui.add_space(6.0);
            clic = ui
                .add(egui::Button::new(RichText::new("Réessayer").color(Color32::WHITE)).fill(BLEU).min_size(Vec2::new(140.0, 32.0)))
                .clicked();
        });
    clic
}

/// « 574 Mo », « 2,7 Go ».
fn taille(octets: u64) -> String {
    if octets >= 1_000_000_000 {
        format!("{:.1} Go", octets as f64 / 1e9).replace('.', ",")
    } else {
        format!("{} Mo", (octets as f64 / 1e6).round())
    }
}

fn duree_restante(s: u64) -> String {
    match s {
        0..60 => "moins d'une minute restante".to_string(),
        60..3600 => format!("environ {} min restantes", s.div_ceil(60)),
        _ => format!("environ {} h {:02} restantes", s / 3600, (s % 3600).div_ceil(60).min(59)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tailles_et_durees() {
        assert_eq!(taille(574_041_195), "574 Mo");
        assert_eq!(taille(2_740_937_888), "2,7 Go");
        assert_eq!(duree_restante(42), "moins d'une minute restante");
        assert_eq!(duree_restante(61), "environ 2 min restantes");
        assert_eq!(duree_restante(3600 + 5 * 60), "environ 1 h 05 restantes");
    }
}
