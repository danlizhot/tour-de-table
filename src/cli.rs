//! Mode ligne de commande :
//! * `tourdetable --transcrire enregistrement.(wav|mp3|m4a|mp4|flac|ogg) [modele.bin]`
//! * `tourdetable --compte-rendu ordre-du-jour.docx (transcription.txt | enregistrement.wav)`
//!
//! Même chaîne que l'application, sans interface : l'enregistrement devient une rencontre,
//! découpée par point d'après les frontières détectées, acceptées sans vérification.

use std::time::Instant;

use anyhow::{Context, Result, bail};

use std::path::Path;

use crate::audio::SAMPLE_RATE;
use crate::audio::{frontieres, import};
use crate::config::{self, Config};
use crate::quebec::Glossary;
use crate::storage::{Agenda, Meeting};
use crate::stt::{Engine, Reply, Request, Settings};

pub fn transcribe_file(fichier: &str, model: Option<&str>) -> Result<()> {
    let m = transcribe(fichier, model, None)?;
    eprintln!("Rencontre enregistrée dans {}", m.dir().display());
    Ok(())
}

/// Importe `fichier` comme l'application : copie dans `source/`, blocs dans `_import/`,
/// transcription bloc par bloc, puis découpage par point (frontières détectées acceptées
/// telles quelles).
fn transcribe(fichier: &str, model: Option<&str>, agenda: Option<Agenda>) -> Result<Meeting> {
    let cfg = Config::load();
    let model_path = model.map(std::path::PathBuf::from).unwrap_or_else(config::model_path);
    let path = Path::new(fichier);

    let engine = Engine::spawn(|| {});
    engine.tx.send(Request::Configure(Settings {
        threads: config::threads(),
        beam_size: config::BEAM_SIZE,
        glossary: Glossary::parse(&cfg.glossary),
        agenda_hint: agenda.as_ref().map(|a| crate::agenda::hint(&a.text)).unwrap_or_default(),
    }))?;
    let t = Instant::now();
    engine.tx.send(Request::Load { path: model_path.clone(), use_gpu: config::USE_GPU })?;
    match engine.rx.recv()? {
        Reply::Loaded(Ok(name)) => eprintln!("Modèle {name} chargé en {:.1} s", t.elapsed().as_secs_f32()),
        Reply::Loaded(Err(e)) => bail!(e),
        _ => bail!("réponse inattendue"),
    }

    let nom = path.file_stem().map(|n| n.to_string_lossy().into_owned());
    let started_at = path.metadata().and_then(|m| m.modified()).map_or_else(|_| chrono::Local::now(), Into::into);
    let mut meeting = Meeting::nouvelle(&model_path.file_name().unwrap_or_default().to_string_lossy(), nom, started_at);
    meeting.agenda = agenda;
    let dir = meeting.dir();
    let r = importer(&engine, path, &dir, &mut meeting);
    if r.is_err() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    r.map(|()| meeting)
}

fn importer(engine: &Engine, path: &Path, dir: &Path, meeting: &mut Meeting) -> Result<()> {
    meeting.save()?;
    let source = import::copier_source(path, dir)?;
    let travail = dir.join(import::TRAVAIL);
    let t = Instant::now();
    let index = import::ecrire_blocs(&source, &travail, &import::Avancement::default())?;
    let fin = index.blocs.last().map_or(0, |b| b.decalage + b.echantillons);
    let duration = fin as f32 / SAMPLE_RATE as f32;
    let n = index.blocs.len();
    eprintln!("{duration:.1} s d'audio en {n} blocs ({:.1} s)", t.elapsed().as_secs_f32());

    // Un bloc à la fois : le moteur regroupe les énoncés d'un bloc comme il le ferait pendant
    // une rencontre où il prend du retard.
    let t = Instant::now();
    let (mut total, mut passes) = (0, 0);
    import::transcrire_blocs(&travail, &index, |k, enonces| {
        let attendus = enonces.len();
        for u in enonces {
            engine.tx.send(Request::Transcribe { utterance: u, context: String::new() })?;
        }
        let mut done = 0;
        while done < attendus {
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
        total += attendus;
        eprintln!("Bloc {}/{n} transcrit", k + 1);
        Ok(())
    })?;
    eprintln!("{total} énoncés en {passes} passes");
    let secs = t.elapsed().as_secs_f32();
    eprintln!("Transcription en {secs:.1} s (×{:.1} temps réel)", duration / secs.max(0.001));
    meeting.duration_s = duration as u64;

    let frontieres = match meeting.agenda.as_ref().map(|a| &a.points[..]) {
        Some(points) if !points.is_empty() => crate::decoupage::detecter_hors_ligne(&meeting.segments, points),
        _ => Vec::new(),
    };
    for f in &frontieres {
        eprintln!("{} : {} — « {} »", crate::storage::clock(f.debut_ms), meeting.intertitre(f.point), f.extrait);
    }
    frontieres::appliquer(dir, meeting, &frontieres)?;
    meeting.save()?;
    meeting.export_all()?;
    Ok(())
}

pub fn compte_rendu(agenda_path: &str, source: &str) -> Result<()> {
    let agenda = crate::agenda::read(Path::new(agenda_path))?;
    eprintln!("Ordre du jour lu ({} caractères) :\n{agenda}\n", agenda.chars().count());

    let (transcript, duration) = if import::is_audio(Path::new(source)) {
        let nom = Path::new(agenda_path).file_name().unwrap_or_default().to_string_lossy().into_owned();
        let m = transcribe(source, None, Some(Agenda::new(nom, agenda.clone())))?;
        eprintln!("Rencontre enregistrée dans {}", m.dir().display());
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
    let moteur = config::moteur();
    let req = crate::summary::Request {
        moteur: &moteur,
        progress: None,
        title: &title,
        date: &chrono::Local::now().format("%Y-%m-%d").to_string(),
        duration: &duration,
        agenda: Some(&agenda),
        transcript: &transcript,
    };
    let cr = crate::summary::compte_rendu(&req)?;
    println!("{cr}");
    eprintln!("Compte rendu rédigé en {:.0} s par {}", t.elapsed().as_secs_f32(), moteur.nom());
    Ok(())
}
