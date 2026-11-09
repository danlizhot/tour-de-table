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

use crate::repartition;

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
## Suites à donner\n\
Regroupe les suites à donner par responsable : un titre de niveau 3 par personne ou organisation \
(« ### Actions de [nom] »), puis une liste à puces d'actions courtes qui commencent par un verbe à l'infinitif, \
avec l'échéance entre parenthèses si elle a été dite. Sans responsable connu : « ### Actions à préciser ». \
Aucun tableau. « Salle » et « À distance » désignent des micros, jamais des personnes : ne les écris pas.\n";

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
- « Point non abordé ou reporté. » remplace tout le contenu d'un point : ne l'ajoute jamais sous un point que tu as résumé.\n\
- Présences : une liste à puces, prénom et nom, des personnes qui ont participé, jamais un groupe.\n\
- L'ordre du jour sert uniquement à structurer : les faits (lieu, heure, montants, noms) viennent \
de la transcription seulement. Ne recopie pas le lieu ou l'heure de cette rencontre pour la prochaine.\n\
Structure du document :\n\
# Compte rendu\n\
**{titre}**\n\
**Date** : {date}{heure}\n\
## Présences\n\
## 1. … (chaque point de l'ordre du jour)\n\
## Levée de la séance\n\
La levée de la séance est un court paragraphe qui résume l'esprit de la rencontre.\n";

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
/// Point de procédure (adoption) : une ou deux phrases.
const JETONS_SANS_SUJET: usize = 150;
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
    /// Termes du Lexique des paramètres (noms de l'organisation, sigles).
    pub lexique: Vec<String>,
}

/// Chaque point est rédigé avec toute la transcription (méthode par défaut) ; TOURDETABLE_DECOUPE
/// revient à l'ancienne méthode (la transcription du point seulement), pour comparer.
pub fn transcription_complete() -> bool {
    std::env::var_os("TOURDETABLE_DECOUPE").is_none()
}

/// Points et échanges hors point d'une rencontre découpée par point (vide sinon), à partir de
/// la transcription nettoyée quand elle existe.
pub fn points_de(m: &crate::storage::Meeting) -> (Vec<PointCr>, String) {
    // La transcription complète n'a pas besoin du découpage (repère seulement).
    let complet = transcription_complete();
    let Some(agenda) = m.agenda.as_ref().filter(|a| !a.points.is_empty() && (complet || !m.periodes.is_empty())) else {
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
    rediger_compte_rendu(r, &mut rediger, transcription_complete())
}

/// `complet` : chaque point rédigé avec la transcription complète, sinon avec la sienne seulement.
fn rediger_compte_rendu(r: &Request, rediger: &mut Redacteur, complet: bool) -> Result<String> {
    let mut doc = if r.points.is_empty() || r.agenda.is_none() {
        en_un_appel(r, rediger)?
    } else if complet {
        par_point_complet(r, rediger)?
    } else {
        par_point(r, rediger)?
    };

    // Garde-fou : un modèle local peut se tromper dans un calcul ou inventer un
    // chiffre. On signale tout nombre absent des sources.
    let sources = format!("{} {} {} {} {}", r.agenda.unwrap_or(""), r.transcript, r.hors_point, r.date, r.duration);
    let suspects = unverified_numbers(&doc, &sources);
    if !suspects.is_empty() {
        doc.push_str(&a_verifier(&suspects));
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
    Ok(sans_consignes_recopiees(&generate(rediger, &prompt, JETONS_DOCUMENT)?))
}

/// Ce que le modèle recopie des consignes : précision entre parenthèses dans un titre
/// (« ## Présences (liste à puces…) »), mais pas « (information) » de l'ordre du jour ; « Point non abordé ou reporté. » sous un point résumé.
/// Une seule phrase avec la formule (« Mentionné, mais non discuté. ») : la formule seule.
fn sans_consignes_recopiees(md: &str) -> String {
    let mut sections: Vec<(String, Vec<&str>)> = vec![(String::new(), Vec::new())];
    for ligne in md.lines() {
        if let Some(titre) = ligne.strip_prefix("## ") {
            let titre = match titre.split_once(" (") {
                Some((debut, fin)) if ["liste à puces", "paragraphe", "prénom et nom", "jamais un groupe"].iter().any(|c| fin.contains(c)) => debut,
                _ => titre,
            };
            sections.push((format!("## {}", titre.trim_end()), Vec::new()));
        } else {
            sections.last_mut().expect("au moins une").1.push(ligne);
        }
    }
    let formule = |l: &str| l.trim().trim_start_matches(['-', '*', '•']).trim() == NON_ABORDE;
    let mut out = Vec::new();
    for (titre, lignes) in &sections {
        if !titre.is_empty() {
            out.push(titre.clone());
        }
        let contenu: Vec<&str> = lignes.iter().copied().filter(|l| !l.trim().is_empty() && !formule(l)).collect();
        let avec_formule = lignes.iter().any(|l| formule(l));
        let resume = contenu.len() >= 2 || contenu.iter().any(|l| l.trim_start().starts_with(['-', '*', '•']));
        if avec_formule && !resume {
            out.push(NON_ABORDE.to_string());
        } else {
            out.extend(lignes.iter().filter(|l| !formule(l)).map(|l| l.to_string()));
        }
    }
    out.join("\n")
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

/// Sépare le préfixe commun (lu une seule fois par le programme de rédaction) de la consigne.
const SEPARATEUR_PREFIXE: char = '\u{1}';

const POINT_COMPLET: &str = "Tu es secrétaire de réunion. Ci-dessus : l'ordre du jour et la transcription complète \
de la rencontre. Rédige uniquement le point « {point} » de l'ordre du jour, sans titre.\n\
Les marques « — titre — » de la transcription viennent d'un découpage automatique et peuvent se tromper : un point \
peut avoir été discuté sans être annoncé, avant ou après sa marque, ou en plusieurs fois. Cherche dans toute la \
transcription ce qui se rapporte à ce point. Écris « Point non abordé ou reporté. » seulement si rien ne s'y rapporte.\n\
Les autres points de l'ordre du jour sont rédigés à part : {autres}. Laisse-leur ce qui les concerne, même si on en \
parle pendant ce point-ci. Ignore les conversations d'avant le début et d'après la fin de la rencontre.\n\
Garde les détails utiles : dates, échéances, chiffres, noms de dossiers, explications et raisons, décisions et suivis. \
N'additionne pas de montants : un total n'apparaît que s'il est dit.\n";

/// Avec le Lexique : les noms complets viennent de la liste, jamais d'une invention.
const NOMS_DU_LEXIQUE: &str = "Pour nommer une personne, prends son nom complet dans la liste « Noms et termes de \
l'organisation » quand le prénom ou le nom entendu y correspond, même mal transcrit. N'invente aucun nom de famille. \
Une personne de la liste qui n'est pas entendue dans la transcription n'était pas à la rencontre.\n";

/// Présences dont le prénom ou le nom est entendu dans la transcription ; les autres sont retirées.
fn presences_entendues(presences: &str, transcription: &str) -> String {
    let bas = transcription.to_lowercase();
    presences
        .lines()
        .filter(|l| {
            let nom = l.trim().trim_start_matches(['-', '*', '•']).replace("**", "");
            let mots: Vec<&str> = nom.split_whitespace().filter(|m| m.chars().count() > 2).collect();
            // Marque de la reconnaissance des voix (« Voix 3 (Participant) ») : pas une personne.
            let voix = nom.trim().strip_prefix("Voix ").is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()));
            !voix && (mots.is_empty() || mots.iter().any(|m| bas.contains(&m.to_lowercase())))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Mot de bienvenue : rarement plus qu'une ouverture.
const POINT_BIENVENUE: &str = "Forme : une ou deux phrases seulement (« La direction souhaite la bienvenue aux membres \
de l'équipe et ouvre la rencontre. »), plus une phrase sur une annonce faite à l'ouverture ou sur un sujet de ce point \
s'il y en a. Ne résume pas le reste de la rencontre. Pas de liste.\n";

/// Essai : explication des marques de voix au début de chaque passage.
const VOIX_MARQUEES: &str = "=== QUI PARLE ===\nChaque passage commence par la voix reconnue automatiquement : un prénom \
(« Linda : ») quand la voix a pu être nommée, sinon un numéro (« Voix 4 : »). « Linda, puis Voix 4 : » veut dire que \
deux personnes parlent dans ce passage. Sers-t'en pour savoir qui dit quoi, surtout au tour de table. Une « Voix N » \
garde son anonymat : n'invente pas son nom, sauf si la transcription le dit clairement. Ne recopie jamais ces marques \
dans le compte rendu.\n\n";

/// Première ligne qui recopie le numéro du point (« 2. Adoption de l'ordre du jour »), retirée.
fn sans_titre_recopie(reponse: &str, titre: &str) -> String {
    let numero = titre.split_whitespace().next().filter(|n| n.ends_with('.') && n.starts_with(|c: char| c.is_ascii_digit()));
    let texte = reponse.trim_start();
    match (numero, texte.split_once('\n')) {
        (Some(n), Some((premiere, reste))) if premiere.trim().starts_with(n) && premiere.chars().count() < 100 => reste.to_string(),
        _ => reponse.to_string(),
    }
}

/// Adoption de l'ordre du jour ou d'un compte rendu : une ou deux phrases.
const POINT_ADOPTION: &str = "Forme : une ou deux phrases seulement sur ce qui est adopté sous ce point-ci, tel que le \
nomme son titre (« Le compte rendu du 3 mars est adopté sans modification. »), plus une phrase pour une modification \
demandée s'il y en a une. Ne résume pas le reste de la rencontre. Pas de liste.\n";

fn est_adoption(titre: &str) -> bool {
    titre.to_lowercase().contains("adoption")
}

/// Fin de la rencontre : une phrase (la levée de la séance a sa propre section).
const POINT_FIN: &str = "Forme : une seule phrase sur la façon dont la rencontre se termine (« La rencontre prend fin \
après le dernier point. »), avec l'heure seulement si elle est dite. Ne résume pas la rencontre. Pas de liste.\n";

fn est_fin(titre: &str) -> bool {
    let t = titre.to_lowercase();
    ["fin de la rencontre", "fin de la réunion", "fin de la séance", "levée", "clôture"].iter().any(|m| t.contains(m))
}

/// Numéro en tête d'un intertitre (« 2.1 » pour « 2.1. Budget »).
fn numero_du_titre(titre: &str) -> Option<&str> {
    let n = titre.split_whitespace().next()?.trim_end_matches('.');
    (!n.is_empty() && n.chars().all(|c| c.is_ascii_digit() || c == '.')).then_some(n)
}

/// Le point `i` est un sous-point du point `j` (« 2. », puis « 2.3. »).
fn sous_point(titres: &[&str], j: usize, i: usize) -> bool {
    match (numero_du_titre(titres[j]), titres.get(i).and_then(|t| numero_du_titre(t))) {
        (Some(n), Some(m)) => m.strip_prefix(n).is_some_and(|r| r.starts_with('.')),
        _ => false,
    }
}

/// Le point suivant est un sous-point de celui-ci (« 2. », puis « 2.1. »).
fn a_des_sous_points(titres: &[&str], i: usize) -> bool {
    sous_point(titres, i, i + 1)
}

/// Au plus `n` phrases complètes : une réponse coupée par la limite de jetons ne finit pas au milieu d'une phrase.
fn phrases(texte: &str, n: usize) -> String {
    let mut fin = 0;
    let mut compte = 0;
    for (i, c) in texte.char_indices() {
        if matches!(c, '.' | '!' | '?') && texte[i + 1..].chars().next().is_none_or(char::is_whitespace) {
            fin = i + 1;
            compte += 1;
            if compte == n {
                break;
            }
        }
    }
    if fin == 0 { texte.trim().to_string() } else { texte[..fin].trim().to_string() }
}

/// Varia de la transcription complète : seulement ce qui ne relève d'aucun autre point.
const VARIA_COMPLET: &str = "Le varia ne contient que les sujets qui ne relèvent d'aucun autre point de l'ordre du jour.\n";

/// Sujets placés sous les autres points (voir `repartition`) : un sujet n'est pas rédigé deux fois.
const AUTRES_POINTS: &str = "Sujets rédigés sous d'autres points : ne les rédige pas ici, même s'ils sont discutés pendant ce point-ci.\n";

/// Consigne d'un point : ses sujets, et ceux des autres points à leur laisser.
fn consigne_repartition(sujets: &[Vec<String>], titres: &[&str], i: usize) -> String {
    let liste = |v: &[String]| v.iter().map(|s| format!("- {s}\n")).collect::<String>();
    let mut c = if sujets[i].is_empty() {
        "La répartition faite d'avance n'a rangé aucun sujet sous ce point ; elle a pu en oublier. Cherche toi-même dans \
         la transcription ce qui s'y rapporte, sans reprendre les sujets rangés sous les autres points. Écris « Point non \
         abordé ou reporté. » si rien ne s'y rapporte.\n"
            .to_string()
    } else {
        format!("Sujets de ce point, d'après la répartition faite d'avance (rédige-les tous, en détail, avec ce qu'en dit la transcription) :\n{}\
N'ajoute aucun sujet rangé sous un autre point (liste ci-dessous), même discuté pendant ce point-ci. Un autre sujet n'est ajouté que s'il relève clairement de ce point et ne figure dans aucune liste.\n", liste(&sujets[i]))
    };
    let autres: String = (0..sujets.len())
        .filter(|&j| j != i && !sujets[j].is_empty())
        .map(|j| format!("« {} » :\n{}", titres[j], liste(&sujets[j])))
        .collect();
    if !autres.is_empty() {
        c.push_str(AUTRES_POINTS);
        c.push_str(&autres);
    }
    c
}

/// Chaque point est rédigé avec toute la transcription et l'ordre du jour sous les yeux, après une
/// répartition des sujets entre les points ; le découpage n'est plus qu'un repère.
fn par_point_complet(r: &Request, rediger: &mut Redacteur) -> Result<String> {
    // Ni sources (« Salle » finirait en responsable d'un suivi).
    let transcription: Vec<String> = r
        .transcript
        .lines()
        .map(|l| {
            ["] Salle : ", "] À distance : ", "] Réunion : "].iter().find_map(|s| l.split_once(s).map(|(t, x)| format!("{t}] {x}"))).unwrap_or_else(|| l.to_string())
        })
        .collect();
    let lexique = if r.lexique.is_empty() {
        String::new()
    } else {
        format!("=== NOMS ET TERMES DE L'ORGANISATION ===\n{}\n\n", r.lexique.join("\n"))
    };
    // Essai : transcription marquée par la reconnaissance des voix (« Linda : », « Voix 4 : »).
    let voix = if std::env::var_os("TOURDETABLE_VOIX").is_some() { VOIX_MARQUEES } else { "" };
    let prefixe = format!(
        "=== ORDRE DU JOUR ===\n{}\n\n{lexique}{voix}=== TRANSCRIPTION COMPLÈTE ===\n{}\n=== FIN DE LA TRANSCRIPTION ===\n\n{SEPARATEUR_PREFIXE}",
        r.agenda.unwrap_or(""),
        transcription.join("\n")
    );
    let noms = if r.lexique.is_empty() { "" } else { NOMS_DU_LEXIQUE };
    let n = r.points.len();
    let titres: Vec<&str> = r.points.iter().map(|p| p.titre.as_str()).collect();
    // Point qui a des sous-points (« 2. Filière forestière », puis « 2.1. ») : intertitre seulement, rien n'y est rangé.
    let parents: Vec<bool> = (0..n).map(|i| a_des_sous_points(&titres, i)).collect();
    // Points rédigés, avec leur point parent pour contexte.
    let rediges: Vec<usize> = (0..n).filter(|&i| !parents[i]).collect();
    let libelles: Vec<String> = (0..n)
        .map(|i| match (0..i).rev().find(|&j| parents[j]).filter(|&j| sous_point(&titres, j, i)) {
            Some(j) => format!("{} (sous « {} »)", titres[i], titres[j]),
            None => titres[i].to_string(),
        })
        .collect();
    let libelles: Vec<&str> = libelles.iter().map(String::as_str).collect();
    // Placement des sujets (voir `repartition`) : liste datée, une question par sujet, contrôle de l'ordre.
    let numeros_tous: Vec<&str> = titres.iter().map(|t| numero_du_titre(t).unwrap_or("")).collect();
    let mut prompt = prefixe.clone();
    prompt.push_str(repartition::LISTE_SUJETS);
    prompt.push_str(noms);
    let reponse = generate(rediger, &prompt, repartition::JETONS_LISTE)?;
    let mut tous = repartition::sujets(&reponse);
    log::info!("Liste des sujets : {}\n{reponse}", tous.len());
    // Un sujet peut aller sous tout point rédigé, sauf la levée de la séance.
    let choix: Vec<usize> = rediges.iter().copied().filter(|&i| !est_fin(titres[i])).collect();
    // La consigne et la liste des points, communes à toutes les questions, sont gardées avec le préfixe.
    let prefixe_questions = format!(
        "{}{}{SEPARATEUR_PREFIXE}",
        prefixe.trim_end_matches(SEPARATEUR_PREFIXE),
        repartition::entete_questions(&libelles, &choix)
    );
    let demander = |s: &repartition::Sujet, entre: Option<(usize, usize)>, rediger: &mut Redacteur| -> Result<Option<usize>> {
        let mut prompt = prefixe_questions.clone();
        prompt.push_str(&repartition::question(s, entre, &libelles));
        let reponse = generate(rediger, &prompt, repartition::JETONS_QUESTION)?;
        let point = repartition::point_choisi(&reponse, &numeros_tous, &choix);
        log::info!("Sujet « {} » → {reponse}{}", s.texte, entre.map_or(String::new(), |(a, b)| format!(" (entre « {} » et « {} »)", titres[a], titres[b])));
        Ok(point)
    };
    for s in &mut tous {
        s.point = demander(s, None, rediger)?;
    }
    for (k, entre) in repartition::hors_suite(&tous).into_iter().take(repartition::MAX_REPRISES) {
        if let Some(p) = demander(&tous[k], Some(entre), rediger)? {
            tous[k].point = Some(p);
        }
    }
    let mut sujets = vec![Vec::new(); n];
    for s in &tous {
        if let Some(p) = s.point {
            sujets[p].push(s.texte.clone());
        }
    }
    // Réponse inutilisable : chaque point cherche lui-même ce qui le concerne, comme avant.
    let reparti = sujets.iter().any(|v| !v.is_empty());

    // Point sans sujet : une question ciblée, plutôt qu'une recherche qui reprendrait les sujets des autres
    // ou inventerait une date.
    let mut non_abordes = vec![false; n];
    if reparti {
        for &i in &choix {
            let bienvenue = ["bienvenue", "ouverture"].iter().any(|m| titres[i].to_lowercase().contains(m));
            if !sujets[i].is_empty() || bienvenue || est_adoption(titres[i]) || Forme::du_point(titres[i]) == Forme::Varia {
                continue;
            }
            let autres: Vec<(&str, Vec<String>)> = (0..n).filter(|&j| j != i).map(|j| (titres[j], sujets[j].clone())).collect();
            let mut prompt = prefixe.clone();
            prompt.push_str(&repartition::consigne_point_vide(titres[i], &autres));
            let reponse = generate(rediger, &prompt, repartition::JETONS_POINT_VIDE)?;
            log::info!("Point sans sujet « {} » : {reponse}", titres[i]);
            sujets[i] = repartition::sujets_du_point_vide(&reponse);
            non_abordes[i] = sujets[i].is_empty();
        }
    }
    let mut sections = Vec::with_capacity(n);
    for (i, p) in r.points.iter().enumerate() {
        if let Some(a) = r.avancement {
            a.points.store(n, Ordering::Relaxed);
            a.point.store(i + 1, Ordering::Relaxed);
        }
        if parents[i] {
            sections.push(format!("## {}", p.titre));
            continue;
        }
        if non_abordes[i] {
            sections.push(format!("## {}\n\n{NON_ABORDE}", p.titre));
            continue;
        }
        let forme = Forme::du_point(&p.titre);
        // Varia sans sujet réparti : cherché par le modèle, il reprendrait toute la rencontre.
        if forme == Forme::Varia && reparti && sujets[i].is_empty() {
            sections.push(format!("## {}\n\nAucun sujet.", p.titre));
            continue;
        }
        let consigne = forme
            .consigne()
            .replace("de 3 à 6 éléments", "de 3 à 10 éléments")
            .replace("150 mots au plus", "400 mots au plus")
            .replace("200 mots au plus", "400 mots au plus")
            .replace("2 à 4 puces courtes", "2 à 5 puces")
            .replace("50 mots au plus par personne", "80 mots au plus par personne");
        let autres: Vec<String> = r.points.iter().filter(|q| q.titre != p.titre).map(|q| format!("« {} »", q.titre)).collect();
        let bienvenue = ["bienvenue", "ouverture"].iter().any(|m| p.titre.to_lowercase().contains(m));
        let mut prompt = prefixe.clone();
        prompt.push_str(&POINT_COMPLET.replace("{point}", &p.titre).replace("{autres}", &autres.join(", ")));
        let adoption = est_adoption(&p.titre);
        let fin = est_fin(&p.titre);
        if reparti {
            // Mot de bienvenue ou adoption sans sujet réparti : seulement la liste des sujets des autres points. La
            // consigne d'un point vide (chercher soi-même, sinon « non abordé ») donnerait un résumé de toute la
            // rencontre, ou une adoption dite « non abordée » parce qu'elle tient en une phrase.
            let consigne = consigne_repartition(&sujets, &titres, i);
            let consigne = match consigne.split_once(AUTRES_POINTS) {
                _ if !(bienvenue || adoption) || !sujets[i].is_empty() => consigne,
                Some((_, autres)) => format!("{AUTRES_POINTS}{autres}"),
                None => String::new(),
            };
            prompt.push_str(&consigne);
        }
        prompt.push_str(noms);
        if bienvenue {
            prompt.push_str(POINT_BIENVENUE);
        } else if adoption {
            prompt.push_str(POINT_ADOPTION);
        } else if fin {
            prompt.push_str(POINT_FIN);
        } else {
            prompt.push_str(&consigne);
        }
        if forme == Forme::Varia {
            prompt.push_str(VARIA_COMPLET);
        }
        prompt.push_str(FORME);
        prompt.push_str(STYLE);
        let jetons = if forme == Forme::TourDeTable { 2 * JETONS_TOUR_DE_TABLE } else { 2 * JETONS_POINT };
        // Point sans sujet : une ou deux phrases au plus, jamais un résumé de toute la rencontre.
        let court = adoption || fin || bienvenue;
        let jetons = if court { JETONS_SANS_SUJET } else { jetons };
        let reponse = generate(rediger, &prompt, jetons)?;
        // Titre recopié en texte simple (« 5. Horaire des Fêtes ») : retiré comme un titre Markdown.
        let reponse = reponse.trim_start().strip_prefix(p.titre.as_str()).unwrap_or(&reponse).to_string();
        // Titre recopié autrement (« 6. Suivi des tâches à effectuer » pour « 6. Suivi tâches à effectuer »).
        let reponse = sans_titre_recopie(&reponse, &p.titre);
        let corps = sans_repetitions(&sans_titre(&reponse));
        // Sous-titre « ## Site web » dans un point : il deviendrait un point de l'ordre du jour.
        let corps = corps
            .lines()
            .map(|l| l.trim_start().strip_prefix('#').map_or_else(|| l.to_string(), |t| format!("**{}**", t.trim_start_matches('#').trim())))
            // « **6. Affaires nouvelles : Aires protégées** » : le titre du point n'est pas répété.
            .map(|l| match l.strip_prefix(&format!("**{} : ", p.titre)) {
                Some(reste) => format!("**{reste}"),
                None => l,
            })
            .filter(|l| l.trim() != format!("**{}**", p.titre))
            .collect::<Vec<_>>()
            .join("\n");
        let corps = if corps.contains(NON_ABORDE.trim_end_matches('.')) { NON_ABORDE.to_string() } else { corps };
        let corps = if court { phrases(&corps, 3) } else { corps };
        log::info!("Compte rendu complet : « {} » rédigé", p.titre);
        sections.push(format!("## {}\n\n{corps}", p.titre));
    }
    let sections = sections.join("\n\n");
    let point_prochaine = titres.iter().any(|t| est_prochaine_rencontre(t));
    let mut prompt = prefixe.clone();
    let assemblage = ASSEMBLAGE.split("## Sujets hors ordre du jour").next().unwrap_or(ASSEMBLAGE);
    prompt.push_str(&assemblage.replace("{prochaine}", if point_prochaine { "" } else { ASSEMBLAGE_PROCHAINE }));
    prompt.push_str("Ne répète pas les points déjà rédigés. N'indique l'heure de la fin que si elle est dite dans la transcription.\n");
    prompt.push_str(noms);
    prompt.push_str(FORME);
    prompt.push_str(STYLE);
    prompt.push_str("\n\n=== POINTS RÉDIGÉS ===\n");
    prompt.push_str(&sections);
    // Sans sujets hors ordre du jour : chaque point a déjà cherché dans toute la transcription.
    let fin = sections_de_fin(&generate(rediger, &prompt, JETONS_ASSEMBLAGE)?);
    let fin = Fin { sujets: String::new(), presences: presences_entendues(&fin.presences, r.transcript), ..fin };
    Ok(document(r, &sections, point_prochaine, &fin))
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
    Ok(document(r, sections, point_prochaine, &fin))
}

fn document(r: &Request, sections: &str, point_prochaine: bool, fin: &Fin) -> String {
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
    doc.trim_end().to_string()
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
    // « Marco (Voix 1, 6, 9) » : la parenthèse est retirée avant de couper aux virgules.
    let texte: String = texte.lines().map(sans_voix_entre_parentheses).collect::<Vec<_>>().join("\n");
    for brut in texte.lines().flat_map(|l| l.split([',', ';'])) {
        let nom = brut.trim().trim_start_matches(['-', '*', '•']).trim().trim_end_matches('.').replace("**", "");
        let nom = nom.strip_prefix("Mme ").or_else(|| nom.strip_prefix("M. ")).unwrap_or(&nom).trim().to_string();
        // « Annie (Annie) » : la parenthèse répète le nom.
        let nom = match nom.split_once(" (") {
            Some((avant, apres)) if avant.contains(apres.trim_end_matches(')').trim()) => avant.trim().to_string(),
            _ => nom,
        };
        let bas = nom.to_lowercase();
        let groupe = ["les ", "des ", "l'", "l’", "et ", "participant"].iter().any(|p| bas.starts_with(p));
        if nom.is_empty() || groupe || nom.starts_with('*') || nom.split_whitespace().count() > 4 || noms.contains(&nom) {
            continue;
        }
        noms.push(nom);
    }
    noms.iter().map(|n| format!("- {n}")).collect::<Vec<_>>().join("\n")
}

/// Parenthèse de marques de voix (« (Voix 1, 6) ») ou de remarque (« (prénom uniquement, nom non précisé) ») retirée.
fn sans_voix_entre_parentheses(ligne: &str) -> String {
    let mut out = ligne.to_string();
    let mut depuis = 0;
    while let Some(debut) = out[depuis..].find(" (").map(|d| depuis + d) {
        let fin = out[debut..].find(')').map_or(out.len(), |f| debut + f + 1);
        let dedans = out[debut + 2..fin].trim_end_matches(')').to_lowercase();
        if dedans.starts_with("voix") || dedans.contains(',') || dedans.contains("nom ") || dedans.contains("prénom") {
            out.replace_range(debut..fin, "");
        } else {
            depuis = fin;
        }
    }
    out
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
    fin.levee = sans_heure_de_fin(&levee);
    fin
}

/// « La rencontre se termine à 16 h 00 » : le modèle recopie souvent l'heure du début. L'heure de
/// fin n'est presque jamais dite ; on la retire de la levée de la séance.
fn sans_heure_de_fin(levee: &str) -> String {
    let mut texte = levee.to_string();
    for prefixe in [" à ", " vers "] {
        while let Some(i) = texte.match_indices(prefixe).map(|(i, _)| i).find(|&i| {
            let reste = &texte[i + prefixe.len()..];
            let chiffres = reste.chars().take_while(char::is_ascii_digit).count();
            // Seulement l'heure de la fin (« se termine à », « prend fin vers »), pas celle de la prochaine rencontre.
            let avant: String = texte[..i].chars().rev().take(20).collect::<Vec<_>>().into_iter().rev().collect();
            chiffres > 0 && reste[chiffres..].starts_with(" h") && ["termine", "fin", "levée"].iter().any(|m| avant.contains(m))
        }) {
            let reste = &texte[i + prefixe.len()..];
            let mut fin = reste.chars().take_while(char::is_ascii_digit).count() + 2;
            let minutes = reste[fin..].strip_prefix(' ').map_or(0, |m| m.chars().take_while(char::is_ascii_digit).count());
            if minutes > 0 {
                fin += 1 + minutes;
            }
            texte.replace_range(i..i + prefixe.len() + fin, "");
        }
    }
    texte
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
                // Cents écrits à l'anglaise (« 57 949.30 $ ») : comme « 57 949,30 ».
                let cents = chars.get(j) == Some(&'.')
                    && (1..=2).all(|k| chars.get(j + k).is_some_and(|c| c.is_ascii_digit()))
                    && chars.get(j + 3).is_none_or(|c| !c.is_ascii_digit() && *c != '.');
                if cents {
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

/// Nombres absents des sources, avec la section où chacun paraît (« 5. Liste des chèques »).
fn unverified_numbers(doc: &str, sources: &str) -> Vec<(String, String)> {
    let known: std::collections::HashSet<String> = numbers(sources).into_iter().collect();
    let mut suspects: Vec<(String, String)> = Vec::new();
    let mut section = String::new();
    for line in doc.lines() {
        if let Some(titre) = line.strip_prefix("## ") {
            section = titre.trim().to_string();
            continue;
        }
        // Les numéros de points (« ### 3. ») viennent de l'ordre du jour reformaté.
        let body = line.trim_start_matches(['#', ' ']);
        for n in numbers(body) {
            if !known.contains(&n) && !suspects.iter().any(|(m, _)| *m == n) && n.parse::<u32>().map_or(true, |v| v > 31) {
                suspects.push((n, section.clone()));
            }
        }
    }
    suspects
}

/// Avertissement de fin : les chiffres à vérifier, regroupés par section.
fn a_verifier(suspects: &[(String, String)]) -> String {
    let mut sections: Vec<(&str, Vec<String>)> = Vec::new();
    for (n, s) in suspects {
        let n = milliers(n);
        match sections.iter_mut().find(|(t, _)| t == s) {
            Some((_, v)) => v.push(n),
            None => sections.push((s, vec![n])),
        }
    }
    let mut texte = "\n\n> **À vérifier** : ces chiffres n'apparaissent ni dans la transcription ni dans l'ordre du jour :".to_string();
    for (s, v) in sections {
        let lieu = if s.is_empty() { "En-tête".to_string() } else if s.starts_with(|c: char| c.is_ascii_digit()) { format!("Point {s}") } else { s.to_string() };
        texte.push_str(&format!("\n> - {} : {}", lieu.trim_end_matches('.'), v.join(", ")));
    }
    texte
}

/// « 1340,02 » → « 1 340,02 » (espace insécable entre les milliers).
fn milliers(n: &str) -> String {
    let (entier, decimales) = n.split_once(',').map_or((n, None), |(e, d)| (e, Some(d)));
    if entier.len() <= 4 && decimales.is_none() || entier.len() <= 3 {
        return n.to_string();
    }
    let chiffres: Vec<char> = entier.chars().collect();
    let mut out = String::new();
    for (i, c) in chiffres.iter().enumerate() {
        if i > 0 && (chiffres.len() - i).is_multiple_of(3) {
            out.push('\u{a0}');
        }
        out.push(*c);
    }
    decimales.map_or(out.clone(), |d| format!("{out},{d}"))
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
        // Seulement un minimum : le programme de rédaction l'agrandit selon les jetons réels.
        let contexte = (prompt.chars().count() / 3 + max_jetons + 256).clamp(4096, 12288);
        let (prefixe, invite) = prompt.split_once(SEPARATEUR_PREFIXE).unwrap_or(("", prompt));
        let requete = json!({
            "modele": self.modele,
            "prefixe": prefixe,
            "invite": invite,
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
    fn consignes_recopiees_retirees() {
        let md = "# Compte rendu\n## Présences (liste à puces, prénom et nom)\n- Julie Roy\n\n\
                  ## 1. Mot de bienvenue\nLa direction ouvre la séance.\n*   Lecture de l'ordre du jour.\nPoint non abordé ou reporté.\n\n\
                  ## 2. Adoption (procès-verbal)\nMentionné, mais aucune discussion.\nPoint non abordé ou reporté.\n\n## 3. Varia\nPoint non abordé ou reporté.";
        assert_eq!(
            sans_consignes_recopiees(md),
            "# Compte rendu\n## Présences\n- Julie Roy\n\n## 1. Mot de bienvenue\nLa direction ouvre la séance.\n*   Lecture de l'ordre du jour.\n\n\
             ## 2. Adoption (procès-verbal)\nPoint non abordé ou reporté.\n## 3. Varia\nPoint non abordé ou reporté."
        );
        // Une parenthèse qui fait partie du titre reste.
        assert_eq!(sans_consignes_recopiees("## 4. Budget (Société sylvicole)\nTexte."), "## 4. Budget (Société sylvicole)\nTexte.");
    }

    #[test]
    fn compte_rendu_avec_transcription_complete() {
        let moteur = moteur();
        let p = |titre: &str| PointCr { titre: titre.into(), transcription: None, resume: None };
        let points = vec![p("1. Ouverture"), p("2. Finances"), p("2.1. Budget"), p("3. Varia"), p("4. Fin de la rencontre")];
        let mut invites: Vec<(String, usize)> = Vec::new();
        let doc = rediger_compte_rendu(
            &requete(&moteur, points, "", false),
            &mut |invite: &str, max: usize| {
                invites.push((invite.to_string(), max));
                Ok(if invite.contains("fais la liste des sujets") {
                    "00:05 | Mot du président\n00:10 | Budget de 2 500 $".into()
                } else if invite.contains("Sujet (vers [00:05])") {
                    "1".into()
                } else if invite.contains("Sujet (vers [00:10])") {
                    "Point 2.1".into()
                } else if invite.contains("le point « 2.1. Budget »") {
                    "Le budget de 2 500 $ est présenté.".into()
                } else if invite.contains("le point « 4. Fin de la rencontre »") {
                    "La rencontre prend fin. Elle a duré longtemps. Tout le monde part. Encore une phrase.".into()
                } else if invite.contains("le point « 1. Ouverture »") {
                    "Mme Tremblay ouvre la rencontre.".into()
                } else {
                    "## Présences\n- Julie Roy\n## Levée de la séance\nLa séance est levée.".into()
                })
            },
            true,
        )
        .unwrap();
        // Liste des sujets, une question par sujet, puis ouverture, budget et fin : ni le point parent ni le varia sans
        // sujet, puis les sections de fin.
        assert_eq!(invites.len(), 7, "{doc}");
        // La question ne propose ni le point parent ni la levée de la séance ; la liste des points est gardée avec le
        // préfixe, le sujet vient après.
        let question = &invites[1].0;
        assert!(question.contains("2.1. Budget (sous « 2. Finances »)\n") && !question.contains("4. Fin"), "{question}");
        let (gardee, suite) = question.split_once(SEPARATEUR_PREFIXE).unwrap();
        assert!(gardee.contains("=== FIN DE LA TRANSCRIPTION ===") && gardee.ends_with("3. Varia\n\n"), "{gardee}");
        assert!(suite.starts_with("Sujet (vers [00:05]) : Mot du président\n") && !suite.contains(SEPARATEUR_PREFIXE), "{suite}");
        assert!(doc.contains("## 2. Finances\n\n## 2.1. Budget\n\nLe budget de 2 500 $ est présenté."), "{doc}");
        assert!(doc.contains("## 3. Varia\n\nAucun sujet."), "{doc}");
        assert!(doc.contains("La rencontre prend fin. Elle a duré longtemps. Tout le monde part.\n"), "{doc}");
        // Le mot de bienvenue reçoit ses propres sujets (le mot du président) ; le budget ne reprend pas ceux des autres.
        let ouverture = &invites.iter().find(|(i, _)| i.contains("le point « 1. Ouverture »")).unwrap().0;
        assert!(ouverture.contains("Sujets de ce point") && ouverture.contains("- Mot du président"), "{ouverture}");
        let budget = &invites.iter().find(|(i, _)| i.contains("le point « 2.1. Budget »")).unwrap().0;
        assert!(budget.contains("- Budget de 2 500 $") && budget.contains("- Mot du président"), "{budget}");
    }

    /// Une question par sujet, deuxième question pour un sujet hors de l'ordre de la rencontre, point sans
    /// sujet (voir `repartition`).
    #[test]
    fn sujet_deplace_et_point_non_aborde() {
        let moteur = moteur();
        let p = |titre: &str| PointCr { titre: titre.into(), transcription: None, resume: None };
        let points = vec![p("4. Rapport budgétaire"), p("6. Suivi tâches"), p("8. Évaluation des travaux"), p("11. Prochaine rencontre")];
        let r = requete(&moteur, points, "", false);
        let mut invites: Vec<String> = Vec::new();
        let doc = rediger_compte_rendu(
            &r,
            &mut |invite: &str, _| {
                invites.push(invite.to_string());
                Ok(if invite.contains("fais la liste des sujets") {
                    "02:00 | Factures reçues\n12:20 | États financiers\n28:30 | Soumission reçue\n30:00 | Huit vérifications".into()
                } else if invite.contains("Sujet (vers [02:00])") {
                    "4".into()
                } else if invite.contains("Sujet (vers [28:30])") || invite.contains("Sujet (vers [30:00])") {
                    "8".into()
                } else if invite.contains("Sujet (vers [12:20])") {
                    // D'abord le point 11 ; avec ses voisins, le point 6.
                    if invite.contains("après le point « 4. Rapport budgétaire » et avant le point « 8. Évaluation") { "6" } else { "11" }.into()
                } else if invite.contains("a-t-il été discuté") {
                    "Aucun".into()
                } else if invite.contains("Rédige uniquement le point") {
                    "Rédigé.".into()
                } else {
                    "## Présences\n- Julie Roy\n## Levée de la séance\nLa séance est levée.".into()
                })
            },
            true,
        )
        .unwrap();
        assert_eq!(invites.iter().filter(|i| i.contains("Sujet (vers [12:20])")).count(), 2, "{invites:?}");
        let suivi = invites.iter().find(|i| i.contains("Rédige uniquement le point « 6. Suivi tâches »")).expect("point 6");
        assert!(suivi.contains("- États financiers"), "{suivi}");
        // Le point 11 n'a reçu aucun sujet : question ciblée, puis « non abordé » sans rédaction (ni date inventée).
        assert!(invites.iter().any(|i| i.contains("« 11. Prochaine rencontre » de l'ordre du jour a-t-il")), "{invites:?}");
        assert!(!invites.iter().any(|i| i.contains("Rédige uniquement le point « 11.")), "{invites:?}");
        assert!(doc.contains("## 11. Prochaine rencontre\n\nPoint non abordé ou reporté."), "{doc}");
    }

    #[test]
    fn points_parents_et_fin() {
        let t = ["1. Bienvenue", "2. Filière forestière", "2.1. Scierie", "2.2. Étude", "3. Énergie", "12. Affaires nouvelles", "12.1. Varia"];
        let parents: Vec<bool> = (0..t.len()).map(|i| a_des_sous_points(&t, i)).collect();
        assert_eq!(parents, [false, true, false, false, false, true, false]);
        assert!(!a_des_sous_points(&["1. A", "10. B"], 0));
        assert!(est_fin("7. Fin de la rencontre") && est_fin("9. Levée de la séance") && !est_fin("3. Financement"));
    }

    #[test]
    fn phrases_completes() {
        assert_eq!(phrases("L'ordre du jour est adopté. La suite porte sur le budget, les", 3), "L'ordre du jour est adopté.");
        assert_eq!(phrases("Un. Deux. Trois. Quatre.", 3), "Un. Deux. Trois.");
        assert_eq!(phrases("Montant de 1.5 M$ adopté.", 3), "Montant de 1.5 M$ adopté.");
        assert_eq!(liste_des_presences("- Annie (Annie)\n- Daniel Dufour (Daniel)\n- Gilles Brès"), "- Annie\n- Daniel Dufour\n- Gilles Brès");
        assert_eq!(liste_des_presences("- Marco (Voix 1, 6, 9)\n- Annie (Voix 4)"), "- Marco\n- Annie");
        assert_eq!(liste_des_presences("- Annie (prénom uniquement, nom non précisé)\n- Daniel (direction)"), "- Annie\n- Daniel (direction)");
        assert_eq!(sans_heure_de_fin("La rencontre se termine à 16 h 00 après le budget. Prochaine à 9 h."), "La rencontre se termine après le budget. Prochaine à 9 h.");
        assert_eq!(sans_heure_de_fin("Elle prend fin vers 18 h, à la salle 222."), "Elle prend fin, à la salle 222.");
    }

    #[test]
    fn titre_recopie_retire() {
        assert_eq!(sans_titre_recopie("6. Suivi des tâches\n\nLa direction…", "6. Suivi tâches à effectuer").trim(), "La direction…");
        assert_eq!(sans_titre_recopie("6 membres présents.\nSuite", "6. Suivi"), "6 membres présents.\nSuite");
        assert_eq!(presences_entendues("- Voix 1 (Direction)\n- Daniel Dufour", "Daniel parle"), "- Daniel Dufour");
    }

    #[test]
    fn repartition_des_sujets() {
        let sujets = vec![vec!["Ouverture".to_string()], vec!["Pont 00248 limité à 48 tonnes".into(), "Chèques 1473 à 1477".into()], vec!["Varia".into()]];
        let c = consigne_repartition(&sujets, &["Bienvenue", "Affaires", "Varia"], 0);
        assert!(c.contains("- Ouverture") && c.contains("« Affaires » :\n- Pont") && c.contains("« Varia » :\n- Varia"), "{c}");
        assert!(consigne_repartition(&[vec![], vec![]], &["Bienvenue", "Varia"], 1).contains("Point non abordé"));
    }

    #[test]
    fn signale_les_chiffres_inventes() {
        let src = "On a dépensé 2 300 $ sur un budget de 2 500 $. Prochaine rencontre le 19 novembre 2026.";
        let doc = "Budget de 2 500 $, dépenses de 2 300 $ : dépassement de 200 $. Le 19 novembre 2026.";
        assert_eq!(unverified_numbers(doc, src), vec![("200".to_string(), String::new())]);
        let doc = "## 5. Liste des chèques (information).\nTotal de 1 340,02 $.\n## 7. Coupure\n130 000 $ et 5000 $.";
        let avis = a_verifier(&unverified_numbers(doc, src));
        assert!(avis.contains("> - Point 5. Liste des chèques (information) : 1\u{a0}340,02\n> - Point 7. Coupure : 130\u{a0}000, 5000"), "{avis}");
        assert_eq!(numbers("12 000 $ et 2,5 % et 14 h 30"), vec!["12000", "2,5", "14", "30"]);
        // Montant de l'ordre du jour écrit avec un point, repris avec une virgule.
        assert!(unverified_numbers("Chèque de 57 949,30 $, point 2.1.", "1473 Ectec 57 949.30 $").is_empty());
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
            lexique: Vec::new(),
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
        let doc = rediger_compte_rendu(&r, &mut simule(&mut invites), false).unwrap();
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
        rediger_compte_rendu(&requete(&moteur, p, "", false), &mut simule(&mut invites), false).unwrap();
        assert!(invites[3].contains("1 à 3 puces seulement"), "{}", invites[3]);
        assert!(!invites.last().unwrap().contains("## Prochaine rencontre"));
    }

    #[test]
    fn reprise_d_un_compte_rendu_interrompu() {
        let dir = std::env::temp_dir().join(format!("tourdetable-test-{}-cr", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let moteur = moteur();
        let mut invites = Vec::new();
        rediger_compte_rendu(&requete(&moteur, points(Some(&dir)), "", true), &mut simule(&mut invites), false).unwrap();
        assert_eq!(invites.len(), 4);
        let resume = std::fs::read_to_string(dir.join("03-budget/resume.md")).unwrap();
        assert!(resume.starts_with("<!-- tourdetable ") && resume.contains("- Budget de 2 500 $."));
        assert!(!dir.join("04-varia/resume.md").exists());

        // Reprise : seul l'assemblage est refait.
        invites.clear();
        let doc = rediger_compte_rendu(&requete(&moteur, points(Some(&dir)), "", true), &mut simule(&mut invites), false).unwrap();
        assert_eq!(invites.len(), 1, "{invites:?}");
        assert!(doc.contains("Il est convenu que Mme Roy publie l'annonce."));

        // Transcription du point modifiée (nettoyage relancé) : il est rédigé à nouveau.
        let mut p = points(Some(&dir));
        p[2].transcription = Some("Budget adopté.".into());
        invites.clear();
        rediger_compte_rendu(&requete(&moteur, p, "", true), &mut simule(&mut invites), false).unwrap();
        assert_eq!(invites.len(), 2);
        // Résumé d'une forme antérieure (consignes différentes) : refait.
        let ancien = std::fs::read_to_string(dir.join("01-ouverture/resume.md")).unwrap();
        let corps = ancien.split_once('\n').unwrap().1;
        let entete_ancienne = format!("<!-- tourdetable {} -->", empreinte("1. Ouverture\nOn ouvre à 19 h."));
        std::fs::write(dir.join("01-ouverture/resume.md"), format!("{entete_ancienne}\n{corps}")).unwrap();
        invites.clear();
        rediger_compte_rendu(&requete(&moteur, points(Some(&dir)), "", true), &mut simule(&mut invites), false).unwrap();
        assert_eq!(invites.len(), 3, "{invites:?}");
        // « Régénérer » : tout est refait.
        invites.clear();
        rediger_compte_rendu(&requete(&moteur, points(Some(&dir)), "", false), &mut simule(&mut invites), false).unwrap();
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
        let doc = rediger_compte_rendu(&requete(&moteur, Vec::new(), "", true), &mut appel, false).unwrap();
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
