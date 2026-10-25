//! Import d'enregistrements existants (Teams, Zoom, téléphone, enregistreuse…).
//! Décodage entièrement en Rust (Symphonia) : rien à installer.
//!
//! ```text
//! <rencontre>/
//!   source/enregistrement.m4a        copie du fichier importé, jamais modifiée
//!   _import/index-audio.json         dossier de travail : blocs d'environ 2 minutes, hors point,
//!   _import/00-hors-point/passage-01/bloc-001.wav    en attente du découpage par point
//! ```
//!
//! Le fichier est décodé en flux (jamais entièrement en mémoire), écrit en blocs par le même
//! `Ecrivain` que pendant une rencontre, puis transcrit bloc par bloc. À la validation du
//! découpage, `audio::frontieres::appliquer` range les blocs par point et supprime `_import/`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result, anyhow, bail};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use super::blocs::{self, Ecrivain, Index};
use super::dsp::{Resampler, Segmenter, Utterance, downmix};
use super::{SAMPLE_RATE, Source};
use crate::config;

/// Dossier de travail de l'import, dans le dossier de la rencontre.
pub const TRAVAIL: &str = "_import";
/// Copie du fichier importé.
pub const SOURCE: &str = "source";

/// Formats acceptés : enregistrements Teams (.mp4), Zoom et téléphones (.m4a),
/// MP3, WAV, FLAC, OGG.
pub const EXTENSIONS: &[&str] = &["mp3", "m4a", "mp4", "aac", "wav", "flac", "ogg", "oga"];

pub fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.contains(&e.to_lowercase().as_str()))
}

/// Décode le fichier en audio mono 16 kHz (format attendu par Whisper), au fil de la lecture :
/// `rappel` reçoit l'audio de chaque paquet dès qu'il est décodé, le fichier n'est jamais
/// entièrement en mémoire. Retourne le nombre d'échantillons produits.
pub fn decode(path: &Path, mut rappel: impl FnMut(&[f32]) -> Result<()>) -> Result<u64> {
    let file = std::fs::File::open(path).with_context(|| format!("Ouverture de {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .context("Format de fichier non reconnu")?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| anyhow!("Aucune piste audio dans ce fichier"))?;
    let track_id = track.id;
    let params = match &track.codec_params {
        Some(CodecParameters::Audio(p)) => p.clone(),
        _ => bail!("Piste audio illisible"),
    };
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .context("Codec audio non pris en charge")?;

    let mut out = Vec::new();
    let mut total = 0u64;
    let mut resampler: Option<(u32, Resampler)> = None;
    let mut interleaved: Vec<f32> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e).context("Lecture du fichier"),
        };
        if packet.track_id != track_id {
            continue;
        }
        let buf = match decoder.decode(&packet) {
            Ok(buf) => buf,
            // Un paquet abîmé n'empêche pas de lire le reste.
            Err(Error::DecodeError(e)) => {
                log::warn!("Paquet audio ignoré : {e}");
                continue;
            }
            Err(e) => return Err(e).context("Décodage audio"),
        };
        let rate = buf.spec().rate();
        let channels = buf.spec().channels().count().max(1);
        interleaved.clear();
        buf.copy_to_vec_interleaved(&mut interleaved);
        let mono = downmix(&interleaved, channels);
        // Une fréquence qui change en cours de fichier (rare) recrée le rééchantillonneur.
        if resampler.as_ref().is_none_or(|(r, _)| *r != rate) {
            resampler = Some((rate, Resampler::new(rate)));
        }
        out.clear();
        resampler.as_mut().unwrap().1.process(&mono, &mut out);
        if !out.is_empty() {
            total += out.len() as u64;
            rappel(&out)?;
        }
    }
    if total < SAMPLE_RATE as u64 {
        bail!("Le fichier contient moins d'une seconde d'audio");
    }
    Ok(total)
}

/// Avancement d'un import, lu par l'interface.
#[derive(Default)]
pub struct Avancement {
    /// Blocs écrits dans `_import/` pendant le décodage.
    pub ecrits: AtomicUsize,
    /// Nombre de blocs, connu à la fin du décodage (0 avant).
    pub total: AtomicUsize,
    /// Énoncés transcrits (ou en erreur) depuis le début de l'import.
    pub enonces_transcrits: AtomicUsize,
    /// Énoncés envoyés à la transcription, cumulés bloc par bloc.
    cumul: Mutex<Vec<usize>>,
}

impl Avancement {
    /// Note les énoncés d'un bloc envoyés à la transcription.
    pub fn bloc_envoye(&self, enonces: usize) {
        let mut c = self.cumul.lock().unwrap_or_else(|e| e.into_inner());
        let avant = c.last().copied().unwrap_or(0);
        c.push(avant + enonces);
    }

    /// Énoncés envoyés avant le bloc `k`.
    pub fn envoyes_avant(&self, k: usize) -> usize {
        let c = self.cumul.lock().unwrap_or_else(|e| e.into_inner());
        k.checked_sub(1).and_then(|i| c.get(i)).copied().unwrap_or(0)
    }

    /// Blocs dont tous les énoncés sont transcrits.
    pub fn blocs_transcrits(&self) -> usize {
        let fait = self.enonces_transcrits.load(Ordering::Relaxed);
        let c = self.cumul.lock().unwrap_or_else(|e| e.into_inner());
        c.iter().take_while(|&&n| n <= fait).count()
    }
}

/// Copie le fichier importé dans `source/` du dossier de la rencontre ; retourne la copie.
pub fn copier_source(fichier: &Path, dossier: &Path) -> Result<PathBuf> {
    let dest = dossier.join(SOURCE);
    std::fs::create_dir_all(&dest).with_context(|| format!("Création de {}", dest.display()))?;
    let nom = fichier.file_name().ok_or_else(|| anyhow!("Nom de fichier invalide"))?;
    let copie = dest.join(nom);
    let tmp = dest.join(format!("{}.tmp", nom.to_string_lossy()));
    std::fs::copy(fichier, &tmp).with_context(|| format!("Copie de {}", fichier.display()))?;
    std::fs::rename(&tmp, &copie).context("Copie du fichier importé")?;
    Ok(copie)
}

/// L'import attend-il encore son découpage par point (blocs dans `_import/`) ?
pub fn en_attente(dossier: &Path) -> bool {
    blocs::existe(&dossier.join(TRAVAIL))
}

/// Décode `fichier` et l'écrit en blocs d'environ 2 minutes dans `travail`, coupés au silence
/// comme pendant une rencontre. Retourne l'index des blocs.
pub fn ecrire_blocs(fichier: &Path, travail: &Path, avancement: &Avancement) -> Result<Index> {
    std::fs::create_dir_all(travail).with_context(|| format!("Création de {}", travail.display()))?;
    let mut ecrivain = Ecrivain::new(travail.to_path_buf(), vec![Source::Mixte], Vec::new());
    // Le découpeur ne sert qu'à savoir si quelqu'un parle : les énoncés sont produits plus tard,
    // bloc par bloc.
    let mut seg = Segmenter::new(Source::Mixte, config::VAD_SENSITIVITY);
    let mut jetes = Vec::new();
    // Morceaux de 30 ms, comme les trames du découpeur.
    let morceau = SAMPLE_RATE as usize * 30 / 1000;
    decode(fichier, |audio| {
        for m in audio.chunks(morceau) {
            seg.push(m, &mut jetes);
            jetes.clear();
            ecrivain.pousser(Source::Mixte, m);
            ecrivain.avancer(seg.en_enonce())?;
        }
        avancement.ecrits.store(ecrivain.blocs_fermes(), Ordering::Relaxed);
        Ok(())
    })?;
    let index = ecrivain.terminer()?;
    avancement.ecrits.store(index.blocs.len(), Ordering::Relaxed);
    avancement.total.store(index.blocs.len(), Ordering::Relaxed);
    Ok(index)
}

/// Relit les blocs dans l'ordre et en tire les énoncés : `rappel(k, énoncés)` est appelé une fois
/// par bloc. Un seul découpeur pour tous les blocs : un énoncé à cheval sur deux blocs revient
/// entier avec le second. Un bloc illisible est remplacé par du silence (horodatages intacts).
pub fn transcrire_blocs(travail: &Path, index: &Index, mut rappel: impl FnMut(usize, Vec<Utterance>) -> Result<()>) -> Result<()> {
    let mut seg = Segmenter::new(Source::Mixte, config::VAD_SENSITIVITY);
    let n = index.blocs.len();
    for (k, b) in index.blocs.iter().enumerate() {
        let audio: Vec<f32> = match blocs::lire_bloc(travail, b) {
            Ok(mut v) => {
                v.resize(b.echantillons as usize, 0);
                v.into_iter().map(blocs::dequantifier).collect()
            }
            Err(e) => {
                log::warn!("Import : {} illisible ({e:#}), remplacé par du silence", b.fichier);
                vec![0.0; b.echantillons as usize]
            }
        };
        let mut enonces = Vec::new();
        seg.push(&audio, &mut enonces);
        if k + 1 == n {
            seg.flush(&mut enonces);
        }
        rappel(k, enonces)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_un_wav_stereo_48k() {
        let dir = std::env::temp_dir().join(format!("tdt-import-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("essai.wav");
        let spec = hound::WavSpec { channels: 2, sample_rate: 48_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for i in 0..96_000 {
            let s = ((i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 10_000.0) as i16;
            w.write_sample(s).unwrap();
            w.write_sample(s).unwrap();
        }
        w.finalize().unwrap();
        let mut audio = Vec::new();
        let n = decode(&path, |m| {
            audio.extend_from_slice(m);
            Ok(())
        })
        .unwrap();
        // 2 s à 16 kHz
        assert_eq!(n, audio.len() as u64);
        assert!((audio.len() as i64 - 32_000).abs() < 200, "{}", audio.len());
        assert!(is_audio(&path) && !is_audio(Path::new("ordre.docx")));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 10 minutes à 16 kHz font 38 Mo une fois décodées : ni le décodage ni l'écriture des blocs
    /// ne doivent en garder plus qu'une infime partie en mémoire.
    #[test]
    fn memoire_bornee_pendant_le_decodage() {
        let dir = std::env::temp_dir().join(format!("tdt-import-memoire-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("long.wav");
        let spec = hound::WavSpec { channels: 1, sample_rate: SAMPLE_RATE, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        let secs = 600u32;
        for i in 0..secs * SAMPLE_RATE {
            // Parole (sinus) 6 s sur 10, comme dans une rencontre.
            let voix = (i / SAMPLE_RATE) % 10 < 6;
            let s = if voix { ((i as f32 * 300.0 * std::f32::consts::TAU / SAMPLE_RATE as f32).sin() * 8_000.0) as i16 } else { (i % 7) as i16 };
            w.write_sample(s).unwrap();
        }
        w.finalize().unwrap();
        let taille_decodee = secs as usize * SAMPLE_RATE as usize * std::mem::size_of::<f32>();

        let (mut n, mut plus_grand) = (0u64, 0usize);
        let (r, pointe) = crate::memoire::pointe(|| {
            decode(&path, |m| {
                n += m.len() as u64;
                plus_grand = plus_grand.max(m.len());
                Ok(())
            })
        });
        assert_eq!(r.unwrap(), (secs * SAMPLE_RATE) as u64);
        assert_eq!(n, (secs * SAMPLE_RATE) as u64);
        assert!(plus_grand < SAMPLE_RATE as usize, "morceau de {plus_grand} échantillons");
        assert!(pointe < 1 << 20, "décodage : {pointe} octets alloués au plus fort (fichier décodé : {taille_decodee})");

        let avancement = Avancement::default();
        let (index, pointe) = crate::memoire::pointe(|| ecrire_blocs(&path, &dir.join(TRAVAIL), &avancement).unwrap());
        assert_eq!(index.blocs.len(), 5);
        assert_eq!(avancement.total.load(Ordering::Relaxed), 5);
        // Le plus gros : l'énoncé en cours du découpeur (25 s au plus).
        assert!(pointe < 8 << 20, "écriture des blocs : {pointe} octets alloués au plus fort (fichier décodé : {taille_decodee})");
        let _ = std::fs::remove_dir_all(dir);
    }
}
