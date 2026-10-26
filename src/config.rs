//! Paramètres persistants de Tour de table (fichier JSON dans le dossier de config de l'OS).

use std::path::PathBuf;
use std::sync::OnceLock;

use directories::{ProjectDirs, UserDirs};
use serde::{Deserialize, Serialize};

/// Façon de combiner les deux sources audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptureMode {
    /// Micro et son système transcrits séparément : on sait qui parle
    /// (salle vs à distance). Idéal avec un casque ou un haut-parleur
    /// de conférence avec annulation d'écho.
    Separe,
    /// Les deux sources sont additionnées avant la transcription. À privilégier
    /// quand les participants à distance sortent dans les haut-parleurs de la
    /// salle (sinon le micro les capte aussi et on les transcrirait deux fois).
    Mixe,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub capture_mic: bool,
    pub capture_system: bool,
    /// Identifiant cpal du micro ; `None` = périphérique par défaut.
    pub mic_device: Option<String>,
    /// Identifiant de la sortie à enregistrer (Windows/macOS) ou nom de la
    /// source « monitor » PulseAudio/PipeWire (Linux) ; `None` = défaut.
    pub system_device: Option<String>,
    pub mode: CaptureMode,
    /// Lexique : un terme par ligne (noms propres, acronymes, jargon) ou
    /// une correction `mauvais => bon`.
    pub glossary: String,
    /// Passer seul au point dont l'annonce est entendue ; sinon, son bouton est mis en évidence.
    pub passage_auto: bool,

    // Anciens choix, lus seulement pour journaliser la bascule sur les choix
    // automatiques ; ils disparaissent du fichier au prochain enregistrement.
    // Les autres anciens champs (fils, faisceau, GPU, sensibilité, audio,
    // compte rendu automatique, Ollama…) sont simplement ignorés.
    #[serde(skip_serializing, rename = "model")]
    ancien_modele: Option<String>,
    #[serde(skip_serializing, rename = "llm_backend")]
    ancien_moteur: Option<String>,
    #[serde(skip_serializing, rename = "llm_model")]
    ancien_modele_redaction: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            capture_mic: true,
            capture_system: true,
            mic_device: None,
            system_device: None,
            mode: CaptureMode::Separe,
            glossary: String::new(),
            passage_auto: false,
            ancien_modele: None,
            ancien_moteur: None,
            ancien_modele_redaction: None,
        }
    }
}

// ─────────── Réglages automatiques (aucun choix offert à l'utilisateur) ───────────

/// Fils de calcul : tous les cœurs moins deux (marge pour l'interface et
/// l'audio), au plus 8, car whisper.cpp plafonne vite au-delà.
pub fn threads() -> usize {
    let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
    cores.saturating_sub(2).clamp(1, 8)
}

/// Taille du faisceau de Whisper.
pub const BEAM_SIZE: u8 = 5;

/// Accélération GPU dès que l'exécutable est compilé avec un moteur GPU.
pub const USE_GPU: bool = cfg!(any(feature = "metal", feature = "vulkan", feature = "cuda"));

/// Sensibilité de la détection de voix, 0.0 (peu sensible) à 1.0 (très sensible).
pub const VAD_SENSITIVITY: f32 = 0.5;

/// Modèle Whisper, toujours le même.
pub fn model_path() -> PathBuf {
    models_dir().join(crate::models::RECOMMENDED)
}

/// Moteur de rédaction (nettoyage et compte rendu) : le modèle du palier de mémoire vive.
pub fn moteur() -> crate::summary::Moteur {
    crate::summary::Moteur { modele: models_dir().join(crate::models::modele_redaction().file), fils: threads() }
}

fn dirs() -> ProjectDirs {
    ProjectDirs::from("ca", "TourDeTable", "TourDeTable").expect("dossier personnel introuvable")
}

pub fn data_dir() -> PathBuf {
    dirs().data_dir().to_path_buf()
}

pub fn models_dir() -> PathBuf {
    data_dir().join("modeles")
}

/// Rencontres : dans le dossier Musique de l'utilisateur (localisé : « Musique » sous Windows en
/// français, `~/Music` sous macOS, `XDG_MUSIC_DIR` sous Linux), à la vue de tous ses logiciels.
/// Les modèles, la configuration, le journal et la corbeille restent dans `data_dir()`.
pub fn meetings_dir() -> PathBuf {
    static DOSSIER: OnceLock<PathBuf> = OnceLock::new();
    DOSSIER
        .get_or_init(|| match UserDirs::new() {
            Some(u) => u.audio_dir().map_or_else(|| u.home_dir().join("Music"), PathBuf::from).join("Tour de table"),
            None => old_meetings_dir(),
        })
        .clone()
}

/// Emplacement des rencontres jusqu'à la version 0.1.1.
fn old_meetings_dir() -> PathBuf {
    data_dir().join("rencontres")
}

/// Déplace les rencontres de l'ancien emplacement vers le dossier Musique. Relancée à chaque
/// démarrage : une rencontre qui n'a pas pu être déplacée est retentée la fois suivante.
pub fn migrate_meetings() {
    let (old, new) = (old_meetings_dir(), meetings_dir());
    if old != new {
        crate::storage::migrer(&old, &new);
    }
}

fn config_path() -> PathBuf {
    dirs().config_dir().join("tourdetable.json")
}

/// L'application s'appelait « Noter » : au premier lancement sous le nouveau
/// nom, on déplace ses données (modèles, rencontres) et sa configuration
/// plutôt que de tout retélécharger.
pub fn migrate_from_noter() {
    let Some(old) = ProjectDirs::from("ca", "Noter", "Noter") else { return };
    let new = dirs();
    let moves = [
        (old.data_dir().to_path_buf(), new.data_dir().to_path_buf()),
        (old.config_dir().join("noter.json"), config_path()),
        // macOS : données et configuration partagent un dossier, l'ancien
        // fichier a donc suivi les données au déplacement précédent.
        (new.data_dir().join("noter.json"), config_path()),
    ];
    for (from, to) in moves {
        if !from.exists() || to.exists() {
            continue;
        }
        if let Some(parent) = to.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::rename(&from, &to) {
            Ok(()) => log::info!("Données de Noter déplacées : {} → {}", from.display(), to.display()),
            Err(e) => log::warn!("Migration de {} : {e}", from.display()),
        }
    }
    // Retire les anciens dossiers devenus vides (remove_dir échoue s'ils ne le sont pas).
    for dir in old.config_dir().ancestors().take(3) {
        if std::fs::remove_dir(dir).is_err() {
            break;
        }
    }
}

impl Config {
    pub fn load() -> Self {
        std::fs::read_to_string(config_path()).ok().map(|s| Self::from_json(&s)).unwrap_or_default()
    }

    /// Lit la configuration ; un fichier illisible redonne les valeurs par défaut.
    fn from_json(json: &str) -> Self {
        let mut cfg: Self = serde_json::from_str(json).unwrap_or_else(|e| {
            log::warn!("Configuration illisible, valeurs par défaut : {e}");
            Self::default()
        });
        for note in cfg.oublier_anciens_choix() {
            log::info!("{note}");
        }
        cfg
    }

    /// Retire les anciens choix de modèle et décrit ceux qui différaient des choix automatiques.
    fn oublier_anciens_choix(&mut self) -> Vec<String> {
        let mut notes = Vec::new();
        if let Some(m) = self.ancien_modele.take().filter(|m| m != crate::models::RECOMMENDED) {
            notes.push(format!("Modèle Whisper « {m} » remplacé par {}", crate::models::RECOMMENDED));
        }
        if self.ancien_moteur.take().is_some_and(|b| b == "Ollama") {
            notes.push("Rédaction par Ollama retirée : modèle intégré utilisé".to_string());
        }
        let redaction = crate::models::modele_redaction().file;
        if let Some(m) = self.ancien_modele_redaction.take().filter(|m| m != redaction) {
            notes.push(format!("Modèle de rédaction « {m} » remplacé par {redaction}"));
        }
        notes
    }

    pub fn save(&self) {
        let path = config_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    log::warn!("Impossible d'enregistrer la configuration : {e}");
                }
            }
            Err(e) => log::warn!("Sérialisation de la configuration : {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fichier complet d'une version qui offrait encore tous les choix.
    const ANCIENNE: &str = r#"{
  "capture_mic": true,
  "capture_system": false,
  "mic_device": "Jabra Speak 510",
  "system_device": null,
  "mode": "Mixe",
  "model": "ggml-medium-q5_0.bin",
  "threads": 3,
  "beam_size": 2,
  "use_gpu": true,
  "vad_sensitivity": 0.9,
  "glossary": "Hydro-Québec\nMRC => MRC",
  "save_audio": false,
  "llm_backend": "Ollama",
  "auto_compte_rendu": false,
  "llm_model": "Qwen2.5-7B-Instruct-Q4_K_M.gguf",
  "ollama_url": "http://localhost:11434",
  "ollama_model": "llama3.2"
}"#;

    #[test]
    fn charge_une_ancienne_configuration_complete() {
        let mut cfg: Config = serde_json::from_str(ANCIENNE).expect("ancienne configuration lisible");
        assert!(cfg.capture_mic && !cfg.capture_system);
        assert_eq!(cfg.mic_device.as_deref(), Some("Jabra Speak 510"));
        assert_eq!(cfg.system_device, None);
        assert_eq!(cfg.mode, CaptureMode::Mixe);
        assert_eq!(cfg.glossary, "Hydro-Québec\nMRC => MRC");

        let notes = cfg.oublier_anciens_choix();
        assert_eq!(notes.len(), 3, "{notes:?}");
        assert!(notes[0].contains("ggml-medium-q5_0.bin"));
        assert!(notes[1].contains("Ollama"));
        assert!(notes[2].contains("Qwen2.5-7B"));

        // Les anciens champs ne sont plus réécrits.
        let json = serde_json::to_string(&cfg).unwrap();
        for champ in ["model", "threads", "beam_size", "use_gpu", "vad_sensitivity", "save_audio", "llm_backend", "ollama"] {
            assert!(!json.contains(champ), "{champ} encore présent : {json}");
        }
        let relu: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(relu.mode, CaptureMode::Mixe);
        assert_eq!(relu.mic_device.as_deref(), Some("Jabra Speak 510"));
    }

    #[test]
    fn anciens_choix_identiques_aux_choix_automatiques_sans_message() {
        let json = format!(
            r#"{{"model": "{}", "llm_backend": "Integre", "llm_model": "{}"}}"#,
            crate::models::RECOMMENDED,
            crate::models::modele_redaction().file
        );
        let mut cfg: Config = serde_json::from_str(&json).unwrap();
        assert!(cfg.oublier_anciens_choix().is_empty());
    }

    #[test]
    fn fichier_illisible_ou_vide() {
        assert_eq!(Config::from_json("pas du JSON").mode, CaptureMode::Separe);
        let cfg = Config::from_json("{}");
        assert!(cfg.capture_mic && cfg.capture_system && cfg.mic_device.is_none());
    }

    #[test]
    fn valeurs_automatiques() {
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        assert_eq!(threads(), cores.saturating_sub(2).clamp(1, 8));
        assert!((1..=8).contains(&threads()));
        assert_eq!(BEAM_SIZE, 5);
        assert_eq!(VAD_SENSITIVITY, 0.5);
        assert_eq!(USE_GPU, cfg!(any(feature = "metal", feature = "vulkan", feature = "cuda")));
        assert!(model_path().ends_with(crate::models::RECOMMENDED));
        let moteur = moteur();
        assert!(moteur.modele.ends_with(crate::models::modele_redaction().file));
        assert_eq!(moteur.fils, threads());
    }
}
