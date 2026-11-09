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
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::context::session::{LlamaStateSeqFlags, SeqState};
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{LlamaChatMessage, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::token::LlamaToken;
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
struct Requete {
    modele: String,
    #[serde(default)]
    systeme: String,
    invite: String,
    /// Début commun à une série de requêtes (la transcription complète) : lu une seule fois,
    /// son état est gardé et repris tant que les requêtes suivantes commencent pareil.
    #[serde(default)]
    prefixe: String,
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
    /// Au plus [`PREFIXES`] : le préfixe de base (ordre du jour et transcription) et un préfixe qui le prolonge
    /// (consigne commune à une série de questions), pour revenir au premier sans tout relire.
    prefixes: Vec<Prefixe>,
}

const PREFIXES: usize = 2;

/// État du contexte après la lecture d'un préfixe commun.
struct Prefixe {
    texte: String,
    jetons: Vec<LlamaToken>,
    contexte: u32,
    etat: SeqState,
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
        *charge = Some(Charge { backend, modele: None, prefixes: Vec::new() });
    }
    let Charge { backend, modele: garde, prefixes: garde_prefixes } = charge.as_mut().expect("initialisé");
    if garde.as_ref().is_none_or(|(chemin, gpu, _)| *chemin != r.modele || *gpu != r.gpu) {
        *garde = None; // libère l'ancien modèle avant de charger le suivant
        garde_prefixes.clear();
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
    let invite = format!("{}{}", r.prefixe, r.invite);
    messages.push(LlamaChatMessage::new("user".into(), invite.clone())?);
    let mut texte_invite = match modele.apply_chat_template(&gabarit, &messages, true) {
        Ok(t) => t,
        // Gemma 4 : gabarit que cette version de llama.cpp ne sait pas appliquer. Même texte que
        // le gabarit, sans réflexion.
        Err(_) if gabarit.to_str().is_ok_and(|g| g.contains("<|turn>")) => {
            let systeme = if r.systeme.is_empty() { String::new() } else { format!("<|turn>system\n{}<turn|>\n", r.systeme) };
            format!("<bos>{systeme}<|turn>user\n{invite}<turn|>\n<|turn>model\n<|channel>thought\n<channel|>")
        }
        Err(e) => return Err(e.into()),
    };
    // Modèles qui réfléchissent par défaut (Qwen3.5, Granite 4.2) : un bloc de réflexion vide,
    // comme le produit leur gabarit avec `enable_thinking=False`, fait répondre directement.
    if gabarit.to_str().is_ok_and(|g| g.contains("enable_thinking")) {
        texte_invite.push_str("<think>\n\n</think>\n\n");
    }

    let vocab = modele.vocab();
    // Le préfixe est découpé en jetons à part, pour que son découpage ne dépende pas de la suite.
    let fin_prefixe = (!r.prefixe.is_empty()).then(|| texte_invite.find(&r.prefixe).map(|i| i + r.prefixe.len())).flatten();
    let (tete, queue) = texte_invite.split_at(fin_prefixe.unwrap_or(0));
    // Préfixe qui prolonge un préfixe gardé : ses jetons sont repris tels quels, seule la suite est découpée.
    let base = garde_prefixes
        .iter()
        .enumerate()
        .filter(|(_, p)| tete.len() > p.texte.len() && tete.starts_with(&p.texte))
        .max_by_key(|(_, p)| p.texte.len())
        .map(|(i, _)| i);
    let mut jetons = match base {
        Some(i) => {
            let p = &garde_prefixes[i];
            let mut j = p.jetons.clone();
            j.extend(vocab.tokenize(&tete.as_bytes()[p.texte.len()..], false, true));
            j
        }
        None => vocab.tokenize(tete.as_bytes(), false, true),
    };
    let n_prefixe = jetons.len();
    jetons.extend(vocab.tokenize(queue.as_bytes(), false, true));
    // Le contexte demandé est estimé d'après le nombre de caractères : une transcription bruitée
    // compte bien plus de jetons que prévu. On l'agrandit donc à l'invite réelle plus la réponse.
    let voulu = (jetons.len() + r.max_jetons.max(256)).max(r.contexte as usize);
    // Avec un préfixe, le même contexte pour toute la série (sinon le préfixe est relu) : calculé
    // sur le préfixe seul, avec 8 192 jetons de marge pour la question et la réponse. Un préfixe qui en
    // prolonge un autre garde son contexte, s'il suffit.
    let voulu = match base.map(|i| garde_prefixes[i].contexte as usize) {
        Some(c) if c >= voulu => c,
        _ if n_prefixe > 0 => voulu.max(n_prefixe + 8192).div_ceil(8192) * 8192,
        _ => voulu,
    };
    let contexte = voulu.min(modele.n_ctx_train() as usize).max(2048) as u32;
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

    // Lecture de l'invite, par lots : le préfixe est repris de la requête précédente s'il est
    // identique, sinon lu puis gardé.
    let mut lot = LlamaBatch::new(LOT, 1);
    let mut debut = 0;
    if n_prefixe > 0 && n_prefixe < jetons.len() {
        let meme = |p: &&Prefixe| p.contexte == contexte && p.jetons.len() <= n_prefixe && p.jetons == jetons[..p.jetons.len()];
        // Le plus long préfixe gardé qui commence l'invite : lui-même, ou celui qu'il prolonge.
        let repris = garde_prefixes
            .iter()
            .filter(meme)
            .max_by_key(|p| p.jetons.len())
            .filter(|p| ctx.state_seq_set(&p.etat, 0).is_ok())
            .map_or(0, |p| p.jetons.len());
        if repris < n_prefixe {
            lire(&mut ctx, &mut lot, &jetons[repris..n_prefixe], repris, false)?;
            let etat = ctx.state_seq_get(0, LlamaStateSeqFlags::empty()).map_err(|e| anyhow::anyhow!("{e:?}"))?;
            // On garde celui qu'on prolonge ; les autres sont libérés.
            garde_prefixes.retain(|p| repris > 0 && p.jetons.len() == repris && p.contexte == contexte);
            garde_prefixes.truncate(PREFIXES - 1);
            garde_prefixes.push(Prefixe { texte: tete.to_string(), jetons: jetons[..n_prefixe].to_vec(), contexte, etat });
        }
        debut = n_prefixe;
    }
    lire(&mut ctx, &mut lot, &jetons[debut..], debut, true)?;

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

/// Lit des jetons à partir de la position `debut` ; `logits` : ceux du dernier, pour la suite.
fn lire(ctx: &mut LlamaContext, lot: &mut LlamaBatch, jetons: &[LlamaToken], debut: usize, logits: bool) -> Result<()> {
    let dernier = jetons.len() - 1;
    for (i, morceau) in jetons.chunks(LOT).enumerate() {
        lot.clear();
        for (j, jeton) in morceau.iter().enumerate() {
            let k = i * LOT + j;
            lot.add(*jeton, (debut + k) as i32, &[0], logits && k == dernier)?;
        }
        ctx.decode(lot).context("Lecture de l'invite")?;
    }
    Ok(())
}
