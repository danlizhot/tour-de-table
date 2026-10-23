//! Paramètres persistants de Noter (fichier JSON dans le dossier de config de l'OS).

use std::path::PathBuf;

use directories::ProjectDirs;
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
    /// Nom de fichier du modèle ggml dans le dossier des modèles.
    pub model: String,
    pub threads: usize,
    pub beam_size: u8,
    pub use_gpu: bool,
    /// Sensibilité de la détection de voix, 0.0 (peu sensible) à 1.0 (très sensible).
    pub vad_sensitivity: f32,
    /// Lexique : un terme par ligne (noms propres, acronymes, jargon) ou
    /// une correction `mauvais => bon`.
    pub glossary: String,
    pub save_audio: bool,
    pub ollama_url: String,
    pub ollama_model: String,
}

impl Default for Config {
    fn default() -> Self {
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        Self {
            capture_mic: true,
            capture_system: true,
            mic_device: None,
            system_device: None,
            mode: CaptureMode::Separe,
            model: crate::models::RECOMMENDED.to_string(),
            // whisper.cpp plafonne vite au-delà de 8 fils ; on laisse de la marge à l'UI et à l'audio.
            threads: cores.saturating_sub(2).clamp(1, 8),
            beam_size: 5,
            use_gpu: cfg!(any(feature = "metal", feature = "vulkan", feature = "cuda")),
            vad_sensitivity: 0.5,
            glossary: String::new(),
            save_audio: true,
            ollama_url: "http://localhost:11434".into(),
            ollama_model: "llama3.2".into(),
        }
    }
}

fn dirs() -> ProjectDirs {
    ProjectDirs::from("ca", "Noter", "Noter").expect("dossier personnel introuvable")
}

pub fn data_dir() -> PathBuf {
    dirs().data_dir().to_path_buf()
}

pub fn models_dir() -> PathBuf {
    data_dir().join("modeles")
}

pub fn meetings_dir() -> PathBuf {
    data_dir().join("rencontres")
}

fn config_path() -> PathBuf {
    dirs().config_dir().join("noter.json")
}

impl Config {
    pub fn load() -> Self {
        std::fs::read_to_string(config_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
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

    pub fn model_path(&self) -> PathBuf {
        models_dir().join(&self.model)
    }
}
