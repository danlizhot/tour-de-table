//! Import d'enregistrements existants (Teams, Zoom, téléphone, enregistreuse…).
//! Décodage entièrement en Rust (Symphonia) : rien à installer.

use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use super::SAMPLE_RATE;
use super::dsp::{Resampler, downmix};

/// Formats acceptés : enregistrements Teams (.mp4), Zoom et téléphones (.m4a),
/// MP3, WAV, FLAC, OGG.
pub const EXTENSIONS: &[&str] = &["mp3", "m4a", "mp4", "aac", "wav", "flac", "ogg", "oga"];

pub fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.contains(&e.to_lowercase().as_str()))
}

/// Décode tout le fichier en audio mono 16 kHz (format attendu par Whisper).
pub fn decode(path: &Path) -> Result<Vec<f32>> {
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
        resampler.as_mut().unwrap().1.process(&mono, &mut out);
    }
    if out.len() < SAMPLE_RATE as usize {
        bail!("Le fichier contient moins d'une seconde d'audio");
    }
    Ok(out)
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
        let audio = decode(&path).unwrap();
        // 2 s à 16 kHz
        assert!((audio.len() as i64 - 32_000).abs() < 200, "{}", audio.len());
        assert!(is_audio(&path) && !is_audio(Path::new("ordre.docx")));
        let _ = std::fs::remove_dir_all(dir);
    }
}
