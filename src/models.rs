//! Catalogue et téléchargement des modèles Whisper (format ggml de whisper.cpp).

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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

/// Modèles de langue pour rédiger les comptes rendus (format GGUF, llama.cpp).
/// Licence Apache 2.0 tous les deux : utilisables sans restriction par un organisme.
pub const LLM_RECOMMENDED: &str = "Qwen3-4B-Instruct-2507-Q4_K_M.gguf";

pub struct LlmInfo {
    pub file: &'static str,
    pub label: &'static str,
    pub url: &'static str,
    pub size_mb: u32,
    /// Mémoire vive pendant la rédaction ; servira au choix automatique selon la RAM.
    #[allow(dead_code)]
    pub ram_mb: u32,
    pub note: &'static str,
}

pub const LLM_CATALOG: &[LlmInfo] = &[
    LlmInfo {
        file: LLM_RECOMMENDED,
        label: "Qwen3 4B Instruct",
        url: "https://huggingface.co/unsloth/Qwen3-4B-Instruct-2507-GGUF/resolve/main/Qwen3-4B-Instruct-2507-Q4_K_M.gguf",
        size_mb: 2500,
        ram_mb: 3200,
        note: "Recommandé avec 8 Go de mémoire",
    },
    LlmInfo {
        file: "Qwen2.5-7B-Instruct-Q4_K_M.gguf",
        label: "Qwen2.5 7B Instruct",
        url: "https://huggingface.co/bartowski/Qwen2.5-7B-Instruct-GGUF/resolve/main/Qwen2.5-7B-Instruct-Q4_K_M.gguf",
        size_mb: 4700,
        ram_mb: 5200,
        note: "Meilleure rédaction ; 16 Go de mémoire conseillés",
    },
];

pub fn llm_info(file: &str) -> Option<&'static LlmInfo> {
    LLM_CATALOG.iter().find(|m| m.file == file)
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

pub fn installed(file: &str) -> bool {
    path(file).metadata().is_ok_and(|m| m.len() > 1_000_000)
}

#[derive(Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub cancel: AtomicBool,
}

pub fn download(file: &str, progress: Arc<Progress>) -> Result<()> {
    let dest = path(file);
    std::fs::create_dir_all(dest.parent().unwrap())?;
    let part = dest.with_extension("part");

    let mut resp = ureq::get(&url(file)).call().context("Connexion à Hugging Face")?;
    let total = resp.body().content_length().unwrap_or(0);
    progress.total.store(total, Ordering::Relaxed);
    let mut reader = resp.body_mut().as_reader();
    let mut out = std::io::BufWriter::new(std::fs::File::create(&part)?);
    let mut buf = vec![0u8; 1 << 16];
    let mut done = 0u64;
    loop {
        if progress.cancel.load(Ordering::Relaxed) {
            drop(out);
            let _ = std::fs::remove_file(&part);
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
    drop(out);
    if total > 0 && done != total {
        let _ = std::fs::remove_file(&part);
        bail!("Téléchargement incomplet ({done} / {total} octets)");
    }
    std::fs::rename(&part, &dest)?;
    Ok(())
}
