//! Une session d'enregistrement : ouvre les sources, aligne tout sur
//! l'horloge murale, découpe en énoncés et les envoie au moteur Whisper.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use crossbeam_channel::{RecvTimeoutError, Sender};

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
    /// Dossier où écrire les fichiers WAV, si l'audio doit être conservé.
    pub wav_dir: Option<PathBuf>,
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
}

struct Track {
    resampler: Resampler,
    aligner: Aligner,
    /// Tampon 16 kHz en attente de mixage (mode mixé seulement).
    mix_buf: Vec<f32>,
}

type Wav = hound::WavWriter<std::io::BufWriter<std::fs::File>>;

impl Recorder {
    pub fn start(opts: Options, out: Sender<Utterance>) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let mic_level = Arc::new(Level::default());
        let sys_level = Arc::new(Level::default());
        let echantillons = Arc::new(AtomicU64::new(0));
        let (ready_tx, ready_rx) = crossbeam_channel::bounded(1);

        let thread = {
            let (stop, mic_level, sys_level, ech) = (stop.clone(), mic_level.clone(), sys_level.clone(), echantillons.clone());
            std::thread::Builder::new().name("audio".into()).spawn(move || {
                run(opts, out, stop, mic_level, sys_level, ech, ready_tx);
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
        })
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

fn run(
    opts: Options,
    out: Sender<Utterance>,
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
    let mut wavs: HashMap<Source, Wav> = HashMap::new();
    if let Some(dir) = &opts.wav_dir {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        for &s in segmenters.keys() {
            let name = match s {
                Source::Salle => "salle.wav",
                Source::Distance => "distance.wav",
                Source::Mixte => "reunion.wav",
            };
            match hound::WavWriter::create(dir.join(name), spec) {
                Ok(w) => {
                    wavs.insert(s, w);
                }
                Err(e) => log::warn!("WAV {name} : {e}"),
            }
        }
    }

    let start = Instant::now();
    let mut resampled = Vec::with_capacity(4096);
    let mut utterances = Vec::new();
    let mut feed = |source: Source, samples: &[f32], segs: &mut HashMap<Source, Segmenter>, utts: &mut Vec<Utterance>| {
        if let Some(seg) = segs.get_mut(&source) {
            seg.push(samples, utts);
        }
        if let Some(w) = wavs.get_mut(&source) {
            for &x in samples {
                let _ = w.write_sample((x.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
            }
        }
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
                        feed(chunk.source, &block, &mut segmenters, &mut utterances);
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
                    feed(source, &pad, &mut segmenters, &mut utterances);
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
                feed(Source::Mixte, &sum, &mut segmenters, &mut utterances);
            }
        }

        // Ce qui a été transmis aux découpeurs (le reste attend le mixage).
        let transmis = tracks.values().map(|t| t.aligner.written - t.mix_buf.len() as u64).max().unwrap_or(0);
        echantillons.store(transmis, Ordering::Relaxed);

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
    for (_, w) in wavs {
        if let Err(e) = w.finalize() {
            log::warn!("Finalisation WAV : {e}");
        }
    }
}
