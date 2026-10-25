//! Traitement du signal sans dépendance : mono, rééchantillonnage 16 kHz,
//! détection d'activité vocale (VAD) et découpage en énoncés.

use cpal::{FromSample, Sample};

use super::{SAMPLE_RATE, Source};

pub fn downmix<T: Sample>(data: &[T], channels: usize) -> Vec<f32>
where
    f32: FromSample<T>,
{
    if channels <= 1 {
        return data.iter().map(|&s| f32::from_sample(s)).collect();
    }
    let scale = 1.0 / channels as f32;
    data.chunks_exact(channels)
        .map(|frame| frame.iter().map(|&s| f32::from_sample(s)).sum::<f32>() * scale)
        .collect()
}

pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

/// Rééchantillonneur vers 16 kHz : filtre passe-bas FIR (sinc fenêtré de Blackman)
/// suivi d'une interpolation linéaire. Largement suffisant pour la parole,
/// et sans allocation dans la boucle chaude.
pub struct Resampler {
    step: f64,
    pos: f64,
    taps: Vec<f32>,
    /// Historique filtré nécessaire à l'interpolation entre deux blocs.
    hist: Vec<f32>,
    /// Entrée brute retenue pour le filtre FIR.
    raw: Vec<f32>,
}

impl Resampler {
    pub fn new(input_rate: u32) -> Self {
        let step = input_rate as f64 / SAMPLE_RATE as f64;
        let taps = if step > 1.0 {
            // Coupure juste sous Nyquist de la sortie (8 kHz) pour éviter le repliement.
            let fc = 0.45 / step;
            let n = 48;
            (0..=n)
                .map(|i| {
                    let x = i as f64 - n as f64 / 2.0;
                    let sinc = if x == 0.0 {
                        2.0 * fc
                    } else {
                        (2.0 * std::f64::consts::PI * fc * x).sin() / (std::f64::consts::PI * x)
                    };
                    let w = 0.42 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos()
                        + 0.08 * (4.0 * std::f64::consts::PI * i as f64 / n as f64).cos();
                    (sinc * w) as f32
                })
                .collect()
        } else {
            vec![1.0]
        };
        let delay = taps.len() - 1;
        Self { step, pos: 0.0, raw: vec![0.0; delay], taps, hist: Vec::new() }
    }

    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.step == 1.0 {
            out.extend_from_slice(input);
            return;
        }
        self.raw.extend_from_slice(input);
        let n = self.taps.len();
        let filtered = self.raw.windows(n).map(|w| {
            w.iter().zip(&self.taps).map(|(a, b)| a * b).sum::<f32>()
        });
        self.hist.extend(filtered);
        self.raw.drain(..self.raw.len() - (n - 1));

        while self.pos + 1.0 < self.hist.len() as f64 {
            let i = self.pos as usize;
            let frac = (self.pos - i as f64) as f32;
            out.push(self.hist[i] * (1.0 - frac) + self.hist[i + 1] * frac);
            self.pos += self.step;
        }
        let consumed = (self.pos as usize).min(self.hist.len());
        self.hist.drain(..consumed);
        self.pos -= consumed as f64;
    }
}

/// Un énoncé prêt pour Whisper.
pub struct Utterance {
    pub source: Source,
    /// Position du début, en échantillons depuis le début de la rencontre.
    pub start: u64,
    pub samples: Vec<f32>,
}

const FRAME: usize = (SAMPLE_RATE as usize) * 30 / 1000; // 30 ms
const PRE_ROLL_FRAMES: usize = 10; // 300 ms gardés avant le début de la parole
const MIN_SPEECH_FRAMES: usize = 10; // < 300 ms de voix = bruit (toux, clic)
const MAX_FRAMES: usize = 25_000 / 30; // Whisper travaille par fenêtres de 30 s

/// Détecteur de voix à seuil adaptatif + découpeur en énoncés.
///
/// Le plancher de bruit suit lentement le niveau ambiant ; une trame est
/// « voix » quand elle le dépasse d'une marge qui dépend de la sensibilité.
pub struct Segmenter {
    source: Source,
    margin_db: f32,
    hangover_frames: usize,
    noise_db: f32,
    pending: Vec<f32>,
    /// Échantillons consommés depuis le début (horloge du segmenteur).
    clock: u64,
    pre_roll: std::collections::VecDeque<Vec<f32>>,
    current: Option<Current>,
}

struct Current {
    start: u64,
    samples: Vec<f32>,
    frames: usize,
    speech_frames: usize,
    silence_run: usize,
    /// Trame la plus calme des dernières secondes : point de coupe si l'énoncé est trop long.
    quietest: (usize, f32),
}

impl Segmenter {
    pub fn new(source: Source, sensitivity: f32) -> Self {
        let s = sensitivity.clamp(0.0, 1.0);
        Self {
            source,
            margin_db: 14.0 - 8.0 * s, // 14 dB (peu sensible) → 6 dB (très sensible)
            hangover_frames: 22,       // ~650 ms de silence ferment un énoncé
            noise_db: -60.0,
            pending: Vec::with_capacity(FRAME * 4),
            clock: 0,
            pre_roll: Default::default(),
            current: None,
        }
    }

    pub fn push(&mut self, samples: &[f32], out: &mut Vec<Utterance>) {
        self.pending.extend_from_slice(samples);
        let mut offset = 0;
        while self.pending.len() - offset >= FRAME {
            let frame = self.pending[offset..offset + FRAME].to_vec();
            offset += FRAME;
            self.frame(frame, out);
        }
        self.pending.drain(..offset);
    }

    /// Un énoncé est en cours : on ne coupe pas un bloc audio au milieu d'une phrase.
    pub fn en_enonce(&self) -> bool {
        self.current.is_some()
    }

    pub fn flush(&mut self, out: &mut Vec<Utterance>) {
        if let Some(cur) = self.current.take() {
            self.emit(cur, out);
        }
    }

    fn frame(&mut self, frame: Vec<f32>, out: &mut Vec<Utterance>) {
        let rms = (frame.iter().map(|s| s * s).sum::<f32>() / FRAME as f32).sqrt();
        let db = 20.0 * rms.max(1e-7).log10();
        let speech = db > self.noise_db + self.margin_db && db > -58.0;

        // Plancher de bruit : descend vite, monte lentement (et pas pendant la voix).
        if db < self.noise_db {
            self.noise_db = 0.9 * self.noise_db + 0.1 * db;
        } else if !speech {
            self.noise_db = 0.995 * self.noise_db + 0.005 * db;
        }
        let frame_start = self.clock;
        self.clock += FRAME as u64;

        match self.current.as_mut() {
            None => {
                if speech {
                    let pre: Vec<f32> = self.pre_roll.drain(..).flatten().collect();
                    let start = frame_start - pre.len() as u64;
                    let mut samples = pre;
                    samples.extend_from_slice(&frame);
                    self.current = Some(Current {
                        start,
                        samples,
                        frames: 1,
                        speech_frames: 1,
                        silence_run: 0,
                        quietest: (0, f32::MAX),
                    });
                } else {
                    self.pre_roll.push_back(frame);
                    if self.pre_roll.len() > PRE_ROLL_FRAMES {
                        self.pre_roll.pop_front();
                    }
                }
            }
            Some(cur) => {
                cur.samples.extend_from_slice(&frame);
                cur.frames += 1;
                if speech {
                    cur.speech_frames += 1;
                    cur.silence_run = 0;
                } else {
                    cur.silence_run += 1;
                }
                if cur.frames > MAX_FRAMES - 100 && db < cur.quietest.1 {
                    cur.quietest = (cur.frames, db);
                }
                if cur.silence_run >= self.hangover_frames {
                    let cur = self.current.take().unwrap();
                    self.emit(cur, out);
                } else if cur.frames >= MAX_FRAMES {
                    // Énoncé trop long : on coupe à la trame la plus calme, le reste continue.
                    let mut cur = self.current.take().unwrap();
                    let cut = cur.quietest.0.clamp(1, cur.frames) * FRAME;
                    let rest = cur.samples.split_off(cut.min(cur.samples.len()));
                    let rest_frames = rest.len() / FRAME;
                    let next_start = cur.start + cut as u64;
                    self.emit(cur, out);
                    self.current = Some(Current {
                        start: next_start,
                        samples: rest,
                        frames: rest_frames,
                        speech_frames: rest_frames,
                        silence_run: 0,
                        quietest: (0, f32::MAX),
                    });
                }
            }
        }
    }

    fn emit(&self, cur: Current, out: &mut Vec<Utterance>) {
        if cur.speech_frames >= MIN_SPEECH_FRAMES {
            out.push(Utterance { source: self.source, start: cur.start, samples: cur.samples });
        }
    }
}

/// Aligne une source sur l'horloge murale : la capture loopback de Windows
/// ne livre *rien* quand aucun son ne joue ; on comble alors avec du silence
/// pour que les horodatages et le mixage restent justes.
pub struct Aligner {
    pub written: u64,
}

impl Aligner {
    /// Retourne le nombre d'échantillons de silence à insérer avant `incoming`.
    pub fn gap(&self, expected_now: u64, incoming: usize) -> usize {
        let tolerance = SAMPLE_RATE as u64 / 4; // 250 ms de gigue acceptée
        let after = self.written + incoming as u64;
        if expected_now > after + tolerance { (expected_now - after) as usize } else { 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, secs: f32, freq: f32, amp: f32) -> Vec<f32> {
        (0..(rate as f32 * secs) as usize)
            .map(|i| amp * (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin())
            .collect()
    }

    #[test]
    fn resample_48k_preserves_duration_and_tone() {
        let input = sine(48_000, 1.0, 440.0, 0.5);
        let mut r = Resampler::new(48_000);
        let mut out = Vec::new();
        for chunk in input.chunks(480) {
            r.process(chunk, &mut out);
        }
        assert!((out.len() as i64 - 16_000).abs() < 40, "len = {}", out.len());
        let p = peak(&out[1000..]);
        assert!((p - 0.5).abs() < 0.05, "peak = {p}");
    }

    #[test]
    fn resample_attenuates_aliasing() {
        // 12 kHz n'existe pas à 16 kHz : il doit être fortement atténué.
        let input = sine(48_000, 0.5, 12_000.0, 0.5);
        let mut r = Resampler::new(48_000);
        let mut out = Vec::new();
        r.process(&input, &mut out);
        assert!(peak(&out[500..]) < 0.05);
    }

    #[test]
    fn segmenter_finds_speech_between_silences() {
        let mut seg = Segmenter::new(Source::Salle, 0.5);
        let mut out = Vec::new();
        let quiet = vec![0.0005f32; 16_000];
        seg.push(&quiet, &mut out);
        seg.push(&sine(16_000, 1.5, 300.0, 0.3), &mut out);
        seg.push(&quiet, &mut out);
        assert_eq!(out.len(), 1);
        let u = &out[0];
        let start_s = u.start as f32 / 16_000.0;
        assert!((0.6..=1.0).contains(&start_s), "début = {start_s}");
        assert!(u.samples.len() > 16_000 * 3 / 2);
    }

    #[test]
    fn segmenter_splits_long_speech() {
        let mut seg = Segmenter::new(Source::Salle, 0.5);
        let mut out = Vec::new();
        seg.push(&vec![0.0005f32; 8_000], &mut out);
        seg.push(&sine(16_000, 60.0, 300.0, 0.3), &mut out);
        seg.flush(&mut out);
        assert!(out.len() >= 3);
        assert!(out.iter().all(|u| u.samples.len() <= 16_000 * 26));
        let total: usize = out.iter().map(|u| u.samples.len()).sum();
        assert!(total >= 16_000 * 60);
    }

    #[test]
    fn aligner_pads_only_real_gaps() {
        let a = Aligner { written: 16_000 };
        assert_eq!(a.gap(16_100, 160), 0);
        assert_eq!(a.gap(32_000, 160), 32_000 - 16_160);
    }
}
