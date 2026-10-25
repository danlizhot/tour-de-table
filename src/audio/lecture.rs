//! Écoute d'un court extrait (vérification des frontières d'un import) sur la sortie par défaut.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
    pub erreur: Receiver<String>,
}

impl Lecture {
    pub fn en_cours(&self) -> bool {
        !self.fini.load(Ordering::Relaxed)
    }
}

impl Drop for Lecture {
    fn drop(&mut self) {
        self.arret.store(true, Ordering::Relaxed);
    }
}

/// Joue l'audio des blocs de `debut` à `fin` (échantillons) ; extraction et lecture dans un fil.
pub fn jouer(dir: PathBuf, index: Index, debut: u64, fin: u64) -> Lecture {
    let arret = Arc::new(AtomicBool::new(false));
    let fini = Arc::new(AtomicBool::new(false));
    let (tx, erreur) = crossbeam_channel::bounded(1);
    let (a, f) = (arret.clone(), fini.clone());
    std::thread::spawn(move || {
        let audio = super::frontieres::extraire(&dir, &index, debut, fin);
        if let Err(e) = lire(audio, &a) {
            log::error!("Écoute de l'extrait : {e:#}");
            let _ = tx.send(format!("Écoute impossible : {e:#}"));
        }
        f.store(true, Ordering::Relaxed);
    });
    Lecture { arret, fini, erreur }
}

fn lire(audio: Vec<f32>, arret: &AtomicBool) -> Result<()> {
    let device = cpal::default_host().default_output_device().ok_or_else(|| anyhow!("aucune sortie audio"))?;
    let supported = device.default_output_config().context("Format de la sortie audio")?;
    let rate = supported.sample_rate();
    let channels = supported.channels() as usize;
    let config: cpal::StreamConfig = supported.config();
    // Interpolation linéaire vers la fréquence de la sortie : suffisant pour réécouter la parole.
    let pas = SAMPLE_RATE as f64 / rate as f64;
    let n = (audio.len() as f64 / pas) as usize;
    let sortie: Arc<Vec<f32>> = Arc::new(
        (0..n)
            .map(|i| {
                let x = i as f64 * pas;
                let j = x as usize;
                let t = (x - j as f64) as f32;
                let a = audio[j];
                let b = audio.get(j + 1).copied().unwrap_or(a);
                a + (b - a) * t
            })
            .collect(),
    );
    let position = Arc::new(AtomicUsize::new(0));
    let err_fn = |e| log::error!("Flux de sortie audio : {e}");

    macro_rules! build {
        ($t:ty) => {{
            let (sortie, position) = (sortie.clone(), position.clone());
            device.build_output_stream::<$t, _, _>(
                config.clone(),
                move |data: &mut [$t], _| {
                    let mut p = position.load(Ordering::Relaxed);
                    for trame in data.chunks_mut(channels) {
                        let v = sortie.get(p).copied().unwrap_or(0.0);
                        p += 1;
                        for x in trame {
                            *x = <$t as cpal::FromSample<f32>>::from_sample_(v);
                        }
                    }
                    position.store(p, Ordering::Relaxed);
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
    while !arret.load(Ordering::Relaxed) && position.load(Ordering::Relaxed) < n + rate as usize / 10 {
        std::thread::sleep(Duration::from_millis(30));
    }
    Ok(())
}
