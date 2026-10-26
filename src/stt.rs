//! Moteur de transcription : whisper.cpp (via whisper-rs) dans un fil dédié.
//! Le modèle reste chargé entre les rencontres pour démarrer instantanément.

use std::collections::VecDeque;
use std::path::PathBuf;

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState};

use crate::audio::{SAMPLE_RATE, Source, dsp::Utterance};
use crate::quebec::{self, Glossary};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Segment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub source: Source,
    pub text: String,
}

pub struct Settings {
    pub threads: usize,
    pub beam_size: u8,
    pub glossary: Glossary,
    /// Sujets et noms tirés de l'ordre du jour de la rencontre en cours.
    pub agenda_hint: String,
}

pub enum Request {
    Load { path: PathBuf, use_gpu: bool },
    /// Libère la mémoire du modèle (palier 8 Go, pendant la rédaction) ; `Load` le recharge.
    Unload,
    Transcribe { utterance: Utterance, context: String },
    Configure(Settings),
}

pub enum Reply {
    Loaded(Result<String, String>),
    /// `handled` : nombre d'énoncés traités (plusieurs s'ils ont été regroupés).
    Segments { segments: Vec<Segment>, handled: usize },
    Error { message: String, handled: usize },
}

/// Durée maximale d'un regroupement d'énoncés (Whisper travaille par 30 s).
const MAX_BATCH_SAMPLES: u64 = 28 * SAMPLE_RATE as u64;
/// Contexte audio minimal de l'encodeur (768 trames = 15 s). En dessous, Turbo
/// perd les majuscules et peut boucler sur les énoncés courts (mesuré).
const MIN_AUDIO_CTX: usize = 768;

pub struct Engine {
    pub tx: Sender<Request>,
    pub rx: Receiver<Reply>,
}

impl Engine {
    pub fn spawn(wake: impl Fn() + Send + 'static) -> Self {
        let (req_tx, req_rx) = crossbeam_channel::unbounded::<Request>();
        let (rep_tx, rep_rx) = crossbeam_channel::unbounded::<Reply>();
        std::thread::Builder::new()
            .name("whisper".into())
            .spawn(move || worker(req_rx, rep_tx, wake))
            .expect("fil whisper");
        Self { tx: req_tx, rx: rep_rx }
    }
}

struct Loaded {
    name: String,
    state: WhisperState,
    _ctx: WhisperContext,
}

fn worker(rx: Receiver<Request>, tx: Sender<Reply>, wake: impl Fn()) {
    let mut model: Option<Loaded> = None;
    let mut settings = Settings { threads: 4, beam_size: 5, glossary: Glossary::parse(""), agenda_hint: String::new() };
    let mut deferred: VecDeque<Request> = VecDeque::new();
    loop {
        let req = match deferred.pop_front() {
            Some(r) => r,
            None => match rx.recv() {
                Ok(r) => r,
                Err(_) => break,
            },
        };
        let reply = match req {
            Request::Configure(s) => {
                settings = s;
                continue;
            }
            Request::Unload => {
                model = None;
                continue;
            }
            Request::Load { path, use_gpu } => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if model.as_ref().is_some_and(|m| m.name == name) {
                    Reply::Loaded(Ok(name))
                } else {
                    model = None; // libère la mémoire avant de charger le suivant
                    match load(&path, use_gpu) {
                        Ok(m) => {
                            model = Some(m);
                            Reply::Loaded(Ok(name))
                        }
                        Err(e) => Reply::Loaded(Err(format!("{e:#}"))),
                    }
                }
            }
            Request::Transcribe { utterance, context } => {
                let (utterance, handled) = batch(utterance, &rx, &mut deferred);
                match model.as_mut() {
                    None => Reply::Error { message: "Aucun modèle chargé".into(), handled },
                    Some(m) => match transcribe(&mut m.state, &settings, utterance, &context) {
                        Ok(segments) => Reply::Segments { segments, handled },
                        Err(e) => Reply::Error { message: format!("Transcription : {e:#}"), handled },
                    },
                }
            }
        };
        if tx.send(reply).is_err() {
            break;
        }
        wake();
    }
}

/// Regroupement adaptatif : si d'autres énoncés de la même source attendent
/// déjà, on les fusionne (en gardant les vrais silences entre eux, donc des
/// horodatages exacts) pour payer une seule passe d'encodeur. Sans retard,
/// chaque énoncé part seul et la latence reste minimale.
fn batch(mut first: Utterance, rx: &Receiver<Request>, deferred: &mut VecDeque<Request>) -> (Utterance, usize) {
    let mut handled = 1;
    while deferred.is_empty() {
        let Ok(next) = rx.try_recv() else { break };
        match next {
            Request::Transcribe { utterance: u, context }
                if u.source == first.source
                    && u.start >= first.start + first.samples.len() as u64
                    && u.start + u.samples.len() as u64 - first.start <= MAX_BATCH_SAMPLES =>
            {
                let _ = context;
                let gap = u.start - (first.start + first.samples.len() as u64);
                first.samples.resize(first.samples.len() + gap as usize, 0.0);
                first.samples.extend_from_slice(&u.samples);
                handled += 1;
            }
            other => deferred.push_back(other),
        }
    }
    (first, handled)
}

fn load(path: &std::path::Path, use_gpu: bool) -> Result<Loaded> {
    let mut params = WhisperContextParameters::default();
    params.use_gpu(use_gpu).flash_attn(use_gpu);
    let ctx = WhisperContext::new_with_params(path, params)
        .with_context(|| format!("Chargement du modèle {}", path.display()))?;
    anyhow::ensure!(ctx.is_multilingual(), "Ce modèle est anglais seulement ; choisissez un modèle multilingue");
    let state = ctx.create_state().context("Création de l'état Whisper")?;
    Ok(Loaded {
        name: path.file_name().unwrap().to_string_lossy().into_owned(),
        state,
        _ctx: ctx,
    })
}

fn transcribe(state: &mut WhisperState, s: &Settings, utt: Utterance, context: &str) -> Result<Vec<Segment>> {
    let mut audio = utt.samples;
    // Les sons système et les micros lointains sont souvent faibles : on normalise.
    let peak = crate::audio::dsp::peak(&audio);
    if peak > 1e-4 && peak < 0.5 {
        let gain = (0.7 / peak).min(20.0);
        audio.iter_mut().for_each(|x| *x *= gain);
    }
    // whisper.cpp exige au moins 1 s ; on complète avec du silence.
    let min = SAMPLE_RATE as usize + SAMPLE_RATE as usize / 10;
    if audio.len() < min {
        audio.resize(min, 0.0);
    }

    let strategy = if s.beam_size > 1 {
        SamplingStrategy::BeamSearch { beam_size: s.beam_size as i32, patience: -1.0 }
    } else {
        SamplingStrategy::Greedy { best_of: 1 }
    };
    let prompt = s.glossary.prompt(context, &s.agenda_hint);
    let mut p = FullParams::new(strategy);
    p.set_language(Some("fr"));
    p.set_translate(false);
    p.set_n_threads(s.threads as i32);
    p.set_initial_prompt(&prompt);
    p.set_no_context(true);
    p.set_suppress_blank(true);
    p.set_suppress_nst(true);
    p.set_no_speech_thold(0.6);
    p.set_print_progress(false);
    p.set_print_realtime(false);
    p.set_print_special(false);
    p.set_print_timestamps(false);
    // L'encodeur traite toujours 30 s (1500 trames) : pour un énoncé de 5 s,
    // c'est 6× trop de calcul. On réduit le contexte audio à la durée réelle
    // (+ marge), sans descendre sous MIN_AUDIO_CTX.
    let frames = audio.len() * 50 / SAMPLE_RATE as usize;
    p.set_audio_ctx(((frames + 128).max(MIN_AUDIO_CTX).div_ceil(64) * 64).min(1500) as i32);

    state.full(p, &audio)?;

    let offset_ms = utt.start * 1000 / SAMPLE_RATE as u64;
    let mut out = Vec::new();
    for seg in state.as_iter() {
        if seg.no_speech_probability() > 0.8 {
            continue;
        }
        let raw = seg.to_str_lossy()?;
        if quebec::is_hallucination(&raw) {
            continue;
        }
        let text = quebec::normalize(&s.glossary.apply(&raw));
        if text.is_empty() {
            continue;
        }
        out.push(Segment {
            start_ms: offset_ms + seg.start_timestamp().max(0) as u64 * 10,
            end_ms: offset_ms + seg.end_timestamp().max(0) as u64 * 10,
            source: utt.source,
            text,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utt(source: Source, start_s: f32, len_s: f32) -> Request {
        Request::Transcribe {
            utterance: Utterance {
                source,
                start: (start_s * SAMPLE_RATE as f32) as u64,
                samples: vec![0.1; (len_s * SAMPLE_RATE as f32) as usize],
            },
            context: String::new(),
        }
    }

    #[test]
    fn regroupe_les_enonces_en_attente() {
        let (tx, rx) = crossbeam_channel::unbounded();
        tx.send(utt(Source::Salle, 3.0, 4.0)).unwrap(); // fusionné (silence de 1 s conservé)
        tx.send(utt(Source::Salle, 20.0, 9.0)).unwrap(); // dépasserait 28 s : reporté
        let Request::Transcribe { utterance: first, .. } = utt(Source::Salle, 0.0, 2.0) else { unreachable!() };
        let mut deferred = VecDeque::new();
        let (merged, handled) = batch(first, &rx, &mut deferred);
        assert_eq!(handled, 2);
        assert_eq!(merged.samples.len(), 7 * SAMPLE_RATE as usize);
        assert_eq!(merged.samples[2 * SAMPLE_RATE as usize + 100], 0.0); // le silence est là
        assert_eq!(deferred.len(), 1);
    }

    #[test]
    fn ne_melange_pas_les_sources() {
        let (tx, rx) = crossbeam_channel::unbounded();
        tx.send(utt(Source::Distance, 2.5, 1.0)).unwrap();
        let Request::Transcribe { utterance: first, .. } = utt(Source::Salle, 0.0, 2.0) else { unreachable!() };
        let mut deferred = VecDeque::new();
        let (_, handled) = batch(first, &rx, &mut deferred);
        assert_eq!(handled, 1);
        assert_eq!(deferred.len(), 1);
    }
}
