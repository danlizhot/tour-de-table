//! Compte rendu de rencontre par un modèle de langue local.
//!
//! Par défaut, le modèle est intégré : le programme auxiliaire
//! `tourdetable-redaction` (llama.cpp) le fait tourner sur l'ordinateur, sans
//! rien à installer. Ollama reste possible en option avancée. Dans les deux
//! cas, rien ne quitte la machine.
//!
//! Avec un ordre du jour, le compte rendu suit ses points dans l'ordre ;
//! sans ordre du jour, on produit un résumé structuré.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::json;

/// Qui rédige.
#[derive(Clone, Debug)]
pub enum Moteur {
    Integre { modele: PathBuf, fils: usize },
    Ollama { url: String, modele: String },
}

impl Moteur {
    pub fn nom(&self) -> String {
        match self {
            Moteur::Integre { modele, .. } => crate::models::llm_info(
                &modele.file_name().unwrap_or_default().to_string_lossy(),
            )
            .map_or_else(|| modele.display().to_string(), |m| m.label.to_string()),
            Moteur::Ollama { modele, .. } => format!("Ollama · {modele}"),
        }
    }
}

const STYLE: &str = "Rédige en français québécois standard, selon l'orthographe, la terminologie \
et la typographie de l'Office québécois de la langue française : « courriel », « rencontre », \
« 14 h 30 », « 2 500 $ », pas d'espace avant ? et !. Garde telles quelles les expressions \
québécoises citées. Reprends les montants et les chiffres exactement comme ils ont été dits, \
sans faire de calcul ni en tirer de conclusion (dépassement, économie…). N'invente rien : si une information manque, écris « non précisé ». \
Réponds uniquement avec le document, en Markdown.";

const SANS_ORDRE_DU_JOUR: &str = "Tu es secrétaire de réunion. À partir de la transcription, produis :\n\
## Résumé (5 phrases maximum)\n\
## Points discutés (liste)\n\
## Décisions\n\
## Suivis\n\
Les suivis sont un tableau à trois colonnes : Action | Responsable | Échéance.\n";

const AVEC_ORDRE_DU_JOUR: &str = "Tu es secrétaire de réunion. Rédige le compte rendu de la rencontre \
en suivant l'ordre du jour fourni, point par point et dans le même ordre, avec la même numérotation.\n\
Pour chaque point : un titre de niveau 3, puis **Discussion** (l'essentiel en quelques phrases), \
**Décisions** et **Suivis** (qui fait quoi, pour quand).\n\
Règles :\n\
- Si un point entier n'a pas été discuté, écris seulement « Point non abordé ou reporté. » sous son titre.\n\
- S'il a été discuté sans décision ou sans suivi, écris « Aucune » ou « Aucun » dans la rubrique vide.\n\
- Une proposition adoptée, votée ou acceptée par le groupe est une décision : écris « Il est résolu de… ».\n\
- Chaque fois qu'une personne accepte ou se voit confier une tâche (« je m'en occupe », \
« tu t'en occupes? Correct »), c'est un suivi.\n\
- Le tableau des suivis reprend TOUS les suivis de tous les points, sans exception. Pour l'échéance, \
reprends le délai tel qu'il a été dit (« d'ici la fin novembre », « pour le 15 décembre »).\n\
- L'ordre du jour sert uniquement à structurer : les faits (lieu, heure, montants, noms) viennent \
de la transcription seulement. Ne recopie pas le lieu ou l'heure de cette rencontre pour la prochaine.\n\
Structure du document :\n\
# Compte rendu — {titre}\n\
**Date** : {date} · **Durée** : {duree}\n\
## Points de l'ordre du jour\n\
## Sujets hors ordre du jour\n\
## Tableau des suivis\n\
## Prochaine rencontre\n\
Le tableau des suivis a trois colonnes : Action | Responsable | Échéance. \
Si la transcription annonce la date ou l'heure de la prochaine rencontre, écris-les sous « Prochaine rencontre » ; \
sinon, omets cette section. Omets « Sujets hors ordre du jour » si tout a été couvert par l'ordre du jour.\n";

/// Au-delà, la transcription est d'abord condensée par tranches : les petits
/// modèles locaux perdent le fil sur des contextes trop longs, et la mémoire
/// du contexte (cache KV) grossit avec sa taille.
const CHUNK_CHARS: usize = 24_000;

pub struct Request<'a> {
    pub moteur: &'a Moteur,
    /// Jetons rédigés jusqu'ici (affichage de l'avancement).
    pub progress: Option<&'a AtomicUsize>,
    pub title: &'a str,
    pub date: &'a str,
    pub duration: &'a str,
    pub agenda: Option<&'a str>,
    pub transcript: &'a str,
}

pub fn compte_rendu(r: &Request) -> Result<String> {
    let transcript = if r.transcript.chars().count() > CHUNK_CHARS {
        condense(r)?
    } else {
        r.transcript.to_string()
    };

    let mut prompt = match r.agenda {
        Some(_) => AVEC_ORDRE_DU_JOUR
            .replace("{titre}", r.title)
            .replace("{date}", r.date)
            .replace("{duree}", r.duration),
        None => SANS_ORDRE_DU_JOUR.to_string(),
    };
    prompt.push_str(STYLE);
    if let Some(agenda) = r.agenda {
        prompt.push_str("\n\n=== ORDRE DU JOUR ===\n");
        prompt.push_str(agenda);
    }
    prompt.push_str("\n\n=== TRANSCRIPTION ===\n");
    prompt.push_str(&transcript);
    let mut doc = generate(r, &prompt)?;

    // Garde-fou : un modèle local peut se tromper dans un calcul ou inventer un
    // chiffre. On signale tout nombre absent des sources.
    let sources = format!("{} {} {} {}", r.agenda.unwrap_or(""), r.transcript, r.date, r.duration);
    let suspects = unverified_numbers(&doc, &sources);
    if !suspects.is_empty() {
        doc.push_str(&format!(
            "\n\n> **À vérifier** : ces chiffres n'apparaissent ni dans la transcription ni dans \
             l'ordre du jour : {}.",
            suspects.join(", ")
        ));
    }
    Ok(doc)
}

/// Nombres du texte, normalisés (« 2 500 », « 2500 » et « 2,5 » restent distincts
/// de leurs voisins) : chiffres groupés par milliers avec espace ou espace insécable.
fn numbers(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_digit() && (i == 0 || !chars[i - 1].is_ascii_digit()) {
            let mut n = String::new();
            let mut j = i;
            loop {
                while j < chars.len() && chars[j].is_ascii_digit() {
                    n.push(chars[j]);
                    j += 1;
                }
                // Groupe de milliers : « 2 500 », « 12 000 » (exactement 3 chiffres après l'espace).
                let sep = chars.get(j).is_some_and(|c| matches!(c, ' ' | '\u{a0}' | '\u{202f}'));
                let three = (1..=3).all(|k| chars.get(j + k).is_some_and(|c| c.is_ascii_digit()))
                    && chars.get(j + 4).is_none_or(|c| !c.is_ascii_digit());
                if sep && three {
                    j += 1;
                    continue;
                }
                if chars.get(j) == Some(&',') && chars.get(j + 1).is_some_and(|c| c.is_ascii_digit()) {
                    n.push(',');
                    j += 1;
                    continue;
                }
                break;
            }
            out.push(n);
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

fn unverified_numbers(doc: &str, sources: &str) -> Vec<String> {
    let known: std::collections::HashSet<String> = numbers(sources).into_iter().collect();
    let mut suspects: Vec<String> = Vec::new();
    for line in doc.lines() {
        // Les numéros de points (« ### 3. ») viennent de l'ordre du jour reformaté.
        let body = line.trim_start_matches(|c: char| c == '#' || c == ' ');
        for n in numbers(body) {
            if !known.contains(&n) && !suspects.contains(&n) && n.parse::<u32>().map_or(true, |v| v > 31) {
                suspects.push(n);
            }
        }
    }
    suspects
}

/// Condense chaque tranche de la transcription en notes détaillées.
fn condense(r: &Request) -> Result<String> {
    let chars: Vec<char> = r.transcript.chars().collect();
    let mut notes = String::new();
    let mut start = 0;
    let mut part = 1;
    while start < chars.len() {
        let mut end = (start + CHUNK_CHARS).min(chars.len());
        // Coupe à la fin d'un paragraphe pour ne pas trancher une phrase.
        if end < chars.len() {
            if let Some(nl) = chars[start..end].iter().rposition(|&c| c == '\n') {
                if nl > CHUNK_CHARS / 2 {
                    end = start + nl;
                }
            }
        }
        let chunk: String = chars[start..end].iter().collect();
        let mut prompt = String::from(
            "Voici un passage d'une transcription de réunion. Prends des notes détaillées : sujets, \
             arguments, chiffres, noms, décisions et tâches avec responsables. Garde les horodatages \
             [mm:ss] des sujets. ",
        );
        prompt.push_str(STYLE);
        if let Some(agenda) = r.agenda {
            prompt.push_str("\n\nRattache chaque note au point de l'ordre du jour concerné :\n");
            prompt.push_str(agenda);
        }
        prompt.push_str("\n\n=== PASSAGE ===\n");
        prompt.push_str(&chunk);
        notes.push_str(&format!("\n--- Notes, partie {part} ---\n"));
        notes.push_str(&generate(r, &prompt)?);
        start = end;
        part += 1;
    }
    Ok(notes)
}

fn generate(r: &Request, prompt: &str) -> Result<String> {
    let texte = match r.moteur {
        Moteur::Integre { modele, fils } => generate_integre(modele, *fils, prompt, r.progress)?,
        Moteur::Ollama { url, modele } => generate_ollama(url, modele, prompt)?,
    };
    Ok(tidy_answer(&crate::quebec::normalize_document(texte.trim())))
}

/// Emplacement du programme auxiliaire : à côté de l'exécutable principal.
fn helper() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let path = exe
        .with_file_name(format!("tourdetable-redaction{}", std::env::consts::EXE_SUFFIX));
    if !path.exists() {
        bail!("Programme de rédaction introuvable ({}) : réinstallez Tour de table", path.display());
    }
    Ok(path)
}

fn generate_integre(modele: &std::path::Path, fils: usize, prompt: &str, progress: Option<&AtomicUsize>) -> Result<String> {
    if !modele.exists() {
        bail!("Le modèle de rédaction n'est pas installé : téléchargez-le dans les Paramètres");
    }
    // Contexte de travail : invite (≈ 3 caractères par jeton en français) + réponse.
    // Plafonné à 12 k jetons pour rester raisonnable sur un poste de 8 Go.
    let contexte = (prompt.chars().count() / 3 + 3072).clamp(8192, 12288);
    let mut cmd = Command::new(helper()?);
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        // Programme console : sans ce drapeau, une fenêtre noire apparaîtrait.
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().context("Lancement du programme de rédaction")?;
    let requete = json!({
        "modele": modele,
        "invite": prompt,
        "contexte": contexte,
        "max_jetons": 3072,
        "temperature": 0.2,
        "fils": fils,
    });
    {
        let mut stdin = child.stdin.take().expect("entrée redirigée");
        stdin.write_all(requete.to_string().as_bytes())?;
    } // fermer l'entrée signale la fin de la requête

    let mut texte = None;
    let mut erreur = None;
    for line in BufReader::new(child.stdout.take().expect("sortie redirigée")).lines() {
        let line = line?;
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        if let (Some(n), Some(p)) = (v["progres"].as_u64(), progress) {
            p.store(n as usize, Ordering::Relaxed);
        }
        if let Some(t) = v["texte"].as_str() {
            texte = Some(t.to_string());
        }
        if let Some(e) = v["erreur"].as_str() {
            erreur = Some(e.to_string());
        }
    }
    let status = child.wait()?;
    match (texte, erreur) {
        (Some(t), _) => Ok(t),
        (None, Some(e)) => bail!("Rédaction : {e}"),
        (None, None) => bail!("Le programme de rédaction s'est arrêté ({status}) : mémoire insuffisante?"),
    }
}

fn generate_ollama(base_url: &str, model: &str, prompt: &str) -> Result<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(1800)))
        .build()
        .into();
    let url = format!("{}/api/generate", base_url.trim_end_matches('/'));
    // Ollama tronque silencieusement à 2048-4096 jetons par défaut : on dimensionne
    // le contexte (≈ 3 caractères par jeton en français) + la place de la réponse.
    let num_ctx = (prompt.chars().count() / 3 + 3072).clamp(8192, 16384);
    let resp = agent
        .post(&url)
        .send_json(json!({
            "model": model,
            "prompt": prompt,
            "stream": false,
            "options": { "temperature": 0.2, "num_ctx": num_ctx }
        }))
        ;
    let mut resp = match resp {
        Err(ureq::Error::StatusCode(404)) => {
            anyhow::bail!("Le modèle « {model} » n'est pas installé dans Ollama : lancez « ollama pull {model} »")
        }
        other => other.with_context(|| format!("Ollama injoignable à {base_url} : est-il démarré?"))?,
    };
    let body: serde_json::Value = resp.body_mut().with_config().limit(32 << 20).read_json()?;
    body["response"].as_str().map(str::to_string).context("Réponse Ollama inattendue")
}

/// Les petits modèles ajoutent parfois « Aucun » après un contenu réel
/// (« **Suivis** : Marc s'en occupe. Aucun ») : on retire ce reliquat.
fn tidy_answer(md: &str) -> String {
    md.lines()
        .map(|line| {
            for tail in [" Aucun", " Aucune", " Aucun.", " Aucune."] {
                if let Some(body) = line.strip_suffix(tail) {
                    let content = body.rsplit_once(':').map_or(body, |(_, c)| c).trim();
                    if !content.is_empty() {
                        return body.trim_end().to_string();
                    }
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signale_les_chiffres_inventes() {
        let src = "On a dépensé 2 300 $ sur un budget de 2 500 $. Prochaine rencontre le 19 novembre 2026.";
        let doc = "Budget de 2 500 $, dépenses de 2 300 $ : dépassement de 200 $. Le 19 novembre 2026.";
        assert_eq!(unverified_numbers(doc, src), vec!["200".to_string()]);
        assert_eq!(numbers("12 000 $ et 2,5 % et 14 h 30"), vec!["12000", "2,5", "14", "30"]);
    }

    #[test]
    fn retire_les_aucun_parasites() {
        assert_eq!(tidy_answer("**Suivis** : Marc publie l'annonce. Aucun"), "**Suivis** : Marc publie l'annonce.");
        assert_eq!(tidy_answer("**Suivis** : Aucun"), "**Suivis** : Aucun");
        assert_eq!(tidy_answer("**Décisions** : Aucune"), "**Décisions** : Aucune");
        assert_eq!(tidy_answer("**Décisions** : Aucune."), "**Décisions** : Aucune.");
    }
}
