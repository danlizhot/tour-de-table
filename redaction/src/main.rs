//! Programme auxiliaire de Tour de table : rédige un texte avec un modèle de
//! langue local (llama.cpp, format GGUF), sans Ollama ni connexion Internet.
//!
//! Il est séparé de l'application principale parce que whisper.cpp et
//! llama.cpp embarquent chacun leur propre copie de ggml : réunis dans un même
//! exécutable, leurs symboles entreraient en conflit.
//!
//! Protocole : une requête JSON par ligne sur l'entrée standard ; pour chacune, des lignes JSON
//! sur la sortie standard : `{"progres": n}` (jetons produits) puis `{"texte": "…"}` ou
//! `{"erreur": "…"}`. Le modèle reste chargé d'une requête à l'autre (un seul chargement pour
//! toute une série) ; le programme s'arrête à la fermeture de l'entrée standard.

use std::io::{BufRead, Write};
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
    /// Couches du modèle sur la puce graphique (si compilé avec Vulkan ou Metal).
    #[serde(default)]
    gpu: bool,
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

/// Modèle chargé, gardé pour les requêtes suivantes qui le demandent.
struct Charge {
    backend: LlamaBackend,
    modele: Option<(String, bool, LlamaModel)>,
}

fn main() {
    let mut charge: Option<Charge> = None;
    for ligne in std::io::stdin().lock().lines() {
        let Ok(ligne) = ligne else { break };
        if ligne.trim().is_empty() {
            continue;
        }
        let resultat = serde_json::from_str::<Requete>(&ligne).context("Requête invalide").and_then(|r| rediger(&mut charge, &r));
        match resultat {
            Ok(texte) => emettre(json!({ "texte": texte })),
            Err(e) => emettre(json!({ "erreur": format!("{e:#}") })),
        }
    }
}

fn rediger(charge: &mut Option<Charge>, r: &Requete) -> Result<String> {
    if charge.is_none() {
        let backend = LlamaBackend::init()?;
        llama_cpp_2::send_logs_to_tracing(llama_cpp_2::LogOptions::default().with_logs_enabled(false));
        *charge = Some(Charge { backend, modele: None });
    }
    let Charge { backend, modele: garde } = charge.as_mut().expect("initialisé");
    if garde.as_ref().is_none_or(|(chemin, gpu, _)| *chemin != r.modele || *gpu != r.gpu) {
        *garde = None; // libère l'ancien modèle avant de charger le suivant
        // Sans mmap : sur processeur, llama.cpp réorganise les poids (« repack ») et
        // garderait sinon deux copies du modèle en mémoire (fichier + réorganisée).
        let params_modele = LlamaModelParams::default().with_use_mmap(false).with_n_gpu_layers(if r.gpu { 999 } else { 0 });
        let m = LlamaModel::load_from_file(backend, &r.modele, &params_modele)
            .with_context(|| format!("Chargement du modèle {}", r.modele))?;
        *garde = Some((r.modele.clone(), r.gpu, m));
    }
    let modele = &garde.as_ref().expect("chargé").2;
    let backend = &*backend;

    // Gabarit de conversation propre au modèle (ChatML pour Qwen, etc.).
    let gabarit = modele.chat_template(None).context("Le modèle n'a pas de gabarit de conversation")?;
    let mut messages = Vec::new();
    if !r.systeme.is_empty() {
        messages.push(LlamaChatMessage::new("system".into(), r.systeme.clone())?);
    }
    messages.push(LlamaChatMessage::new("user".into(), r.invite.clone())?);
    let mut texte_invite = modele.apply_chat_template(&gabarit, &messages, true)?;
    // Modèles qui réfléchissent par défaut (Qwen3.5, Granite 4.2) : un bloc de réflexion vide,
    // comme le produit leur gabarit avec `enable_thinking=False`, fait répondre directement.
    if gabarit.to_str().is_ok_and(|g| g.contains("enable_thinking")) {
        texte_invite.push_str("<think>\n\n</think>\n\n");
    }

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
    let mut ctx = modele.new_context(backend, params).context("Création du contexte")?;

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
        // Légère pénalité de répétition : sur un long tour de table, un petit modèle finit par
        // reprendre les mêmes phrases en boucle. (Le compte rendu n'a plus de tableau Markdown,
        // que la pénalité abîmait.)
        LlamaSampler::penalties(modele.n_vocab(), 256, 1.1, 0.0, 0.0),
        LlamaSampler::top_p(0.9, 1),
        LlamaSampler::temp(r.temperature),
        LlamaSampler::dist(1234),
    ]);

    let mut sortie: Vec<u8> = Vec::new();
    for (n, pos) in (0..max_jetons).zip(jetons.len()..) {
        let jeton = echantillonneur.sample(&ctx, lot.n_tokens() - 1);
        echantillonneur.accept(jeton);
        if vocab.is_eog(jeton) {
            break;
        }
        sortie.extend(vocab.token_to_piece(jeton, false, None));
        lot.clear();
        lot.add(jeton, pos as i32, &[0], true)?;
        ctx.decode(&mut lot).context("Génération")?;
        if n % 16 == 0 {
            emettre(json!({ "progres": n }));
        }
    }
    Ok(String::from_utf8_lossy(&sortie).trim().to_string())
}
