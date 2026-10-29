//! Découpage automatique par point : reconnaît dans la parole transcrite l'annonce d'un
//! changement de point de l'ordre du jour (« on passe au point 4 », « point suivant », titre
//! d'un point…) et lui donne un score de 0 à 100. Aucune dépendance à l'interface : on lui
//! donne chaque segment final et l'état du découpage, il répond par une détection.
//!
//! Les expressions sont dans `data/decoupage.txt`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::OnceLock;

use crate::agenda::Point;
use crate::storage::Declenchement;
use crate::stt::Segment;

/// Score minimal pour proposer un changement de point.
pub const SEUIL: u8 = 75;
/// Fenêtre où l'on cherche la mention du titre d'un point.
pub const FENETRE_TITRE_MS: u64 = 15_000;
/// Un point commencé depuis moins longtemps est rarement déjà terminé.
pub const DEBUT_RECENT_MS: u64 = 30_000;
/// Écart minimal entre le début du point actif et la frontière d'un changement automatique.
pub const ECART_MINIMAL_MS: u64 = 1000;
/// Nom du journal des détections, dans le dossier de la rencontre.
pub const JOURNAL: &str = "decoupage.log";

const FORTE: i32 = 90;
const FAIBLE: i32 = 80;
const SANS_ANNONCE: i32 = 60;
const SUIVANT: i32 = 70;
const MOT_CLE: i32 = 80;
const TITRE: f32 = 40.0;
const TITRE_COMPLET: i32 = 10;
const TITRE_INTRODUIT: i32 = 25;
const TITRE_AVEC_NUMERO: i32 = 10;
const PREVU: i32 = 15;
const PENALITE_RECENT: i32 = 25;
const PENALITE_SAUT: i32 = 30;
const PENALITE_RECUL: i32 = 15;
const PENALITE_PASSE: i32 = 50;
const PENALITE_RESTRICTION: i32 = 40;
/// Alignement hors ligne : score minimal d'une frontière, une fois l'ordre du jour pris en compte.
const SEUIL_ALIGNEMENT: i32 = 55;
/// Point sauté sans annonce forte (jusqu'à `SAUT_MAX`).
const SAUT_PAR_POINT: i32 = 10;
const SAUT_MAX: i32 = 30;
/// Retour à un point déjà atteint sans annonce forte.
const REVISITE: i32 = 30;
/// Mots autour d'une mention où chercher une marque du passé.
const PORTEE_PASSE: usize = 6;

const DONNEES: &str = include_str!("../data/decoupage.txt");

type Expr = Vec<String>;

#[derive(Default)]
struct Expressions {
    annonces: Vec<Expr>,
    faibles: Vec<Expr>,
    suivants: Vec<Expr>,
    titres: Vec<(Expr, Vec<String>)>,
    introductions: Vec<Expr>,
    passe: Vec<Expr>,
    restrictions: Vec<Expr>,
    vides: HashSet<String>,
}

fn expressions() -> &'static Expressions {
    static E: OnceLock<Expressions> = OnceLock::new();
    E.get_or_init(|| lire_expressions(DONNEES))
}

fn lire_expressions(texte: &str) -> Expressions {
    let mut e = Expressions::default();
    let mut section = "";
    for ligne in texte.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        if let Some(nom) = ligne.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = match nom {
                "annonce" | "annonce-faible" | "suivant" | "titre" | "introduction" | "passe" | "restriction" | "vides" => nom,
                _ => "",
            };
            continue;
        }
        match section {
            "annonce" => e.annonces.push(normaliser(ligne)),
            "annonce-faible" => e.faibles.push(normaliser(ligne)),
            "suivant" => e.suivants.push(normaliser(ligne)),
            "introduction" => e.introductions.push(normaliser(ligne)),
            "passe" => e.passe.push(normaliser(ligne)),
            "restriction" => e.restrictions.push(normaliser(ligne)),
            "vides" => e.vides.extend(normaliser(ligne)),
            "titre" => {
                if let Some((expr, cles)) = ligne.split_once('=') {
                    let cles = cles.split('|').flat_map(normaliser).collect();
                    e.titres.push((normaliser(expr), cles));
                }
            }
            _ => {}
        }
    }
    e.annonces.retain(|a| !a.is_empty());
    e.faibles.retain(|a| !a.is_empty());
    e
}

// ───────────────────────── Normalisation ─────────────────────────

/// Minuscules sans accents, découpées en mots ; apostrophes et traits d'union séparent les mots
/// (« s'en » → « s en », « dix-sept » → « dix sept »). « 4.2 » et « 4,2 » donnent « 4.2 ».
pub fn normaliser(texte: &str) -> Vec<String> {
    let chars: Vec<char> = texte.chars().flat_map(char::to_lowercase).collect();
    let mut mots = Vec::new();
    let mut mot = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c.is_alphanumeric() {
            sans_accent(c, &mut mot);
        } else if matches!(c, '.' | ',')
            && mot.ends_with(|d: char| d.is_ascii_digit())
            && chars.get(i + 1).is_some_and(char::is_ascii_digit)
        {
            mot.push('.');
        } else if !mot.is_empty() {
            mots.push(std::mem::take(&mut mot));
        }
    }
    if !mot.is_empty() {
        mots.push(mot);
    }
    mots
}

fn sans_accent(c: char, out: &mut String) {
    match c {
        'à' | 'â' | 'ä' | 'á' => out.push('a'),
        'é' | 'è' | 'ê' | 'ë' => out.push('e'),
        'î' | 'ï' | 'í' => out.push('i'),
        'ô' | 'ö' | 'ó' => out.push('o'),
        'ù' | 'û' | 'ü' | 'ú' => out.push('u'),
        'ç' => out.push('c'),
        'ÿ' => out.push('y'),
        'œ' => out.push_str("oe"),
        'æ' => out.push_str("ae"),
        _ => out.push(c),
    }
}

fn finit_par(mots: &[String], fin: usize, expr: &[String]) -> bool {
    fin >= expr.len() && fin <= mots.len() && mots[fin - expr.len()..fin] == *expr
}

fn positions<'a>(mots: &'a [String], expr: &'a [String]) -> impl Iterator<Item = usize> + 'a {
    let n = expr.len();
    (0..(mots.len() + 1).saturating_sub(n)).filter(move |&i| n > 0 && mots[i..i + n] == *expr)
}

fn contient(mots: &[String], expr: &[String]) -> bool {
    positions(mots, expr).next().is_some()
}

// ───────────────────────── Nombres ─────────────────────────

fn petit(m: &str) -> Option<u32> {
    Some(match m {
        "un" | "une" => 1,
        "deux" => 2,
        "trois" => 3,
        "quatre" => 4,
        "cinq" => 5,
        "six" => 6,
        "sept" => 7,
        "huit" => 8,
        "neuf" => 9,
        "dix" => 10,
        "onze" => 11,
        "douze" => 12,
        "treize" => 13,
        "quatorze" => 14,
        "quinze" => 15,
        "seize" => 16,
        _ => return None,
    })
}

fn dizaine(m: &str) -> Option<u32> {
    Some(match m {
        "vingt" => 20,
        "trente" => 30,
        "quarante" => 40,
        "cinquante" => 50,
        "soixante" => 60,
        "septante" => 70,
        "huitante" | "octante" => 80,
        "nonante" => 90,
        _ => return None,
    })
}

/// Nombre en lettres de 1 à 99 au début de `mots` : (valeur, mots lus).
fn nombre_en_lettres<S: AsRef<str>>(mots: &[S]) -> Option<(u32, usize)> {
    let m = |k: usize| mots.get(k).map_or("", |s| s.as_ref());
    let jusqua_19 = |k: usize| -> Option<(u32, usize)> {
        let v = petit(m(k))?;
        match petit(m(k + 1)) {
            Some(u @ 7..=9) if v == 10 => Some((10 + u, 2)),
            _ => Some((v, 1)),
        }
    };
    let (base, lu) = if m(0) == "quatre" && matches!(m(1), "vingt" | "vingts") {
        (80, 2)
    } else if let Some(d) = dizaine(m(0)) {
        (d, 1)
    } else {
        return jusqua_19(0);
    };
    if m(lu) == "et"
        && let Some(u @ (1 | 11)) = petit(m(lu + 1))
    {
        return Some((base + u, lu + 2));
    }
    match jusqua_19(lu) {
        Some((u, n)) if u < 10 || base == 60 || base == 80 => Some((base + u, lu + n)),
        _ => Some((base, lu)),
    }
}

/// Radical cardinal d'un ordinal en lettres : « quatrième » → « quatre ».
fn radical(m: &str) -> Option<String> {
    let s = m.strip_suffix("ieme")?;
    Some(match s {
        "cinqu" => "cinq".into(),
        "neuv" => "neuf".into(),
        "vingt" | "sept" | "huit" => s.into(),
        _ if s.ends_with(['r', 'z', 't']) => format!("{s}e"),
        _ => s.into(),
    })
}

/// « 3e », « 3eme », « 1er », « 2nd »…
fn ordinal_en_chiffres(m: &str) -> Option<u32> {
    let i = m.find(|c: char| !c.is_ascii_digit())?;
    let (n, suffixe) = m.split_at(i);
    matches!(suffixe, "e" | "eme" | "ieme" | "er" | "re" | "ere" | "nd" | "nde").then(|| n.parse().ok()).flatten()
}

/// Ordinal au début de `mots` : (valeur, mots lus).
fn ordinal(mots: &[String]) -> Option<(u32, usize)> {
    let premier = mots.first()?.as_str();
    match premier {
        "premier" | "premiere" => return Some((1, 1)),
        "second" | "seconde" => return Some((2, 1)),
        _ => {}
    }
    if let Some(n) = ordinal_en_chiffres(premier) {
        return Some((n, 1));
    }
    // Le dernier mot porte la marque : « vingt et unième », « quatre-vingt-dixième ».
    for k in 1..=5.min(mots.len()) {
        let Some(r) = radical(&mots[k - 1]) else { continue };
        let mut v: Vec<&str> = mots[..k - 1].iter().map(String::as_str).collect();
        v.push(&r);
        if let Some((n, lu)) = nombre_en_lettres(&v)
            && lu == k
        {
            return Some((n, k));
        }
        return None;
    }
    None
}

/// Numéro cardinal au début de `mots` : « 4 », « 4.2 », « quatre », « quatre point deux ».
fn numero(mots: &[String]) -> Option<(String, usize)> {
    let simple = |mots: &[String]| -> Option<(String, usize)> {
        let m = mots.first()?;
        if m.chars().next()?.is_ascii_digit() {
            let ok = m.split('.').all(|c| !c.is_empty() && c.len() <= 2 && c.chars().all(|d| d.is_ascii_digit()));
            return ok.then(|| (m.trim_start_matches('0').to_string(), 1)).filter(|(n, _)| !n.is_empty());
        }
        nombre_en_lettres(mots).map(|(n, lu)| (n.to_string(), lu))
    };
    let (mut n, mut lu) = simple(mots)?;
    if !n.contains('.')
        && mots.get(lu).is_some_and(|m| m == "point")
        && let Some((sous, l)) = simple(&mots[lu + 1..]).filter(|(s, _)| !s.contains('.'))
    {
        n = format!("{n}.{sous}");
        lu += 1 + l;
    }
    Some((n, lu))
}

// ───────────────────────── Mentions d'un point numéroté ─────────────────────────

#[derive(Debug)]
struct Mention {
    numero: String,
    /// Premier mot de la mention (« point » ou l'ordinal).
    debut: usize,
    /// Mot qui suit la mention.
    fin: usize,
    /// Annonce reconnue avant la mention, et sa force.
    annonce: Option<(String, bool)>,
    numero_dit: bool,
}

fn mentions(mots: &[String], e: &Expressions) -> Vec<Mention> {
    let mut v = Vec::new();
    let annonce = |debut: usize, sans_point: bool| -> Option<(String, bool)> {
        let chercher = |liste: &[Expr], forte: bool| {
            liste.iter().find_map(|a| {
                let a = if sans_point { &a[..a.len() - 1] } else { &a[..] };
                (!a.is_empty() && finit_par(mots, debut, a)).then(|| (a.join(" "), forte))
            })
        };
        let liste_point = |l: &[Expr]| -> Vec<Expr> { l.iter().filter(|a| a.last().is_some_and(|m| m == "point")).cloned().collect() };
        chercher(&liste_point(&e.annonces), true).or_else(|| chercher(&liste_point(&e.faibles), false))
    };
    let mut i = 0;
    while i < mots.len() {
        // « point (numéro) 4 », « point quatre point deux »
        if mots[i] == "point" {
            let mut j = i + 1;
            let numero_dit = mots.get(j).is_some_and(|m| matches!(m.as_str(), "numero" | "no" | "num" | "n"));
            if numero_dit {
                j += 1;
            }
            if let Some((n, lu)) = numero(&mots[j.min(mots.len())..]) {
                v.push(Mention { numero: n, debut: i, fin: j + lu, annonce: annonce(i + 1, false), numero_dit });
                i = j + lu;
                continue;
            }
        }
        // « le troisième point »
        if let Some((n, lu)) = ordinal(&mots[i..])
            && mots.get(i + lu).is_some_and(|m| m == "point")
        {
            v.push(Mention { numero: n.to_string(), debut: i, fin: i + lu + 1, annonce: annonce(i, true), numero_dit: false });
            i += lu + 1;
            continue;
        }
        i += 1;
    }
    v
}

// ───────────────────────── Titres ─────────────────────────

fn mots_du_titre(titre: &str, e: &Expressions) -> Vec<String> {
    let mut v: Vec<String> = normaliser(titre).into_iter().filter(|m| m.len() > 1 && !e.vides.contains(m)).collect();
    v.dedup();
    v
}

fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut ligne: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diag = ligne[0];
        ligne[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let haut = ligne[j + 1];
            ligne[j + 1] = (haut + 1).min(ligne[j] + 1).min(diag + usize::from(ca != cb));
            diag = haut;
        }
    }
    ligne[b.len()]
}

/// Mots semblables : identiques, ou longs et à une faute près (« érables » / « érable »).
fn semblable(a: &str, b: &str) -> bool {
    a == b || (a.len() >= 5 && b.len() >= 5 && distance(a, b) <= 1 + usize::from(a.len().min(b.len()) >= 8))
}

// ───────────────────────── Détecteur ─────────────────────────

/// État du découpage au moment d'un segment.
pub struct Contexte<'a> {
    pub points: &'a [Point],
    /// Point de la période en cours.
    pub actif: Option<usize>,
    /// Dernier point traité (hors point : celui d'avant).
    pub dernier: Option<usize>,
    /// Début de la période en cours.
    pub debut_actif_ms: Option<u64>,
}

impl Contexte<'_> {
    /// Point suivant prévu : celui qui suit le point actif, ou le dernier traité.
    pub fn prevu(&self) -> Option<usize> {
        let p = match self.actif.or(self.dernier) {
            Some(a) => a + 1,
            None => 0,
        };
        (p < self.points.len()).then_some(p)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Detection {
    pub point: usize,
    pub score: u8,
    pub raison: String,
    /// Début de l'énoncé qui contient l'annonce : frontière entre les deux points.
    pub frontiere_ms: u64,
}

#[derive(Clone, Default)]
struct Candidat {
    score: i32,
    raisons: Vec<String>,
    /// Numéro ou ordinal dit explicitement.
    explicite: bool,
    /// Numéro précédé d'une annonce forte (« on passe au point 4 »).
    forte: bool,
    passe: bool,
    restriction: bool,
}

/// Candidats d'un segment, indépendants de l'état du découpage : un par point désigné, plus
/// l'annonce du point suivant, qui dépend du point actif.
struct Candidats {
    points: HashMap<usize, Candidat>,
    suivant: Option<Candidat>,
}

/// Garde le meilleur candidat de chaque point.
fn garder(candidats: &mut HashMap<usize, Candidat>, point: usize, c: Candidat) {
    let ancien = candidats.entry(point).or_default();
    if c.score > ancien.score {
        *ancien = c;
    }
}

#[derive(Default)]
pub struct Detecteur {
    /// Mots des segments récents : (fin, mots).
    recents: VecDeque<(u64, Vec<String>)>,
}

impl Detecteur {
    /// Candidats d'un segment final, sans bonus ni pénalité liés au point actif.
    fn candidats(&mut self, fin_ms: u64, texte: &str, points: &[Point]) -> Candidats {
        let e = expressions();
        let mots = normaliser(texte);
        self.recents.push_back((fin_ms, mots.clone()));
        while self.recents.front().is_some_and(|(f, _)| f + FENETRE_TITRE_MS < fin_ms) {
            self.recents.pop_front();
        }
        let passe_partout = e.passe.iter().any(|p| contient(&mots, p));
        let mut candidats: HashMap<usize, Candidat> = HashMap::new();
        let mut proposer = |point: usize, c: Candidat| garder(&mut candidats, point, c);

        // 1. Annonce et numéro
        for m in mentions(&mots, e) {
            let Some(point) = points.iter().position(|p| p.numero == m.numero) else { continue };
            let (score, raison) = match &m.annonce {
                Some((a, true)) => (FORTE, format!("annonce « {a} » et numéro {}", m.numero)),
                Some((a, false)) => (FAIBLE, format!("annonce faible « {a} » et numéro {}", m.numero)),
                None if m.numero_dit => (FAIBLE, format!("« point numéro {} »", m.numero)),
                None => (SANS_ANNONCE, format!("numéro {} sans annonce", m.numero)),
            };
            // Marque du passé : autour de la mention ; après seulement si l'annonce est forte
            // (« comme on disait, on passe au point 4 »).
            let forte = matches!(m.annonce, Some((_, true)));
            let de = if forte { m.fin } else { m.debut.saturating_sub(PORTEE_PASSE) };
            let a = (m.fin + PORTEE_PASSE).min(mots.len());
            let passe = e.passe.iter().any(|p| positions(&mots, p).any(|i| i >= de && i < a));
            let restriction = e.restrictions.iter().any(|r| finit_par(&mots, m.fin + r.len(), r));
            proposer(point, Candidat { score, raisons: vec![raison], explicite: true, forte, passe, restriction });
        }

        // 2. Annonce sans numéro
        let suivant = e.suivants.iter().find(|s| contient(&mots, s)).map(|s| Candidat {
            score: SUIVANT,
            raisons: vec![format!("annonce « {} »", s.join(" "))],
            passe: passe_partout,
            ..Default::default()
        });
        for (expr, cles) in &e.titres {
            if !contient(&mots, expr) {
                continue;
            }
            for (i, p) in points.iter().enumerate() {
                let titre = normaliser(&p.titre);
                if cles.iter().any(|c| titre.iter().any(|t| t.starts_with(c.as_str()))) {
                    let raisons = vec![format!("« {} » désigne « {} »", expr.join(" "), p.titre)];
                    proposer(i, Candidat { score: MOT_CLE, raisons, passe: passe_partout, ..Default::default() });
                }
            }
        }

        // 3. Titre d'un point dans les dernières secondes
        let fenetre: Vec<&String> = self.recents.iter().flat_map(|(_, m)| m).collect();
        let introduit = e.introductions.iter().find(|i| contient(&mots, i));
        let mut titres: HashMap<usize, (i32, String)> = HashMap::new();
        for (i, p) in points.iter().enumerate() {
            let t = mots_du_titre(&p.titre, e);
            if t.is_empty() || !t.iter().any(|w| mots.iter().any(|m| semblable(m, w))) {
                continue;
            }
            let trouves = t.iter().filter(|w| fenetre.iter().any(|m| semblable(m, w))).count();
            let part = trouves as f32 / t.len() as f32;
            if part < 0.5 {
                continue;
            }
            let mut score = (TITRE * part).round() as i32;
            let mut raison = format!("titre « {} » mentionné ({trouves}/{} mots)", p.titre, t.len());
            if t.len() >= 2 && part >= 0.66 {
                score += TITRE_COMPLET;
            }
            if let Some(intro) = introduit {
                score += TITRE_INTRODUIT;
                raison.push_str(&format!(", introduit par « {} »", intro.join(" ")));
            }
            titres.insert(i, (score, raison.clone()));
            proposer(i, Candidat { score, raisons: vec![raison], passe: passe_partout, ..Default::default() });
        }
        for (i, c) in candidats.iter_mut() {
            if c.explicite && titres.contains_key(i) {
                c.score += TITRE_AVEC_NUMERO;
                c.raisons.push(format!("titre mentionné (+{TITRE_AVEC_NUMERO})"));
            }
        }
        Candidats { points: candidats, suivant }
    }

    /// Analyse un segment final. Retourne le meilleur candidat, même sous le seuil (pour le
    /// journal), ou `None` si rien dans le segment ne désigne un point.
    pub fn analyser(&mut self, debut_ms: u64, fin_ms: u64, texte: &str, ctx: &Contexte) -> Option<Detection> {
        let Candidats { points: mut candidats, suivant } = self.candidats(fin_ms, texte, ctx.points);
        if let Some(c) = suivant
            && let Some(point) = ctx.prevu()
        {
            garder(&mut candidats, point, c);
        }
        if let Some(a) = ctx.actif {
            candidats.remove(&a);
        }

        // 4. Bonus et pénalités
        let prevu = ctx.prevu();
        let repere = ctx.actif.or(ctx.dernier);
        let mut meilleur: Option<(usize, Candidat)> = None;
        for (point, mut c) in candidats {
            if Some(point) == prevu {
                c.score += PREVU;
                c.raisons.push(format!("point suivant prévu (+{PREVU})"));
            }
            if let Some(d) = ctx.debut_actif_ms
                && debut_ms.saturating_sub(d) < DEBUT_RECENT_MS
            {
                c.score -= PENALITE_RECENT;
                c.raisons.push(format!("point actif commencé il y a moins de {} s (−{PENALITE_RECENT})", DEBUT_RECENT_MS / 1000));
            }
            if !c.explicite
                && let Some(r) = repere
            {
                if point.abs_diff(r) > 2 {
                    c.score -= PENALITE_SAUT;
                    c.raisons.push(format!("saut de plus de 2 points sans numéro (−{PENALITE_SAUT})"));
                } else if point < r {
                    c.score -= PENALITE_RECUL;
                    c.raisons.push(format!("retour en arrière sans numéro (−{PENALITE_RECUL})"));
                }
            }
            if c.passe {
                c.score -= PENALITE_PASSE;
                c.raisons.push(format!("mention au passé ou au futur (−{PENALITE_PASSE})"));
            }
            if c.restriction {
                c.score -= PENALITE_RESTRICTION;
                c.raisons.push(format!("« point » suivi d'un qualificatif (−{PENALITE_RESTRICTION})"));
            }
            c.score = c.score.clamp(0, 100);
            let mieux = match &meilleur {
                None => true,
                Some((p, m)) => (c.score, c.explicite, Some(point) == prevu) > (m.score, m.explicite, Some(*p) == prevu),
            };
            if mieux {
                meilleur = Some((point, c));
            }
        }
        meilleur.map(|(point, c)| Detection { point, score: c.score as u8, raison: c.raisons.join(" ; "), frontiere_ms: debut_ms })
    }
}

/// Début d'un point dans une rencontre déjà transcrite, proposé puis vérifié par l'utilisateur.
#[derive(Clone, Debug, PartialEq)]
pub struct Frontiere {
    /// Indice dans `Agenda::points` ; `None` : retour hors point.
    pub point: Option<usize>,
    pub debut_ms: u64,
    /// Paroles qui ont déclenché la détection (vide pour une frontière ajoutée à la main).
    pub extrait: String,
    pub raison: String,
    pub declenchement: Declenchement,
}

/// Score d'une frontière vers le point `vers` depuis l'état (`actif`, `atteint` : premier point
/// jamais atteint), commencé à `debut_actif_ms` ; `None` sous le seuil de l'alignement.
fn transition(c: &Candidat, actif: Option<usize>, atteint: usize, vers: usize, debut_actif_ms: Option<u64>, debut_ms: u64) -> Option<(i32, String)> {
    let mut score = c.score;
    let mut raisons = c.raisons.clone();
    if vers >= atteint {
        let saut = (vers - atteint) as i32;
        if vers == actif.map_or(0, |a| a + 1) {
            score += PREVU;
            raisons.push(format!("point suivant prévu (+{PREVU})"));
        } else if saut > 0 && !c.forte {
            let p = (SAUT_PAR_POINT * saut).min(SAUT_MAX);
            score -= p;
            raisons.push(format!("{saut} point(s) sauté(s) sans annonce forte (−{p})"));
        }
    } else if c.forte {
        score -= PENALITE_RECUL;
        raisons.push(format!("retour à un point déjà atteint (−{PENALITE_RECUL})"));
    } else {
        score -= REVISITE;
        raisons.push(format!("retour à un point déjà atteint sans annonce forte (−{REVISITE})"));
    }
    if debut_actif_ms.is_some_and(|d| debut_ms.saturating_sub(d) < DEBUT_RECENT_MS) {
        score -= PENALITE_RECENT;
        raisons.push(format!("point précédent commencé il y a moins de {} s (−{PENALITE_RECENT})", DEBUT_RECENT_MS / 1000));
    }
    if c.passe {
        score -= PENALITE_PASSE;
        raisons.push(format!("mention au passé ou au futur (−{PENALITE_PASSE})"));
    }
    if c.restriction {
        score -= PENALITE_RESTRICTION;
        raisons.push(format!("« point » suivi d'un qualificatif (−{PENALITE_RESTRICTION})"));
    }
    let score = score.clamp(0, 100);
    (score >= SEUIL_ALIGNEMENT).then(|| (score, format!("score {score} : {}", raisons.join(" ; "))))
}

/// Découpage d'une rencontre déjà transcrite (import). Toute la transcription est connue : au
/// lieu de décider segment par segment, on garde tous les candidats, même faibles, et on choisit
/// la suite de frontières la plus cohérente avec l'ordre du jour (programmation dynamique). Une
/// mention en passant (« au point 5, c'est l'horaire des fêtes », pendant le tour de table) perd
/// ainsi face aux vraies annonces qui suivent, et une simple mention du titre suffit quand elle
/// tombe au bon moment.
pub fn detecter_hors_ligne(segments: &[Segment], points: &[Point]) -> Vec<Frontiere> {
    /// Meilleure suite de frontières menant à un état.
    #[derive(Clone, Copy)]
    struct Etat {
        gain: i32,
        noeud: Option<usize>,
        debut_ms: Option<u64>,
    }
    /// Frontière d'une suite, chaînée à la précédente.
    struct Noeud {
        precedent: Option<usize>,
        segment: usize,
        point: usize,
        raison: String,
    }
    let n = points.len();
    // État : (point actif, premier point jamais atteint), le point actif décalé de 1 (0 : aucun).
    let indice = |actif: Option<usize>, atteint: usize| actif.map_or(0, |a| a + 1) * (n + 1) + atteint;
    let mut etats: Vec<Option<Etat>> = vec![None; (n + 1) * (n + 1)];
    etats[0] = Some(Etat { gain: 0, noeud: None, debut_ms: None });
    let mut noeuds: Vec<Noeud> = Vec::new();
    let mut detecteur = Detecteur::default();
    for (k, s) in segments.iter().enumerate() {
        let Candidats { points: candidats, suivant } = detecteur.candidats(s.end_ms, &s.text, points);
        if candidats.is_empty() && suivant.is_none() {
            continue;
        }
        // Une seule frontière par segment : on part des états d'avant ce segment.
        let avant = etats.clone();
        for (i, e) in avant.iter().enumerate() {
            let Some(e) = e else { continue };
            if e.debut_ms.is_some_and(|d| s.start_ms < d + ECART_MINIMAL_MS) {
                continue;
            }
            let (actif, atteint) = ((i / (n + 1)).checked_sub(1), i % (n + 1));
            let prevu = actif.map_or(0, |a| a + 1);
            let suivant = suivant.as_ref().filter(|_| prevu < n).map(|c| (prevu, c));
            for (vers, c) in candidats.iter().map(|(p, c)| (*p, c)).chain(suivant) {
                if Some(vers) == actif {
                    continue;
                }
                let Some((score, raison)) = transition(c, actif, atteint, vers, e.debut_ms, s.start_ms) else { continue };
                // Chaque frontière retenue ajoute au moins 1 : à égalité de candidats, la plus forte.
                let gain = e.gain + score - SEUIL_ALIGNEMENT + 1;
                let j = indice(Some(vers), atteint.max(vers + 1));
                if etats[j].is_none_or(|x| gain > x.gain) {
                    noeuds.push(Noeud { precedent: e.noeud, segment: k, point: vers, raison });
                    etats[j] = Some(Etat { gain, noeud: Some(noeuds.len() - 1), debut_ms: Some(s.start_ms) });
                }
            }
        }
    }
    let meilleur = etats.iter().flatten().max_by_key(|e| e.gain).and_then(|e| e.noeud);
    let mut frontieres = Vec::new();
    let mut courant = meilleur;
    while let Some(i) = courant {
        let nd = &noeuds[i];
        let s = &segments[nd.segment];
        frontieres.push(Frontiere {
            point: Some(nd.point),
            debut_ms: s.start_ms,
            extrait: s.text.clone(),
            raison: nd.raison.clone(),
            declenchement: Declenchement::Auto,
        });
        courant = nd.precedent;
    }
    frontieres.reverse();
    frontieres
}

/// Ajoute une ligne horodatée au journal des détections de la rencontre.
pub fn journaliser(dossier: &Path, ligne: &str) {
    use std::io::Write;
    let r = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dossier.join(JOURNAL))
        .and_then(|mut f| writeln!(f, "{} | {ligne}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S")));
    if let Err(e) = r {
        log::warn!("Journal du découpage : {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ordre_du_jour() -> Vec<Point> {
        crate::agenda::points(
            "1. Ouverture de l'assemblée\n2. Adoption de l'ordre du jour\n3. Adoption du procès-verbal\n4. Budget 2027\n  4.1 Patinoire municipale\n  4.2 Parc des Érables\n5. Correspondance\n6. Varia\n7. Levée de l'assemblée",
        )
    }

    /// Indice du point de numéro donné.
    fn idx(points: &[Point], numero: &str) -> usize {
        points.iter().position(|p| p.numero == numero).unwrap()
    }

    /// Analyse une phrase dite 5 minutes après le début du point actif (ou `depuis_s`).
    fn analyser(phrase: &str, actif: &str, depuis_s: u64) -> Option<Detection> {
        let points = ordre_du_jour();
        let actif = idx(&points, actif);
        let ctx = Contexte { points: &points, actif: Some(actif), dernier: Some(actif), debut_actif_ms: Some(600_000) };
        let debut = 600_000 + depuis_s * 1000;
        Detecteur::default().analyser(debut, debut + 3000, phrase, &ctx)
    }

    #[track_caller]
    fn propose(phrase: &str, actif: &str, attendu: &str) {
        let points = ordre_du_jour();
        let d = analyser(phrase, actif, 300).unwrap_or_else(|| panic!("rien détecté : {phrase}"));
        assert_eq!(points[d.point].numero, attendu, "{phrase} : {d:?}");
        assert!(d.score >= SEUIL, "{phrase} : {d:?}");
        assert_eq!(d.frontiere_ms, 900_000);
    }

    #[track_caller]
    fn ignore(phrase: &str, actif: &str, depuis_s: u64) {
        if let Some(d) = analyser(phrase, actif, depuis_s) {
            assert!(d.score < SEUIL, "{phrase} : {d:?}");
        }
    }

    #[test]
    fn nombres_en_lettres() {
        let n = |t: &str| nombre_en_lettres(&normaliser(t)).map(|(v, _)| v);
        assert_eq!(n("quatre"), Some(4));
        assert_eq!(n("dix-sept"), Some(17));
        assert_eq!(n("vingt et un"), Some(21));
        assert_eq!(n("trente-deux"), Some(32));
        assert_eq!(n("soixante et onze"), Some(71));
        assert_eq!(n("soixante-douze"), Some(72));
        assert_eq!(n("quatre-vingts"), Some(80));
        assert_eq!(n("quatre-vingt-dix-neuf"), Some(99));
        assert_eq!(n("nonante"), Some(90));
        assert_eq!(n("budget"), None);
        let o = |t: &str| ordinal(&normaliser(t)).map(|(v, _)| v);
        assert_eq!(o("premier"), Some(1));
        assert_eq!(o("troisième"), Some(3));
        assert_eq!(o("quatrième"), Some(4));
        assert_eq!(o("cinquième"), Some(5));
        assert_eq!(o("neuvième"), Some(9));
        assert_eq!(o("douzième"), Some(12));
        assert_eq!(o("vingt et unième"), Some(21));
        assert_eq!(o("quatre-vingt-dixième"), Some(90));
        assert_eq!(o("4e"), Some(4));
        assert_eq!(o("1er"), Some(1));
        assert_eq!(o("problème"), None);
        assert_eq!(numero(&normaliser("quatre point deux")).map(|(n, _)| n), Some("4.2".into()));
        assert_eq!(numero(&normaliser("4,2")).map(|(n, _)| n), Some("4.2".into()));
    }

    #[test]
    fn vrais_positifs_numeros() {
        propose("Bon ben, on passe au point 4.", "3", "4");
        propose("On s'en va au point numéro quatre.", "3", "4");
        propose("Au point 4.2, le parc des Érables.", "4", "4.2");
        propose("Parfait, on passe au point quatre point un.", "4", "4.1");
        propose("Point numéro cinq, la correspondance.", "4.2", "5");
        propose("Pis là, on tombe au point 5.", "4.1", "5");
        propose("Rendu au point sept, la levée de l'assemblée.", "6", "7");
        // Retour explicite en arrière.
        propose("On revient au point 2 rapidement, j'avais oublié quelque chose.", "5", "2");
        // « Comme on disait » avant une annonce forte n'est pas une mention au passé.
        propose("Comme on disait, on passe au point 6.", "5", "6");
    }

    #[test]
    fn vrais_positifs_ordinaux() {
        propose("Le troisième point, c'est l'adoption du procès-verbal.", "2", "3");
        propose("On passe au quatrième point.", "3", "4");
        propose("OK, on passe au 4e point.", "3", "4");
    }

    #[test]
    fn vrais_positifs_titres_et_suivant() {
        propose("OK, point suivant.", "3", "4");
        propose("On passe au prochain point.", "4", "4.1");
        propose("On est rendus dans le varia.", "5", "6");
        propose("Je propose qu'on lève la séance.", "6", "7");
        propose("Parfait. On passe maintenant au budget 2027.", "3", "4");
        propose("On s'en va avec la correspondance.", "4.2", "5");
    }

    #[test]
    fn faux_positifs() {
        ignore("Comme on disait au point 2, le budget est serré.", "4", 300);
        ignore("Le point important, c'est la sécurité des enfants.", "4", 300);
        ignore("On reviendra au point 6 tantôt.", "4", 300);
        ignore("C'est un bon point, on va checker ça.", "4", 300);
        ignore("Le premier point que je veux soulever, c'est le déneigement.", "4.1", 300);
        ignore("Le deuxième point important, c'est la sécurité.", "1", 300);
        ignore("La patinoire municipale, ça coûte cher pareil.", "2", 300);
        ignore("Le budget 2027 va être serré, on en reparlera.", "3", 300);
        ignore("Faut mettre au point le plan de déneigement.", "3", 300);
        ignore("On est au point 4, là.", "4", 300);
        // Point actif commencé il y a 10 s.
        ignore("Point suivant.", "3", 10);
    }

    #[test]
    fn point_suivant_hors_point() {
        let points = ordre_du_jour();
        let ctx = Contexte { points: &points, actif: None, dernier: Some(2), debut_actif_ms: None };
        let d = Detecteur::default().analyser(5000, 7000, "Bon, prochain point.", &ctx).unwrap();
        assert_eq!((points[d.point].numero.as_str(), d.score >= SEUIL), ("4", true), "{d:?}");
        let ctx = Contexte { points: &points, actif: None, dernier: None, debut_actif_ms: None };
        let d = Detecteur::default().analyser(5000, 7000, "On passe au point suivant.", &ctx).unwrap();
        assert_eq!(d.point, 0);
    }

    #[test]
    fn titre_dans_les_15_dernieres_secondes() {
        let points = ordre_du_jour();
        let ctx = Contexte { points: &points, actif: Some(5), dernier: Some(5), debut_actif_ms: Some(0) };
        let mut d = Detecteur::default();
        assert!(d.analyser(300_000, 303_000, "Il nous reste la patinoire.", &ctx).is_none_or(|d| d.score < SEUIL));
        // « Parc des Érables » : un mot il y a 10 s, l'autre maintenant, avec une introduction.
        let ctx = Contexte { actif: Some(3), dernier: Some(3), ..ctx };
        let _ = d.analyser(310_000, 312_000, "Pour le parc", &ctx);
        let r = d.analyser(320_000, 323_000, "on passe aux érables.", &ctx).unwrap();
        assert_eq!(points[r.point].numero, "4.2", "{r:?}");
        assert_eq!(r.frontiere_ms, 320_000);
        // Au-delà de 15 s, le premier mot est oublié.
        let r = d.analyser(345_000, 348_000, "on passe aux érables.", &ctx);
        assert!(r.as_ref().is_none_or(|r| r.score < 70), "{r:?}");
    }

    #[test]
    fn donnees_lues() {
        let e = expressions();
        assert!(e.annonces.iter().all(|a| a.last().is_some_and(|m| m == "point")));
        assert!(e.faibles.iter().all(|a| a.last().is_some_and(|m| m == "point")));
        assert!(e.titres.iter().any(|(x, _)| x.join(" ") == "on leve la seance"));
        assert!(e.vides.contains("de"));
    }

    /// Rencontre d'équipe réelle (audio 10, 8 octobre 2026), où la présidente n'annonce
    /// presque jamais de numéro.
    #[test]
    fn alignement_hors_ligne() {
        let points = crate::agenda::points(
            "1. Mot de bienvenue\n2. Tour de table\n3. Retour rencontre RH\n4. Nouvelle équipe (restructuration)\n5. Horaire des Fêtes\n6. Formulaire pour réseaux sociaux\n7. Discussion corridor / mon bureau\n8. Télétravail\n9. Absence semaine du 13 au 16 octobre\n10. Varia\n11. Prochaine rencontre",
        );
        let paroles = [
            (60, "Bon, on va changer ma chaise tout de suite."),
            (764, "Fait que je vais commencer tout de suite avec le tour de table."),
            (900, "Je travaille sur le schéma de couverture de risques."),
            // Mention en passant, pendant le tour de table.
            (1625, "Au point 5 c'est l'horaire des fêtes, ben ça aussi, ça occupe mon automne."),
            (2400, "Moi, je prépare les budgets."),
            (4593, "Fait que là, le point 4 dans le fond c'est une nouvelle équipe."),
            (5088, "Fait que l'horaire des Faites, vous avez vu mon courriel."),
            (5097, "Ah, c'est tout ça. L'horreur des fêtes."),
            (5300, "Le point au point 8 qui est là, le télétravail."),
            (5400, "Le télétravail, je trouve que ça va bien."),
        ];
        let segments: Vec<Segment> = paroles
            .iter()
            .map(|&(s, t)| Segment { start_ms: s * 1000, end_ms: s * 1000 + 5000, source: crate::audio::Source::Mixte, text: t.into() })
            .collect();
        let trouve: Vec<(&str, u64)> =
            detecter_hors_ligne(&segments, &points).iter().map(|f| (points[f.point.unwrap()].numero.as_str(), f.debut_ms / 1000)).collect();
        assert_eq!(trouve, [("2", 764), ("4", 4593), ("5", 5097), ("8", 5300)]);
    }
}
