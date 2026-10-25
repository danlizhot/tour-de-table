//! Nettoyage de la transcription, entre la transcription et le compte rendu.
//!
//! La transcription brute (`Meeting::segments`) n'est jamais modifiée : le résultat est une liste
//! de phrases, chacune rattachée à son point et aux segments bruts d'où elle vient (lecture
//! synchronisée). Le travail se fait point par point, dans l'ordre chronologique, et reprend au
//! point suivant après une interruption.
//!
//! Tout se fait par règles, sans modèle de langue (quelques secondes pour une rencontre de 2 h) :
//! phrases coupées entre deux énoncés recollées, boucles de Whisper réduites, hésitations,
//! expressions de remplissage et simples interjections retirées (`data/nettoyage.txt`), puis
//! lexique et typographie OQLF de `quebec`. Les québécismes porteurs de sens (« tsé »,
//! « pantoute », « correct »…) ne sont jamais retirés. Le modèle de langue sert seulement au
//! compte rendu : une passe de correction par le modèle a été mesurée à plus que le temps réel
//! sur un poste sans carte graphique.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use crossbeam_channel::Receiver;
use serde::{Deserialize, Serialize};

use crate::audio::Source;
use crate::quebec::{self, Glossary};
use crate::storage::Meeting;
use crate::stt::Segment;

/// Écart maximal entre les deux énoncés d'une phrase coupée.
const ECART_RECOLLE_MS: u64 = 1500;
/// Plus long chevauchement cherché entre deux énoncés recollés, en mots.
const CHEVAUCHEMENT_MAX: usize = 8;
/// Plus longue boucle de Whisper cherchée, en mots.
const BOUCLE_MAX: usize = 30;
/// À partir de combien de répétitions d'affilée une suite est une boucle.
const REPETITIONS_BOUCLE: usize = 3;
/// Plus courte phrase faite seulement d'interjections (« Ok. », « Oui, merci. ») retirée, en mots.
const INTERJECTION_MAX: usize = 5;

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
    /// Ancienne passe de correction par le modèle, retirée : toujours faux. Gardé pour lire les
    /// rencontres déjà nettoyées.
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
    /// Les plus longues d'abord (« je veux dire » avant un éventuel « je »).
    entre_virgules: Vec<Vec<String>>,
    interjections: HashSet<String>,
    /// En minuscules, cherchés tels quels dans la phrase.
    indices_decision: Vec<String>,
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
            "entre-virgules" if !cles.is_empty() => l.entre_virgules.push(cles),
            "interjection" => l.interjections.extend(cles),
            "indice-decision" => l.indices_decision.push(apostrophes(&ligne.to_lowercase())),
            "double-permis" => l.doubles_permis.extend(cles),
            _ => {}
        }
    }
    l.entre_virgules.sort_by_key(|e| std::cmp::Reverse(e.len()));
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
    apostrophes(&mot.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
}

fn apostrophes(s: &str) -> String {
    s.replace(['’', '‘', 'ʼ'], "'")
}

fn commence_en_majuscule(mot: &str) -> bool {
    mot.chars().next().is_some_and(char::is_uppercase)
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
            // Simple interjection (« Ok. ») : retirée, sauf en réponse à une question ou à une
            // proposition, où elle peut valoir un vote.
            let reponse = retenues.last().is_some_and(|(_, prec)| appelle_reponse(prec));
            if retirer_remplissage(&mut p) && (reponse || !est_interjection(&p)) {
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
        let brut = finaliser(&mots.iter().map(|m| m.texte.as_str()).collect::<Vec<_>>().join(" "));
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

/// Retire les hésitations isolées, « bon ben » en début de phrase et les expressions de
/// remplissage isolées entre virgules. Un mot à majuscule en milieu de phrase est gardé : c'est
/// sans doute un nom (« avec Ben hier »). Faux si rien ne reste.
fn retirer_remplissage(p: &mut Vec<Mot>) -> bool {
    let l = listes();
    let mut debut_retire = false;
    let mut i = 0;
    while i < p.len() {
        if !l.hesitations.contains(&p[i].cle) || i > 0 && commence_en_majuscule(&p[i].texte) {
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
        } else if finit_phrase(&retire.texte) {
            p[i - 1].absorber(&[retire]);
        } else {
            // « que, euh, le » → « que le » : la virgule d'ouverture part aussi.
            let prec = &mut p[i - 1];
            if ponctuation(&retire.texte).ends_with(',') && prec.texte.ends_with(',') {
                prec.texte.pop();
            }
            prec.segments.extend(retire.segments);
        }
    }
    let debut = |e: &Vec<String>, p: &[Mot]| {
        p.len() > e.len() && p.iter().zip(e).enumerate().all(|(k, (m, c))| m.cle == *c && (k == 0 || !commence_en_majuscule(&m.texte)))
    };
    while let Some(expr) = l.debuts.iter().find(|e| debut(e, p)) {
        let retires: Vec<Mot> = p.drain(..expr.len()).collect();
        p[0].segments.extend(retires.iter().flat_map(|m| m.segments.iter().copied()));
        debut_retire = true;
    }
    let mut i = 0;
    while i < p.len() {
        let apres_virgule = i == 0 || ponctuation(&p[i - 1].texte).ends_with(',');
        let trouve = l
            .entre_virgules
            .iter()
            .find(|e| {
                let fin = i + e.len();
                apres_virgule
                    && fin <= p.len()
                    && !(i == 0 && fin == p.len())
                    && p[i..fin].iter().zip(e.iter()).all(|(m, c)| m.cle == *c)
                    && (ponctuation(&p[fin - 1].texte).ends_with(',') || i > 0 && fin == p.len())
            })
            .map(Vec::len);
        let Some(n) = trouve else {
            i += 1;
            continue;
        };
        let retires: Vec<Mot> = p.drain(i..i + n).collect();
        if i == 0 {
            p[0].segments.extend(retires.iter().flat_map(|m| m.segments.iter().copied()));
            debut_retire = true;
        } else {
            // « que, genre, le » → « que le » : la virgule d'ouverture part aussi ; « fini, genre. »
            // → « fini. » : le point passe au mot précédent.
            let prec = &mut p[i - 1];
            if prec.texte.ends_with(',') {
                prec.texte.pop();
            }
            let fin = retires.last().filter(|m| finit_phrase(&m.texte)).cloned();
            prec.segments.extend(retires.iter().flat_map(|m| m.segments.iter().copied()));
            if let Some(m) = fin {
                prec.absorber(&[m]);
            }
        }
    }
    if debut_retire {
        majuscule_initiale(&mut p[0].texte);
    }
    true
}

/// Phrase faite seulement d'interjections (« Ok. », « Oui, merci. », « Tout à fait. »).
fn est_interjection(p: &[Mot]) -> bool {
    let mots: Vec<&str> = p.iter().map(|m| m.cle.as_str()).filter(|c| !c.is_empty()).collect();
    !mots.is_empty() && mots.len() <= INTERJECTION_MAX && mots.iter().all(|m| listes().interjections.contains(*m))
}

/// Question ou proposition : la réponse qui suit compte (« Oui. » après « On vote? »).
fn appelle_reponse(p: &[Mot]) -> bool {
    let texte = apostrophes(&p.iter().map(|m| m.texte.as_str()).collect::<Vec<_>>().join(" ").to_lowercase());
    texte.trim_end().ends_with('?') || listes().indices_decision.iter().any(|i| texte.contains(i.as_str()))
}

/// Remet la ponctuation en ordre après les retraits et met la majuscule initiale.
fn finaliser(texte: &str) -> String {
    let mut t = texte.to_string();
    for (a, b) in [(" ,", ","), (",,", ","), (", .", "."), (",.", "."), (", ?", "?"), (",?", "?"), (", !", "!"), (",!", "!"), (" .", "."), ("  ", " ")] {
        while t.contains(a) {
            t = t.replace(a, b);
        }
    }
    let mut t = t.trim_start_matches([',', ';', ' ', '-', '–']).trim_end_matches([',', ' ', ';']).to_string();
    majuscule_initiale(&mut t);
    t
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
/// après chaque groupe terminé.
pub fn executer(m: &Meeting, depart: Option<Nettoyage>, glossaire: &Glossary, avancement: &Avancement, apres_groupe: &mut dyn FnMut(&Nettoyage)) -> Nettoyage {
    let groupes = groupes(m);
    let mut n = match depart {
        Some(n) if n.segments == m.segments.len() && n.groupes == groupes.len() => n,
        _ => Nettoyage { groupes: groupes.len(), segments: m.segments.len(), ..Default::default() },
    };
    n.suspendu = false;
    avancement.groupes.store(groupes.len(), Ordering::Relaxed);
    for (k, (point, indices)) in groupes.iter().enumerate().skip(n.groupes_faits) {
        if avancement.annuler.load(Ordering::Relaxed) {
            break;
        }
        avancement.groupe.store(k + 1, Ordering::Relaxed);
        let segs: Vec<(usize, &Segment)> = indices.iter().map(|&i| (i, &m.segments[i])).collect();
        n.phrases.extend(passe1(&segs, *point, glossaire));
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

/// Nettoie `m` dans un fil, en reprenant à partir de `m.nettoyage`.
pub fn lancer(m: Meeting, glossaire: String, avancement: Arc<Avancement>) -> Receiver<Etape> {
    let (tx, rx) = crossbeam_channel::unbounded();
    std::thread::spawn(move || {
        let glossaire = Glossary::parse(&glossaire);
        let fin = executer(&m, m.nettoyage.clone(), &glossaire, &avancement, &mut |n| {
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
            // « Oui » répond à une question : gardé ; « Parfait. » seul est une simple interjection.
            ["On va regarder le budget pour l'an prochain.", "C'est correct?", "Il reste", "Deux points.", "Puis", "On m'entend-tu?", "Oui"]
        );
        assert_eq!((p[0].segments.clone(), p[0].debut_ms, p[0].fin_ms), (vec![0, 1], 0, 8000));
        assert_eq!(p[1].segments, vec![1]);
        assert_eq!(p[5].source, Source::Distance);
    }

    #[test]
    fn boucles_de_whisper() {
        let s = [
            seg(0, 1000, Source::Salle, "On commence. On commence. On commence. On commence."),
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
            ["On commence.", "On va voir ça demain.", "C'est beau, c'est beau.", "La subvention de Desjardins couvre le local et le traiteur pour la soirée."]
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
        assert_eq!(textes(&p), ["On commence-tu?", "Le budget est correct.", "Nous nous sommes entendus.", "Bon ben."]);
        // « Euh... » seul : son segment passe à la phrase suivante.
        assert_eq!(p[2].segments, vec![2, 3]);
    }

    #[test]
    fn remplissage_entre_virgules() {
        let s = [
            seg(0, 1000, Source::Salle, "Je pense que, genre, le budget est correct."),
            seg(1000, 2000, Source::Salle, "Ce genre de dossier fait partie, en fait, du mandat."),
            seg(2000, 3000, Source::Salle, "En fait, on a fini, genre."),
            seg(3000, 4000, Source::Salle, "Comme je disais, c'est réglé."),
            seg(4000, 5000, Source::Salle, "Genre."),
        ];
        assert_eq!(
            textes(&nettoyer(&s)),
            ["Je pense que le budget est correct.", "Ce genre de dossier fait partie du mandat.", "On a fini.", "Comme je disais, c'est réglé.", "Genre."]
        );
    }

    #[test]
    fn interjections_et_votes() {
        let s = [
            seg(0, 1000, Source::Salle, "On commence par le point 1."),
            seg(1000, 2000, Source::Distance, "Ok."),
            seg(2000, 3000, Source::Salle, "Je propose d'adopter le budget."),
            seg(3000, 4000, Source::Distance, "Oui."),
            seg(4000, 5000, Source::Salle, "D'accord, merci."),
            seg(5000, 6000, Source::Salle, "Tu t'en occupes, Julie?"),
            seg(6000, 7000, Source::Distance, "Correct."),
            seg(7000, 8000, Source::Salle, "On passe au point suivant."),
            seg(8000, 9000, Source::Salle, "Non."),
        ];
        let p = nettoyer(&s);
        // Le vote et la réponse à la question restent ; « correct » et « non » ne sont jamais des interjections.
        assert_eq!(
            textes(&p),
            ["On commence par le point 1.", "Je propose d'adopter le budget.", "Oui.", "Tu t'en occupes, Julie?", "Correct.", "On passe au point suivant.", "Non."]
        );
        // Les segments des interjections retirées restent rattachés à une phrase voisine.
        assert_eq!(p[1].segments, vec![1, 2]);
        assert_eq!(p[3].segments, vec![4, 5]);
    }

    #[test]
    fn prenom_ben_et_ponctuation() {
        let s = [
            seg(0, 1000, Source::Salle, "Bon ben, j'en ai parlé avec Ben hier."),
            seg(1000, 2000, Source::Salle, "Bon, Ben a appelé la caisse."),
            seg(2000, 3000, Source::Salle, "On a reçu, euh, la lettre , hum."),
        ];
        assert_eq!(textes(&nettoyer(&s)), ["J'en ai parlé avec Ben hier.", "Bon, Ben a appelé la caisse.", "On a reçu la lettre."]);
    }

    #[test]
    fn quebecismes_gardes() {
        let s = [seg(0, 5000, Source::Salle, "Tsé, c'est correct pantoute, pis on va checker ça tantôt.")];
        assert_eq!(textes(&nettoyer(&s)), ["Tsé, c'est correct pantoute, pis on va checker ça tantôt."]);
        // Aucun mot retiré par les listes n'est un mot de l'invite québécoise.
        let invite: HashSet<String> = quebec::BASE_PROMPT.split_whitespace().map(cle).collect();
        let l = listes();
        let expressions = l.hesitations.iter().map(|h| vec![h.clone()]).chain(l.entre_virgules.iter().cloned());
        let prompt: Vec<String> = quebec::BASE_PROMPT.split_whitespace().map(cle).collect();
        for e in expressions {
            let dans = if e.len() == 1 { invite.contains(&e[0]) } else { prompt.windows(e.len()).any(|w| w == e) };
            assert!(!dans, "« {} » est dans quebec::BASE_PROMPT", e.join(" "));
        }
        for q in ["tsé", "pantoute", "correct", "pis", "tantôt"] {
            assert!(!l.interjections.contains(q) && !l.hesitations.contains(q), "{q}");
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
            seg(1200, 3000, Source::Salle, "commencer la rencontre. Bienvenue. Bienvenue. Bienvenue."),
        ];
        let a = nettoyer(&s);
        assert_eq!(a, nettoyer(&s));
        assert_eq!(textes(&a), ["On va commencer la rencontre.", "Bienvenue."]);
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
        let n = executer(&m, None, &Glossary::parse(""), &av, &mut |n| etats.push(n.groupes_faits));
        assert!(n.termine() && !n.sans_modele);
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
        // Phrase du premier groupe modifiée : la reprise la garde telle quelle.
        let mut deja = n.phrases[..1].to_vec();
        deja[0].texte = "Déjà nettoyé.".into();
        let partiel = Nettoyage { phrases: deja, groupes_faits: 1, groupes: 3, segments: 4, suspendu: true, ..Default::default() };
        let mut etats = Vec::new();
        let reprise = executer(&m, Some(partiel), &Glossary::parse(""), &av, &mut |n| etats.push(n.groupes_faits));
        assert_eq!(etats, [2, 3]);
        assert!(reprise.termine() && !reprise.suspendu);
        assert_eq!(reprise.phrases[0].texte, "Déjà nettoyé.");
        assert_eq!(reprise.phrases[1..], n.phrases[1..]);

        // Transcription modifiée depuis : on recommence au début.
        let perime = Nettoyage { groupes_faits: 1, groupes: 3, segments: 2, ..Default::default() };
        assert_eq!(executer(&m, Some(perime), &Glossary::parse(""), &av, &mut |_| {}).phrases, n.phrases);
    }

    #[test]
    fn annulation() {
        let m = rencontre();
        let av = Avancement::default();
        av.annuler.store(true, Ordering::Relaxed);
        let n = executer(&m, None, &Glossary::parse(""), &av, &mut |_| {});
        assert!(!n.termine());
        assert_eq!(n.groupes_faits, 0);
    }
}
