//! Mode ligne de commande :
//! * `tourdetable --transcrire enregistrement.wav [modele.bin]`
//! * `tourdetable --compte-rendu ordre-du-jour.docx (transcription.txt | enregistrement.wav)`
//!
//! Même chaîne que l'application, sans interface.

use std::time::Instant;

use anyhow::{Context, Result, bail};

use crate::audio::dsp::{Resampler, Segmenter};
use crate::audio::{SAMPLE_RATE, Source};
use crate::config::Config;
use crate::quebec::Glossary;
use crate::storage::Meeting;
use crate::stt::{Engine, Reply, Request, Settings};

pub fn transcribe_file(wav: &str, model: Option<&str>) -> Result<()> {
    transcribe(wav, model).map(|_| ())
}

fn transcribe(wav: &str, model: Option<&str>) -> Result<Meeting> {
    let cfg = Config::load();
    let model_path = model.map(std::path::PathBuf::from).unwrap_or_else(|| cfg.model_path());

    let mut reader = hound::WavReader::open(wav).with_context(|| format!("Lecture de {wav}"))?;
    let spec = reader.spec();
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader.samples::<i32>().map(|s| s.map(|v| v as f32 / max)).collect::<Result<_, _>>()?
        }
    };
    let mono = crate::audio::dsp::downmix(&raw, spec.channels as usize);
    let mut audio = Vec::with_capacity(mono.len() / 2);
    Resampler::new(spec.sample_rate).process(&mono, &mut audio);
    let duration = audio.len() as f32 / SAMPLE_RATE as f32;

    let mut utterances = Vec::new();
    let mut seg = Segmenter::new(Source::Salle, cfg.vad_sensitivity);
    seg.push(&audio, &mut utterances);
    seg.flush(&mut utterances);
    eprintln!("{duration:.1} s d'audio, {} énoncés détectés", utterances.len());

    let engine = Engine::spawn(|| {});
    engine.tx.send(Request::Configure(Settings {
        threads: cfg.threads,
        beam_size: cfg.beam_size,
        glossary: Glossary::parse(&cfg.glossary),
        agenda_hint: String::new(),
    }))?;
    let t = Instant::now();
    engine.tx.send(Request::Load { path: model_path.clone(), use_gpu: cfg.use_gpu })?;
    match engine.rx.recv()? {
        Reply::Loaded(Ok(name)) => eprintln!("Modèle {name} chargé en {:.1} s", t.elapsed().as_secs_f32()),
        Reply::Loaded(Err(e)) => bail!(e),
        _ => bail!("réponse inattendue"),
    }

    let mut meeting = Meeting::new(&model_path.file_name().unwrap_or_default().to_string_lossy());
    // Tout est mis en file d'un coup : le moteur regroupe les énoncés comme
    // il le ferait pendant une rencontre où il prend du retard.
    let t = Instant::now();
    let total = utterances.len();
    for u in utterances {
        engine.tx.send(Request::Transcribe { utterance: u, context: String::new() })?;
    }
    let (mut done, mut passes) = (0, 0);
    while done < total {
        match engine.rx.recv()? {
            Reply::Segments { segments, handled } => {
                done += handled;
                passes += 1;
                for s in segments {
                    println!("[{}] {}", crate::storage::clock(s.start_ms), s.text);
                    meeting.insert(s);
                }
            }
            Reply::Error { message, handled } => {
                done += handled;
                eprintln!("{message}");
            }
            Reply::Loaded(_) => {}
        }
    }
    eprintln!("{total} énoncés en {passes} passes");
    let secs = t.elapsed().as_secs_f32();
    eprintln!("Transcription en {secs:.1} s (×{:.1} temps réel)", duration / secs.max(0.001));
    meeting.duration_s = duration as u64;
    Ok(meeting)
}

pub fn compte_rendu(agenda_path: &str, source: &str) -> Result<()> {
    let cfg = Config::load();
    let agenda = crate::agenda::read(std::path::Path::new(agenda_path))?;
    eprintln!("Ordre du jour lu ({} caractères) :\n{agenda}\n", agenda.chars().count());

    let (transcript, duration) = if source.to_lowercase().ends_with(".wav") {
        let m = transcribe(source, None)?;
        (m.plain_text(), crate::storage::clock(m.duration_s * 1000))
    } else {
        (std::fs::read_to_string(source).with_context(|| format!("Lecture de {source}"))?, "non précisée".into())
    };
    let t = Instant::now();
    // Titre : première ligne de l'ordre du jour (souvent le nom de l'instance).
    let title = agenda
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| {
            let l = l.strip_suffix("Ordre du jour").or_else(|| l.strip_suffix("ordre du jour")).unwrap_or(l);
            l.trim_end_matches(|c: char| c == '–' || c == '-' || c == ':' || c.is_whitespace()).to_string()
        })
        .unwrap_or_else(|| std::path::Path::new(source).file_stem().unwrap_or_default().to_string_lossy().into_owned());
    let req = crate::summary::Request {
        base_url: &cfg.ollama_url,
        model: &cfg.ollama_model,
        title: &title,
        date: &chrono::Local::now().format("%Y-%m-%d").to_string(),
        duration: &duration,
        agenda: Some(&agenda),
        transcript: &transcript,
    };
    let cr = crate::summary::compte_rendu(&req)?;
    println!("{cr}");
    eprintln!("Compte rendu rédigé en {:.0} s par {}", t.elapsed().as_secs_f32(), cfg.ollama_model);
    Ok(())
}
