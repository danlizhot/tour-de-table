//! Programme auxiliaire de Tour de table : rédige un texte avec un modèle de
//! langue local (llama.cpp, format GGUF), sans Ollama ni connexion Internet.
//!
//! Il est séparé de l'application principale parce que whisper.cpp et
//! llama.cpp embarquent chacun leur propre copie de ggml : réunis dans un même
//! exécutable, leurs symboles entreraient en conflit.
//!
//! Protocole : une requête JSON par ligne sur l'entrée standard ; pour chacune, des lignes JSON
//! sur la sortie standard : `{"progres": n}` (jetons produits), `{"journal": "…"}` (durées de lecture et
//! d'écriture) puis `{"texte": "…"}` (`{"textes": […]}` pour un lot d'invites) ou `{"erreur": "…"}`. Le modèle reste chargé d'une requête à l'autre (un seul chargement pour
//! toute une série) ; le programme s'arrête à la fermeture de l'entrée standard.

use std::io::{BufRead, Write};
use std::num::NonZeroU32;
use std::time::{Duration, Instant};

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
    #[serde(default)]
    invite: String,
    /// Plusieurs invites qui suivent le même préfixe, rédigées en même temps (une séquence chacune dans les
    /// mêmes lots). L'écriture d'un jeton est limitée par la relecture du modèle en mémoire : en écrire plusieurs
    /// d'un même passage coûte à peine plus qu'un seul. Réponse : `{"textes": […]}`.
    #[serde(default)]
    invites: Vec<String>,
    /// Limite de jetons de chaque invite du lot ; `max_jetons` pour celles qui n'en ont pas.
    #[serde(default)]
    max_jetons_lot: Vec<usize>,
    /// Nombre d'invites du lot rédigées en même temps ; dès qu'une finit, la suivante prend sa place. 0 : toutes.
    #[serde(default)]
    simultanees: usize,
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
        let resultat = serde_json::from_str::<Requete>(&ligne).context("Requête invalide").and_then(|r| {
            if r.invites.is_empty() {
                rediger(&mut charge, &r).map(|texte| json!({ "texte": texte }))
            } else {
                rediger_lot(&mut charge, &r).map(|textes| json!({ "textes": textes }))
            }
        });
        match resultat {
            Ok(reponse) => emettre(reponse),
            Err(e) => emettre(json!({ "erreur": format!("{e:#}") })),
        }
    }
}

/// Charge le modèle de la requête s'il ne l'est pas déjà.
fn charger(charge: &mut Option<Charge>, r: &Requete) -> Result<()> {
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
    Ok(())
}

/// Texte complet de l'invite (préfixe et `invite`), selon le gabarit de conversation du modèle.
fn texte_de_l_invite(modele: &LlamaModel, r: &Requete, invite: &str) -> Result<String> {
    // Gabarit de conversation propre au modèle (ChatML pour Qwen, etc.).
    let gabarit = modele.chat_template(None).context("Le modèle n'a pas de gabarit de conversation")?;
    let mut messages = Vec::new();
    if !r.systeme.is_empty() {
        messages.push(LlamaChatMessage::new("system".into(), r.systeme.clone())?);
    }
    let invite = format!("{}{}", r.prefixe, invite);
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
    Ok(texte_invite)
}

fn echantillonneur(modele: &LlamaModel, temperature: f32) -> LlamaSampler {
    LlamaSampler::chain_simple([
        // Légère pénalité de répétition : sur un long tour de table, un petit modèle finit par
        // reprendre les mêmes phrases en boucle. (Le compte rendu n'a plus de tableau Markdown,
        // que la pénalité abîmait.)
        LlamaSampler::penalties(modele.n_vocab(), 256, 1.1, 0.0, 0.0),
        LlamaSampler::top_p(0.9, 1),
        LlamaSampler::temp(temperature),
        LlamaSampler::dist(1234),
    ])
}

fn fils_de(r: &Requete) -> i32 {
    if r.fils > 0 { r.fils } else { std::thread::available_parallelism().map_or(4, |n| n.get() as i32).clamp(1, 8) }
}

fn rediger(charge: &mut Option<Charge>, r: &Requete) -> Result<String> {
    charger(charge, r)?;
    let Charge { backend, modele: garde, prefixes: garde_prefixes } = charge.as_mut().expect("initialisé");
    let modele = &garde.as_ref().expect("chargé").2;
    let backend = &*backend;
    let texte_invite = texte_de_l_invite(modele, r, &r.invite)?;

    let vocab = modele.vocab();
    // Le préfixe est découpé en jetons à part, pour que son découpage ne dépende pas de la suite.
    let fin_prefixe = (!r.prefixe.is_empty()).then(|| texte_invite.find(&r.prefixe).map(|i| i + r.prefixe.len())).flatten();
    let (tete, queue) = texte_invite.split_at(fin_prefixe.unwrap_or(0));
    // Préfixe qui prolonge un préfixe gardé : ses jetons sont repris tels quels, seule la suite est découpée.
    let base = garde_prefixes
        .iter()
        .enumerate()
        .filter(|(_, p)| tete.starts_with(&p.texte))
        .max_by_key(|(_, p)| p.texte.len())
        .map(|(i, _)| i);
    // Un préfixe déjà gardé reprend ses propres jetons : découpé à la suite d'un préfixe plus court, gardé lui aussi,
    // il pourrait différer d'un jeton à la jointure, et toute la transcription serait relue.
    let mut jetons = match base {
        Some(i) => {
            let p = &garde_prefixes[i];
            let mut j = p.jetons.clone();
            if tete.len() > p.texte.len() {
                j.extend(vocab.tokenize(&tete.as_bytes()[p.texte.len()..], false, true));
            }
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

    let fils = fils_de(r);
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
    let depart = Instant::now();
    let mut relus = 0;
    if n_prefixe > 0 && n_prefixe < jetons.len() {
        let meme = |p: &&Prefixe| p.jetons.len() <= n_prefixe && p.jetons == jetons[..p.jetons.len()];
        // Le plus long préfixe gardé qui commence l'invite : lui-même, ou celui qu'il prolonge.
        let repris = garde_prefixes
            .iter()
            .filter(meme)
            .max_by_key(|p| p.jetons.len())
            .filter(|p| ctx.state_seq_set(&p.etat, 0).is_ok())
            .map_or(0, |p| p.jetons.len());
        relus = n_prefixe - repris;
        if repris < n_prefixe {
            lire(&mut ctx, &mut lot, &jetons[repris..n_prefixe], repris, false)?;
            let etat = ctx.state_seq_get(0, LlamaStateSeqFlags::empty()).map_err(|e| anyhow::anyhow!("{e:?}"))?;
            // On garde celui qu'on prolonge ; les autres sont libérés.
            garde_prefixes.retain(|p| repris > 0 && p.jetons.len() == repris);
            garde_prefixes.truncate(PREFIXES - 1);
            garde_prefixes.push(Prefixe { texte: tete.to_string(), jetons: jetons[..n_prefixe].to_vec(), contexte, etat });
        }
        debut = n_prefixe;
    }
    let lecture_prefixe = depart.elapsed();
    lire(&mut ctx, &mut lot, &jetons[debut..], debut, true)?;
    let lecture = depart.elapsed();

    let mut echantillonneur = echantillonneur(modele, r.temperature);

    let mut sortie: Vec<u8> = Vec::new();
    let mut ecrits = 0;
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
        ecrits = n + 1;
        if n % 16 == 0 {
            emettre(json!({ "progres": n }));
        }
    }
    emettre(json!({ "journal": format!(
        "préfixe de {n_prefixe} jetons ({relus} lus) en {:.0} s, invite de {} jetons lue en {:.0} s, {ecrits} jetons écrits en {:.0} s",
        lecture_prefixe.as_secs_f32(),
        jetons.len() - debut,
        (lecture - lecture_prefixe).as_secs_f32(),
        (depart.elapsed() - lecture).as_secs_f32()
    ) }));
    Ok(String::from_utf8_lossy(&sortie).trim().to_string())
}

/// Plusieurs invites à la suite du même préfixe, rédigées en même temps : le préfixe est lu (ou repris) une fois
/// dans la séquence 0, partagé avec les autres séquences, puis chaque passage du modèle écrit un jeton par invite.
fn rediger_lot(charge: &mut Option<Charge>, r: &Requete) -> Result<Vec<String>> {
    charger(charge, r)?;
    let Charge { backend, modele: garde, prefixes: garde_prefixes } = charge.as_mut().expect("initialisé");
    let modele = &garde.as_ref().expect("chargé").2;
    let backend = &*backend;
    let vocab = modele.vocab();
    if r.prefixe.is_empty() {
        bail!("Un lot d'invites demande un préfixe commun");
    }
    let n = r.invites.len();
    let mut tete = String::new();
    let mut queues: Vec<Vec<LlamaToken>> = Vec::with_capacity(n);
    for invite in &r.invites {
        let texte = texte_de_l_invite(modele, r, invite)?;
        let fin = texte.find(&r.prefixe).map(|i| i + r.prefixe.len()).context("Préfixe introuvable dans l'invite")?;
        let (t, q) = texte.split_at(fin);
        if tete.is_empty() {
            tete = t.to_string();
        }
        let q = vocab.tokenize(q.as_bytes(), false, true);
        if q.is_empty() {
            bail!("Invite vide");
        }
        queues.push(q);
    }
    // Jetons du préfixe : repris d'un préfixe gardé (lui-même, ou celui qu'il prolonge) pour un découpage identique.
    let base = garde_prefixes.iter().filter(|p| tete.starts_with(&p.texte)).max_by_key(|p| p.texte.len());
    let prefixe: Vec<LlamaToken> = match base {
        Some(p) => {
            let mut j = p.jetons.clone();
            if tete.len() > p.texte.len() {
                j.extend(vocab.tokenize(&tete.as_bytes()[p.texte.len()..], false, true));
            }
            j
        }
        None => vocab.tokenize(tete.as_bytes(), false, true),
    };
    let n_prefixe = prefixe.len();
    let max: Vec<usize> = (0..n).map(|s| r.max_jetons_lot.get(s).copied().unwrap_or(r.max_jetons).max(1)).collect();
    // `postes` invites à la fois : la séquence 0 garde le préfixe intact, les séquences 1 à `postes` rédigent.
    let postes = if r.simultanees == 0 { n } else { r.simultanees.min(n) };
    let mut tailles: Vec<usize> = queues.iter().zip(&max).map(|(q, m)| q.len() + m.max(&16)).collect();
    tailles.sort_unstable_by(|a, b| b.cmp(a));
    let suite: usize = tailles.iter().take(postes).sum();
    let voulu = (n_prefixe + suite + 256).div_ceil(8192) * 8192;
    if voulu > modele.n_ctx_train() as usize * postes {
        bail!("Texte trop long pour le modèle : {} jetons", n_prefixe + suite);
    }
    let contexte = voulu as u32;
    let fils = fils_de(r);
    let params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(contexte))
        .with_n_batch(LOT as u32)
        .with_n_seq_max(postes as u32 + 1)
        .with_kv_unified(true)
        .with_n_threads(fils)
        .with_n_threads_batch(fils);
    let mut ctx = modele.new_context(backend, params).context("Création du contexte")?;
    let mut lot = LlamaBatch::new(LOT, 1);

    // Préfixe dans la séquence 0 : état gardé le plus long, puis lecture du reste.
    let depart = Instant::now();
    let repris = garde_prefixes
        .iter()
        .filter(|p| p.jetons.len() <= n_prefixe && p.jetons == prefixe[..p.jetons.len()])
        .max_by_key(|p| p.jetons.len())
        .filter(|p| ctx.state_seq_set(&p.etat, 0).is_ok())
        .map_or(0, |p| p.jetons.len());
    if repris < n_prefixe {
        lire(&mut ctx, &mut lot, &prefixe[repris..], repris, false)?;
        let etat = ctx.state_seq_get(0, LlamaStateSeqFlags::empty()).map_err(|e| anyhow::anyhow!("{e:?}"))?;
        if repris > 0 {
            garde_prefixes.retain(|p| p.jetons.len() == repris);
        } else if let Some(court) = garde_prefixes.iter().map(|p| p.jetons.len()).min() {
            // Préfixe sans rapport avec ceux gardés (l'ordre du jour seul) : le plus court, la base, reste gardé pour
            // les requêtes suivantes qui reprennent toute la transcription.
            garde_prefixes.retain(|p| p.jetons.len() == court);
        }
        garde_prefixes.truncate(PREFIXES - 1);
        garde_prefixes.push(Prefixe { texte: tete.clone(), jetons: prefixe.clone(), contexte, etat });
    }
    let lecture_prefixe = depart.elapsed();
    let (mut lecture, mut lus, mut passages, mut finies) = (Duration::ZERO, 0usize, 0usize, 0usize);
    // Les invites les plus longues à écrire d'abord : les courtes comblent la fin, où les postes se vident.
    let mut attente: Vec<usize> = (0..n).collect();
    attente.sort_by(|a, b| max[*b].cmp(&max[*a]));
    let mut attente: std::collections::VecDeque<usize> = attente.into();
    struct Poste {
        invite: usize,
        echantillonneur: LlamaSampler,
        position: usize,
        ecrits: usize,
        /// Jeton à lire au prochain passage : le dernier de l'invite, puis chaque jeton écrit.
        jeton: LlamaToken,
    }
    let mut en_cours: Vec<Option<Poste>> = (0..postes).map(|_| None).collect();
    let mut servi = vec![false; postes];
    let mut sorties: Vec<Vec<u8>> = vec![Vec::new(); n];
    let mut total = 0usize;
    loop {
        // Poste libre : le préfixe y est recopié, puis l'invite suivante lue, sauf son dernier jeton.
        for k in 0..postes {
            if en_cours[k].is_some() {
                continue;
            }
            let Some(invite) = attente.pop_front() else { continue };
            let seq = k as i32 + 1;
            if servi[k] && !ctx.clear_kv_cache_seq(Some(seq as u32), None, None).map_err(|e| anyhow::anyhow!("{e:?}"))? {
                bail!("Séquence {seq} impossible à vider");
            }
            servi[k] = true;
            ctx.copy_kv_cache_seq(0, seq, None, None).map_err(|e| anyhow::anyhow!("{e:?}"))?;
            let q = &queues[invite];
            let t = Instant::now();
            for (m, morceau) in q[..q.len() - 1].chunks(LOT).enumerate() {
                lot.clear();
                for (j, jeton) in morceau.iter().enumerate() {
                    lot.add(*jeton, (n_prefixe + m * LOT + j) as i32, &[seq], false)?;
                }
                ctx.decode(&mut lot).context("Lecture des invites")?;
            }
            lecture += t.elapsed();
            lus += q.len() - 1;
            en_cours[k] = Some(Poste {
                invite,
                echantillonneur: echantillonneur(modele, r.temperature),
                position: n_prefixe + q.len() - 1,
                ecrits: 0,
                jeton: q[q.len() - 1],
            });
        }
        // Un passage du modèle : un jeton par poste occupé.
        lot.clear();
        let mut ordre = Vec::with_capacity(postes);
        for (k, poste) in en_cours.iter_mut().enumerate() {
            if let Some(p) = poste {
                lot.add(p.jeton, p.position as i32, &[k as i32 + 1], true)?;
                p.position += 1;
                ordre.push(k);
            }
        }
        if ordre.is_empty() {
            break;
        }
        ctx.decode(&mut lot).context("Génération")?;
        passages += 1;
        for (j, &k) in ordre.iter().enumerate() {
            let p = en_cours[k].as_mut().expect("occupé");
            let jeton = p.echantillonneur.sample(&ctx, j as i32);
            p.echantillonneur.accept(jeton);
            let mut fini = vocab.is_eog(jeton);
            if !fini {
                sorties[p.invite].extend(vocab.token_to_piece(jeton, false, None));
                p.ecrits += 1;
                p.jeton = jeton;
                total += 1;
                if total.is_multiple_of(16) {
                    emettre(json!({ "progres": total }));
                }
                fini = p.ecrits >= max[p.invite];
            }
            if fini {
                en_cours[k] = None;
                finies += 1;
                emettre(json!({ "progres": total, "finies": finies }));
            }
        }
    }
    emettre(json!({ "journal": format!(
        "{n} invites, {postes} à la fois : préfixe de {n_prefixe} jetons ({} lus) en {:.0} s, invites de {lus} jetons lues en {:.0} s, \
         {total} jetons écrits en {:.0} s ({passages} passages)",
        n_prefixe - repris,
        lecture_prefixe.as_secs_f32(),
        lecture.as_secs_f32(),
        (depart.elapsed() - lecture_prefixe - lecture).as_secs_f32()
    ) }));
    Ok(sorties.iter().map(|s| String::from_utf8_lossy(s).trim().to_string()).collect())
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
