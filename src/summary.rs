//! Compte rendu de rencontre par un modèle de langue local.
//!
//! Par défaut, le modèle est intégré : le programme auxiliaire
//! `tourdetable-redaction` (llama.cpp) le fait tourner sur l'ordinateur, sans
//! rien à installer. Rien ne quitte la machine.
//!
//! Avec un ordre du jour, le compte rendu suit ses points dans l'ordre ;
//! sans ordre du jour, on produit un résumé structuré.
//!
//! Forme du compte rendu, d'après de vrais comptes rendus d'organisme : en-tête (titre, date,
//! heure), présences, puis chaque point de l'ordre du jour avec son numéro, rédigé au présent
//! et à la troisième personne (phrase d'introduction, puces, « Il est convenu… »), et la levée
//! de la séance. Le tour de table donne un paragraphe par personne, le varia un sous-titre par
//! sujet.
//!
//! Rencontre découpée par point : un appel par point, avec seulement sa transcription
//! (nettoyée si elle existe), enregistré dans `resume.md` de son dossier pour reprendre un
//! compte rendu interrompu ; puis un dernier appel court, à partir des points rédigés et des
//! échanges hors point, pour les présences, les sujets hors ordre du jour et la levée de la
//! séance. Toute la série se fait avec un seul chargement du modèle (`Session`).

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
    /// Sur la puce graphique (décidé par `gpu`).
    pub gpu: bool,
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

/// Forme commune à tous les points : celle des comptes rendus d'organisme.
const FORME: &str = "Écris comme dans un compte rendu officiel : au présent, à la troisième personne, \
ton neutre et factuel (« La direction présente… », « Les participants échangent sur… », « Il est convenu que… »). \
La transcription ne dit pas qui parle : nomme une personne seulement quand son nom est dit avec ce qu'elle fait \
(« Mme » ou « M. » suivi du nom de famille s'il est connu, sinon le prénom) ; pour la personne qui anime, écris \
« la direction », et pour les autres « un participant » ou « une participante ». Jamais « je », « on » ni « Salle ». \
Les noms entre crochets des exemples ne sont que des modèles : n'emploie que des noms entendus dans la transcription. Aucun tableau, aucune rubrique « Discussion », « Décisions » ou « Suivis », \
aucune étiquette du genre « Projet : » ou « Statut : ».\n";

const AVEC_ORDRE_DU_JOUR: &str = "Tu es secrétaire de réunion. Rédige le compte rendu de la rencontre \
en suivant l'ordre du jour fourni, point par point et dans le même ordre, avec la même numérotation.\n\
Pour chaque point : un titre de niveau 2 (« ## 3. Titre »), une phrase d'introduction, puis une liste à puces \
des éléments principaux, et s'il y a lieu une phrase sur ce qui est convenu ou reste à suivre.\n\
Règles :\n\
- Si un point entier n'a pas été discuté, écris seulement « Point non abordé ou reporté. » sous son titre.\n\
- Pour un tour de table, une ligne en gras par personne (« **[Prénom Nom]** »), suivie de 2 à 4 puces courtes \
qui commencent par un verbe (« Poursuit… », « Prépare… »). Seulement des personnes, jamais un groupe (« les comptables »).\n\
- Si la transcription s'arrête avant un point, écris « Point non abordé ou reporté. » : n'invente rien.\n\
- L'ordre du jour sert uniquement à structurer : les faits (lieu, heure, montants, noms) viennent \
de la transcription seulement. Ne recopie pas le lieu ou l'heure de cette rencontre pour la prochaine.\n\
Structure du document :\n\
# Compte rendu\n\
**{titre}**\n\
**Date** : {date}{heure}\n\
## Présences (liste à puces, prénom et nom, des personnes qui prennent la parole ; jamais un groupe)\n\
## 1. … (chaque point de l'ordre du jour)\n\
## Levée de la séance (un court paragraphe qui résume l'esprit de la rencontre)\n";

/// Au-delà, la transcription (ou celle d'un point) est d'abord condensée par tranches : les
/// petits modèles locaux perdent le fil sur des contextes trop longs, et la mémoire du contexte
/// (cache KV) grossit avec sa taille.
const CHUNK_CHARS: usize = 24_000;

const NON_ABORDE: &str = "Point non abordé ou reporté.";

/// Longueur maximale d'une réponse, en jetons. La rédaction (quelques jetons par seconde
/// sur processeur seul) prend l'essentiel du temps d'un compte rendu : chaque point est court.
const JETONS_POINT: usize = 600;
/// Tour de table : un paragraphe par personne, souvent une dizaine.
const JETONS_TOUR_DE_TABLE: usize = 1200;
const JETONS_ASSEMBLAGE: usize = 900;
const JETONS_NOTES: usize = 1200;
const JETONS_DOCUMENT: usize = 3072;

const PAR_POINT: &str = "Tu es secrétaire de réunion. Voici la transcription d'un seul point de l'ordre du jour : \
« {point} ». Rédige uniquement ce point, sans titre.\n";

/// Point ordinaire : introduction, puces, conclusion.
const POINT_ORDINAIRE: &str = "Forme : une phrase d'introduction (« La direction présente… », « Les participants \
échangent sur… »), puis une liste à puces de 3 à 6 éléments principaux, une idée courte par puce, puis, s'il y a lieu, \
une phrase sur ce qui est convenu ou reste à suivre (« Il est convenu que… », « La direction indique qu'aucune décision \
n'a encore été prise. »). Un point très bref peut tenir en une ou deux phrases, sans liste. 150 mots au plus.\n";

/// Tour de table : un paragraphe par personne.
const POINT_TOUR_DE_TABLE: &str = "Forme : pour chaque personne qui prend la parole, dans l'ordre, son prénom et son nom \
en gras sur une ligne (« **[Prénom Nom]** »), puis 2 à 4 puces courtes sur ses dossiers et ses travaux en cours, \
qui commencent par un verbe (« Poursuit les travaux… », « Prépare… »). Seulement des personnes, jamais un groupe \
(« les comptables »). Si le prénom d'une personne n'a pas été dit, écris « **Participant(e)** » : n'invente \
aucun nom. Pas de phrase d'introduction. 50 mots au plus par personne.\n";

/// Notes d'un long tour de table : qui parle doit survivre à la condensation.
const NOTES_TOUR_DE_TABLE: &str = "C'est un tour de table : fais une note par prise de parole, dans l'ordre, qui \
commence par le prénom de la personne s'il est dit (« Sophie : », souvent annoncé par la personne précédente : « à \
toi, Karim »), sinon par « Personne suivante : ». N'invente aucun nom. ";

/// Varia : un sous-titre par sujet.
const POINT_VARIA: &str = "Forme : pour chaque sujet distinct, un sous-titre en gras sur une ligne (« **[Sujet]** »), puis une ou deux phrases, ou une phrase suivie de puces. 200 mots au plus.\n";

/// Prochaine rencontre : des puces seulement.
const POINT_PROCHAINE: &str = "Forme : 1 à 3 puces seulement : la date, l'heure ou le moment annoncé pour la prochaine \
rencontre, et les autres rencontres prévues d'ici là. Ne recopie pas le lieu ou l'heure de cette rencontre-ci.\n";

const ASSEMBLAGE: &str = "Tu es secrétaire de réunion. Les points de l'ordre du jour sont déjà rédigés (ci-dessous). \
Rédige seulement les sections suivantes, en Markdown, dans cet ordre :\n\
## Présences\n\
Une liste à puces, prénom et nom, des personnes qui ont participé à la rencontre : celles qui prennent la parole \
ou présentent leurs dossiers dans les points rédigés. N'ajoute pas une personne seulement mentionnée (absente, \
partenaire externe), ni un groupe (« les comptables »).\n\
## Levée de la séance\n\
Un paragraphe de 2 ou 3 phrases : la rencontre se termine, l'esprit des échanges et les principaux sujets qui \
se poursuivront.\n\
{prochaine}\
## Sujets hors ordre du jour\n\
Les sujets de travail discutés dans les échanges hors des points (transcription ci-dessous), en quelques puces \
courtes. Omets cette section s'il n'y en a pas ou si ce ne sont que des échanges personnels.\n\
Ne répète pas les points déjà rédigés.\n";

/// Section demandée à l'assemblage quand l'ordre du jour n'a pas de point « Prochaine rencontre ».
const ASSEMBLAGE_PROCHAINE: &str = "## Prochaine rencontre\n\
La date, l'heure et le lieu de la prochaine rencontre s'ils ont été annoncés ; sinon, omets cette section. \
Ne recopie pas le lieu ou l'heure de cette rencontre-ci.\n";

const PRESENCES_A_VERIFIER: &str = "*Liste établie d'après les personnes qui prennent la parole : à vérifier.*";

/// Forme d'un point, reconnue à son titre.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Forme {
    Ordinaire,
    TourDeTable,
    Varia,
    Prochaine,
}

impl Forme {
    fn du_point(titre: &str) -> Self {
        let t = titre.to_lowercase();
        if t.contains("tour de table") {
            Forme::TourDeTable
        } else if t.contains("varia") || t.contains("divers") || t.contains("affaires nouvelles") {
            Forme::Varia
        } else if est_prochaine_rencontre(&t) {
            Forme::Prochaine
        } else {
            Forme::Ordinaire
        }
    }

    fn consigne(self) -> &'static str {
        match self {
            Forme::Ordinaire => POINT_ORDINAIRE,
            Forme::TourDeTable => POINT_TOUR_DE_TABLE,
            Forme::Varia => POINT_VARIA,
            Forme::Prochaine => POINT_PROCHAINE,
        }
    }
}

fn est_prochaine_rencontre(titre: &str) -> bool {
    let t = titre.to_lowercase();
    t.contains("prochaine") && (t.contains("rencontre") || t.contains("réunion") || t.contains("séance"))
}

const MOIS: [&str; 12] = ["janvier", "février", "mars", "avril", "mai", "juin", "juillet", "août", "septembre", "octobre", "novembre", "décembre"];

/// « 1er », « 2 »…
fn jour_du_mois(jour: u32) -> String {
    if jour == 1 { "1er".to_string() } else { jour.to_string() }
}

/// « 8 octobre 2026 ».
pub fn date_longue(d: &chrono::DateTime<chrono::Local>) -> String {
    use chrono::Datelike;
    format!("{} {} {}", jour_du_mois(d.day()), MOIS[d.month0() as usize], d.year())
}

/// Date et heure annoncées dans l'en-tête de l'ordre du jour (« 8 OCTOBRE 2026, 11h00 ») :
/// plus sûres que le début d'un enregistrement importé, souvent la date de la copie du fichier.
fn date_et_heure_de(agenda: &str) -> (Option<String>, Option<String>) {
    let entete: Vec<String> = agenda.lines().map(str::trim).filter(|l| !l.is_empty()).take(6).map(str::to_lowercase).collect();
    let mut date = None;
    let mut heure = None;
    for ligne in &entete {
        let mots: Vec<&str> = ligne.split(|c: char| c.is_whitespace() || c == ',').filter(|m| !m.is_empty()).collect();
        for (i, mot) in mots.iter().enumerate() {
            // Le texte d'un PDF coupe parfois un mot en deux (« AVR IL ») : on essaie aussi le mot suivant.
            let mois_en = |n: usize| {
                let mot: String = mots[i..(i + n).min(mots.len())].concat();
                MOIS.iter().find(|m| **m == mot).map(|m| (*m, n))
            };
            if date.is_none()
                && let Some((mois, n)) = mois_en(1).or_else(|| mois_en(2))
                && let Some(jour) = i.checked_sub(1).and_then(|j| mots[j].trim_end_matches("er").parse::<u32>().ok()).filter(|j| (1..=31).contains(j))
                && let Some(annee) = mots.get(i + n).filter(|a| a.len() == 4 && a.chars().all(|c| c.is_ascii_digit()))
            {
                date = Some(format!("{} {mois} {annee}", jour_du_mois(jour)));
            }
            // « 11h00 », « 11h », « 11 h 00 » ou « 10 h30 ».
            let chiffres = |m: &&&str| m.len() == 2 && m.chars().all(|c| c.is_ascii_digit());
            let mot = match mots.get(i + 1) {
                Some(&"h") if mot.chars().all(|c| c.is_ascii_digit()) => format!("{mot}h{}", mots.get(i + 2).filter(chiffres).unwrap_or(&"")),
                Some(suivant) if mot.chars().all(|c| c.is_ascii_digit()) && suivant.strip_prefix('h').is_some_and(|m| chiffres(&&m)) => {
                    format!("{mot}{suivant}")
                }
                _ => mot.to_string(),
            };
            if heure.is_none()
                && let Some((h, m)) = mot.split_once('h')
                && let Ok(h) = h.parse::<u32>()
                && h < 24
                && (m.is_empty() || (m.len() == 2 && m.parse::<u32>().is_ok_and(|m| m < 60)))
            {
                heure = Some(format!("{h} h {}", if m.is_empty() { "00" } else { m }));
            }
        }
    }
    (date, heure)
}

/// « 11 h 00 ».
pub fn heure(d: &chrono::DateTime<chrono::Local>) -> String {
    d.format("%-H h %M").to_string()
}

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
    /// « 8 octobre 2026 ».
    pub date: &'a str,
    /// « 11 h 00 », vide si inconnue.
    pub heure: &'a str,
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
    (points, m.texte_hors_point_pendant().unwrap_or_default())
}

pub fn compte_rendu(r: &Request) -> Result<String> {
    let mut session: Option<Session> = None;
    let mut moteur = r.moteur.clone();
    let mut rediger = |prompt: &str, max_jetons: usize| -> Result<String> {
        loop {
            if session.is_none() {
                session = Some(Session::ouvrir(&moteur)?);
            }
            let s = session.as_mut().expect("ouverte");
            if let Some(a) = r.avancement {
                a.jetons.store(0, Ordering::Relaxed);
            }
            let annuler = r.avancement.map(|a| &a.annuler);
            let reponse = s.rediger(prompt, max_jetons, r.avancement.map(|a| &a.jetons), annuler);
            // Plantage sur la puce graphique (pilote) : on reprend la même requête sur le processeur.
            let annule = annuler.is_some_and(|a| a.load(Ordering::Relaxed));
            if reponse.is_err() && moteur.gpu && s.arrete() && !annule {
                crate::gpu::redaction_plantee();
                moteur.gpu = false;
                session = None;
                continue;
            }
            return reponse;
        }
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

/// Appel au modèle : invite et longueur maximale de la réponse (jetons) → réponse. Simulé dans les tests.
type Redacteur<'a> = dyn FnMut(&str, usize) -> Result<String> + 'a;

/// Comportement d'avant le découpage par point : toute la transcription d'un coup (condensée
/// par tranches si elle est longue).
fn en_un_appel(r: &Request, rediger: &mut Redacteur) -> Result<String> {
    let transcript = if r.transcript.chars().count() > CHUNK_CHARS {
        condense(r.transcript, r.agenda, "", rediger)?
    } else {
        r.transcript.to_string()
    };

    let mut prompt = match r.agenda {
        Some(_) => {
            let (date, heure) = date_et_heure(r);
            let mut p = AVEC_ORDRE_DU_JOUR.replace("{titre}", r.title).replace("{date}", &date).replace("{heure}", &mention_heure(&heure));
            p.push_str(FORME);
            p
        }
        None => SANS_ORDRE_DU_JOUR.to_string(),
    };
    prompt.push_str(STYLE);
    if let Some(agenda) = r.agenda {
        prompt.push_str("\n\n=== ORDRE DU JOUR ===\n");
        prompt.push_str(agenda);
    }
    prompt.push_str("\n\n=== TRANSCRIPTION ===\n");
    prompt.push_str(&transcript);
    generate(rediger, &prompt, JETONS_DOCUMENT)
}

/// Date et heure de l'en-tête : celles de l'ordre du jour s'il les annonce, sinon celles du
/// début de la rencontre.
fn date_et_heure(r: &Request) -> (String, String) {
    let (date, heure) = r.agenda.map(date_et_heure_de).unwrap_or_default();
    (date.unwrap_or_else(|| r.date.to_string()), heure.unwrap_or_else(|| r.heure.to_string()))
}

fn mention_heure(heure: &str) -> String {
    if heure.is_empty() { String::new() } else { format!(" · **Heure** : {heure}") }
}

/// Étape 1 : un appel par point ; étape 2 : présences, sujets hors point et levée de la séance,
/// à partir des points rédigés.
fn par_point(r: &Request, rediger: &mut Redacteur) -> Result<String> {
    let n = r.points.len();
    let etape = |i: usize| {
        if let Some(a) = r.avancement {
            a.points.store(n, Ordering::Relaxed);
            a.point.store(i, Ordering::Relaxed);
        }
    };
    let mut sections = Vec::with_capacity(n);
    for (i, p) in r.points.iter().enumerate() {
        etape(i + 1);
        let corps = match p.transcription.as_deref().filter(|t| !t.trim().is_empty()) {
            None => NON_ABORDE.to_string(),
            Some(t) => resume_du_point(r, p, t, rediger)?,
        };
        sections.push(format!("## {}\n\n{corps}", p.titre));
    }
    etape(n + 1);
    let hors_point = if r.hors_point.chars().count() > CHUNK_CHARS {
        condense(&r.hors_point, None, "", rediger)?
    } else {
        r.hors_point.clone()
    };
    let titres: Vec<&str> = r.points.iter().map(|p| p.titre.as_str()).collect();
    assembler(r, &sections.join("\n\n"), &titres, &hors_point, rediger)
}

/// Le titre est ajouté par Tour de table : on retire celui que le modèle aurait mis.
fn sans_titre(reponse: &str) -> String {
    reponse.lines().skip_while(|l| l.trim().is_empty() || l.trim_start().starts_with('#')).collect::<Vec<_>>().join("\n")
}

/// Réponse qui boucle : une ligne identique à une précédente, ou un paragraphe d'une personne
/// déjà présentée (« **Julie** » une seconde fois au tour de table), est retiré avec ce qui le suit
/// jusqu'à la personne suivante.
fn sans_repetitions(md: &str) -> String {
    let mut vues = std::collections::HashSet::new();
    let mut personnes = std::collections::HashSet::new();
    let mut sauter = false;
    let mut garde = Vec::new();
    for ligne in md.lines() {
        let t = ligne.trim();
        let nom = t.strip_prefix("**").and_then(|r| r.split_once("**")).map(|(n, _)| n.trim().to_lowercase());
        if let Some(nom) = &nom {
            // Plusieurs « Participant(e) » anonymes ne sont pas une boucle.
            sauter = !nom.starts_with("participant") && !personnes.insert(nom.clone());
        }
        if sauter || (nom.is_none() && !t.is_empty() && !vues.insert(t.to_string())) {
            continue;
        }
        garde.push(ligne);
    }
    garde.join("\n")
}

/// Étape finale : présences, sujets hors ordre du jour et levée de la séance, puis le document.
fn assembler(r: &Request, sections: &str, titres: &[&str], hors_point: &str, rediger: &mut Redacteur) -> Result<String> {
    let point_prochaine = titres.iter().any(|t| est_prochaine_rencontre(t));
    let mut prompt = ASSEMBLAGE.replace("{prochaine}", if point_prochaine { "" } else { ASSEMBLAGE_PROCHAINE });
    prompt.push_str(FORME);
    prompt.push_str(STYLE);
    prompt.push_str("\n\n=== POINTS RÉDIGÉS ===\n");
    prompt.push_str(sections);
    prompt.push_str("\n\n=== ÉCHANGES HORS DES POINTS ===\n");
    prompt.push_str(if hors_point.trim().is_empty() { "(aucun)" } else { hors_point });
    let fin = sections_de_fin(&generate(rediger, &prompt, JETONS_ASSEMBLAGE)?);

    let (date, heure) = date_et_heure(r);
    let titre = r.title.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut doc = format!("# Compte rendu\n\n**{titre}**\n\n**Date** : {date}{}\n\n", mention_heure(&heure));
    let presences = liste_des_presences(&fin.presences);
    if !presences.is_empty() {
        doc.push_str(&format!("## Présences\n\n{presences}\n\n{PRESENCES_A_VERIFIER}\n\n"));
    }
    doc.push_str(sections);
    doc.push_str("\n\n");
    if !fin.sujets.is_empty() {
        doc.push_str(&format!("## Sujets hors ordre du jour\n\n{}\n\n", fin.sujets));
    }
    if !point_prochaine && !fin.prochaine.is_empty() {
        doc.push_str(&format!("## Prochaine rencontre\n\n{}\n\n", fin.prochaine));
    }
    if !fin.levee.is_empty() {
        doc.push_str(&format!("## Levée de la séance\n\n{}\n", fin.levee));
    }
    Ok(doc.trim_end().to_string())
}

/// Résumé d'un point : repris de son resume.md s'il correspond toujours à sa transcription,
/// sinon rédigé (en condensant d'abord un point trop long) puis enregistré.
fn resume_du_point(r: &Request, p: &PointCr, transcription: &str, rediger: &mut Redacteur) -> Result<String> {
    let forme = Forme::du_point(&p.titre);
    // Les consignes font partie de l'empreinte : un résumé d'une forme antérieure est refait.
    let entete = format!("<!-- tourdetable {} -->", empreinte(&format!("{}\n{}{FORME}\n{transcription}", p.titre, forme.consigne())));
    if r.reprendre
        && let Some(chemin) = &p.resume
        && let Ok(texte) = std::fs::read_to_string(chemin)
        && let Some(corps) = texte.strip_prefix(&entete)
    {
        log::info!("Compte rendu : « {} » repris de {}", p.titre, chemin.display());
        return Ok(corps.trim().to_string());
    }
    let source = if transcription.chars().count() > CHUNK_CHARS {
        let precision = if forme == Forme::TourDeTable { NOTES_TOUR_DE_TABLE } else { "" };
        condense(transcription, None, precision, rediger)?
    } else {
        transcription.to_string()
    };
    let mut prompt = PAR_POINT.replace("{point}", &p.titre);
    prompt.push_str(forme.consigne());
    prompt.push_str(FORME);
    prompt.push_str(STYLE);
    prompt.push_str("\n\n=== TRANSCRIPTION DU POINT ===\n");
    prompt.push_str(&source);
    let jetons = if forme == Forme::TourDeTable { JETONS_TOUR_DE_TABLE } else { JETONS_POINT };
    let reponse = generate(rediger, &prompt, jetons)?;
    let corps = sans_repetitions(&sans_titre(&reponse));
    // « Point non abordé ou reporté. » accompagné d'explications : la formule seule.
    let corps = if corps.contains(NON_ABORDE.trim_end_matches('.')) { NON_ABORDE.to_string() } else { corps };
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

/// Sections rédigées à l'étape 2.
#[derive(Debug, Default, PartialEq)]
struct Fin {
    presences: String,
    sujets: String,
    prochaine: String,
    levee: String,
}

/// Présences en liste à puces, quelle que soit la forme rendue (liste, noms séparés par des
/// virgules) : sans « Mme » ni « M. », sans groupe (« les comptables »), sans doublon.
fn liste_des_presences(texte: &str) -> String {
    let mut noms: Vec<String> = Vec::new();
    for brut in texte.lines().flat_map(|l| l.split([',', ';'])) {
        let nom = brut.trim().trim_start_matches(['-', '*', '•']).trim().trim_end_matches('.').replace("**", "");
        let nom = nom.strip_prefix("Mme ").or_else(|| nom.strip_prefix("M. ")).unwrap_or(&nom).trim().to_string();
        let bas = nom.to_lowercase();
        let groupe = ["les ", "des ", "l'", "l’", "et ", "participant"].iter().any(|p| bas.starts_with(p));
        if nom.is_empty() || groupe || nom.starts_with('*') || nom.split_whitespace().count() > 4 || noms.contains(&nom) {
            continue;
        }
        noms.push(nom);
    }
    noms.iter().map(|n| format!("- {n}")).collect::<Vec<_>>().join("\n")
}

/// Corps des sections de l'étape 2 ; toute autre section (point répété malgré la consigne…)
/// est ignorée.
fn sections_de_fin(md: &str) -> Fin {
    let mut fin = Fin::default();
    let mut lignes: [Vec<&str>; 4] = Default::default();
    let mut courante = None;
    for ligne in md.lines() {
        if let Some(titre) = ligne.trim_start().strip_prefix("## ") {
            let t = titre.trim().to_lowercase();
            courante = if t.starts_with("présences") || t.starts_with("presences") {
                Some(0)
            } else if t.starts_with("sujets") {
                Some(1)
            } else if t.starts_with("prochaine") {
                Some(2)
            } else if t.starts_with("levée") || t.starts_with("levee") {
                Some(3)
            } else {
                None
            };
            continue;
        }
        if let Some(i) = courante {
            lignes[i].push(ligne);
        }
    }
    // Section « omise » ou vide malgré la consigne : « Cette section est omise car… », « Aucun. »
    let [presences, sujets, prochaine, levee] = lignes.map(|l| l.join("\n").trim().to_string()).map(|t| {
        let bas = t.to_lowercase();
        if bas.contains("omise") || bas.starts_with("aucun") || bas.starts_with("(aucun") { String::new() } else { t }
    });
    fin.presences = presences;
    fin.sujets = sujets;
    fin.prochaine = prochaine;
    fin.levee = levee;
    fin
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
fn condense(texte: &str, agenda: Option<&str>, precision: &str, rediger: &mut Redacteur) -> Result<String> {
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
        prompt.push_str(precision);
        prompt.push_str(STYLE);
        if let Some(agenda) = agenda {
            prompt.push_str("\n\nRattache chaque note au point de l'ordre du jour concerné :\n");
            prompt.push_str(agenda);
        }
        prompt.push_str("\n\n=== PASSAGE ===\n");
        prompt.push_str(&chunk);
        notes.push_str(&format!("\n--- Notes, partie {part} ---\n"));
        notes.push_str(&generate(rediger, &prompt, JETONS_NOTES)?);
        start = end;
        part += 1;
    }
    Ok(notes)
}

fn generate(rediger: &mut Redacteur, prompt: &str, max_jetons: usize) -> Result<String> {
    let texte = sans_gabarits(&rediger(prompt, max_jetons)?);
    Ok(tidy_answer(&crate::quebec::normalize_document(texte.trim())))
}

/// Un exemple des consignes recopié tel quel (« Mme [Nom] présente… ») : « la direction ».
fn sans_gabarits(texte: &str) -> String {
    let mut t = texte.to_string();
    for gabarit in ["Mme [Nom]", "M. [Nom]", "[Prénom Nom]", "[Nom]"] {
        t = t.replace(&format!("**{gabarit}**"), "**Participant(e)**");
        // En début de ligne ou de phrase : « La direction ».
        for debut in ["\n", ". ", "- ", "* "] {
            t = t.replace(&format!("{debut}{gabarit}"), &format!("{debut}La direction"));
        }
        if t.starts_with(gabarit) {
            t = format!("La direction{}", &t[gabarit.len()..]);
        }
        t = t.replace(gabarit, "la direction");
    }
    t
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
    gpu: bool,
}

impl Session {
    pub fn ouvrir(moteur: &Moteur) -> Result<Self> {
        if !moteur.modele.exists() {
            bail!("Le modèle de rédaction n'est pas installé : installez-le depuis la fiche de la rencontre");
        }
        let mut cmd = Command::new(helper()?);
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        if moteur.gpu {
            // Variable héritée du processus principal quand Whisper n'utilise pas la puce.
            cmd.env_remove("GGML_VK_VISIBLE_DEVICES");
        } else {
            // Aucune puce visible pour ggml-vulkan : tout reste sur le processeur.
            cmd.env("GGML_VK_VISIBLE_DEVICES", "");
        }
        #[cfg(windows)]
        {
            // Programme console : sans ce drapeau, une fenêtre noire apparaîtrait.
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = cmd.spawn().context("Lancement du programme de rédaction")?;
        let entree = child.stdin.take();
        let sortie = BufReader::new(child.stdout.take().expect("sortie redirigée")).lines();
        Ok(Self { child, entree, sortie, modele: moteur.modele.clone(), fils: moteur.fils, gpu: moteur.gpu })
    }

    /// Une requête. `annuler` arrête le programme dès sa prochaine ligne de sortie ; la session
    /// est alors inutilisable.
    pub fn rediger(&mut self, prompt: &str, max_jetons: usize, progress: Option<&AtomicUsize>, annuler: Option<&AtomicBool>) -> Result<String> {
        // Contexte de travail : invite (≈ 3 caractères par jeton en français) + réponse.
        // Plafonné à 12 k jetons pour garder la mémoire du contexte raisonnable.
        let contexte = (prompt.chars().count() / 3 + max_jetons + 256).clamp(4096, 12288);
        let requete = json!({
            "modele": self.modele,
            "invite": prompt,
            "contexte": contexte,
            "max_jetons": max_jetons,
            "temperature": 0.2,
            "fils": self.fils,
            "gpu": self.gpu,
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

impl Session {
    /// Le programme s'est arrêté (plantage, annulation) : la session est inutilisable.
    fn arrete(&self) -> bool {
        self.entree.is_none()
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
            // « Il est résolu de adopter » → « Il est résolu d'adopter ».
            let mut l = line.to_string();
            for v in ['a', 'e', 'é', 'è', 'ê', 'i', 'o', 'u', 'h'] {
                l = l.replace(&format!("résolu de {v}"), &format!("résolu d'{v}"));
            }
            l
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
            date: "30 octobre 2026",
            heure: "19 h 00",
            duration: "1:10:00",
            agenda: Some("1. Ouverture\n2. Tour de table\n3. Budget\n4. Varia"),
            transcript: "Budget de 2 500 $. Julie publie l'annonce.",
            points,
            hors_point: hors_point.to_string(),
            reprendre,
        }
    }

    fn moteur() -> Moteur {
        Moteur { modele: PathBuf::from("absent.gguf"), fils: 1, gpu: false }
    }

    /// Modèle simulé : répond selon le point demandé et note les invites reçues.
    fn simule(invites: &mut Vec<String>) -> impl FnMut(&str, usize) -> Result<String> + '_ {
        |invite: &str, max_jetons: usize| {
            // Chaque point est court ; seule l'ancienne rédaction d'un coup a droit à 3 072 jetons.
            assert!(max_jetons <= JETONS_TOUR_DE_TABLE, "{max_jetons}");
            invites.push(invite.to_string());
            Ok(if invite.contains("« 1. Ouverture »") {
                "## 1. Ouverture\nMme Tremblay souhaite la bienvenue et ouvre la rencontre.".into()
            } else if invite.contains("« 2. Tour de table »") {
                "**Julie Roy**\n- Prépare l'annonce.\n**Marc Côté**\n- Poursuit le dossier de la caisse.".into()
            } else if invite.contains("« 3. Budget »") {
                "Mme Tremblay présente le budget.\n- Budget de 2 500 $.\nIl est convenu que Mme Roy publie l'annonce.".into()
            } else {
                "## Présences\n- Julie Roy\n- Marc Côté\n## Sujets hors ordre du jour\nLe stationnement est discuté.\n\
                 ## 1. Ouverture\nRépété.\n## Levée de la séance\nLa rencontre se termine dans un climat positif."
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
            p("1. Ouverture", Some("On ouvre à 19 h."), "01-ouverture"),
            p("2. Tour de table", Some("Moi, Julie, je prépare l'annonce. Marc, la caisse."), "02-tour-de-table"),
            p("3. Budget", Some("Le budget est de 2 500 $. Julie publie l'annonce."), "03-budget"),
            p("4. Varia", None, "04-varia"),
        ]
    }

    #[test]
    fn compte_rendu_par_point() {
        let moteur = moteur();
        let r = requete(&moteur, points(None), "Le stationnement est plein.", false);
        let mut invites = Vec::new();
        let doc = rediger_compte_rendu(&r, &mut simule(&mut invites)).unwrap();
        // Un appel par point abordé, puis l'assemblage ; le point non abordé n'appelle pas le modèle.
        assert_eq!(invites.len(), 4);
        assert!(invites[0].contains("=== TRANSCRIPTION DU POINT ===\nOn ouvre à 19 h.") && !invites[0].contains("budget"));
        // Forme propre au tour de table, forme ordinaire ailleurs.
        assert!(invites[1].contains("en gras sur une ligne") && !invites[2].contains("en gras sur une ligne"));
        assert!(invites[2].contains("phrase d'introduction") && invites[2].contains("Il est convenu"));
        assert!(invites[3].contains("=== POINTS RÉDIGÉS ===") && invites[3].contains("stationnement est plein"));
        // L'ordre du jour n'a pas de point « Prochaine rencontre » : l'assemblage la demande.
        assert!(invites[3].contains("## Prochaine rencontre"));

        let ordre: Vec<usize> = [
            "# Compte rendu",
            "**CA mensuel**",
            "## Présences",
            "## 1. Ouverture",
            "## 2. Tour de table",
            "## 3. Budget",
            "## 4. Varia",
            "## Sujets hors",
            "## Levée de la séance",
        ]
        .iter()
        .map(|t| doc.find(t).unwrap_or_else(|| panic!("{t} absent :\n{doc}")))
        .collect();
        assert!(ordre.windows(2).all(|w| w[0] < w[1]), "{doc}");
        assert!(doc.contains("**Date** : 30 octobre 2026 · **Heure** : 19 h 00"), "{doc}");
        assert!(doc.contains("- Julie Roy\n- Marc Côté\n\n*Liste établie"), "{doc}");
        assert!(doc.contains("## 2. Tour de table\n\n**Julie Roy**\n- Prépare l'annonce."), "{doc}");
        assert!(doc.contains("## 4. Varia\n\nPoint non abordé ou reporté."));
        // Le titre répété par le modèle est retiré, le point répété à l'assemblage aussi.
        assert_eq!(doc.matches("1. Ouverture").count(), 1, "{doc}");
        assert!(!doc.contains("Répété") && !doc.contains('|'), "{doc}");
        assert!(doc.ends_with("## Levée de la séance\n\nLa rencontre se termine dans un climat positif."));
        assert!(!doc.contains("À vérifier"), "{doc}");
    }

    #[test]
    fn point_prochaine_rencontre_dans_l_ordre_du_jour() {
        let moteur = moteur();
        let mut p = points(None);
        p[3] = PointCr { titre: "4. Prochaine rencontre".into(), transcription: Some("On se revoit après les Fêtes.".into()), resume: None };
        let mut invites = Vec::new();
        rediger_compte_rendu(&requete(&moteur, p, "", false), &mut simule(&mut invites)).unwrap();
        assert!(invites[3].contains("1 à 3 puces seulement"), "{}", invites[3]);
        assert!(!invites.last().unwrap().contains("## Prochaine rencontre"));
    }

    #[test]
    fn reprise_d_un_compte_rendu_interrompu() {
        let dir = std::env::temp_dir().join(format!("tourdetable-test-{}-cr", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let moteur = moteur();
        let mut invites = Vec::new();
        rediger_compte_rendu(&requete(&moteur, points(Some(&dir)), "", true), &mut simule(&mut invites)).unwrap();
        assert_eq!(invites.len(), 4);
        let resume = std::fs::read_to_string(dir.join("03-budget/resume.md")).unwrap();
        assert!(resume.starts_with("<!-- tourdetable ") && resume.contains("- Budget de 2 500 $."));
        assert!(!dir.join("04-varia/resume.md").exists());

        // Reprise : seul l'assemblage est refait.
        invites.clear();
        let doc = rediger_compte_rendu(&requete(&moteur, points(Some(&dir)), "", true), &mut simule(&mut invites)).unwrap();
        assert_eq!(invites.len(), 1, "{invites:?}");
        assert!(doc.contains("Il est convenu que Mme Roy publie l'annonce."));

        // Transcription du point modifiée (nettoyage relancé) : il est rédigé à nouveau.
        let mut p = points(Some(&dir));
        p[2].transcription = Some("Budget adopté.".into());
        invites.clear();
        rediger_compte_rendu(&requete(&moteur, p, "", true), &mut simule(&mut invites)).unwrap();
        assert_eq!(invites.len(), 2);
        // Résumé d'une forme antérieure (consignes différentes) : refait.
        let ancien = std::fs::read_to_string(dir.join("01-ouverture/resume.md")).unwrap();
        let corps = ancien.split_once('\n').unwrap().1;
        let entete_ancienne = format!("<!-- tourdetable {} -->", empreinte("1. Ouverture\nOn ouvre à 19 h."));
        std::fs::write(dir.join("01-ouverture/resume.md"), format!("{entete_ancienne}\n{corps}")).unwrap();
        invites.clear();
        rediger_compte_rendu(&requete(&moteur, points(Some(&dir)), "", true), &mut simule(&mut invites)).unwrap();
        assert_eq!(invites.len(), 3, "{invites:?}");
        // « Régénérer » : tout est refait.
        invites.clear();
        rediger_compte_rendu(&requete(&moteur, points(Some(&dir)), "", false), &mut simule(&mut invites)).unwrap();
        assert_eq!(invites.len(), 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gabarits_recopies() {
        assert_eq!(
            sans_gabarits("Mme [Nom] présente le point. Il est convenu que Mme [Nom] assure le suivi.\n**[Prénom Nom]**\n- Prépare."),
            "La direction présente le point. Il est convenu que la direction assure le suivi.\n**Participant(e)**\n- Prépare."
        );
    }

    #[test]
    fn repetitions_retirees() {
        let anonymes = "**Participant(e)**\n- Prépare A.\n**Participant(e)**\n- Prépare B.";
        assert_eq!(sans_repetitions(anonymes), anonymes);
        let md = "**Sophie**\n- Finalise le schéma.\n\n**Karim**\n- Prépare les budgets.\n\n**Sophie**\n- Autre chose.\n\n**Marc**\n- Prépare les budgets.\n- Suit le dossier.";
        assert_eq!(sans_repetitions(md), "**Sophie**\n- Finalise le schéma.\n\n**Karim**\n- Prépare les budgets.\n\n**Marc**\n- Suit le dossier.");
    }

    #[test]
    fn presences_et_sections_vides() {
        let texte = "Anna, Marc Tremblay, les comptables, Mme Bérubé, Anna.\n- **Sophie Gagnon**\n- Les directeurs";
        assert_eq!(liste_des_presences(texte), "- Anna\n- Marc Tremblay\n- Bérubé\n- Sophie Gagnon");
        let fin = sections_de_fin("## Sujets hors ordre du jour\nCette section est omise car rien.\n## Levée de la séance\nFin.");
        assert_eq!((fin.sujets.as_str(), fin.levee.as_str()), ("", "Fin."));
        assert_eq!(date_et_heure_de("Page 1 sur 1\n\nRENCONTRE\n16  AVR IL     2026, 10 h30").0.as_deref(), Some("16 avril 2026"));
    }

    #[test]
    fn rencontre_sans_decoupage() {
        let moteur = moteur();
        let mut invites = Vec::new();
        let mut appel = |i: &str, max_jetons: usize| {
            assert_eq!(max_jetons, JETONS_DOCUMENT);
            invites.push(i.to_string());
            Ok("# Compte rendu\n## Budget\nCoût de 300 $.".to_string())
        };
        let doc = rediger_compte_rendu(&requete(&moteur, Vec::new(), "", true), &mut appel).unwrap();
        assert_eq!(invites.len(), 1);
        assert!(invites[0].contains("en suivant l'ordre du jour fourni") && invites[0].contains("=== TRANSCRIPTION ===\nBudget de 2 500 $"));
        assert!(invites[0].contains("**Date** : 30 octobre 2026 · **Heure** : 19 h 00") && invites[0].contains("au présent"));
        // Garde-fou des chiffres conservé.
        assert!(doc.contains("> **À vérifier**") && doc.contains("300"), "{doc}");
    }

    #[test]
    fn date_et_heure_de_l_ordre_du_jour() {
        let od = "RENCONTRE ÉQUIPE ADMNISTRATIF\n8 OCTOBRE 2026, 11h00\nORDRE DU JOUR\n1. Mot de bienvenue";
        assert_eq!(date_et_heure_de(od), (Some("8 octobre 2026".into()), Some("11 h 00".into())));
        assert_eq!(date_et_heure_de("CA du 1er décembre 2026 à 19 h 30"), (Some("1er décembre 2026".into()), Some("19 h 30".into())));
        assert_eq!(date_et_heure_de("Réunion\n1. Achat de 4 chaises"), (None, None));
        assert_eq!(date_et_heure_de("16 AVRIL 2026, 10 h30"), (Some("16 avril 2026".into()), Some("10 h 30".into())));
        let moteur = moteur();
        let mut r = requete(&moteur, Vec::new(), "", false);
        assert_eq!(date_et_heure(&r), ("30 octobre 2026".into(), "19 h 00".into()));
        r.agenda = Some(od);
        assert_eq!(date_et_heure(&r), ("8 octobre 2026".into(), "11 h 00".into()));
        let d = chrono::TimeZone::with_ymd_and_hms(&chrono::Local, 2026, 10, 8, 9, 5, 0).unwrap();
        assert_eq!((date_longue(&d), heure(&d)), ("8 octobre 2026".into(), "9 h 05".into()));
    }

    #[test]
    fn formes_et_sections() {
        assert_eq!(Forme::du_point("2. Tour de table"), Forme::TourDeTable);
        assert_eq!(Forme::du_point("10. Varia"), Forme::Varia);
        assert_eq!(Forme::du_point("11. Prochaine rencontre"), Forme::Prochaine);
        assert_eq!(Forme::du_point("8. Télétravail"), Forme::Ordinaire);
        let fin = sections_de_fin("Préambule\n## Présences\n- A\n## Autre\nB\n## Levée de la séance\nC");
        assert_eq!(fin, Fin { presences: "- A".into(), levee: "C".into(), ..Fin::default() });
        assert_eq!(sections_de_fin("Rien à signaler."), Fin::default());
        assert_eq!(tidy_answer("Il est résolu de adopter le budget."), "Il est résolu d'adopter le budget.");
        assert_eq!(tidy_answer("Il est résolu de reporter."), "Il est résolu de reporter.");
    }

    #[test]
    fn retire_les_aucun_parasites() {
        assert_eq!(tidy_answer("**Suivis** : Marc publie l'annonce. Aucun"), "**Suivis** : Marc publie l'annonce.");
        assert_eq!(tidy_answer("**Suivis** : Aucun"), "**Suivis** : Aucun");
        assert_eq!(tidy_answer("**Décisions** : Aucune"), "**Décisions** : Aucune");
        assert_eq!(tidy_answer("**Décisions** : Aucune."), "**Décisions** : Aucune.");
    }
}
