//! Programme auxiliaire de Tour de table : la seule partie liée à sherpa-onnx (et onnxruntime).
//!
//! Il est séparé de l'application principale parce que les bibliothèques précompilées de sherpa-onnx et
//! d'onnxruntime (GCC 11 sous Linux) contiennent leurs propres copies des modèles C++ de la bibliothèque
//! standard, dont `std::regex`. Réunies dans un même exécutable avec whisper.cpp, compilé par un GCC plus
//! récent, l'éditeur de liens n'en garde qu'une : whisper.cpp appelait le constructeur de `std::regex`
//! de GCC 11 et son destructeur de GCC 15, d'où « free(): invalid pointer » à la première transcription.
//!
//! Tous les réglages et le regroupement restent dans l'application (`voix.rs`, `voix_direct.rs`). Audio mono
//! à 16 kHz, nombres en petit-boutiste. Deux modes :
//!
//! * `tourdetable-voix tours <segmentation.onnx> <empreinte.onnx> <fils> <fenêtre> <seuil>` : tours de parole
//!   (pyannote). Entrée : le nombre d'échantillons (u64) puis l'audio (f32) ; l'entrée reste ouverte pendant
//!   le calcul, et sa fermeture arrête le programme (processus parent tué). Sortie : une ligne JSON
//!   `[[début_ms, fin_ms, voix du moteur]…]`, dans l'ordre.
//! * `tourdetable-voix empreintes <empreinte.onnx> <fils>` : pour chaque morceau reçu (nombre d'échantillons
//!   en u32, puis f32), une ligne JSON : l'empreinte brute, ou `null` si le morceau est trop court. S'arrête à
//!   la fermeture de l'entrée.

use std::io::{BufReader, BufWriter, Read, Write};

use anyhow::{Context, Result, bail};
use sherpa_onnx::{OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig, SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig};

const FREQUENCE: i32 = 16_000;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let resultat = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["tours", segmentation, empreinte, fils, fenetre, seuil] => (|| {
            tours(segmentation, empreinte, fils.parse()?, fenetre.parse()?, seuil.parse()?)
        })(),
        ["empreintes", empreinte, fils] => fils.parse().map_err(Into::into).and_then(|f| empreintes(empreinte, f)),
        _ => {
            eprintln!("Usage : tourdetable-voix tours segmentation.onnx empreinte.onnx fils fenêtre seuil < audio");
            eprintln!("        tourdetable-voix empreintes empreinte.onnx fils < morceaux");
            std::process::exit(2);
        }
    };
    if let Err(e) = resultat {
        eprintln!("Erreur : {e:#}");
        std::process::exit(1);
    }
}

fn lire_f32(entree: &mut impl Read, n: usize) -> Result<Vec<f32>> {
    let mut octets = vec![0u8; n.checked_mul(4).context("Audio trop long")?];
    entree.read_exact(&mut octets)?;
    Ok(octets.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect())
}

fn tours(segmentation: &str, empreinte: &str, fils: i32, fenetre: f32, seuil: f32) -> Result<()> {
    let mut entree = std::io::stdin();
    let mut n = [0u8; 8];
    entree.read_exact(&mut n).context("Lecture de l'audio")?;
    let audio = lire_f32(&mut entree, usize::try_from(u64::from_le_bytes(n))?).context("Lecture de l'audio")?;
    // Le parent garde l'entrée ouverte jusqu'à la fin : si elle se ferme avant, il a été tué.
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut entree, &mut std::io::sink());
        std::process::exit(1);
    });
    let mut c = OfflineSpeakerDiarizationConfig::default();
    c.segmentation.pyannote.model = Some(segmentation.to_string());
    c.segmentation.pyannote.window_shift_ratio = fenetre;
    c.segmentation.num_threads = fils;
    c.embedding.model = Some(empreinte.to_string());
    c.embedding.num_threads = fils;
    c.clustering.num_clusters = -1;
    c.clustering.threshold = seuil;
    let moteur = OfflineSpeakerDiarization::create(&c).context("Chargement des modèles de reconnaissance des voix")?;
    let res = moteur.process(&audio).context("Reconnaissance des voix")?;
    let tours: Vec<(u64, u64, i32)> =
        res.sort_by_start_time().iter().map(|s| ((s.start * 1000.0) as u64, (s.end * 1000.0) as u64, s.speaker)).collect();
    let mut sortie = std::io::stdout().lock();
    serde_json::to_writer(&mut sortie, &tours)?;
    writeln!(sortie)?;
    sortie.flush()?;
    Ok(())
}

fn empreintes(empreinte: &str, fils: i32) -> Result<()> {
    let ec = SpeakerEmbeddingExtractorConfig { model: Some(empreinte.to_string()), num_threads: fils, ..Default::default() };
    let extracteur = SpeakerEmbeddingExtractor::create(&ec).context("Chargement du modèle d'empreinte")?;
    let mut entree = BufReader::new(std::io::stdin().lock());
    let mut sortie = BufWriter::new(std::io::stdout().lock());
    loop {
        let mut n = [0u8; 4];
        match entree.read_exact(&mut n) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e.into()),
        }
        let echantillons = lire_f32(&mut entree, u32::from_le_bytes(n) as usize)?;
        let Some(flux) = extracteur.create_stream() else { bail!("Flux d'empreinte") };
        flux.accept_waveform(FREQUENCE, &echantillons);
        flux.input_finished();
        let e = if extracteur.is_ready(&flux) { extracteur.compute(&flux) } else { None };
        serde_json::to_writer(&mut sortie, &e)?;
        writeln!(sortie)?;
        sortie.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_lu_tel_quel() {
        let audio = [0.25f32, -1.0, 0.1];
        let octets: Vec<u8> = audio.iter().flat_map(|x| x.to_le_bytes()).collect();
        assert_eq!(lire_f32(&mut octets.as_slice(), 3).unwrap(), audio);
    }
}
