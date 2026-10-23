//! Programme auxiliaire de Tour de table : rédige un texte avec un modèle de
//! langue local (llama.cpp, format GGUF), sans Ollama ni connexion Internet.
//!
//! Il est séparé de l'application principale parce que whisper.cpp et
//! llama.cpp embarquent chacun leur propre copie de ggml : réunis dans un même
//! exécutable, leurs symboles entreraient en conflit.
//!
//! Protocole : une requête JSON sur l'entrée standard, puis des lignes JSON sur
//! la sortie standard : `{"progres": n}` (jetons produits) et, à la fin,
//! `{"texte": "…"}` ou `{"erreur": "…"}`.

use std::io::{Read, Write};
use std::num::NonZeroU32;

use anyhow::{Context, Result, bail};
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{LlamaChatMessage, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
struct Requete {
    modele: String,
    #[serde(default)]
    systeme: String,
    invite: String,
    #[serde(default = "defaut_contexte")]
    contexte: u32,
    #[serde(default = "defaut_max")]
    max_jetons: usize,
    #[serde(default = "defaut_temperature")]
    temperature: f32,
    #[serde(default)]
    fils: i32,
}

fn defaut_contexte() -> u32 {
    8192
}
fn defaut_max() -> usize {
    3072
}
fn defaut_temperature() -> f32 {
    0.2
}

const LOT: usize = 512;

fn emettre(valeur: serde_json::Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{valeur}");
    let _ = out.flush();
}

fn main() {
    let mut entree = String::new();
    let resultat = std::io::stdin()
        .read_to_string(&mut entree)
        .context("Lecture de la requête")
        .and_then(|_| serde_json::from_str::<Requete>(&entree).context("Requête invalide"))
        .and_then(|r| rediger(&r));
    match resultat {
        Ok(texte) => emettre(json!({ "texte": texte })),
        Err(e) => {
            emettre(json!({ "erreur": format!("{e:#}") }));
            std::process::exit(1);
        }
    }
}

fn rediger(r: &Requete) -> Result<String> {
    let backend = LlamaBackend::init()?;
    llama_cpp_2::send_logs_to_tracing(llama_cpp_2::LogOptions::default().with_logs_enabled(false));

    // Sans mmap : sur processeur, llama.cpp réorganise les poids (« repack ») et
    // garderait sinon deux copies du modèle en mémoire (fichier + réorganisée).
    let params_modele = LlamaModelParams::default().with_use_mmap(false);
    let modele = LlamaModel::load_from_file(&backend, &r.modele, &params_modele)
        .with_context(|| format!("Chargement du modèle {}", r.modele))?;

    // Gabarit de conversation propre au modèle (ChatML pour Qwen, etc.).
    let gabarit = modele.chat_template(None).context("Le modèle n'a pas de gabarit de conversation")?;
    let mut messages = Vec::new();
    if !r.systeme.is_empty() {
        messages.push(LlamaChatMessage::new("system".into(), r.systeme.clone())?);
    }
    messages.push(LlamaChatMessage::new("user".into(), r.invite.clone())?);
    let texte_invite = modele.apply_chat_template(&gabarit, &messages, true)?;

    let vocab = modele.vocab();
    let jetons = vocab.tokenize(texte_invite.as_bytes(), false, true);
    let contexte = r.contexte.min(modele.n_ctx_train()).max(2048);
    if jetons.len() + 256 > contexte as usize {
        bail!(
            "Texte trop long pour le modèle : {} jetons pour un contexte de {}",
            jetons.len(),
            contexte
        );
    }
    let max_jetons = r.max_jetons.min(contexte as usize - jetons.len());

    let fils = if r.fils > 0 {
        r.fils
    } else {
        std::thread::available_parallelism().map_or(4, |n| n.get() as i32).clamp(1, 8)
    };
    let params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(contexte))
        .with_n_batch(LOT as u32)
        .with_n_threads(fils)
        .with_n_threads_batch(fils);
    let mut ctx = modele.new_context(&backend, params).context("Création du contexte")?;

    // Lecture de l'invite, par lots.
    let mut lot = LlamaBatch::new(LOT, 1);
    let dernier = jetons.len() - 1;
    for (i, morceau) in jetons.chunks(LOT).enumerate() {
        lot.clear();
        for (j, jeton) in morceau.iter().enumerate() {
            let pos = i * LOT + j;
            lot.add(*jeton, pos as i32, &[0], pos == dernier)?;
        }
        ctx.decode(&mut lot).context("Lecture de l'invite")?;
    }

    let mut echantillonneur = LlamaSampler::chain_simple([
        // Pas de pénalité de répétition : elle abîme les tableaux Markdown, qui
        // répètent « | » et « --- » à chaque ligne.
        LlamaSampler::top_p(0.9, 1),
        LlamaSampler::temp(r.temperature),
        LlamaSampler::dist(1234),
    ]);

    let mut sortie: Vec<u8> = Vec::new();
    let mut pos = jetons.len();
    for n in 0..max_jetons {
        let jeton = echantillonneur.sample(&ctx, lot.n_tokens() - 1);
        echantillonneur.accept(jeton);
        if vocab.is_eog(jeton) {
            break;
        }
        sortie.extend(vocab.token_to_piece(jeton, false, None));
        lot.clear();
        lot.add(jeton, pos as i32, &[0], true)?;
        ctx.decode(&mut lot).context("Génération")?;
        pos += 1;
        if n % 16 == 0 {
            emettre(json!({ "progres": n }));
        }
    }
    Ok(String::from_utf8_lossy(&sortie).trim().to_string())
}
