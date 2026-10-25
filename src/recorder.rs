//! Une session d'enregistrement : ouvre les sources, aligne tout sur
//! l'horloge murale, découpe en énoncés et les envoie au moteur Whisper.
//! L'audio est conservé en blocs d'environ 2 minutes, par point (voir `audio::blocs`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};

use crate::audio::blocs::Ecrivain;
use crate::audio::dsp::{Aligner, Resampler, Segmenter, Utterance};
use crate::audio::{self, Level, SAMPLE_RATE, Source};
use crate::config::CaptureMode;

pub struct Options {
    pub capture_mic: bool,
    pub capture_system: bool,
    pub mic_device: Option<String>,
    pub system_device: Option<String>,
    pub mode: CaptureMode,
    pub sensitivity: f32,
    /// Dossier de la rencontre, où sont écrits les blocs audio.
    pub dossier: PathBuf,
    /// Titres des points de l'ordre du jour, pour nommer les dossiers des blocs.
    pub points: Vec<String>,
}

/// Messages de l'interface au fil audio.
enum Commande {
    /// Change de point ; le fil répond par l'échantillon de la coupe.
    Point(Option<usize>, Sender<u64>),
    /// Changement détecté dans la parole ; l'annonce a commencé à l'échantillon donné.
    PointDetecte(Option<usize>, u64),
    /// Défait le dernier changement détecté ; le fil répond s'il a pu le faire.
    Annuler(Sender<bool>),
    /// Le dernier changement détecté ne sera plus annulé.
    Confirmer,
    Titres(Vec<String>),
}

pub struct Recorder {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    pub mic_level: Arc<Level>,
    pub sys_level: Arc<Level>,
    pub started: Instant,
    /// Échantillons alignés transmis au découpage en énoncés : même horloge que
    /// `Utterance::start`, donc que `Segment::start_ms`. Écrit par le fil audio seulement.
    pub echantillons: Arc<AtomicU64>,
    /// Sources réellement ouvertes et avertissements (source manquante, etc.).
    pub active: Vec<Source>,
    pub warnings: Vec<String>,
    commandes: Sender<Commande>,
}

struct Track {
    resampler: Resampler,
    aligner: Aligner,
    /// Tampon 16 kHz en attente de mixage (mode mixé seulement).
    mix_buf: Vec<f32>,
}

impl Recorder {
    pub fn start(opts: Options, out: Sender<Utterance>) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let mic_level = Arc::new(Level::default());
        let sys_level = Arc::new(Level::default());
        let echantillons = Arc::new(AtomicU64::new(0));
        let (ready_tx, ready_rx) = crossbeam_channel::bounded(1);
        let (commandes, commandes_rx) = crossbeam_channel::unbounded();

        let thread = {
            let (stop, mic_level, sys_level, ech) = (stop.clone(), mic_level.clone(), sys_level.clone(), echantillons.clone());
            std::thread::Builder::new().name("audio".into()).spawn(move || {
                run(opts, out, commandes_rx, stop, mic_level, sys_level, ech, ready_tx);
            })?
        };

        let (active, warnings): (Vec<Source>, Vec<String>) = ready_rx
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|_| (Vec::new(), vec!["Les périphériques audio ne répondent pas".into()]));
        if active.is_empty() {
            stop.store(true, Ordering::Relaxed);
            let _ = thread.join();
            bail!("{}", warnings.join("\n"));
        }
        Ok(Self {
            stop,
            thread: Some(thread),
            mic_level,
            sys_level,
            started: Instant::now(),
            echantillons,
            active,
            warnings,
            commandes,
        })
    }

    /// Ferme les blocs audio en cours et ouvre ceux de `point` (hors point : `None`), à
    /// l'échantillon exact de la demande. Retourne la position de la coupe en millisecondes,
    /// sur l'horloge des segments : le début de la nouvelle période.
    pub fn changer_point(&self, point: Option<usize>) -> u64 {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let _ = self.commandes.send(Commande::Point(point, tx));
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(coupe) => coupe * 1000 / SAMPLE_RATE as u64,
            Err(_) => {
                log::warn!("Le fil audio n'a pas confirmé le changement de point");
                self.position_ms()
            }
        }
    }

    /// Changement de point détecté dans la parole : la coupe se fait maintenant, mais les blocs
    /// sont recoupés à `frontiere_ms`, début de l'énoncé de l'annonce (voir `Ecrivain::recouper`).
    pub fn changer_point_a_posteriori(&self, point: Option<usize>, frontiere_ms: u64) {
        let _ = self.commandes.send(Commande::PointDetecte(point, frontiere_ms * SAMPLE_RATE as u64 / 1000));
    }

    /// Défait le dernier changement détecté (blocs et index) ; `false` si c'est impossible.
    pub fn annuler_changement(&self) -> bool {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let _ = self.commandes.send(Commande::Annuler(tx));
        rx.recv_timeout(Duration::from_secs(2)).unwrap_or(false)
    }

    pub fn confirmer_changement(&self) {
        let _ = self.commandes.send(Commande::Confirmer);
    }

    /// Titres des points, après une modification de l'ordre du jour pendant la rencontre.
    pub fn nommer_points(&self, titres: Vec<String>) {
        let _ = self.commandes.send(Commande::Titres(titres));
    }

    /// Position courante sur l'horloge des segments, en millisecondes.
    pub fn position_ms(&self) -> u64 {
        position_ms(&self.echantillons)
    }

    /// Arrête la capture ; les derniers énoncés sont envoyés avant le retour.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub fn position_ms(echantillons: &AtomicU64) -> u64 {
    echantillons.load(Ordering::Relaxed) * 1000 / SAMPLE_RATE as u64
}

#[allow(clippy::too_many_arguments)]
fn run(
    opts: Options,
    out: Sender<Utterance>,
    commandes: Receiver<Commande>,
    stop: Arc<AtomicBool>,
    mic_level: Arc<Level>,
    sys_level: Arc<Level>,
    echantillons: Arc<AtomicU64>,
    ready: Sender<(Vec<Source>, Vec<String>)>,
) {
    let (tx, rx) = crossbeam_channel::bounded::<audio::Chunk>(512);
    let mut captures = Vec::new();
    let mut tracks: HashMap<Source, Track> = HashMap::new();
    let mut warnings = Vec::new();

    let wanted = [
        (opts.capture_mic, Source::Salle, opts.mic_device.as_deref(), mic_level),
        (opts.capture_system, Source::Distance, opts.system_device.as_deref(), sys_level),
    ];
    for (enabled, source, device, level) in wanted {
        if !enabled {
            continue;
        }
        match audio::start(source, device, tx.clone(), level) {
            Ok((cap, rate)) => {
                captures.push(cap);
                tracks.insert(source, Track {
                    resampler: Resampler::new(rate),
                    aligner: Aligner { written: 0 },
                    mix_buf: Vec::new(),
                });
            }
            Err(e) => warnings.push(format!("{} indisponible : {e:#}", source.label())),
        }
    }
    drop(tx);
    let mut active: Vec<Source> = tracks.keys().copied().collect();
    active.sort_by_key(|s| *s as u8);
    let _ = ready.send((active.clone(), warnings));
    if active.is_empty() {
        return;
    }

    let mixed = opts.mode == CaptureMode::Mixe;
    let mut segmenters: HashMap<Source, Segmenter> = if mixed {
        HashMap::from([(Source::Mixte, Segmenter::new(Source::Mixte, opts.sensitivity))])
    } else {
        active.iter().map(|&s| (s, Segmenter::new(s, opts.sensitivity))).collect()
    };
    let mut sources: Vec<Source> = segmenters.keys().copied().collect();
    sources.sort_by_key(|s| *s as u8);
    let mut ecrivain = Ecrivain::new(opts.dossier.clone(), sources, opts.points.clone());
    let mut erreur_signalee = false;
    let mut signaler = |r: Result<()>| {
        if let Err(e) = r
            && !std::mem::replace(&mut erreur_signalee, true)
        {
            log::error!("Écriture de l'audio : {e:#}");
        }
    };

    let start = Instant::now();
    let mut resampled = Vec::with_capacity(4096);
    let mut utterances = Vec::new();
    let feed = |source: Source, samples: &[f32], segs: &mut HashMap<Source, Segmenter>, utts: &mut Vec<Utterance>, ecr: &mut Ecrivain| {
        if let Some(seg) = segs.get_mut(&source) {
            seg.push(samples, utts);
        }
        ecr.pousser(source, samples);
    };

    loop {
        let stopping = stop.load(Ordering::Relaxed);
        let expected = (start.elapsed().as_secs_f64() * SAMPLE_RATE as f64) as u64;

        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(chunk) => {
                if let Some(track) = tracks.get_mut(&chunk.source) {
                    resampled.clear();
                    track.resampler.process(&chunk.samples, &mut resampled);
                    let gap = track.aligner.gap(expected, resampled.len());
                    let mut block = vec![0.0f32; gap];
                    block.extend_from_slice(&resampled);
                    track.aligner.written += block.len() as u64;
                    if mixed {
                        track.mix_buf.extend_from_slice(&block);
                    } else {
                        feed(chunk.source, &block, &mut segmenters, &mut utterances, &mut ecrivain);
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        // Source muette (loopback sans son) : on comble pour que le VAD puisse
        // clore l'énoncé en cours et que le mixage n'attende pas indéfiniment.
        let half_second = SAMPLE_RATE as u64 / 2;
        for (&source, track) in tracks.iter_mut() {
            if track.aligner.written + half_second < expected {
                let pad = vec![0.0f32; (expected - track.aligner.written - half_second / 2) as usize];
                track.aligner.written += pad.len() as u64;
                if mixed {
                    track.mix_buf.extend_from_slice(&pad);
                } else {
                    feed(source, &pad, &mut segmenters, &mut utterances, &mut ecrivain);
                }
            }
        }

        if mixed {
            let n = tracks.values().map(|t| t.mix_buf.len()).min().unwrap_or(0);
            if n > 0 {
                let mut sum = vec![0.0f32; n];
                for t in tracks.values_mut() {
                    for (acc, x) in sum.iter_mut().zip(t.mix_buf.drain(..n)) {
                        *acc += x;
                    }
                }
                sum.iter_mut().for_each(|x| *x = x.clamp(-1.0, 1.0));
                feed(Source::Mixte, &sum, &mut segmenters, &mut utterances, &mut ecrivain);
            }
        }

        // Ce qui a été transmis aux découpeurs (le reste attend le mixage).
        let transmis = tracks.values().map(|t| t.aligner.written - t.mix_buf.len() as u64).max().unwrap_or(0);
        echantillons.store(transmis, Ordering::Relaxed);

        // Coupes des blocs : ici, jamais dans le fil de l'interface. Un changement de point
        // coupe à l'échantillon transmis le plus avancé ; l'écrivain l'appliquera à toutes les sources.
        while let Ok(c) = commandes.try_recv() {
            match c {
                Commande::Point(point, reponse) => {
                    ecrivain.changer_point(point, transmis);
                    let _ = reponse.send(transmis);
                }
                Commande::PointDetecte(point, frontiere) => ecrivain.changer_point_a_posteriori(point, frontiere, transmis),
                Commande::Annuler(reponse) => {
                    let fait = ecrivain.annuler().unwrap_or_else(|e| {
                        log::error!("Annulation du changement de point : {e:#}");
                        false
                    });
                    let _ = reponse.send(fait);
                }
                Commande::Confirmer => ecrivain.oublier_annulation(),
                Commande::Titres(titres) => ecrivain.nommer_points(titres),
            }
        }
        let parle = segmenters.values().any(Segmenter::en_enonce);
        signaler(ecrivain.avancer(parle));

        if stopping {
            for seg in segmenters.values_mut() {
                seg.flush(&mut utterances);
            }
        }
        for u in utterances.drain(..) {
            let _ = out.send(u);
        }
        if stopping {
            break;
        }
    }

    drop(captures);
    signaler(ecrivain.terminer().map(|_| ()));
}
