//! Écoute d'extraits sur la sortie par défaut : frontières d'un import, paragraphe de la transcription, point de
//! l'ordre du jour.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::Receiver;

use super::SAMPLE_RATE;
use super::blocs::Index;

/// Lecture en cours ; s'arrête quand on la laisse tomber.
pub struct Lecture {
    arret: Arc<AtomicBool>,
    fini: Arc<AtomicBool>,
    pause: Arc<AtomicBool>,
    /// Échantillons (16 kHz) déjà joués, toutes plages confondues.
    joues: Arc<AtomicU64>,
    /// Durée totale des plages, en échantillons.
    total: u64,
    pub erreur: Receiver<String>,
}

impl Lecture {
    pub fn en_cours(&self) -> bool {
        !self.fini.load(Ordering::Relaxed)
    }

    pub fn en_pause(&self) -> bool {
        self.pause.load(Ordering::Relaxed)
    }

    pub fn basculer_pause(&self) {
        self.pause.fetch_xor(true, Ordering::Relaxed);
    }

    /// Temps écoulé et durée totale, en ms.
    pub fn avancement_ms(&self) -> (u64, u64) {
        (super::blocs::ms(self.joues.load(Ordering::Relaxed).min(self.total)), super::blocs::ms(self.total))
    }
}

impl Drop for Lecture {
    fn drop(&mut self) {
        self.arret.store(true, Ordering::Relaxed);
    }
}

/// Audio lu d'avance, en morceaux : un point entier n'est jamais chargé d'un coup.
const MORCEAU: u64 = 20 * SAMPLE_RATE as u64;

/// Joue l'audio des blocs de `debut` à `fin` (échantillons) ; extraction et lecture dans un fil.
pub fn jouer(dir: PathBuf, index: Index, debut: u64, fin: u64) -> Lecture {
    jouer_plages(dir, index, vec![(debut, fin)])
}

/// Joue les plages (échantillons) l'une après l'autre ; extraction par morceaux et lecture dans un fil.
pub fn jouer_plages(dir: PathBuf, index: Index, plages: Vec<(u64, u64)>) -> Lecture {
    let arret = Arc::new(AtomicBool::new(false));
    let fini = Arc::new(AtomicBool::new(false));
    let pause = Arc::new(AtomicBool::new(false));
    let joues = Arc::new(AtomicU64::new(0));
    let total = plages.iter().map(|(d, f)| f.saturating_sub(*d)).sum();
    let (tx, erreur) = crossbeam_channel::bounded(1);
    let (a, f, p, j) = (arret.clone(), fini.clone(), pause.clone(), joues.clone());
    std::thread::spawn(move || {
        let (envoi, morceaux) = crossbeam_channel::bounded::<Vec<f32>>(3);
        let producteur = {
            let a = a.clone();
            std::thread::spawn(move || {
                for (debut, fin) in plages {
                    let mut d = debut;
                    while d < fin && !a.load(Ordering::Relaxed) {
                        let e = (d + MORCEAU).min(fin);
                        let mut m = Some(super::frontieres::extraire(&dir, &index, d, e));
                        while let Some(x) = m.take() {
                            match envoi.send_timeout(x, Duration::from_millis(100)) {
                                Ok(()) => {}
                                Err(crossbeam_channel::SendTimeoutError::Timeout(x)) if !a.load(Ordering::Relaxed) => m = Some(x),
                                Err(_) => return,
                            }
                        }
                        d = e;
                    }
                }
            })
        };
        if let Err(e) = lire(morceaux, &a, &p, &j) {
            log::error!("Écoute de l'extrait : {e:#}");
            let _ = tx.send(format!("Écoute impossible : {e:#}"));
        }
        a.store(true, Ordering::Relaxed);
        let _ = producteur.join();
        f.store(true, Ordering::Relaxed);
    });
    Lecture { arret, fini, pause, joues, total, erreur }
}

/// Joue les morceaux reçus jusqu'au dernier, ou jusqu'à l'arrêt.
fn lire(morceaux: Receiver<Vec<f32>>, arret: &AtomicBool, pause: &Arc<AtomicBool>, joues: &Arc<AtomicU64>) -> Result<()> {
    let device = cpal::default_host().default_output_device().ok_or_else(|| anyhow!("aucune sortie audio"))?;
    let supported = device.default_output_config().context("Format de la sortie audio")?;
    let rate = supported.sample_rate();
    let channels = supported.channels() as usize;
    let config: cpal::StreamConfig = supported.config();
    // Interpolation linéaire vers la fréquence de la sortie : suffisant pour réécouter la parole.
    let pas = SAMPLE_RATE as f64 / rate as f64;
    let epuise = Arc::new(AtomicBool::new(false));
    let err_fn = |e| log::error!("Flux de sortie audio : {e}");

    macro_rules! build {
        ($t:ty) => {{
            let (morceaux, pause, joues, epuise) = (morceaux.clone(), pause.clone(), joues.clone(), epuise.clone());
            // Morceau en cours, position (fractionnaire) dans ce morceau, échantillons des morceaux finis.
            let mut courant: Vec<f32> = Vec::new();
            let mut x = 0.0f64;
            let mut avant = 0u64;
            device.build_output_stream::<$t, _, _>(
                config.clone(),
                move |data: &mut [$t], _| {
                    for trame in data.chunks_mut(channels) {
                        let mut v = 0.0f32;
                        if !pause.load(Ordering::Relaxed) {
                            while x as usize >= courant.len() {
                                match morceaux.try_recv() {
                                    Ok(m) => {
                                        x -= courant.len() as f64;
                                        avant += courant.len() as u64;
                                        courant = m;
                                    }
                                    Err(crossbeam_channel::TryRecvError::Disconnected) => {
                                        epuise.store(true, Ordering::Relaxed);
                                        break;
                                    }
                                    // Morceau suivant pas encore lu : un court silence.
                                    Err(crossbeam_channel::TryRecvError::Empty) => break,
                                }
                            }
                            let i = x as usize;
                            if i < courant.len() {
                                let t = (x - i as f64) as f32;
                                let a = courant[i];
                                let b = courant.get(i + 1).copied().unwrap_or(a);
                                v = a + (b - a) * t;
                                x += pas;
                                joues.store(avant + x as u64, Ordering::Relaxed);
                            }
                        }
                        for s in trame {
                            *s = <$t as cpal::FromSample<f32>>::from_sample_(v);
                        }
                    }
                },
                err_fn,
                None,
            )
        }};
    }

    use cpal::SampleFormat as F;
    let stream = match supported.sample_format() {
        F::F32 => build!(f32),
        F::I16 => build!(i16),
        F::I32 => build!(i32),
        F::U16 => build!(u16),
        F::U8 => build!(u8),
        F::I8 => build!(i8),
        F::F64 => build!(f64),
        other => return Err(anyhow!("format d'échantillon non pris en charge : {other:?}")),
    }
    .context("Ouverture de la sortie audio")?;
    stream.play().context("Démarrage de la lecture")?;
    while !arret.load(Ordering::Relaxed) && !epuise.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(30));
    }
    // Laisse la sortie jouer ce qu'elle a déjà reçu.
    if !arret.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(150));
    }
    Ok(())
}
