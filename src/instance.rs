//! Un seul exemplaire de l'interface : relancer Tour de table (menu Démarrer, notification « Ouvrir ») ramène la
//! fenêtre déjà ouverte, ou cachée en arrière-plan, au lieu d'en ouvrir une deuxième. Deux exemplaires chargeraient
//! les modèles deux fois et écriraient dans les mêmes rencontres.
//!
//! Le premier exemplaire garde un verrou sur `instance.verrou` (libéré par le système à sa sortie, même après un
//! plantage). Un exemplaire suivant dépose `ouvrir` dans le dossier de l'application et s'arrête ; le premier le
//! surveille et revient à l'avant-plan.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const VERROU: &str = "instance.verrou";
const OUVRIR: &str = "ouvrir";

/// Verrou du premier exemplaire, gardé jusqu'à sa sortie.
pub struct Verrou(#[allow(dead_code)] File);

pub enum Demarrage {
    /// Premier exemplaire (ou verrou impossible : on démarre comme avant).
    Premier(Option<Verrou>),
    /// Un exemplaire tourne déjà : il a été prévenu, celui-ci s'arrête.
    DejaOuvert,
}

pub fn demarrer() -> Demarrage {
    demarrer_dans(&crate::config::data_dir())
}

fn demarrer_dans(dir: &Path) -> Demarrage {
    let _ = std::fs::create_dir_all(dir);
    let Ok(fichier) = OpenOptions::new().create(true).truncate(false).write(true).open(dir.join(VERROU)) else {
        return Demarrage::Premier(None);
    };
    match fichier.try_lock() {
        Ok(()) => {
            // Une demande laissée par un exemplaire d'avant ne doit pas ramener la fenêtre plus tard.
            let _ = std::fs::remove_file(dir.join(OUVRIR));
            Demarrage::Premier(Some(Verrou(fichier)))
        }
        Err(TryLockError::WouldBlock) => {
            let _ = std::fs::write(dir.join(OUVRIR), b"");
            Demarrage::DejaOuvert
        }
        // Système de fichiers sans verrou : on démarre comme avant.
        Err(TryLockError::Error(_)) => Demarrage::Premier(None),
    }
}

/// Surveille les demandes d'un exemplaire suivant (un fil, deux fois par seconde) : le drapeau rendu passe à vrai, et
/// `reveil` fait passer l'application, même inactive ou cachée.
pub fn surveiller(reveil: impl Fn() + Send + 'static) -> Arc<AtomicBool> {
    surveiller_dans(crate::config::data_dir(), reveil)
}

fn surveiller_dans(dir: PathBuf, reveil: impl Fn() + Send + 'static) -> Arc<AtomicBool> {
    let drapeau = Arc::new(AtomicBool::new(false));
    let d = drapeau.clone();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(500));
            if std::fs::remove_file(dir.join(OUVRIR)).is_ok() {
                d.store(true, Ordering::Relaxed);
                reveil();
            }
        }
    });
    drapeau
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn un_seul_exemplaire() {
        let dir = std::env::temp_dir().join(format!("tdt-instance-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let Demarrage::Premier(Some(verrou)) = demarrer_dans(&dir) else { panic!("premier exemplaire attendu") };
        let drapeau = surveiller_dans(dir.clone(), || {});
        // Le deuxième est refusé et prévient le premier.
        assert!(matches!(demarrer_dans(&dir), Demarrage::DejaOuvert));
        let debut = std::time::Instant::now();
        while !drapeau.load(Ordering::Relaxed) && debut.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(drapeau.load(Ordering::Relaxed));
        // Verrou libéré (sortie du premier) : le suivant démarre.
        drop(verrou);
        assert!(matches!(demarrer_dans(&dir), Demarrage::Premier(Some(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
