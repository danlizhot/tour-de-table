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
    /// Mémoire vive approximative pendant la transcription (modèle + état).
    pub ram_mb: u32,
    pub note: &'static str,
}

/// Tous multilingues : les modèles « .en » ne savent pas le français.
pub const CATALOG: &[ModelInfo] = &[
    ModelInfo {
        file: RECOMMENDED,
        label: "Large v3 Turbo (q5)",
        size_mb: 574,
        ram_mb: 1200,
        note: "Recommandé (8 Go et +) : très bon sur l'accent québécois, rapide",
    },
    ModelInfo {
        file: "ggml-large-v3-q5_0.bin",
        label: "Large v3 (q5)",
        size_mb: 1080,
        ram_mb: 3000,
        note: "Précision maximale, ~3× plus lent ; 16 Go conseillés",
    },
    ModelInfo {
        file: "ggml-medium-q5_0.bin",
        label: "Medium (q5)",
        size_mb: 539,
        ram_mb: 1700,
        note: "Bon compromis pour portables sans GPU",
    },
    ModelInfo {
        file: "ggml-small-q5_1.bin",
        label: "Small (q5)",
        size_mb: 190,
        ram_mb: 800,
        note: "Machines modestes ; moins précis sur le joual",
    },
];

fn url(file: &str) -> String {
    format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{file}")
}

pub fn path(file: &str) -> PathBuf {
    crate::config::models_dir().join(file)
}

pub fn installed(file: &str) -> bool {
    path(file).metadata().is_ok_and(|m| m.len() > 1_000_000)
}

/// Modèles présents sur le disque, y compris ceux ajoutés à la main
/// (p. ex. un modèle affiné sur le français québécois converti en ggml).
pub fn local_models() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(crate::config::models_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.ends_with(".bin") && installed(n))
        .collect();
    v.sort();
    v
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
