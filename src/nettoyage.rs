//! Nettoyage de la transcription, entre la transcription et le compte rendu.
//!
//! La transcription brute (`Meeting::segments`) n'est jamais modifiée : le résultat est une liste
//! de phrases, chacune rattachée à son point et aux segments bruts d'où elle vient (lecture
//! synchronisée). Le travail se fait point par point, dans l'ordre chronologique, et reprend au
//! point suivant après une interruption.
//!
//! * Passe 1, par règles, sans modèle de langue : phrases coupées entre deux énoncés recollées,
//!   boucles de Whisper réduites, hésitations retirées (`data/nettoyage.txt`), puis lexique et
//!   typographie OQLF de `quebec`.
//! * Passe 2, par le modèle de langue local (`tourdetable-redaction`) : morceaux d'environ
//!   4 minutes, phrases numérotées. Une sortie suspecte est rejetée et la passe 1 est gardée.

use std::collections::HashSet;
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use anyhow::Result;
use crossbeam_channel::Receiver;
use serde::{Deserialize, Serialize};

use crate::audio::Source;
use crate::quebec::{self, Glossary};
use crate::storage::Meeting;
use crate::stt::Segment;

/// Journal des morceaux acceptés ou rejetés, dans le dossier de la rencontre.
pub const JOURNAL: &str = "nettoyage.log";
/// Écart maximal entre les deux énoncés d'une phrase coupée.
const ECART_RECOLLE_MS: u64 = 1500;
/// Plus long chevauchement cherché entre deux énoncés recollés, en mots.
const CHEVAUCHEMENT_MAX: usize = 8;
/// Plus longue boucle de Whisper cherchée, en mots.
const BOUCLE_MAX: usize = 30;
/// À partir de combien de répétitions d'affilée une suite est une boucle.
const REPETITIONS_BOUCLE: usize = 3;
/// Morceau envoyé au modèle : environ 4 minutes de parole, et jamais trop de texte.
const MORCEAU_MS: u64 = 240_000;
const MORCEAU_CARACTERES: usize = 4500;
/// Longueur acceptée de la sortie du modèle, par rapport à son entrée.
const RATIO_MIN: f32 = 0.70;
const RATIO_MAX: f32 = 1.10;

/// Transcription nettoyée d'une rencontre (`Meeting::nettoyage`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Nettoyage {
    pub phrases: Vec<Phrase>,
    /// Groupes (suites consécutives d'un même point, voir `groupes`) déjà nettoyés.
    #[serde(default)]
    pub groupes_faits: usize,
    #[serde(default)]
    pub groupes: usize,
    /// Nombre de segments bruts au départ : si la transcription a changé, on recommence.
    #[serde(default)]
    pub segments: usize,
    /// Modèle de rédaction absent : seule la passe 1 a été appliquée (au moins en partie).
    #[serde(default)]
    pub sans_modele: bool,
    /// Arrêté par l'utilisateur : pas de reprise automatique au démarrage.
    #[serde(default)]
    pub suspendu: bool,
}

impl Nettoyage {
    pub fn termine(&self) -> bool {
        self.groupes_faits >= self.groupes
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Phrase {
    /// Indice dans `Agenda::points` ; `None` : hors point.
    pub point: Option<usize>,
    pub debut_ms: u64,
    pub fin_ms: u64,
    pub source: Source,
    pub texte: String,
    /// Indices dans `Meeting::segments` des segments bruts d'origine.
    pub segments: Vec<usize>,
}

// ───────────────────────── Listes ─────────────────────────

const DONNEES: &str = include_str!("../data/nettoyage.txt");

#[derive(Default)]
struct Listes {
    hesitations: HashSet<String>,
    debuts: Vec<Vec<String>>,
    doubles_permis: HashSet<String>,
}

fn listes() -> &'static Listes {
    static L: OnceLock<Listes> = OnceLock::new();
    L.get_or_init(|| lire_listes(DONNEES))
}

fn lire_listes(texte: &str) -> Listes {
    let mut l = Listes::default();
    let mut section = "";
    for ligne in texte.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        if let Some(nom) = ligne.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = nom;
            continue;
        }
        let cles: Vec<String> = ligne.split_whitespace().map(cle).filter(|c| !c.is_empty()).collect();
        match section {
            "hesitation" => l.hesitations.extend(cles),
            "debut" if !cles.is_empty() => l.debuts.push(cles),
            "double-permis" => l.doubles_permis.extend(cles),
            _ => {}
        }
    }
    l
}

// ───────────────────────── Passe 1 : règles ─────────────────────────

/// Un mot de la transcription et les segments bruts qu'il représente.
#[derive(Clone, Debug)]
struct Mot {
    texte: String,
    cle: String,
    segments: Vec<usize>,
}

impl Mot {
    fn new(texte: &str, segment: usize) -> Self {
        Self { texte: texte.to_string(), cle: cle(texte), segments: vec![segment] }
    }

    /// Reprend les segments des mots retirés et la ponctuation du dernier, quand elle en dit plus
    /// que la sienne (« entendus, hum. » → « entendus. »).
    fn absorber(&mut self, retires: &[Mot]) {
        for r in retires {
            self.segments.extend(&r.segments);
        }
        if let Some(dernier) = retires.last() {
            let p = ponctuation(&dernier.texte);
            let la_mienne = ponctuation(&self.texte);
            if !p.is_empty() && (la_mienne.is_empty() || finit_phrase(&dernier.texte) && !finit_phrase(&self.texte)) {
                let radical = self.texte.len() - la_mienne.len();
                self.texte.truncate(radical);
                self.texte.push_str(p);
            }
        }
    }
}

/// Mot comparable : minuscules, sans la ponctuation collée au début ou à la fin.
fn cle(mot: &str) -> String {
    mot.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()
}

/// Ponctuation collée à la fin d'un mot (« budget?» → « ?» »).
fn ponctuation(mot: &str) -> &str {
    let fin = mot.trim_end_matches(|c: char| !c.is_alphanumeric());
    &mot[fin.len()..]
}

fn finit_phrase(mot: &str) -> bool {
    ponctuation(mot).contains(['.', '?', '!', '…'])
}

fn commence_en_minuscule(texte: &str) -> bool {
    texte.trim_start().chars().next().is_some_and(char::is_lowercase)
}

fn majuscule_initiale(mot: &mut String) {
    if let Some(c) = mot.chars().next()
        && c.is_lowercase()
    {
        let haut: String = c.to_uppercase().collect();
        mot.replace_range(..c.len_utf8(), &haut);
    }
}

/// Suite d'énoncés d'une même source, recollés.
struct Course {
    source: Source,
    mots: Vec<Mot>,
    fin_ms: u64,
}

/// Passe 1 sur un groupe de segments `(indice, segment)` d'un même point.
pub fn passe1(segments: &[(usize, &Segment)], point: Option<usize>, glossaire: &Glossary) -> Vec<Phrase> {
    let mut courses: Vec<Course> = Vec::new();
    for &(i, s) in segments {
        if quebec::is_hallucination(&s.text) {
            continue;
        }
        let mut mots: Vec<Mot> = s.text.split_whitespace().map(|m| Mot::new(m, i)).collect();
        if mots.is_empty() {
            continue;
        }
        let recolle = courses.last().is_some_and(|c| {
            c.source == s.source
                && c.mots.last().is_some_and(|m| !finit_phrase(&m.texte))
                && commence_en_minuscule(&s.text)
                && s.start_ms.saturating_sub(c.fin_ms) < ECART_RECOLLE_MS
        });
        match courses.last_mut() {
            Some(c) if recolle => {
                retirer_chevauchement(&c.mots, &mut mots, i);
                if mots.is_empty() {
                    c.mots.last_mut().expect("course non vide").segments.push(i);
                }
                c.mots.extend(mots);
                c.fin_ms = c.fin_ms.max(s.end_ms);
            }
            _ => courses.push(Course { source: s.source, mots, fin_ms: s.end_ms }),
        }
    }

    let horodatage = |segs: &[usize]| {
        let trouves = segments.iter().filter(|(i, _)| segs.contains(i)).map(|(_, s)| (s.start_ms, s.end_ms));
        trouves.fold((u64::MAX, 0), |(d, f), (a, b)| (d.min(a), f.max(b)))
    };
    let mut retenues: Vec<(Source, Vec<Mot>)> = Vec::new();
    // Segments d'une phrase faite seulement d'hésitations : ils passent à la suivante.
    let mut orphelins: Vec<usize> = Vec::new();
    for mut c in courses {
        reduire_boucles(&mut c.mots);
        reduire_doubles(&mut c.mots);
        for mut p in decouper_phrases(c.mots) {
            if retirer_hesitations(&mut p) {
                p[0].segments.append(&mut orphelins);
                retenues.push((c.source, p));
            } else {
                orphelins.extend(p.iter().flat_map(|m| m.segments.iter().copied()));
            }
        }
    }
    if let Some((_, p)) = retenues.last_mut() {
        p.last_mut().expect("phrase non vide").segments.append(&mut orphelins);
    }
    reduire_phrases_repetees(&mut retenues);

    let mut phrases = Vec::new();
    for (source, mots) in retenues {
        let brut = mots.iter().map(|m| m.texte.as_str()).collect::<Vec<_>>().join(" ");
        let texte = quebec::normalize(&glossaire.apply(&brut));
        if quebec::is_hallucination(&texte) {
            continue;
        }
        let mut segs: Vec<usize> = mots.iter().flat_map(|m| m.segments.iter().copied()).collect();
        segs.sort_unstable();
        segs.dedup();
        let (debut_ms, fin_ms) = horodatage(&segs);
        phrases.push(Phrase { point, debut_ms, fin_ms, source, texte, segments: segs });
    }
    phrases
}

/// Énoncé qui reprend la fin du précédent (« on va regarder le budget » / « le budget pour… ») :
/// les mots répétés sont retirés du début du second.
fn retirer_chevauchement(avant: &[Mot], apres: &mut Vec<Mot>, segment: usize) {
    let max = CHEVAUCHEMENT_MAX.min(avant.len()).min(apres.len());
    for k in (1..=max).rev() {
        let fin = &avant[avant.len() - k..];
        if fin.iter().zip(&apres[..k]).all(|(a, b)| a.cle == b.cle && !a.cle.is_empty()) {
            if k == 1 && listes().doubles_permis.contains(&fin[0].cle) {
                return;
            }
            apres.drain(..k);
            if let Some(m) = apres.first_mut() {
                m.segments.push(segment);
            }
            return;
        }
    }
}

/// Même suite de 1 à `BOUCLE_MAX` mots répétée `REPETITIONS_BOUCLE` fois ou plus d'affilée :
/// on n'en garde que la première.
fn reduire_boucles(mots: &mut Vec<Mot>) {
    let mut i = 0;
    while i < mots.len() {
        let mut reduit = false;
        for n in 1..=BOUCLE_MAX.min((mots.len() - i) / REPETITIONS_BOUCLE) {
            let mut r = 1;
            while i + (r + 1) * n <= mots.len() && (0..n).all(|k| mots[i + k].cle == mots[i + r * n + k].cle) {
                r += 1;
            }
            if r >= REPETITIONS_BOUCLE {
                let retires: Vec<Mot> = mots.drain(i + n..i + r * n).collect();
                mots[i + n - 1].absorber(&retires);
                reduit = true;
                break;
            }
        }
        if !reduit {
            i += 1;
        }
    }
}

/// « le le budget » → « le budget », sauf les doublements corrects (« nous nous »).
fn reduire_doubles(mots: &mut Vec<Mot>) {
    let mut i = 1;
    while i < mots.len() {
        let (a, b) = (&mots[i - 1], &mots[i]);
        if a.cle == b.cle && !a.cle.is_empty() && ponctuation(&a.texte).is_empty() && !listes().doubles_permis.contains(&a.cle) {
            let retire = mots.remove(i);
            mots[i - 1].absorber(&[retire]);
        } else {
            i += 1;
        }
    }
}

fn decouper_phrases(mots: Vec<Mot>) -> Vec<Vec<Mot>> {
    let mut phrases = vec![Vec::new()];
    for m in mots {
        let fin = finit_phrase(&m.texte);
        phrases.last_mut().expect("jamais vide").push(m);
        if fin {
            phrases.push(Vec::new());
        }
    }
    phrases.retain(|p| !p.is_empty());
    phrases
}

/// Retire les hésitations isolées et, en début de phrase, « bon ben ». Faux si rien ne reste.
fn retirer_hesitations(p: &mut Vec<Mot>) -> bool {
    let l = listes();
    let mut debut_retire = false;
    let mut i = 0;
    while i < p.len() {
        if !l.hesitations.contains(&p[i].cle) {
            i += 1;
            continue;
        }
        let retire = p.remove(i);
        if i == 0 {
            debut_retire = true;
            match p.first_mut() {
                Some(suivant) => suivant.segments.extend(retire.segments),
                None => {
                    p.push(retire);
                    return false;
                }
            }
        } else {
            p[i - 1].absorber(&[retire]);
        }
    }
    while let Some(expr) = l.debuts.iter().find(|e| p.len() > e.len() && p.iter().zip(e.iter()).all(|(m, c)| m.cle == *c)) {
        let retires: Vec<Mot> = p.drain(..expr.len()).collect();
        p[0].segments.extend(retires.iter().flat_map(|m| m.segments.iter().copied()));
        debut_retire = true;
    }
    if debut_retire {
        majuscule_initiale(&mut p[0].texte);
    }
    true
}

/// Même phrase répétée `REPETITIONS_BOUCLE` fois ou plus d'affilée (boucle plus longue que
/// `BOUCLE_MAX` mots) : une seule occurrence.
fn reduire_phrases_repetees(phrases: &mut Vec<(Source, Vec<Mot>)>) {
    let cle_phrase = |(s, p): &(Source, Vec<Mot>)| (*s, p.iter().map(|m| m.cle.as_str()).collect::<Vec<_>>().join(" "));
    let mut i = 0;
    while i < phrases.len() {
        let c = cle_phrase(&phrases[i]);
        let r = phrases[i..].iter().take_while(|p| cle_phrase(p) == c).count();
        if r >= REPETITIONS_BOUCLE {
            let retires: Vec<Mot> = phrases.drain(i + 1..i + r).flat_map(|(_, p)| p).collect();
            let dernier = phrases[i].1.last_mut().expect("phrase non vide");
            dernier.segments.extend(retires.iter().flat_map(|m| m.segments.iter().copied()));
        }
        i += 1;
    }
}

// ───────────────────────── Passe 2 : modèle de langue ─────────────────────────

/// Appel au modèle : invite → réponse. Simulé dans les tests.
pub type Redacteur<'a> = dyn FnMut(&str) -> Result<String> + 'a;

/// Ce que le modèle doit respecter : termes du lexique et titres des points.
pub struct Contexte<'a> {
    pub termes: &'a [String],
    pub titres: &'a [String],
}

const CONSIGNE: &str = "Tu corriges la transcription automatique d'une réunion en français québécois. \
Chaque ligne est une phrase numérotée.\n\
Pour chaque phrase : corrige la ponctuation, les accords et les mots manifestement mal reconnus ; \
retire les répétitions et les faux départs.\n\
Ne résume pas. N'ajoute rien et ne retire rien de significatif. Garde tels quels les noms propres, \
les chiffres, les montants, les dates, les sigles et les expressions québécoises (« tsé », « pantoute », \
« correct », « pis », « tantôt »…).\n\
Réponds uniquement avec les phrases corrigées : une par ligne, avec le même numéro et dans le même ordre, \
sans commentaire.";

/// Découpe les phrases en morceaux d'environ 4 minutes de parole.
fn morceaux(phrases: &[Phrase]) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut debut = 0;
    let mut caracteres = 0;
    for (i, p) in phrases.iter().enumerate() {
        let trop_long = p.fin_ms.saturating_sub(phrases[debut].debut_ms) > MORCEAU_MS;
        if i > debut && (trop_long || caracteres + p.texte.len() > MORCEAU_CARACTERES) {
            out.push(debut..i);
            debut = i;
            caracteres = 0;
        }
        caracteres += p.texte.len();
    }
    if debut < phrases.len() {
        out.push(debut..phrases.len());
    }
    out
}

fn invite(phrases: &[Phrase], ctx: &Contexte) -> String {
    let mut s = String::from(CONSIGNE);
    if !ctx.termes.is_empty() {
        s.push_str("\nTermes à écrire exactement ainsi : ");
        s.push_str(&ctx.termes.join(", "));
        s.push('.');
    }
    if !ctx.titres.is_empty() {
        s.push_str("\nPoints de l'ordre du jour : ");
        s.push_str(&ctx.titres.join(" ; "));
        s.push('.');
    }
    s.push_str("\n\n=== PHRASES ===\n");
    for (i, p) in phrases.iter().enumerate() {
        s.push_str(&format!("{}. {}\n", i + 1, p.texte));
    }
    s
}

/// Garde-fous : la sortie du modèle pour `entree`, phrase par phrase, ou la raison du rejet.
fn verifier(entree: &[Phrase], sortie: &str) -> Result<Vec<String>, String> {
    let mut textes: Vec<Option<String>> = vec![None; entree.len()];
    let mut en_trop = Vec::new();
    let mut courant = None;
    for ligne in sortie.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let chiffres = ligne.len() - ligne.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        let reste = ligne[chiffres..].trim_start();
        let numero = (chiffres > 0 && reste.starts_with(['.', ')'])).then(|| ligne[..chiffres].parse::<usize>().ok()).flatten();
        let Some(n) = numero else {
            // Ligne coupée par le modèle : suite de la phrase précédente (texte avant le
            // premier numéro, comme « Voici le texte corrigé : », ignoré).
            if let Some(t) = courant.and_then(|k: usize| textes[k].as_mut()) {
                t.push(' ');
                t.push_str(ligne);
            }
            continue;
        };
        let texte = reste[1..].trim().to_string();
        match textes.get_mut(n.wrapping_sub(1)) {
            Some(slot @ None) => {
                *slot = Some(texte);
                courant = Some(n - 1);
            }
            Some(Some(_)) => return Err(format!("phrase {n} en double")),
            None => en_trop.push(n.to_string()),
        }
    }
    if !en_trop.is_empty() {
        return Err(format!("numéros en trop : {}", en_trop.join(", ")));
    }
    let manquants: Vec<String> = textes.iter().enumerate().filter(|(_, t)| t.is_none()).map(|(i, _)| (i + 1).to_string()).collect();
    if !manquants.is_empty() {
        return Err(format!("numéros manquants : {}", manquants.join(", ")));
    }
    let textes: Vec<String> = textes.into_iter().flatten().collect();

    let longueur = |v: &mut dyn Iterator<Item = &str>| v.map(|t| t.chars().count()).sum::<usize>().max(1) as f32;
    let ratio = longueur(&mut textes.iter().map(String::as_str)) / longueur(&mut entree.iter().map(|p| p.texte.as_str()));
    if ratio < RATIO_MIN {
        return Err(format!("sortie trop courte ({:.0} % de l'entrée)", ratio * 100.0));
    }
    if ratio > RATIO_MAX {
        return Err(format!("sortie trop longue ({:.0} % de l'entrée)", ratio * 100.0));
    }
    let avant: Vec<&str> = entree.iter().map(|p| p.texte.as_str()).collect();
    let apres: HashSet<String> = crate::summary::numbers(&textes.join(" ")).into_iter().collect();
    let mut perdus: Vec<String> = crate::summary::numbers(&avant.join(" ")).into_iter().filter(|n| !apres.contains(n)).collect();
    perdus.dedup();
    if !perdus.is_empty() {
        return Err(format!("nombres disparus : {}", perdus.join(", ")));
    }
    Ok(textes)
}

/// Passe 2 sur les phrases d'un groupe, morceau par morceau. Faux si annulée en cours de route.
fn passe2(phrases: &mut Vec<Phrase>, ctx: &Contexte, glossaire: &Glossary, rediger: &mut Redacteur, etiquette: &str, journal: &mut dyn FnMut(&str), annuler: &AtomicBool) -> bool {
    let mut sortie = Vec::with_capacity(phrases.len());
    let total = morceaux(phrases).len();
    for (k, r) in morceaux(phrases).into_iter().enumerate() {
        if annuler.load(Ordering::Relaxed) {
            return false;
        }
        let entree = &phrases[r.clone()];
        let nom = format!("{etiquette}, morceau {} sur {total} (phrases {} à {})", k + 1, r.start + 1, r.end);
        let verdict = rediger(&invite(entree, ctx)).map_err(|e| format!("{e:#}")).and_then(|t| verifier(entree, &t));
        match verdict {
            Ok(textes) => {
                journal(&format!("{nom} : accepté"));
                for (p, t) in entree.iter().zip(textes) {
                    let texte = quebec::normalize(&glossaire.apply(&t));
                    // Phrase vidée par le modèle (faux départ) : retirée.
                    if !quebec::is_hallucination(&texte) {
                        sortie.push(Phrase { texte, ..p.clone() });
                    }
                }
            }
            Err(raison) => {
                if annuler.load(Ordering::Relaxed) {
                    return false;
                }
                journal(&format!("{nom} : rejeté, passe 1 gardée — {raison}"));
                sortie.extend_from_slice(entree);
            }
        }
    }
    *phrases = sortie;
    true
}

// ───────────────────────── Déroulement ─────────────────────────

/// Segments par suites consécutives d'un même point, avec leurs indices dans `Meeting::segments`
/// (mêmes groupes que `Meeting::segments_par_point`).
pub fn groupes(m: &Meeting) -> Vec<(Option<usize>, Vec<usize>)> {
    let mut out: Vec<(Option<usize>, Vec<usize>)> = Vec::new();
    for (i, s) in m.segments.iter().enumerate() {
        let point = m.point_de(s.start_ms);
        match out.last_mut() {
            Some((p, v)) if *p == point => v.push(i),
            _ => out.push((point, vec![i])),
        }
    }
    out
}

#[derive(Default)]
pub struct Avancement {
    /// Groupe en cours (à partir de 1) et nombre de groupes.
    pub groupe: AtomicUsize,
    pub groupes: AtomicUsize,
    pub annuler: AtomicBool,
}

/// Nettoie `m` à partir de `depart` (reprise), groupe par groupe ; `apres_groupe` reçoit l'état
/// après chaque groupe terminé. Sans `rediger` (modèle absent), seule la passe 1 est appliquée.
pub fn executer(
    m: &Meeting,
    depart: Option<Nettoyage>,
    glossaire: &Glossary,
    mut rediger: Option<&mut Redacteur>,
    avancement: &Avancement,
    journal: &mut dyn FnMut(&str),
    apres_groupe: &mut dyn FnMut(&Nettoyage),
) -> Nettoyage {
    let groupes = groupes(m);
    let mut n = match depart {
        Some(n) if n.segments == m.segments.len() && n.groupes == groupes.len() => n,
        _ => Nettoyage { groupes: groupes.len(), segments: m.segments.len(), ..Default::default() },
    };
    n.suspendu = false;
    n.sans_modele |= rediger.is_none();
    avancement.groupes.store(groupes.len(), Ordering::Relaxed);
    let titres: Vec<String> = m.agenda.iter().flat_map(|a| a.points.iter().map(|p| p.titre.clone())).collect();
    let ctx = Contexte { termes: glossaire.termes(), titres: &titres };
    for (k, (point, indices)) in groupes.iter().enumerate().skip(n.groupes_faits) {
        if avancement.annuler.load(Ordering::Relaxed) {
            break;
        }
        avancement.groupe.store(k + 1, Ordering::Relaxed);
        let segs: Vec<(usize, &Segment)> = indices.iter().map(|&i| (i, &m.segments[i])).collect();
        let mut phrases = passe1(&segs, *point, glossaire);
        if let Some(r) = rediger.as_deref_mut() {
            let etiquette = m.intertitre(*point);
            if !passe2(&mut phrases, &ctx, glossaire, r, &etiquette, journal, &avancement.annuler) {
                break;
            }
        }
        n.phrases.extend(phrases);
        n.groupes_faits = k + 1;
        apres_groupe(&n);
    }
    n
}

/// Étapes envoyées par le fil du nettoyage à l'interface.
pub enum Etape {
    /// Un groupe de plus est nettoyé : état à enregistrer (reprise après une fermeture).
    Groupe(Nettoyage),
    /// Fin, terminé ou annulé (`Nettoyage::termine`).
    Fin(Nettoyage),
}

/// Nettoie `m` avec le modèle de rédaction s'il est installé (passe 1 seulement sinon), en
/// journalisant dans `nettoyage.log`. Reprend à partir de `m.nettoyage`.
pub fn executer_avec_modele(m: &Meeting, glossaire: &Glossary, avancement: &Avancement, apres_groupe: &mut dyn FnMut(&Nettoyage)) -> Nettoyage {
    let moteur = crate::config::moteur();
    let chemin = m.dir().join(JOURNAL);
    let mut journal = |ligne: &str| {
        let ecrit = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&chemin)
            .and_then(|mut f| writeln!(f, "{} {ligne}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S")));
        if let Err(e) = ecrit {
            log::warn!("{} : {e}", chemin.display());
        }
    };
    let mut appel = |invite: &str| crate::summary::generate_integre(&moteur.modele, moteur.fils, invite, None, Some(&avancement.annuler));
    let rediger: Option<&mut Redacteur> = if moteur.modele.exists() {
        Some(&mut appel)
    } else {
        journal(&format!("Modèle de rédaction absent ({}) : passe 1 seulement", moteur.modele.display()));
        None
    };
    executer(m, m.nettoyage.clone(), glossaire, rediger, avancement, &mut journal, apres_groupe)
}

/// Nettoie `m` dans un fil (voir `executer_avec_modele`).
pub fn lancer(m: Meeting, glossaire: String, avancement: Arc<Avancement>) -> Receiver<Etape> {
    let (tx, rx) = crossbeam_channel::unbounded();
    std::thread::spawn(move || {
        let glossaire = Glossary::parse(&glossaire);
        let fin = executer_avec_modele(&m, &glossaire, &avancement, &mut |n| {
            let _ = tx.send(Etape::Groupe(n.clone()));
        });
        let _ = tx.send(Etape::Fin(fin));
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{Agenda, Declenchement};

    fn seg(start_ms: u64, end_ms: u64, source: Source, text: &str) -> Segment {
        Segment { start_ms, end_ms, source, text: text.into() }
    }

    fn nettoyer(segments: &[Segment]) -> Vec<Phrase> {
        let v: Vec<(usize, &Segment)> = segments.iter().enumerate().collect();
        passe1(&v, None, &Glossary::parse(""))
    }

    fn textes(p: &[Phrase]) -> Vec<&str> {
        p.iter().map(|p| p.texte.as_str()).collect()
    }

    #[test]
    fn recolle_les_phrases_coupees() {
        let s = [
            seg(0, 4000, Source::Salle, "On va regarder le budget"),
            seg(4500, 8000, Source::Salle, "le budget pour l'an prochain. C'est correct?"),
            // Trop loin : pas recollé, même sans ponctuation.
            seg(10_000, 12_000, Source::Salle, "Il reste"),
            seg(14_000, 15_000, Source::Salle, "deux points."),
            // Autre source : jamais recollée.
            seg(15_500, 16_000, Source::Salle, "Puis"),
            seg(16_200, 17_000, Source::Distance, "on m'entend-tu?"),
            // Majuscule : nouvel énoncé.
            seg(17_100, 18_000, Source::Distance, "Oui"),
            seg(18_100, 19_000, Source::Distance, "Parfait."),
        ];
        let p = nettoyer(&s);
        assert_eq!(
            textes(&p),
            ["On va regarder le budget pour l'an prochain.", "C'est correct?", "Il reste", "deux points.", "Puis", "on m'entend-tu?", "Oui", "Parfait."]
        );
        assert_eq!((p[0].segments.clone(), p[0].debut_ms, p[0].fin_ms), (vec![0, 1], 0, 8000));
        assert_eq!(p[1].segments, vec![1]);
        assert_eq!(p[5].source, Source::Distance);
    }

    #[test]
    fn boucles_de_whisper() {
        let s = [
            seg(0, 1000, Source::Salle, "Merci. Merci. Merci. Merci."),
            seg(2000, 9000, Source::Salle, "On va voir on va voir on va voir ça demain."),
            // Deux fois seulement : gardé tel quel.
            seg(10_000, 11_000, Source::Salle, "C'est beau, c'est beau."),
            seg(12_000, 13_000, Source::Salle, "La subvention de Desjardins couvre le local et le traiteur pour la soirée."),
            seg(13_000, 14_000, Source::Salle, "La subvention de Desjardins couvre le local et le traiteur pour la soirée."),
            seg(14_000, 15_000, Source::Salle, "La subvention de Desjardins couvre le local et le traiteur pour la soirée."),
        ];
        let p = nettoyer(&s);
        assert_eq!(
            textes(&p),
            ["Merci.", "On va voir ça demain.", "C'est beau, c'est beau.", "La subvention de Desjardins couvre le local et le traiteur pour la soirée."]
        );
        // Les segments d'une boucle retirée restent rattachés à la phrase gardée.
        assert_eq!(p[3].segments, vec![3, 4, 5]);
        assert_eq!((p[3].debut_ms, p[3].fin_ms), (12_000, 15_000));
    }

    #[test]
    fn boucle_longue_de_phrases() {
        let longue = "Alors comme je le disais tantôt au comité on a reçu la lettre du ministère qui confirme le montant pour les trois prochaines années pis on attend encore la signature finale.";
        let s: Vec<Segment> = (0..4).map(|i| seg(i * 10_000, i * 10_000 + 9000, Source::Salle, longue)).collect();
        let p = nettoyer(&s);
        assert_eq!(textes(&p), [longue]);
        assert_eq!(p[0].segments, vec![0, 1, 2, 3]);
    }

    #[test]
    fn hesitations_et_mots_doubles() {
        let s = [
            seg(0, 1000, Source::Salle, "Euh, bon ben on commence-tu?"),
            seg(1000, 2000, Source::Salle, "Le le budget est, euh, correct."),
            seg(2000, 3000, Source::Salle, "Euh..."),
            seg(3000, 4000, Source::Salle, "Nous nous sommes entendus, hum."),
            seg(4000, 5000, Source::Salle, "Bon ben."),
        ];
        let p = nettoyer(&s);
        assert_eq!(textes(&p), ["On commence-tu?", "Le budget est, correct.", "Nous nous sommes entendus.", "Bon ben."]);
        // « Euh... » seul : son segment passe à la phrase suivante.
        assert_eq!(p[2].segments, vec![2, 3]);
    }

    #[test]
    fn quebecismes_gardes() {
        let s = [seg(0, 5000, Source::Salle, "Tsé, c'est correct pantoute, pis on va checker ça tantôt.")];
        assert_eq!(textes(&nettoyer(&s)), ["Tsé, c'est correct pantoute, pis on va checker ça tantôt."]);
        // Aucune hésitation de la liste n'est un mot de l'invite québécoise.
        let invite: HashSet<String> = quebec::BASE_PROMPT.split_whitespace().map(cle).collect();
        for h in &listes().hesitations {
            assert!(!invite.contains(h), "« {h} » est dans quebec::BASE_PROMPT");
        }
    }

    #[test]
    fn hallucinations_lexique_et_typographie() {
        let s = [
            seg(0, 1000, Source::Salle, "Sous-titrage Société Radio-Canada"),
            seg(1000, 2000, Source::Salle, "On appelle hydro québec à 14h30 pour les $500 ?"),
        ];
        let v: Vec<(usize, &Segment)> = s.iter().enumerate().collect();
        let p = passe1(&v, Some(2), &Glossary::parse("hydro québec => Hydro-Québec"));
        assert_eq!(textes(&p), ["On appelle Hydro-Québec à 14 h 30 pour les 500 $?"]);
        assert_eq!((p[0].point, p[0].segments.clone()), (Some(2), vec![1]));
    }

    #[test]
    fn passe1_deterministe() {
        let s = [
            seg(0, 1000, Source::Salle, "Euh on on on va commencer"),
            seg(1200, 3000, Source::Salle, "commencer la rencontre. Merci. Merci. Merci."),
        ];
        let a = nettoyer(&s);
        assert_eq!(a, nettoyer(&s));
        assert_eq!(textes(&a), ["On va commencer la rencontre.", "Merci."]);
    }

    // ── Passe 2 ──

    fn phrases(textes: &[&str]) -> Vec<Phrase> {
        textes
            .iter()
            .enumerate()
            .map(|(i, t)| Phrase { point: None, debut_ms: i as u64 * 1000, fin_ms: i as u64 * 1000 + 900, source: Source::Salle, texte: t.to_string(), segments: vec![i] })
            .collect()
    }

    const ENTREE: [&str; 3] = ["on a reçu 2 500 $ du ministère", "c'est correct pantoute", "la prochaine rencontre est le 19 novembre"];

    #[test]
    fn garde_fous_acceptent_une_correction() {
        let e = phrases(&ENTREE);
        let sortie = "Voici les phrases :\n1. On a reçu 2 500 $ du ministère.\n2) C'est correct pantoute.\n3. La prochaine rencontre est\nle 19 novembre.";
        assert_eq!(
            verifier(&e, sortie).unwrap(),
            ["On a reçu 2 500 $ du ministère.", "C'est correct pantoute.", "La prochaine rencontre est le 19 novembre."]
        );
    }

    #[test]
    fn garde_fous_rejettent() {
        let e = phrases(&ENTREE);
        let cas = [
            ("1. On a reçu de l'argent.\n2. Correct.\n3. Rencontre le 19.", "trop courte"),
            (
                "1. On a reçu 2 500 $ du ministère de l'Éducation pour le projet.\n2. C'est tout à fait correct pantoute.\n3. La prochaine rencontre est le 19 novembre.",
                "trop longue",
            ),
            ("1. On a reçu 2 500 $ du ministère.\n3. La prochaine rencontre est le 19 novembre, c'est correct.", "manquants : 2"),
            ("1. On a reçu 2 500 $ du ministère.\n2. C'est correct pantoute.\n3. La prochaine rencontre est le 19 novembre.\n4. Merci.", "en trop : 4"),
            ("1. On a reçu 2 500 $ du ministère.\n1. On a reçu 2 500 $.\n2. C'est correct pantoute.\n3. Le 19 novembre.", "1 en double"),
            ("1. On a reçu 2 400 $ du ministère.\n2. C'est correct pantoute.\n3. La prochaine rencontre est le 19 novembre.", "disparus : 2500"),
        ];
        for (sortie, raison) in cas {
            let r = verifier(&e, sortie).unwrap_err();
            assert!(r.contains(raison), "« {r} » ne contient pas « {raison} »");
        }
    }

    #[test]
    fn passe2_garde_la_passe1_quand_la_sortie_est_rejetee() {
        let mut p = phrases(&ENTREE);
        let mut reponses = vec![
            Ok("1. On a reçu 2 500 $ du ministère.\n2. C'est correct pantoute.\n3. La prochaine rencontre est le 19 novembre.".to_string()),
        ]
        .into_iter();
        let mut rediger = |_: &str| reponses.next().unwrap_or_else(|| Ok(String::new()));
        let mut lignes = Vec::new();
        let ctx = Contexte { termes: &[], titres: &[] };
        let fini = passe2(&mut p, &ctx, &Glossary::parse(""), &mut rediger, "Hors point", &mut |l| lignes.push(l.to_string()), &AtomicBool::new(false));
        assert!(fini);
        assert_eq!(p[1].texte, "C'est correct pantoute.");
        assert_eq!(lignes, ["Hors point, morceau 1 sur 1 (phrases 1 à 3) : accepté"]);

        // Sortie rejetée, puis erreur du modèle : la passe 1 reste.
        let mut p = phrases(&ENTREE);
        let mut lignes = Vec::new();
        let mut rediger = |_: &str| Ok("1. Résumé : argent reçu.".to_string());
        passe2(&mut p, &ctx, &Glossary::parse(""), &mut rediger, "Hors point", &mut |l| lignes.push(l.to_string()), &AtomicBool::new(false));
        assert_eq!(p, phrases(&ENTREE));
        assert!(lignes[0].contains("rejeté, passe 1 gardée — numéros manquants : 2, 3"), "{lignes:?}");
        let mut rediger = |_: &str| -> Result<String> { anyhow::bail!("mémoire insuffisante") };
        let mut lignes = Vec::new();
        passe2(&mut p, &ctx, &Glossary::parse(""), &mut rediger, "Hors point", &mut |l| lignes.push(l.to_string()), &AtomicBool::new(false));
        assert_eq!(p, phrases(&ENTREE));
        assert!(lignes[0].ends_with("mémoire insuffisante"));
    }

    #[test]
    fn invite_et_morceaux() {
        let mut p = phrases(&ENTREE);
        let ctx = Contexte { termes: &["Hydro-Québec".to_string()], titres: &["Ouverture".to_string(), "Budget".to_string()] };
        let i = invite(&p, &ctx);
        assert!(i.contains("Termes à écrire exactement ainsi : Hydro-Québec."));
        assert!(i.contains("Points de l'ordre du jour : Ouverture ; Budget."));
        assert!(i.ends_with("1. on a reçu 2 500 $ du ministère\n2. c'est correct pantoute\n3. la prochaine rencontre est le 19 novembre\n"));
        assert_eq!(morceaux(&p), vec![0..3]);
        p[2].debut_ms = 300_000;
        p[2].fin_ms = 301_000;
        assert_eq!(morceaux(&p), vec![0..2, 2..3]);
    }

    // ── Déroulement ──

    fn rencontre() -> Meeting {
        let mut m = Meeting::new("test.bin");
        m.agenda = Some(Agenda::new("od.txt".into(), "1. Ouverture\n2. Budget".into()));
        m.insert(seg(0, 1000, Source::Salle, "Euh bonjour tout le monde."));
        m.debuter_point(Some(0), 2000, Declenchement::Manuel);
        m.insert(seg(2000, 3000, Source::Salle, "On ouvre la séance"));
        m.insert(seg(3200, 4000, Source::Salle, "la séance à 19 h."));
        m.debuter_point(Some(1), 5000, Declenchement::Manuel);
        m.insert(seg(5000, 6000, Source::Salle, "Le le budget est adopté."));
        m.fermer_periode(7000);
        m
    }

    #[test]
    fn point_par_point_et_reprise() {
        let m = rencontre();
        let brut = serde_json::to_string(&m.segments).unwrap();
        let av = Avancement::default();
        let mut etats = Vec::new();
        let n = executer(&m, None, &Glossary::parse(""), None, &av, &mut |_| {}, &mut |n| etats.push(n.groupes_faits));
        assert!(n.termine() && n.sans_modele);
        assert_eq!(etats, [1, 2, 3]);
        assert_eq!(serde_json::to_string(&m.segments).unwrap(), brut, "la transcription brute n'est jamais modifiée");
        let points: Vec<_> = n.phrases.iter().map(|p| (p.point, p.texte.as_str(), p.segments.clone())).collect();
        assert_eq!(
            points,
            [
                (None, "Bonjour tout le monde.", vec![0]),
                (Some(0), "On ouvre la séance à 19 h.", vec![1, 2]),
                (Some(1), "Le budget est adopté.", vec![3]),
            ]
        );

        // Interrompu après le premier groupe : la reprise ne refait que la suite.
        let partiel = Nettoyage { phrases: n.phrases[..1].to_vec(), groupes_faits: 1, groupes: 3, segments: 4, suspendu: true, ..Default::default() };
        let mut appels = 0;
        let mut rediger = |invite: &str| {
            appels += 1;
            assert!(!invite.contains("Bonjour"));
            let lignes: Vec<String> = invite.split("=== PHRASES ===\n").nth(1).unwrap().lines().map(str::to_string).collect();
            Ok(lignes.join("\n"))
        };
        let reprise = executer(&m, Some(partiel), &Glossary::parse(""), Some(&mut rediger), &av, &mut |_| {}, &mut |_| {});
        assert_eq!(appels, 2);
        assert!(reprise.termine() && !reprise.suspendu && !reprise.sans_modele);
        assert_eq!(reprise.phrases, n.phrases);

        // Transcription modifiée depuis : on recommence au début.
        let perime = Nettoyage { groupes_faits: 1, groupes: 3, segments: 2, ..Default::default() };
        assert_eq!(executer(&m, Some(perime), &Glossary::parse(""), None, &av, &mut |_| {}, &mut |_| {}).phrases, n.phrases);
    }

    #[test]
    fn annulation() {
        let m = rencontre();
        let av = Avancement::default();
        av.annuler.store(true, Ordering::Relaxed);
        let n = executer(&m, None, &Glossary::parse(""), None, &av, &mut |_| {}, &mut |_| {});
        assert!(!n.termine());
        assert_eq!(n.groupes_faits, 0);
    }
}
