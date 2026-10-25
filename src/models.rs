//! Catalogue et téléchargement des modèles : Whisper (format ggml de whisper.cpp) et rédaction
//! (GGUF, llama.cpp). Le modèle de rédaction est choisi d'après la mémoire vive, sans choix
//! offert à l'utilisateur.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result, bail};

pub const RECOMMENDED: &str = "ggml-large-v3-turbo-q5_0.bin";

pub struct ModelInfo {
    pub file: &'static str,
    pub label: &'static str,
    pub size_mb: u32,
}

/// Le seul modèle Whisper : multilingue (les modèles « .en » ne savent pas le
/// français), quantifié en 5 bits, ~1,2 Go de RAM en transcription, convient dès 8 Go.
pub const WHISPER: ModelInfo = ModelInfo { file: RECOMMENDED, label: "Large v3 Turbo (q5)", size_mb: 574 };

/// Taille exacte et SHA-256 de chaque fichier téléchargé, vérifiés à la fin du téléchargement.
struct Empreinte {
    file: &'static str,
    octets: u64,
    sha256: &'static str,
}

const EMPREINTES: &[Empreinte] = &[Empreinte {
    file: RECOMMENDED,
    octets: 574_041_195,
    sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
}];

/// Palier de mémoire vive : avec 8 Go, Whisper est déchargé pendant la rédaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Palier {
    /// 8 Go et moins : Whisper est déchargé pendant la rédaction.
    Huit,
    /// 16 Go et plus.
    Seize,
}

const GIO: u64 = 1 << 30;

impl Palier {
    /// Un poste « de 16 Go » en déclare un peu moins (mémoire réservée au système ou à la carte
    /// graphique intégrée) : les seuils sont donc sous les tailles nominales.
    pub fn selon_ram(octets: u64) -> Self {
        match octets {
            o if o >= 14 * GIO => Palier::Seize,
            _ => Palier::Huit,
        }
    }

    pub fn nom(self) -> &'static str {
        match self {
            Palier::Huit => "8 Go",
            Palier::Seize => "16 Go",
        }
    }
}

/// Modèle de langue du compte rendu (GGUF Q4_K_M, llama.cpp), le même sur tous les postes.
/// Licence Apache 2.0 : utilisable sans restriction par un organisme. Qwen3.5 4B (architecture
/// hybride, attention linéaire « Gated DeltaNet ») lit les longues transcriptions vite : sur une
/// vraie rencontre de 27 min (processeur seul), 13 min de compte rendu contre 29 pour Qwen3 4B
/// Instruct 2507. Il réfléchit par défaut : le programme de rédaction le fait répondre
/// directement. Ministral 3 8B était plus lent encore ; Qwen3 30B-A3B demandait 18,6 Go.
pub struct LlmInfo {
    pub file: &'static str,
    pub label: &'static str,
    pub url: &'static str,
    pub octets: u64,
    pub sha256: &'static str,
    /// Mémoire vive pendant la rédaction (contexte de 8 000 jetons compris), estimée.
    #[allow(dead_code)]
    pub ram_mb: u32,
}

pub const LLM_CATALOG: &[LlmInfo] = &[
    LlmInfo {
        file: "Qwen3.5-4B-Q4_K_M.gguf",
        label: "Qwen3.5 4B",
        url: "https://huggingface.co/unsloth/Qwen3.5-4B-GGUF/resolve/main/Qwen3.5-4B-Q4_K_M.gguf",
        octets: 2_740_937_888,
        sha256: "00fe7986ff5f6b463e62455821146049db6f9313603938a70800d1fb69ef11a4",
        ram_mb: 3300,
    },
];

pub fn llm_info(file: &str) -> Option<&'static LlmInfo> {
    LLM_CATALOG.iter().find(|m| m.file == file)
}

static PALIER: OnceLock<Palier> = OnceLock::new();

/// Palier de ce poste, calculé une fois par lancement d'après la mémoire vive totale.
pub fn palier() -> Palier {
    *PALIER.get_or_init(|| {
        let ram = ram_totale();
        let p = Palier::selon_ram(ram);
        log::info!("Mémoire vive : {:.1} Gio, palier {}", ram as f64 / GIO as f64, p.nom());
        p
    })
}

/// Mesure du nettoyage et du compte rendu sur un autre palier que celui du poste (ligne de
/// commande seulement). Sans effet si le palier a déjà été calculé.
pub fn forcer_palier(p: Palier) {
    let _ = PALIER.set(p);
}

/// Modèle de rédaction.
pub fn modele_redaction() -> &'static LlmInfo {
    &LLM_CATALOG[0]
}

fn ram_totale() -> u64 {
    use sysinfo::{MemoryRefreshKind, RefreshKind, System};
    System::new_with_specifics(RefreshKind::nothing().with_memory(MemoryRefreshKind::nothing().with_ram())).total_memory()
}

/// Modèles de rédaction d'un autre palier encore présents (mémoire ajoutée ou retirée depuis
/// leur téléchargement) : à supprimer une fois le modèle du palier actuel installé.
#[allow(dead_code)]
pub fn modeles_redaction_obsoletes() -> Vec<PathBuf> {
    let actuel = modele_redaction().file;
    LLM_CATALOG.iter().filter(|m| m.file != actuel).map(|m| path(m.file)).filter(|p| p.exists()).collect()
}

fn attendu(file: &str) -> Option<(u64, &'static str)> {
    llm_info(file).map(|m| (m.octets, m.sha256)).or_else(|| EMPREINTES.iter().find(|e| e.file == file).map(|e| (e.octets, e.sha256)))
}

fn url(file: &str) -> String {
    match llm_info(file) {
        Some(info) => info.url.to_string(),
        None => format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{file}"),
    }
}

pub fn path(file: &str) -> PathBuf {
    crate::config::models_dir().join(file)
}

/// Fichier présent et de la taille attendue (l'empreinte, coûteuse, n'est vérifiée qu'au téléchargement).
pub fn installed(file: &str) -> bool {
    let taille = path(file).metadata().map_or(0, |m| m.len());
    match attendu(file) {
        Some((octets, _)) => taille == octets,
        None => taille > 1_000_000,
    }
}

#[derive(Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub cancel: AtomicBool,
}

/// Télécharge `file` dans le dossier des modèles. Un téléchargement interrompu reprend où il
/// s'était arrêté (fichier `.part`, en-tête Range) ; la taille et le SHA-256 sont vérifiés à la fin.
pub fn download(file: &str, progress: Arc<Progress>) -> Result<()> {
    let dest = path(file);
    std::fs::create_dir_all(dest.parent().unwrap())?;
    let part = dest.with_extension(format!("{}.part", dest.extension().unwrap_or_default().to_string_lossy()));
    let attendu = attendu(file);

    let mut deja = part.metadata().map_or(0, |m| m.len());
    if attendu.is_some_and(|(octets, _)| deja > octets) {
        deja = 0;
    }
    if attendu.is_none_or(|(octets, _)| deja < octets) {
        let mut requete = ureq::get(&url(file));
        if deja > 0 {
            requete = requete.header("Range", format!("bytes={deja}-"));
        }
        let mut resp = requete.call().context("Connexion à Hugging Face")?;
        // 206 : le serveur reprend ; 200 : il renvoie tout, on recommence.
        if resp.status() != 206 {
            deja = 0;
        }
        let reste = resp.body().content_length().unwrap_or(0);
        let total = attendu.map_or(deja + reste, |(octets, _)| octets);
        progress.total.store(total, Ordering::Relaxed);
        let mut reader = resp.body_mut().as_reader();
        let fichier = std::fs::OpenOptions::new().create(true).write(true).append(deja > 0).truncate(deja == 0).open(&part)?;
        let mut out = std::io::BufWriter::new(fichier);
        let mut buf = vec![0u8; 1 << 16];
        let mut done = deja;
        progress.done.store(done, Ordering::Relaxed);
        loop {
            if progress.cancel.load(Ordering::Relaxed) {
                // Le fichier .part reste : le prochain téléchargement reprendra là.
                out.flush()?;
                bail!("Téléchargement annulé");
            }
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
            done += n as u64;
            progress.done.store(done, Ordering::Relaxed);
        }
        out.flush()?;
        if attendu.is_none() && reste > 0 && done != deja + reste {
            bail!("Téléchargement incomplet ({done} / {} octets) : relancez-le pour le reprendre", deja + reste);
        }
    }
    if let Some((octets, sha256)) = attendu
        && let Err(e) = verifier(&part, octets, sha256)
    {
        // Une partie corrompue ne doit pas être reprise : on repartira de zéro.
        if part.metadata().map_or(0, |m| m.len()) >= octets {
            let _ = std::fs::remove_file(&part);
        }
        return Err(e);
    }
    std::fs::rename(&part, &dest)?;
    Ok(())
}

/// Taille exacte puis SHA-256 du fichier.
pub fn verifier(chemin: &Path, octets: u64, sha256: &str) -> Result<()> {
    let taille = chemin.metadata()?.len();
    if taille != octets {
        bail!("Téléchargement incomplet ({taille} / {octets} octets) : relancez-le pour le reprendre");
    }
    let calcule = crate::audio::blocs::empreinte(chemin)?;
    if calcule != sha256 {
        bail!("Fichier corrompu (SHA-256 {calcule}, attendu {sha256}) : il sera retéléchargé");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palier_selon_la_ram() {
        assert_eq!(Palier::selon_ram(4 * GIO), Palier::Huit);
        // Poste de 8 Go : ~7,6 Gio déclarés.
        assert_eq!(Palier::selon_ram(7 * GIO + GIO / 2), Palier::Huit);
        assert_eq!(Palier::selon_ram(14 * GIO - 1), Palier::Huit);
        // Poste de 16 Go : ~15,3 Gio, parfois moins avec une carte graphique intégrée.
        assert_eq!(Palier::selon_ram(14 * GIO), Palier::Seize);
        assert_eq!(Palier::selon_ram(15 * GIO + GIO / 3), Palier::Seize);
        // Poste de 32 Go et plus.
        assert_eq!(Palier::selon_ram(31 * GIO), Palier::Seize);
        assert_eq!(Palier::selon_ram(128 * GIO), Palier::Seize);
        assert_eq!(modele_redaction().file, "Qwen3.5-4B-Q4_K_M.gguf");
    }

    #[test]
    fn catalogue_coherent() {
        for m in LLM_CATALOG {
            assert!(m.url.ends_with(m.file), "{}", m.file);
            assert_eq!(m.sha256.len(), 64);
            assert!(m.sha256.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
            assert_eq!(attendu(m.file), Some((m.octets, m.sha256)));
        }
        assert_eq!(attendu(RECOMMENDED).map(|(o, _)| o), Some(574_041_195));
        assert_eq!(attendu("inconnu.bin"), None);
    }

    #[test]
    fn verification_sha256() {
        let dir = std::env::temp_dir().join(format!("tourdetable-test-{}-sha", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("modele.gguf.part");
        std::fs::write(&f, b"abc").unwrap();
        // SHA-256 de « abc ».
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        verifier(&f, 3, abc).unwrap();
        let e = verifier(&f, 4, abc).unwrap_err().to_string();
        assert!(e.contains("incomplet (3 / 4 octets)"), "{e}");
        let e = verifier(&f, 3, &"0".repeat(64)).unwrap_err().to_string();
        assert!(e.contains("corrompu") && e.contains(abc), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
