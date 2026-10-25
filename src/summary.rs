//! Compte rendu de rencontre par un modèle de langue local.
//!
//! Par défaut, le modèle est intégré : le programme auxiliaire
//! `tourdetable-redaction` (llama.cpp) le fait tourner sur l'ordinateur, sans
//! rien à installer. Rien ne quitte la machine.
//!
//! Avec un ordre du jour, le compte rendu suit ses points dans l'ordre ;
//! sans ordre du jour, on produit un résumé structuré.
//!
//! Rencontre découpée par point : un appel par point, avec seulement sa transcription
//! (nettoyée si elle existe), enregistré dans `resume.md` de son dossier pour reprendre un
//! compte rendu interrompu ; puis un dernier appel court pour les sujets hors point et la
//! prochaine rencontre. Le tableau des suivis réunit ceux de tous les points, sans passer par
//! le modèle. Toute la série se fait avec un seul chargement du modèle (`Session`).

use std::io::{BufRead, BufReader, Lines, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use anyhow::{Context, Result, bail};
use serde_json::json;

/// Qui rédige : le modèle intégré, avec ses fils de calcul.
#[derive(Clone, Debug)]
pub struct Moteur {
    pub modele: PathBuf,
    pub fils: usize,
}

impl Moteur {
    pub fn nom(&self) -> String {
        crate::models::llm_info(&self.modele.file_name().unwrap_or_default().to_string_lossy())
            .map_or_else(|| self.modele.display().to_string(), |m| m.label.to_string())
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

/// Au-delà, la transcription (ou celle d'un point) est d'abord condensée par tranches : les
/// petits modèles locaux perdent le fil sur des contextes trop longs, et la mémoire du contexte
/// (cache KV) grossit avec sa taille.
const CHUNK_CHARS: usize = 24_000;

const NON_ABORDE: &str = "Point non abordé ou reporté.";

const PAR_POINT: &str = "Tu es secrétaire de réunion. Voici la transcription d'un seul point de l'ordre du jour : \
« {point} ». Rédige uniquement ce point, sans titre, en trois rubriques dans cet ordre :\n\
**Discussion** : l'essentiel en quelques phrases.\n\
**Décisions** : une proposition adoptée, votée ou acceptée par le groupe est une décision : écris « Il est résolu de… ». \
S'il n'y en a pas, écris « Aucune ».\n\
**Suivis** : un tableau à trois colonnes, Action | Responsable | Échéance, une ligne par tâche. Chaque fois qu'une \
personne accepte ou se voit confier une tâche (« je m'en occupe », « tu t'en occupes? Correct »), c'est un suivi. \
Reprends l'échéance telle qu'elle a été dite (« d'ici la fin novembre », « pour le 15 décembre »), sinon « non précisée ». \
S'il n'y en a pas, écris « Aucun » au lieu du tableau.\n\
Si la date, l'heure ou le lieu de la prochaine rencontre sont annoncés, mentionne-les dans la discussion.\n";

const ASSEMBLAGE: &str = "Tu es secrétaire de réunion. Les points de l'ordre du jour sont déjà rédigés (ci-dessous). \
Rédige seulement les deux sections suivantes, en Markdown, dans cet ordre :\n\
## Sujets hors ordre du jour\n\
Les sujets discutés dans les échanges hors des points (transcription ci-dessous), avec leurs décisions et leurs suivis \
(tableau Action | Responsable | Échéance). Omets cette section s'il n'y en a pas.\n\
## Prochaine rencontre\n\
La date, l'heure et le lieu de la prochaine rencontre s'ils ont été annoncés ; sinon, omets cette section. \
Ne recopie pas le lieu ou l'heure de cette rencontre-ci.\n\
Ne répète pas les points déjà rédigés et ne fais pas de tableau récapitulatif.\n";

/// Avancement d'un compte rendu, lu par l'interface.
#[derive(Default)]
pub struct Avancement {
    /// Jetons rédigés par l'appel en cours.
    pub jetons: AtomicUsize,
    /// Point en cours (à partir de 1 ; `points + 1` : assemblage) et nombre de points.
    pub point: AtomicUsize,
    pub points: AtomicUsize,
    pub annuler: AtomicBool,
}

/// Un point de l'ordre du jour d'une rencontre découpée par point.
pub struct PointCr {
    /// « 2.1. Budget »
    pub titre: String,
    /// Transcription du point (toutes ses périodes) ; `None` : aucune période.
    pub transcription: Option<String>,
    /// resume.md du dossier du point.
    pub resume: Option<PathBuf>,
}

pub struct Request<'a> {
    pub moteur: &'a Moteur,
    pub avancement: Option<&'a Avancement>,
    pub title: &'a str,
    pub date: &'a str,
    pub duration: &'a str,
    pub agenda: Option<&'a str>,
    pub transcript: &'a str,
    /// Points dans l'ordre de l'ordre du jour, si la rencontre est découpée par point ; sinon vide.
    pub points: Vec<PointCr>,
    /// Échanges hors des points (rencontre découpée par point).
    pub hors_point: String,
    /// Réutiliser les resume.md encore valides (reprise) ; sinon tout est rédigé à nouveau.
    pub reprendre: bool,
}

/// Points et échanges hors point d'une rencontre découpée par point (vide sinon), à partir de
/// la transcription nettoyée quand elle existe.
pub fn points_de(m: &crate::storage::Meeting) -> (Vec<PointCr>, String) {
    let Some(agenda) = m.agenda.as_ref().filter(|a| !a.points.is_empty() && !m.periodes.is_empty()) else {
        return (Vec::new(), String::new());
    };
    let points = (0..agenda.points.len())
        .map(|i| PointCr {
            titre: m.intertitre(Some(i)),
            transcription: m.texte_du_point(Some(i)),
            resume: Some(m.dossier_du_point(Some(i)).join("resume.md")),
        })
        .collect();
    (points, m.texte_du_point(None).unwrap_or_default())
}

pub fn compte_rendu(r: &Request) -> Result<String> {
    let mut session: Option<Session> = None;
    let mut rediger = |prompt: &str| -> Result<String> {
        if session.is_none() {
            session = Some(Session::ouvrir(r.moteur)?);
        }
        let s = session.as_mut().expect("ouverte");
        if let Some(a) = r.avancement {
            a.jetons.store(0, Ordering::Relaxed);
        }
        s.rediger(prompt, r.avancement.map(|a| &a.jetons), r.avancement.map(|a| &a.annuler))
    };
    rediger_compte_rendu(r, &mut rediger)
}

fn rediger_compte_rendu(r: &Request, rediger: &mut Redacteur) -> Result<String> {
    let mut doc = if r.points.is_empty() || r.agenda.is_none() { en_un_appel(r, rediger)? } else { par_point(r, rediger)? };

    // Garde-fou : un modèle local peut se tromper dans un calcul ou inventer un
    // chiffre. On signale tout nombre absent des sources.
    let sources = format!("{} {} {} {} {}", r.agenda.unwrap_or(""), r.transcript, r.hors_point, r.date, r.duration);
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

/// Appel au modèle : invite → réponse. Simulé dans les tests.
type Redacteur<'a> = dyn FnMut(&str) -> Result<String> + 'a;

/// Comportement d'avant le découpage par point : toute la transcription d'un coup (condensée
/// par tranches si elle est longue).
fn en_un_appel(r: &Request, rediger: &mut Redacteur) -> Result<String> {
    let transcript = if r.transcript.chars().count() > CHUNK_CHARS {
        condense(r.transcript, r.agenda, rediger)?
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
    generate(rediger, &prompt)
}

/// Étape 1 : un appel par point ; étape 2 : sujets hors point et prochaine rencontre. Le
/// tableau des suivis reprend toutes les lignes des tableaux de suivis rédigés.
fn par_point(r: &Request, rediger: &mut Redacteur) -> Result<String> {
    let n = r.points.len();
    let etape = |i: usize| {
        if let Some(a) = r.avancement {
            a.points.store(n, Ordering::Relaxed);
            a.point.store(i, Ordering::Relaxed);
        }
    };
    let mut sections = Vec::with_capacity(n);
    let mut suivis = Vec::new();
    for (i, p) in r.points.iter().enumerate() {
        etape(i + 1);
        let corps = match p.transcription.as_deref().filter(|t| !t.trim().is_empty()) {
            None => NON_ABORDE.to_string(),
            Some(t) => resume_du_point(r, p, t, rediger)?,
        };
        suivis.extend(lignes_de_tableau(&corps));
        sections.push(format!("### {}\n\n{corps}", p.titre));
    }
    let sections = sections.join("\n\n");

    etape(n + 1);
    let hors_point = if r.hors_point.chars().count() > CHUNK_CHARS {
        condense(&r.hors_point, None, rediger)?
    } else {
        r.hors_point.clone()
    };
    let mut prompt = String::from(ASSEMBLAGE);
    prompt.push_str(STYLE);
    prompt.push_str("\n\n=== POINTS RÉDIGÉS ===\n");
    prompt.push_str(&sections);
    prompt.push_str("\n\n=== ÉCHANGES HORS DES POINTS ===\n");
    prompt.push_str(if hors_point.trim().is_empty() { "(aucun)" } else { &hors_point });
    let fin = generate(rediger, &prompt)?;
    let (sujets, prochaine) = sections_de_fin(&fin);
    suivis.extend(lignes_de_tableau(&sujets));

    let mut doc = format!(
        "# Compte rendu — {}\n**Date** : {} · **Durée** : {}\n\n## Points de l'ordre du jour\n\n{sections}\n\n",
        r.title, r.date, r.duration
    );
    if !sujets.is_empty() {
        doc.push_str(&format!("## Sujets hors ordre du jour\n\n{sujets}\n\n"));
    }
    doc.push_str("## Tableau des suivis\n\n");
    if suivis.is_empty() {
        doc.push_str("Aucun suivi.\n");
    } else {
        doc.push_str("| Action | Responsable | Échéance |\n|---|---|---|\n");
        for l in &suivis {
            doc.push_str(l);
            doc.push('\n');
        }
    }
    if !prochaine.is_empty() {
        doc.push_str(&format!("\n## Prochaine rencontre\n\n{prochaine}\n"));
    }
    Ok(doc.trim_end().to_string())
}

/// Résumé d'un point : repris de son resume.md s'il correspond toujours à sa transcription,
/// sinon rédigé (en condensant d'abord un point trop long) puis enregistré.
fn resume_du_point(r: &Request, p: &PointCr, transcription: &str, rediger: &mut Redacteur) -> Result<String> {
    let entete = format!("<!-- tourdetable {} -->", empreinte(&format!("{}\n{transcription}", p.titre)));
    if r.reprendre
        && let Some(chemin) = &p.resume
        && let Ok(texte) = std::fs::read_to_string(chemin)
        && let Some(corps) = texte.strip_prefix(&entete)
    {
        log::info!("Compte rendu : « {} » repris de {}", p.titre, chemin.display());
        return Ok(corps.trim().to_string());
    }
    let source = if transcription.chars().count() > CHUNK_CHARS {
        condense(transcription, None, rediger)?
    } else {
        transcription.to_string()
    };
    let mut prompt = PAR_POINT.replace("{point}", &p.titre);
    prompt.push_str(STYLE);
    prompt.push_str("\n\n=== TRANSCRIPTION DU POINT ===\n");
    prompt.push_str(&source);
    let reponse = generate(rediger, &prompt)?;
    // Le titre est ajouté par Tour de table : on retire celui que le modèle aurait mis.
    let corps = reponse.lines().skip_while(|l| l.trim().is_empty() || l.trim_start().starts_with('#')).collect::<Vec<_>>().join("\n");
    if let Some(chemin) = &p.resume {
        let ecrit = chemin.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(chemin, format!("{entete}\n{corps}\n")));
        if let Err(e) = ecrit {
            log::warn!("{} : {e}", chemin.display());
        }
    }
    Ok(corps)
}

fn empreinte(texte: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(texte.as_bytes()))[..16].to_string()
}

/// Lignes de données des tableaux Markdown (ni en-tête, ni séparateur, ni « Aucun »).
fn lignes_de_tableau(md: &str) -> Vec<String> {
    md.lines()
        .map(str::trim)
        .filter(|l| l.starts_with('|') && l.ends_with('|') && l.len() > 2)
        .filter(|l| !l.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')))
        .filter(|l| {
            let bas = l.to_lowercase();
            !(bas.contains("action") && bas.contains("responsable"))
        })
        .filter(|l| !l.trim_start_matches(['|', ' ']).to_lowercase().starts_with("aucun"))
        .map(str::to_string)
        .collect()
}

/// Corps des sections « Sujets hors ordre du jour » et « Prochaine rencontre » de la réponse
/// de l'étape 2 ; toute autre section (tableau récapitulatif malgré la consigne…) est ignorée.
fn sections_de_fin(md: &str) -> (String, String) {
    let (mut sujets, mut prochaine) = (Vec::new(), Vec::new());
    let mut courante: Option<&mut Vec<&str>> = None;
    for ligne in md.lines() {
        if let Some(titre) = ligne.trim_start().strip_prefix("## ") {
            let t = titre.trim().to_lowercase();
            courante = if t.starts_with("sujets") {
                Some(&mut sujets)
            } else if t.starts_with("prochaine") {
                Some(&mut prochaine)
            } else {
                None
            };
            continue;
        }
        if let Some(v) = courante.as_mut() {
            v.push(ligne);
        }
    }
    (sujets.join("\n").trim().to_string(), prochaine.join("\n").trim().to_string())
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
        let body = line.trim_start_matches(['#', ' ']);
        for n in numbers(body) {
            if !known.contains(&n) && !suspects.contains(&n) && n.parse::<u32>().map_or(true, |v| v > 31) {
                suspects.push(n);
            }
        }
    }
    suspects
}

/// Condense chaque tranche d'une transcription en notes détaillées.
fn condense(texte: &str, agenda: Option<&str>, rediger: &mut Redacteur) -> Result<String> {
    let chars: Vec<char> = texte.chars().collect();
    let mut notes = String::new();
    let mut start = 0;
    let mut part = 1;
    while start < chars.len() {
        let mut end = (start + CHUNK_CHARS).min(chars.len());
        // Coupe à la fin d'un paragraphe pour ne pas trancher une phrase.
        if end < chars.len()
            && let Some(nl) = chars[start..end].iter().rposition(|&c| c == '\n')
            && nl > CHUNK_CHARS / 2
        {
            end = start + nl;
        }
        let chunk: String = chars[start..end].iter().collect();
        let mut prompt = String::from(
            "Voici un passage d'une transcription de réunion. Prends des notes détaillées : sujets, \
             arguments, chiffres, noms, décisions et tâches avec responsables. Garde les horodatages \
             [mm:ss] des sujets. ",
        );
        prompt.push_str(STYLE);
        if let Some(agenda) = agenda {
            prompt.push_str("\n\nRattache chaque note au point de l'ordre du jour concerné :\n");
            prompt.push_str(agenda);
        }
        prompt.push_str("\n\n=== PASSAGE ===\n");
        prompt.push_str(&chunk);
        notes.push_str(&format!("\n--- Notes, partie {part} ---\n"));
        notes.push_str(&generate(rediger, &prompt)?);
        start = end;
        part += 1;
    }
    Ok(notes)
}

fn generate(rediger: &mut Redacteur, prompt: &str) -> Result<String> {
    let texte = rediger(prompt)?;
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

/// Programme de rédaction lancé une fois et gardé pour toute une série d'appels : le modèle
/// n'est chargé qu'à la première requête. Il s'arrête quand la session est abandonnée.
pub struct Session {
    child: Child,
    entree: Option<ChildStdin>,
    sortie: Lines<BufReader<ChildStdout>>,
    modele: PathBuf,
    fils: usize,
}

impl Session {
    pub fn ouvrir(moteur: &Moteur) -> Result<Self> {
        if !moteur.modele.exists() {
            bail!("Le modèle de rédaction n'est pas installé : installez-le depuis la fiche de la rencontre");
        }
        let mut cmd = Command::new(helper()?);
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        #[cfg(windows)]
        {
            // Programme console : sans ce drapeau, une fenêtre noire apparaîtrait.
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = cmd.spawn().context("Lancement du programme de rédaction")?;
        let entree = child.stdin.take();
        let sortie = BufReader::new(child.stdout.take().expect("sortie redirigée")).lines();
        Ok(Self { child, entree, sortie, modele: moteur.modele.clone(), fils: moteur.fils })
    }

    /// Une requête. `annuler` arrête le programme dès sa prochaine ligne de sortie ; la session
    /// est alors inutilisable.
    pub fn rediger(&mut self, prompt: &str, progress: Option<&AtomicUsize>, annuler: Option<&AtomicBool>) -> Result<String> {
        // Contexte de travail : invite (≈ 3 caractères par jeton en français) + réponse.
        // Plafonné à 12 k jetons pour rester raisonnable sur un poste de 8 Go.
        let contexte = (prompt.chars().count() / 3 + 3072).clamp(8192, 12288);
        let requete = json!({
            "modele": self.modele,
            "invite": prompt,
            "contexte": contexte,
            "max_jetons": 3072,
            "temperature": 0.2,
            "fils": self.fils,
        });
        let entree = self.entree.as_mut().context("Programme de rédaction arrêté")?;
        writeln!(entree, "{requete}").and_then(|()| entree.flush()).context("Envoi au programme de rédaction")?;
        for line in self.sortie.by_ref() {
            let line = line?;
            if annuler.is_some_and(|a| a.load(Ordering::Relaxed)) {
                self.entree = None;
                let _ = self.child.kill();
                bail!("Rédaction annulée");
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
            if let (Some(n), Some(p)) = (v["progres"].as_u64(), progress) {
                p.store(n as usize, Ordering::Relaxed);
            }
            if let Some(t) = v["texte"].as_str() {
                return Ok(t.to_string());
            }
            if let Some(e) = v["erreur"].as_str() {
                bail!("Rédaction : {e}");
            }
        }
        self.entree = None;
        let status = self.child.wait()?;
        bail!("Le programme de rédaction s'est arrêté ({status}) : mémoire insuffisante?")
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Fermer l'entrée termine le programme ; on n'attend pas une rédaction en cours.
        self.entree = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
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

    fn requete<'a>(moteur: &'a Moteur, points: Vec<PointCr>, hors_point: &str, reprendre: bool) -> Request<'a> {
        Request {
            moteur,
            avancement: None,
            title: "CA mensuel",
            date: "2026-10-30",
            duration: "1:10:00",
            agenda: Some("1. Ouverture\n2. Budget\n3. Varia"),
            transcript: "Budget de 2 500 $. Julie publie l'annonce.",
            points,
            hors_point: hors_point.to_string(),
            reprendre,
        }
    }

    fn moteur() -> Moteur {
        Moteur { modele: PathBuf::from("absent.gguf"), fils: 1 }
    }

    /// Modèle simulé : répond selon le point demandé et note les invites reçues.
    fn simule(invites: &mut Vec<String>) -> impl FnMut(&str) -> Result<String> + '_ {
        |invite: &str| {
            invites.push(invite.to_string());
            Ok(if invite.contains("« 1. Ouverture »") {
                "### 1. Ouverture\n**Discussion** : Ouverture à 19 h.\n**Décisions** : Aucune\n**Suivis** : Aucun".into()
            } else if invite.contains("« 2. Budget »") {
                "**Discussion** : Budget de 2 500 $.\n**Décisions** : Il est résolu d'adopter le budget.\n**Suivis** :\n\
                 | Action | Responsable | Échéance |\n|---|---|---|\n| Publier l'annonce | Julie | d'ici la fin novembre |\n\
                 | Appeler la caisse | Marc | non précisée |"
                    .into()
            } else {
                "## Sujets hors ordre du jour\nLe stationnement.\n| Action | Responsable | Échéance |\n|---|---|---|\n\
                 | Écrire à la Ville | Lise | non précisée |\n## Tableau des suivis\n| Inventé | X | Y |\n\
                 ## Prochaine rencontre\nLe 19 novembre à 19 h."
                    .into()
            })
        }
    }

    fn points(dossier: Option<&std::path::Path>) -> Vec<PointCr> {
        let p = |titre: &str, t: Option<&str>, nom: &str| PointCr {
            titre: titre.into(),
            transcription: t.map(str::to_string),
            resume: dossier.map(|d| d.join(nom).join("resume.md")),
        };
        vec![
            p("1. Ouverture", Some("[00:01] Salle : On ouvre à 19 h."), "01-ouverture"),
            p("2. Budget", Some("[00:05] Salle : Le budget est de 2 500 $. Julie publie l'annonce."), "02-budget"),
            p("3. Varia", None, "03-varia"),
        ]
    }

    #[test]
    fn compte_rendu_par_point() {
        let moteur = moteur();
        let r = requete(&moteur, points(None), "[00:30] Salle : Le stationnement est plein.", false);
        let mut invites = Vec::new();
        let doc = rediger_compte_rendu(&r, &mut simule(&mut invites)).unwrap();
        // Un appel par point abordé, puis l'assemblage ; le point non abordé n'appelle pas le modèle.
        assert_eq!(invites.len(), 3);
        assert!(invites[0].contains("[00:01] Salle : On ouvre") && !invites[0].contains("budget"));
        assert!(invites[2].contains("=== POINTS RÉDIGÉS ===") && invites[2].contains("stationnement est plein"));

        let ordre: Vec<usize> = ["### 1. Ouverture", "### 2. Budget", "### 3. Varia", "## Sujets hors", "## Tableau des suivis", "## Prochaine"]
            .iter()
            .map(|t| doc.find(t).unwrap_or_else(|| panic!("{t} absent :\n{doc}")))
            .collect();
        assert!(ordre.windows(2).all(|w| w[0] < w[1]), "{doc}");
        assert!(doc.starts_with("# Compte rendu — CA mensuel\n**Date** : 2026-10-30 · **Durée** : 1:10:00"));
        assert!(doc.contains("### 3. Varia\n\nPoint non abordé ou reporté."));
        // Le titre répété par le modèle est retiré.
        assert_eq!(doc.matches("1. Ouverture").count(), 1, "{doc}");
        // Tableau des suivis complet : tous les points et le hors point, rien d'inventé.
        let tableau = &doc[doc.find("## Tableau des suivis").unwrap()..doc.find("## Prochaine").unwrap()];
        for action in ["Publier l'annonce", "Appeler la caisse", "Écrire à la Ville"] {
            assert!(tableau.contains(action), "{action} absent :\n{tableau}");
        }
        assert!(!doc.contains("Inventé"));
        assert!(doc.ends_with("## Prochaine rencontre\n\nLe 19 novembre à 19 h."));
        assert!(!doc.contains("À vérifier"), "{doc}");
    }

    #[test]
    fn reprise_d_un_compte_rendu_interrompu() {
        let dir = std::env::temp_dir().join(format!("tourdetable-test-{}-cr", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let moteur = moteur();
        let mut invites = Vec::new();
        rediger_compte_rendu(&requete(&moteur, points(Some(&dir)), "", true), &mut simule(&mut invites)).unwrap();
        assert_eq!(invites.len(), 3);
        let resume = std::fs::read_to_string(dir.join("02-budget/resume.md")).unwrap();
        assert!(resume.starts_with("<!-- tourdetable ") && resume.contains("| Publier l'annonce |"));
        assert!(!dir.join("03-varia/resume.md").exists());

        // Reprise : seul l'assemblage est refait.
        invites.clear();
        let doc = rediger_compte_rendu(&requete(&moteur, points(Some(&dir)), "", true), &mut simule(&mut invites)).unwrap();
        assert_eq!(invites.len(), 1, "{invites:?}");
        assert!(doc.contains("| Appeler la caisse | Marc |"));

        // Transcription du point modifiée (nettoyage relancé) : il est rédigé à nouveau.
        let mut p = points(Some(&dir));
        p[1].transcription = Some("[00:05] Salle : Budget adopté.".into());
        invites.clear();
        rediger_compte_rendu(&requete(&moteur, p, "", true), &mut simule(&mut invites)).unwrap();
        assert_eq!(invites.len(), 2);
        // « Régénérer » : tout est refait.
        invites.clear();
        rediger_compte_rendu(&requete(&moteur, points(Some(&dir)), "", false), &mut simule(&mut invites)).unwrap();
        assert_eq!(invites.len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ancienne_rencontre_sans_periodes() {
        let moteur = moteur();
        let mut invites = Vec::new();
        let mut appel = |i: &str| {
            invites.push(i.to_string());
            Ok("# Compte rendu — CA mensuel\n### 1. Ouverture\nCoût de 300 $.".to_string())
        };
        let doc = rediger_compte_rendu(&requete(&moteur, Vec::new(), "", true), &mut appel).unwrap();
        assert_eq!(invites.len(), 1);
        assert!(invites[0].contains("en suivant l'ordre du jour fourni") && invites[0].contains("=== TRANSCRIPTION ===\nBudget de 2 500 $"));
        // Garde-fou des chiffres conservé.
        assert!(doc.contains("> **À vérifier**") && doc.contains("300"), "{doc}");

        // Rencontre d'avant le découpage : aucun point.
        let mut m = crate::storage::Meeting::new("test.bin");
        m.agenda = Some(crate::storage::Agenda::new("od.txt".into(), "1. Ouverture\n2. Budget".into()));
        assert!(points_de(&m).0.is_empty());
    }

    #[test]
    fn tableaux_et_sections() {
        let md = "| Action | Responsable | Échéance |\n|:--|--|--|\n| Publier | Julie | 15 décembre |\n| Aucun | | |\nTexte | pas un tableau";
        assert_eq!(lignes_de_tableau(md), ["| Publier | Julie | 15 décembre |"]);
        let (sujets, prochaine) = sections_de_fin("Préambule\n## Sujets hors ordre du jour\nA\n## Autre\nB\n## Prochaine rencontre\nC");
        assert_eq!((sujets.as_str(), prochaine.as_str()), ("A", "C"));
        assert_eq!(sections_de_fin("Rien à signaler."), (String::new(), String::new()));
    }

    #[test]
    fn retire_les_aucun_parasites() {
        assert_eq!(tidy_answer("**Suivis** : Marc publie l'annonce. Aucun"), "**Suivis** : Marc publie l'annonce.");
        assert_eq!(tidy_answer("**Suivis** : Aucun"), "**Suivis** : Aucun");
        assert_eq!(tidy_answer("**Décisions** : Aucune"), "**Décisions** : Aucune");
        assert_eq!(tidy_answer("**Décisions** : Aucune."), "**Décisions** : Aucune.");
    }
}
