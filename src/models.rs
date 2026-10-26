//! Catalogue et téléchargement des modèles : Whisper (format ggml de whisper.cpp) et rédaction
//! (GGUF, llama.cpp). Le même modèle de rédaction sur tous les postes, sans choix offert à
//! l'utilisateur. Les deux sont installés automatiquement au premier démarrage
//! (`ui/installation.rs`) : l'application est distribuée sans aucun modèle.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result, bail};

pub const RECOMMENDED: &str = "ggml-large-v3-turbo-q5_0.bin";

pub struct ModelInfo {
    pub file: &'static str,
    pub label: &'static str,
}

/// Le seul modèle Whisper : multilingue (les modèles « .en » ne savent pas le
/// français), quantifié en 5 bits, ~1,2 Go de RAM en transcription.
pub const WHISPER: ModelInfo = ModelInfo { file: RECOMMENDED, label: "Large v3 Turbo (q5)" };

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

const GIO: u64 = 1 << 30;

/// Mémoire vive minimale : 16 Go. Un poste « de 16 Go » en déclare un peu moins (mémoire
/// réservée au système ou à la carte graphique intégrée) : le seuil est donc sous la taille
/// nominale.
const MEMOIRE_MINIMALE: u64 = 14 * GIO;

fn memoire_suffisante(octets: u64) -> bool {
    octets >= MEMOIRE_MINIMALE
}

/// Mémoire vive du poste en Gio si elle est sous le minimum de 16 Go, calculée au lancement.
pub fn memoire_insuffisante() -> Option<f64> {
    let ram = ram_totale();
    log::info!("Mémoire vive : {:.1} Gio", ram as f64 / GIO as f64);
    (!memoire_suffisante(ram)).then_some(ram as f64 / GIO as f64)
}

/// Modèle de langue du compte rendu (GGUF Q4_K_M, llama.cpp), le même sur tous les postes.
/// Licence Apache 2.0 : utilisable sans restriction par un organisme. Qwen3.5 9B (architecture
/// hybride, attention linéaire « Gated DeltaNet ») lit les longues transcriptions vite pour sa
/// taille. Qwen3.5 4B, plus rapide (13 min de compte rendu pour une rencontre de 27 min, processeur
/// seul), inventait trop de sujets : d'où le minimum de 16 Go. Il réfléchit par défaut : le
/// programme de rédaction le fait répondre directement. Ministral 3 8B était plus lent ; Qwen3
/// 30B-A3B demandait 18,6 Go.
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
        file: "Qwen3.5-9B-Q4_K_M.gguf",
        label: "Qwen3.5 9B",
        url: "https://huggingface.co/unsloth/Qwen3.5-9B-GGUF/resolve/main/Qwen3.5-9B-Q4_K_M.gguf",
        octets: 5_680_522_464,
        sha256: "03b74727a860a56338e042c4420bb3f04b2fec5734175f4cb9fa853daf52b7e8",
        ram_mb: 6600,
    },
];

/// Modèles de rédaction des versions précédentes, supprimés une fois le modèle actuel installé.
const ANCIENS_MODELES_REDACTION: &[&str] = &[
    "Qwen3.5-4B-Q4_K_M.gguf",
    "Qwen3-4B-Instruct-2507-Q4_K_M.gguf",
    "Qwen3-30B-A3B-Instruct-2507-Q4_K_M.gguf",
    "Ministral-3-8B-Instruct-2512-Q4_K_M.gguf",
    "Qwen2.5-7B-Instruct-Q4_K_M.gguf",
];

pub fn llm_info(file: &str) -> Option<&'static LlmInfo> {
    LLM_CATALOG.iter().find(|m| m.file == file)
}

/// Modèle de rédaction.
pub fn modele_redaction() -> &'static LlmInfo {
    &LLM_CATALOG[0]
}

fn ram_totale() -> u64 {
    use sysinfo::{MemoryRefreshKind, RefreshKind, System};
    System::new_with_specifics(RefreshKind::nothing().with_memory(MemoryRefreshKind::nothing().with_ram())).total_memory()
}

/// Modèles de rédaction remplacés encore présents, avec leurs fichiers `.part` et `.verifie` : à
/// supprimer une fois le modèle actuel installé et vérifié.
pub fn modeles_redaction_obsoletes() -> Vec<PathBuf> {
    let actuel = modele_redaction().file;
    LLM_CATALOG
        .iter()
        .map(|m| m.file)
        .chain(ANCIENS_MODELES_REDACTION.iter().copied())
        .filter(|f| *f != actuel)
        .flat_map(|f| [path(f), partiel(f), marque(f)])
        .filter(|p| p.exists())
        .collect()
}

/// Modèle à installer, avec un libellé en langage simple pour l'écran d'installation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Requis {
    pub file: &'static str,
    pub libelle: &'static str,
    pub octets: u64,
}

/// Modèles nécessaires sur ce poste, dans l'ordre d'installation : la reconnaissance vocale
/// d'abord (l'application est utilisable dès qu'elle est là), puis la rédaction.
pub fn requis() -> [Requis; 2] {
    let redaction = modele_redaction();
    [
        Requis { file: RECOMMENDED, libelle: "Reconnaissance de la parole", octets: EMPREINTES[0].octets },
        Requis { file: redaction.file, libelle: "Rédaction des comptes rendus", octets: redaction.octets },
    ]
}

/// État d'un modèle au démarrage. Vérification rapide : la taille, et la marque laissée par
/// une vérification complète précédente ; l'empreinte n'est recalculée que pour un fichier
/// jamais vérifié (copié à l'avance par le service informatique, ou téléchargé par une version
/// antérieure).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Etat {
    Pret,
    AVerifier,
    ATelecharger,
}

pub fn etat(file: &str) -> Etat {
    if !installed(file) {
        return Etat::ATelecharger;
    }
    match attendu(file) {
        Some((_, sha256)) if std::fs::read_to_string(marque(file)).is_ok_and(|m| m.trim() == sha256) => Etat::Pret,
        Some(_) => Etat::AVerifier,
        None => Etat::Pret,
    }
}

/// Empreinte complète d'un modèle déjà en place ; un fichier corrompu est supprimé.
pub fn verifier_installe(file: &str) -> Result<()> {
    let (octets, sha256) = attendu(file).with_context(|| format!("Modèle inconnu : {file}"))?;
    if let Err(e) = verifier(&path(file), octets, sha256) {
        supprimer(file);
        return Err(e);
    }
    marquer(file, sha256);
    Ok(())
}

/// Octets qui restent à télécharger (un fichier `.part` reprend là où il s'était arrêté).
pub fn octets_restants(r: &Requis) -> u64 {
    r.octets.saturating_sub(partiel(r.file).metadata().map_or(0, |m| m.len()).min(r.octets))
}

/// Retire un modèle, sa partie téléchargée et sa marque de vérification.
pub fn supprimer(file: &str) {
    for p in [path(file), partiel(file), marque(file)] {
        if p.exists()
            && let Err(e) = std::fs::remove_file(&p)
        {
            log::warn!("Suppression de {} : {e}", p.display());
        }
    }
}

/// Marque d'une vérification complète réussie : l'empreinte attendue, à côté du modèle.
fn marque(file: &str) -> PathBuf {
    crate::config::models_dir().join(format!("{file}.verifie"))
}

fn marquer(file: &str, sha256: &str) {
    if let Err(e) = std::fs::write(marque(file), sha256) {
        log::warn!("Marque de vérification de {file} : {e}");
    }
}

fn partiel(file: &str) -> PathBuf {
    crate::config::models_dir().join(format!("{file}.part"))
}

/// Espace libre du disque qui contient `dossier` (ou son plus proche parent existant).
pub fn espace_libre(dossier: &Path) -> Option<u64> {
    let existant = dossier.ancestors().find(|p| p.exists())?;
    let chemin = std::fs::canonicalize(existant).unwrap_or_else(|_| existant.to_path_buf());
    // Windows : canonicalize donne « \\?\C:\… », les points de montage sont « C:\ ».
    let chemin = PathBuf::from(chemin.to_string_lossy().trim_start_matches(r"\\?\"));
    let disques = sysinfo::Disks::new_with_refreshed_list();
    disques
        .list()
        .iter()
        .filter(|d| chemin.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(sysinfo::Disk::available_space)
}

/// Serveur des modèles injoignable, connexion coupée ou refusée : on peut réessayer.
#[derive(Debug)]
pub struct Reseau(pub String);

impl std::fmt::Display for Reseau {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Reseau {}

/// Fichier complet dont l'empreinte ne correspond pas : il a été supprimé.
#[derive(Debug)]
pub struct Corrompu(pub String);

impl std::fmt::Display for Corrompu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Corrompu {}

/// Client HTTP des téléchargements. Réseaux d'organisation : le proxy des variables
/// `HTTPS_PROXY`, `HTTP_PROXY` ou `ALL_PROXY` (et `NO_PROXY`) est suivi, et les certificats
/// sont vérifiés par le magasin du système, qui contient l'autorité d'un proxy qui inspecte
/// le trafic chiffré.
fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        let tls = ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build();
        let config = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_recv_response(Some(Duration::from_secs(60)))
            .tls_config(tls)
            .build();
        match config.proxy() {
            Some(p) => log::info!("Téléchargements par le proxy {}", p.uri()),
            None => log::info!("Téléchargements sans proxy"),
        }
        ureq::Agent::new_with_config(config)
    })
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
/// s'était arrêté (fichier `.part`, en-tête Range) ; la taille et le SHA-256 sont vérifiés à la
/// fin. Erreurs : [`Reseau`] (on peut réessayer), [`Corrompu`] (partie supprimée), ou locale
/// (disque plein, dossier interdit en écriture).
pub fn download(file: &str, progress: Arc<Progress>) -> Result<()> {
    let dest = path(file);
    std::fs::create_dir_all(dest.parent().unwrap())?;
    let part = partiel(file);
    let attendu = attendu(file);

    let mut deja = part.metadata().map_or(0, |m| m.len());
    if attendu.is_some_and(|(octets, _)| deja > octets) {
        deja = 0;
    }
    if attendu.is_none_or(|(octets, _)| deja < octets) {
        let mut requete = agent().get(url(file));
        if deja > 0 {
            requete = requete.header("Range", format!("bytes={deja}-"));
        }
        let mut resp = requete.call().map_err(|e| Reseau(format!("Connexion au serveur des modèles : {e}")))?;
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
            let n = match reader.read(&mut buf) {
                Ok(n) => n,
                Err(e) => {
                    out.flush()?;
                    return Err(Reseau(format!("Connexion interrompue : {e}")).into());
                }
            };
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
            done += n as u64;
            progress.done.store(done, Ordering::Relaxed);
        }
        out.flush()?;
        let prevu = attendu.map_or(deja + reste, |(octets, _)| octets);
        if (attendu.is_some() || reste > 0) && done < prevu {
            return Err(Reseau(format!("Connexion interrompue ({done} / {prevu} octets)")).into());
        }
    }
    if let Some((octets, sha256)) = attendu {
        if let Err(e) = verifier(&part, octets, sha256) {
            // Une partie corrompue ne doit pas être reprise : on repartira de zéro.
            if part.metadata().map_or(0, |m| m.len()) >= octets {
                let _ = std::fs::remove_file(&part);
            }
            return Err(e);
        }
        std::fs::rename(&part, &dest)?;
        marquer(file, sha256);
    } else {
        std::fs::rename(&part, &dest)?;
    }
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
        return Err(Corrompu(format!("Fichier corrompu (SHA-256 {calcule}, attendu {sha256})")).into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memoire_minimale() {
        // Poste de 8 Go : ~7,6 Gio déclarés.
        assert!(!memoire_suffisante(7 * GIO + GIO / 2));
        assert!(!memoire_suffisante(14 * GIO - 1));
        // Poste de 16 Go : ~15,3 Gio, parfois moins avec une carte graphique intégrée.
        assert!(memoire_suffisante(14 * GIO));
        assert!(memoire_suffisante(15 * GIO + GIO / 3));
        assert!(memoire_suffisante(128 * GIO));
        assert_eq!(modele_redaction().file, "Qwen3.5-9B-Q4_K_M.gguf");
        assert!(!ANCIENS_MODELES_REDACTION.contains(&modele_redaction().file));
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
