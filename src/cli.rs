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

/// La ligne de commande ne télécharge rien : sans les modèles, elle explique comment les obtenir.
fn exiger_modeles(fichiers: &[&str]) -> Result<()> {
    let manquants: Vec<&str> = fichiers.iter().copied().filter(|f| !crate::models::installed(f)).collect();
    if !manquants.is_empty() {
        bail!(
            "modèle(s) absent(s) : {}.\n\
             Lancez une première fois l'application Tour de table : elle télécharge automatiquement les modèles.\n\
             Sans accès à Internet, copiez les fichiers dans {} (voir la section « Déploiement en organisation » du README).",
            manquants.join(", "),
            config::models_dir().display()
        );
    }
    Ok(())
}

pub fn transcribe_file(fichier: &str, model: Option<&str>) -> Result<()> {
    let m = transcribe(fichier, model, None)?;
    eprintln!("Rencontre enregistrée dans {}", m.dir().display());
    Ok(())
}

/// Importe `fichier` comme l'application : copie dans `source/`, blocs dans `_import/`,
/// transcription bloc par bloc, puis découpage par point (frontières détectées acceptées
/// telles quelles).
fn transcribe(fichier: &str, model: Option<&str>, agenda: Option<Agenda>) -> Result<Meeting> {
    if model.is_none() {
        exiger_modeles(&[crate::models::RECOMMENDED])?;
    }
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
    engine.tx.send(Request::Load { path: model_path.clone(), use_gpu: crate::gpu::whisper() })?;
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
                        // Sur la sortie d'erreur : la sortie standard reste au compte rendu.
                        eprintln!("[{}] {}", crate::storage::clock(s.start_ms), s.text);
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

/// Nettoyage de la transcription, comme dans l'application.
fn nettoyer(m: &mut Meeting) -> Result<()> {
    let t = Instant::now();
    let glossaire = Glossary::parse(&Config::load().glossary);
    let n = crate::nettoyage::executer(m, m.nettoyage.clone(), &glossaire, &Default::default(), &mut |_| {});
    eprintln!("Transcription nettoyée en {:.1} s ({} phrases)", t.elapsed().as_secs_f32(), n.phrases.len());
    m.nettoyage = Some(n);
    m.save()?;
    m.export_all()?;
    Ok(())
}

/// `source` : enregistrement (transcrit puis nettoyé), `rencontre.json` (nettoyé à nouveau, pour
/// mesurer) ou transcription en texte. `modele` : mesure avec un autre modèle de
/// rédaction que celui de l'application.
pub fn compte_rendu(agenda_path: &str, source: &str, modele: Option<&str>) -> Result<()> {
    let mut requis = Vec::new();
    if modele.is_none() {
        requis.push(crate::models::modele_redaction().file);
    }
    if import::is_audio(Path::new(source)) {
        requis.push(crate::models::RECOMMENDED);
    }
    exiger_modeles(&requis)?;
    let mut moteur = config::moteur();
    if let Some(m) = modele {
        moteur.modele = std::path::PathBuf::from(m);
    }
    eprintln!("Modèle de rédaction : {}", moteur.nom());
    let agenda = crate::agenda::read(Path::new(agenda_path))?;
    eprintln!("Ordre du jour lu ({} caractères) :\n{agenda}\n", agenda.chars().count());

    let chemin = Path::new(source);
    let mut duree_nettoyage = None;
    let rencontre = if import::is_audio(chemin) {
        let nom = Path::new(agenda_path).file_name().unwrap_or_default().to_string_lossy().into_owned();
        let m = transcribe(source, None, Some(Agenda::new(nom, agenda.clone())))?;
        eprintln!("Rencontre enregistrée dans {}", m.dir().display());
        Some(m)
    } else if chemin.extension().is_some_and(|e| e == "json") {
        let mut m = crate::storage::charger_fichier(chemin)?;
        // Mesure : tout est refait, nettoyage compris.
        m.nettoyage = None;
        m.summary = None;
        Some(m)
    } else {
        None
    };
    let mut rencontre = rencontre;
    let (transcript, duration, points, hors_point) = match rencontre.as_mut() {
        Some(m) => {
            let t = Instant::now();
            nettoyer(m)?;
            duree_nettoyage = Some(t.elapsed().as_secs_f32());
            let (points, hors_point) = crate::summary::points_de(m);
            (m.texte_compte_rendu(), crate::storage::clock(m.duration_s * 1000), points, hors_point)
        }
        None => (std::fs::read_to_string(source).with_context(|| format!("Lecture de {source}"))?, "non précisée".into(), Vec::new(), String::new()),
    };
    let t = Instant::now();
    // Titre : première ligne de l'ordre du jour (souvent le nom de l'instance), sauf un en-tête
    // de page de PDF (« Page 1 sur 1 »).
    let title = agenda
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !(l.starts_with("Page ") && l.contains(" sur ")))
        .map(|l| {
            let l = l.strip_suffix("Ordre du jour").or_else(|| l.strip_suffix("ordre du jour")).unwrap_or(l);
            l.trim_end_matches(|c: char| c == '–' || c == '-' || c == ':' || c.is_whitespace()).to_string()
        })
        .unwrap_or_else(|| std::path::Path::new(source).file_stem().unwrap_or_default().to_string_lossy().into_owned());
    let par_point = !points.is_empty();
    let debut = rencontre.as_ref().map_or_else(chrono::Local::now, |m| m.started_at);
    let (date, heure) = (crate::summary::date_longue(&debut), rencontre.as_ref().map(|_| crate::summary::heure(&debut)).unwrap_or_default());
    let req = crate::summary::Request {
        moteur: &moteur,
        avancement: None,
        title: &title,
        date: &date,
        heure: &heure,
        duration: &duration,
        agenda: Some(&agenda),
        transcript: &transcript,
        points,
        hors_point,
        reprendre: false,
    };
    let cr = crate::summary::compte_rendu(&req)?;
    println!("{cr}");
    // Comme dans l'application : le compte rendu est enregistré avec la rencontre (compte-rendu.md).
    if let Some(m) = rencontre.as_mut() {
        m.summary = Some(cr.clone());
        m.save()?;
        m.export_all()?;
    }
    let duree_cr = t.elapsed().as_secs_f32();
    let mode = if par_point { "par point" } else { "en un appel" };
    eprintln!("Compte rendu ({mode}) rédigé en {duree_cr:.0} s par {}", moteur.nom());
    if let Some(n) = duree_nettoyage {
        eprintln!("Total : nettoyage {n:.1} s + compte rendu {duree_cr:.0} s = {:.1} min", (n + duree_cr) / 60.0);
    }
    Ok(())
}
