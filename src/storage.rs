//! Rencontres enregistrées : un dossier par rencontre dans Musique/Tour de table, tout reste en local.
//!
//! ```text
//! Musique/Tour de table/2026-10-30-rencontre-ca-mensuel/
//!   rencontre.json     transcription complète (sauvée après chaque segment)
//!   index-audio.json   blocs audio d'environ 2 minutes, par point (voir `audio::blocs`)
//!   00-hors-point/passage-01/bloc-001-salle.wav …
//! ```
//!
//! Les rencontres d'avant les blocs gardent salle.wav, distance.wav ou reunion.wav tels quels.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

use crate::agenda::Point;
use crate::nettoyage::Nettoyage;
use crate::stt::Segment;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Meeting {
    pub id: String,
    /// Nom du dossier dans `config::meetings_dir()`, fixé à la création : renommer la rencontre ne
    /// renomme pas son dossier. Absent des rencontres d'avant la 0.1.2 ; `list` le complète.
    #[serde(default)]
    pub dossier: String,
    pub title: String,
    pub started_at: DateTime<Local>,
    pub duration_s: u64,
    pub model: String,
    pub segments: Vec<Segment>,
    /// Compte rendu (ou résumé, sans ordre du jour) rédigé par le modèle local.
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub agenda: Option<Agenda>,
    /// Découpage de la rencontre par point de l'ordre du jour, dans l'ordre chronologique.
    #[serde(default)]
    pub periodes: Vec<Periode>,
    /// Transcription nettoyée (voir `nettoyage`) ; `segments` reste la transcription brute.
    #[serde(default)]
    pub nettoyage: Option<Nettoyage>,
}

/// Ordre du jour déposé avant ou après la rencontre.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Agenda {
    /// Nom du document d'origine (ou « Saisi à la main »).
    pub file: String,
    pub text: String,
    /// Points reconnus dans `text` à l'attachement, modifiables par l'utilisateur.
    #[serde(default)]
    pub points: Vec<Point>,
}

impl Agenda {
    pub fn new(file: String, text: String) -> Self {
        let points = crate::agenda::points(&text);
        Self { file, text, points }
    }
}

/// Intervalle de la rencontre consacré à un point. Les horodatages sont sur l'horloge des
/// segments (`Segment::start_ms`, échantillons alignés du recorder).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Periode {
    /// Indice dans `Agenda::points` ; `None` : hors point.
    pub point: Option<usize>,
    pub debut_ms: u64,
    /// `None` tant que la période est en cours.
    #[serde(default)]
    pub fin_ms: Option<u64>,
    #[serde(default)]
    pub declenchement: Declenchement,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum Declenchement {
    /// Bouton ou raccourci.
    #[default]
    Manuel,
    /// Détecté dans la parole.
    Auto,
}

/// Paragraphe d'export : début, source et texte.
type Paragraphe = (u64, crate::audio::Source, String);

/// Titre donné à une rencontre qu'on n'a pas encore nommée.
const TITRE_PAR_DEFAUT: &str = "Rencontre du %Y-%m-%d à %H h %M";

impl Meeting {
    pub fn new(model: &str) -> Self {
        Self::nouvelle(model, None, Local::now())
    }

    /// Rencontre au titre et à la date donnés (import d'un enregistrement) ; le nom du dossier
    /// en découle et ne changera plus.
    pub fn nouvelle(model: &str, titre: Option<String>, started_at: DateTime<Local>) -> Self {
        Self::nouvelle_dans(&crate::config::meetings_dir(), model, titre, started_at)
    }

    fn nouvelle_dans(racine: &Path, model: &str, titre: Option<String>, started_at: DateTime<Local>) -> Self {
        let title = titre.unwrap_or_else(|| started_at.format(TITRE_PAR_DEFAUT).to_string());
        Self {
            id: Local::now().format("%Y-%m-%d_%H-%M-%S").to_string(),
            dossier: nom_dossier(racine, &started_at, &title),
            title,
            started_at,
            duration_s: 0,
            model: model.to_string(),
            segments: Vec::new(),
            summary: None,
            agenda: None,
            periodes: Vec::new(),
            nettoyage: None,
        }
    }

    pub fn dir(&self) -> PathBuf {
        self.dir_dans(&crate::config::meetings_dir())
    }

    fn dir_dans(&self, racine: &Path) -> PathBuf {
        // Jamais la racine elle-même : on y supprime parfois le dossier de la rencontre.
        racine.join(if self.dossier.is_empty() { &self.id } else { &self.dossier })
    }

    pub fn save(&self) -> Result<()> {
        self.save_dans(&crate::config::meetings_dir())
    }

    fn save_dans(&self, racine: &Path) -> Result<()> {
        let dir = self.dir_dans(racine);
        std::fs::create_dir_all(&dir)?;
        let tmp = dir.join("rencontre.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, dir.join("rencontre.json")).context("Sauvegarde de la rencontre")?;
        Ok(())
    }

    /// Insère en gardant l'ordre chronologique (les deux sources sont
    /// transcrites dans l'ordre où leurs énoncés se terminent).
    pub fn insert(&mut self, seg: Segment) {
        let pos = self.segments.partition_point(|s| s.start_ms <= seg.start_ms);
        self.segments.insert(pos, seg);
    }

    /// Fin du texte transcrit, donnée comme contexte à Whisper.
    pub fn recent_text(&self) -> String {
        let mut v: Vec<&str> = self.segments.iter().rev().take(4).map(|s| s.text.as_str()).collect();
        v.reverse();
        v.join(" ")
    }

    /// Point de la période en cours, s'il y en a une.
    pub fn point_actif(&self) -> Option<usize> {
        self.periodes.last().filter(|p| p.fin_ms.is_none()).and_then(|p| p.point)
    }

    /// Ferme la période en cours et, pour `Some(point)`, en ouvre une nouvelle à `ms`.
    /// Revenir sur un point déjà traité ouvre une nouvelle période.
    pub fn debuter_point(&mut self, point: Option<usize>, ms: u64, declenchement: Declenchement) {
        if point.is_some() && point == self.point_actif() {
            return;
        }
        self.fermer_periode(ms);
        if point.is_some() {
            self.periodes.push(Periode { point, debut_ms: ms, fin_ms: None, declenchement });
        }
    }

    pub fn fermer_periode(&mut self, ms: u64) {
        if let Some(p) = self.periodes.last_mut().filter(|p| p.fin_ms.is_none()) {
            p.fin_ms = Some(ms.max(p.debut_ms));
        }
    }

    /// Point dont la période contient `start_ms` (la plus récente, si elles se chevauchent).
    pub fn point_de(&self, start_ms: u64) -> Option<usize> {
        self.periodes
            .iter()
            .rev()
            .find(|p| p.debut_ms <= start_ms && p.fin_ms.is_none_or(|f| start_ms < f))
            .and_then(|p| p.point)
    }

    /// Temps passé sur un point, période en cours comprise (jusqu'à `maintenant_ms`).
    pub fn duree_point(&self, point: usize, maintenant_ms: u64) -> u64 {
        self.periodes
            .iter()
            .filter(|p| p.point == Some(point))
            .map(|p| p.fin_ms.unwrap_or(maintenant_ms).saturating_sub(p.debut_ms))
            .sum()
    }

    /// Segments regroupés par suites consécutives d'un même point, dans l'ordre chronologique :
    /// un point repris plus tard forme un nouveau groupe.
    pub fn segments_par_point(&self) -> Vec<(Option<usize>, Vec<&Segment>)> {
        let mut out: Vec<(Option<usize>, Vec<&Segment>)> = Vec::new();
        for s in &self.segments {
            let point = self.point_de(s.start_ms);
            match out.last_mut() {
                Some((p, v)) if *p == point => v.push(s),
                _ => out.push((point, vec![s])),
            }
        }
        out
    }

    /// Intertitre d'un point : « 2.1. Budget », ou « Hors point ».
    pub fn intertitre(&self, point: Option<usize>) -> String {
        match point {
            None => "Hors point".into(),
            Some(i) => match self.agenda.as_ref().and_then(|a| a.points.get(i)) {
                Some(p) => format!("{}. {}", p.numero, p.titre),
                None => format!("Point {}", i + 1),
            },
        }
    }

    /// Adapte les périodes à une modification des points : `f` donne le nouvel indice d'un
    /// point, ou `None` s'il a été supprimé (ses périodes passent hors point).
    pub fn renumeroter_periodes(&mut self, f: impl Fn(usize) -> Option<usize>) {
        for p in &mut self.periodes {
            p.point = p.point.and_then(&f);
        }
        for p in self.nettoyage.iter_mut().flat_map(|n| &mut n.phrases) {
            p.point = p.point.and_then(&f);
        }
    }

    /// Groupes à intertitrer (aucun tant que la rencontre n'est pas découpée par point).
    fn groupes(&self) -> Vec<(Option<String>, Vec<&Segment>)> {
        if self.periodes.is_empty() {
            return vec![(None, self.segments.iter().collect())];
        }
        self.segments_par_point().into_iter().map(|(p, v)| (Some(self.intertitre(p)), v)).collect()
    }

    /// Regroupe les segments consécutifs d'une même source en paragraphes.
    fn paragraphs(segments: &[&Segment]) -> Vec<(u64, crate::audio::Source, String)> {
        let mut out: Vec<(u64, crate::audio::Source, String)> = Vec::new();
        for s in segments {
            match out.last_mut() {
                Some((_, src, text)) if *src == s.source => {
                    text.push(' ');
                    text.push_str(&s.text);
                }
                _ => out.push((s.start_ms, s.source, s.text.clone())),
            }
        }
        out
    }

    /// Paragraphes de la transcription nettoyée, par suites consécutives d'un même point : les
    /// phrases consécutives d'une même source sont réunies.
    fn groupes_nettoyes(&self) -> Vec<(Option<usize>, Vec<Paragraphe>)> {
        let mut out: Vec<(Option<usize>, Vec<Paragraphe>)> = Vec::new();
        for p in self.nettoyage.iter().flat_map(|n| &n.phrases) {
            if out.last().is_none_or(|(point, _)| *point != p.point) {
                out.push((p.point, Vec::new()));
            }
            let paragraphes = &mut out.last_mut().expect("groupe ajouté").1;
            match paragraphes.last_mut() {
                Some((_, src, text)) if *src == p.source => {
                    text.push(' ');
                    text.push_str(&p.texte);
                }
                _ => paragraphes.push((p.debut_ms, p.source, p.texte.clone())),
            }
        }
        out
    }

    /// Nettoyage terminé : c'est la version que lit le compte rendu.
    pub fn nettoyage_termine(&self) -> Option<&Nettoyage> {
        self.nettoyage.as_ref().filter(|n| n.termine() && !n.phrases.is_empty())
    }

    /// Texte donné au compte rendu : la version nettoyée quand elle existe, sinon la brute.
    pub fn texte_compte_rendu(&self) -> String {
        if self.nettoyage_termine().is_none() {
            return self.plain_text();
        }
        let mut blocs = Vec::new();
        for (point, paragraphes) in self.groupes_nettoyes() {
            if !self.periodes.is_empty() {
                blocs.push(format!("— {} —", self.intertitre(point)));
            }
            blocs.extend(paragraphes.iter().map(|(t, src, text)| format!("[{}] {} : {text}", clock(*t), src.label())));
        }
        blocs.join("\n\n")
    }

    /// transcription-nettoyee.md : même présentation que la transcription brute.
    pub fn to_markdown_nettoye(&self) -> String {
        let mut md = format!("# {} — transcription nettoyée\n\n- Date : {}\n\n", self.title, self.started_at.format("%Y-%m-%d %H h %M"));
        if self.nettoyage.as_ref().is_some_and(|n| !n.termine()) {
            md.push_str("> Nettoyage partiel : la fin de la rencontre n'est pas encore nettoyée.\n\n");
        }
        for (point, paragraphes) in self.groupes_nettoyes() {
            if !self.periodes.is_empty() {
                let _ = write!(md, "## {}\n\n", self.intertitre(point));
            }
            for (t, src, text) in paragraphes {
                let _ = write!(md, "[{}] **{}** : {text}\n\n", clock(t), src.label());
            }
        }
        md
    }

    /// transcription.md d'un point (toutes ses périodes) : nettoyée si le nettoyage est terminé.
    fn paragraphes_du_point(&self, point: Option<usize>) -> Vec<Paragraphe> {
        match self.nettoyage_termine() {
            Some(_) => self.groupes_nettoyes().into_iter().filter(|(p, _)| *p == point).flat_map(|(_, v)| v).collect(),
            None => self.segments_par_point().into_iter().filter(|(p, _)| *p == point).flat_map(|(_, v)| Self::paragraphs(&v)).collect(),
        }
    }

    /// Transcription d'un point (toutes ses périodes) pour le compte rendu, nettoyée si le
    /// nettoyage est terminé, en paragraphes sans horodatages (des jetons de plus à lire,
    /// inutiles au résumé) ; `None` s'il n'a pas été abordé.
    pub fn texte_du_point(&self, point: Option<usize>) -> Option<String> {
        let paragraphes = self.paragraphes_du_point(point);
        // Ni horodatages ni sources : « Salle » finirait en responsable d'un suivi.
        let lignes: Vec<String> = paragraphes.into_iter().map(|(_, _, text)| text).collect();
        (!lignes.is_empty()).then(|| lignes.join("\n\n"))
    }

    /// Échanges hors point entre le début du premier point et la fin du dernier : ce qui précède
    /// (installation, conversations d'avant la rencontre) et ce qui suit n'est pas au compte rendu.
    pub fn texte_hors_point_pendant(&self) -> Option<String> {
        let points = self.periodes.iter().filter(|p| p.point.is_some());
        let debut = points.clone().map(|p| p.debut_ms).min()?;
        let fin = points.map(|p| p.fin_ms.unwrap_or(u64::MAX)).max()?;
        let lignes: Vec<String> =
            self.paragraphes_du_point(None).into_iter().filter(|(t, _, _)| (debut..fin).contains(t)).map(|(_, _, text)| text).collect();
        (!lignes.is_empty()).then(|| lignes.join("\n\n"))
    }

    /// Dossier d'un point (audio, transcription.md, resume.md).
    pub fn dossier_du_point(&self, point: Option<usize>) -> PathBuf {
        let titres: Vec<String> = self.agenda.iter().flat_map(|a| a.points.iter().map(|p| p.titre.clone())).collect();
        self.dir().join(crate::audio::blocs::prefixe(point, &titres))
    }

    fn markdown_du_point(&self, point: Option<usize>) -> Option<String> {
        let paragraphes = self.paragraphes_du_point(point);
        if paragraphes.is_empty() {
            return None;
        }
        let mut md = format!("# {}\n\n{}\n\n", self.intertitre(point), self.title);
        for (t, src, text) in paragraphes {
            let _ = write!(md, "[{}] **{}** : {text}\n\n", clock(t), src.label());
        }
        Some(md)
    }

    pub fn plain_text(&self) -> String {
        let mut blocs = Vec::new();
        for (titre, segs) in self.groupes() {
            if let Some(t) = titre {
                blocs.push(format!("— {t} —"));
            }
            blocs.extend(Self::paragraphs(&segs).iter().map(|(t, src, text)| format!("[{}] {} : {text}", clock(*t), src.label())));
        }
        blocs.join("\n\n")
    }

    pub fn to_markdown(&self) -> String {
        let mut md = format!(
            "# {}\n\n- Date : {}\n- Durée : {}\n- Modèle : {}\n\n",
            self.title,
            self.started_at.format("%Y-%m-%d %H h %M"),
            clock(self.duration_s * 1000),
            self.model
        );
        if let Some(a) = &self.agenda {
            let _ = write!(md, "## Ordre du jour\n\n{}\n\n", a.text);
        }
        if let Some(sum) = &self.summary {
            let heading = if self.agenda.is_some() { "Compte rendu" } else { "Résumé" };
            let _ = write!(md, "## {heading}\n\n{sum}\n\n");
        }
        md.push_str("## Transcription\n\n");
        for (titre, segs) in self.groupes() {
            if let Some(t) = titre {
                let _ = write!(md, "### {t}\n\n");
            }
            for (t, src, text) in Self::paragraphs(&segs) {
                let _ = write!(md, "[{}] **{}** : {text}\n\n", clock(t), src.label());
            }
        }
        md
    }

    /// Sous-titres ; le premier sous-titre de chaque point porte son intertitre sur une ligne.
    pub fn to_srt(&self) -> String {
        let mut out = String::new();
        let mut i = 0;
        for (titre, segs) in self.groupes() {
            for (j, s) in segs.iter().enumerate() {
                i += 1;
                let tete = titre.as_ref().filter(|_| j == 0).map(|t| format!("[{t}]\n")).unwrap_or_default();
                let _ = write!(
                    out,
                    "{i}\n{} --> {}\n{tete}{} : {}\n\n",
                    srt_time(s.start_ms),
                    srt_time(s.end_ms.max(s.start_ms + 500)),
                    s.source.label(),
                    s.text
                );
            }
        }
        out
    }

    /// Écrit les exports à côté du JSON et retourne le dossier.
    pub fn export_all(&self) -> Result<PathBuf> {
        let dir = self.dir();
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("transcription.md"), self.to_markdown())?;
        std::fs::write(dir.join("transcription.txt"), self.plain_text())?;
        std::fs::write(dir.join("transcription.srt"), self.to_srt())?;
        if self.nettoyage.is_some() {
            std::fs::write(dir.join("transcription-nettoyee.md"), self.to_markdown_nettoye())?;
        }
        if !self.periodes.is_empty() {
            let n = self.agenda.as_ref().map_or(0, |a| a.points.len());
            for point in std::iter::once(None).chain((0..n).map(Some)) {
                if let Some(md) = self.markdown_du_point(point) {
                    let dossier = self.dossier_du_point(point);
                    std::fs::create_dir_all(&dossier)?;
                    std::fs::write(dossier.join("transcription.md"), md)?;
                }
            }
        }
        if let Some(sum) = &self.summary {
            std::fs::write(dir.join("compte-rendu.md"), sum)?;
            crate::docx::ecrire(sum, &dir.join("compte-rendu.docx"))?;
        }
        Ok(dir)
    }
}

pub fn list() -> Vec<Meeting> {
    list_dans(&crate::config::meetings_dir())
}

fn list_dans(racine: &Path) -> Vec<Meeting> {
    let mut v: Vec<Meeting> = std::fs::read_dir(racine).into_iter().flatten().flatten().filter_map(|e| charger(&e.path())).collect();
    v.sort_by_key(|m| std::cmp::Reverse(m.started_at));
    v
}

/// Rencontre lue depuis un `rencontre.json` quelconque (ligne de commande) : son dossier reste
/// celui du fichier, même hors du dossier des rencontres (`dossier` absolu, que `dir` reprend tel quel).
pub fn charger_fichier(chemin: &Path) -> Result<Meeting> {
    let json = std::fs::read(chemin).with_context(|| format!("Lecture de {}", chemin.display()))?;
    let mut m: Meeting = serde_json::from_slice(&json).with_context(|| format!("{} illisible", chemin.display()))?;
    let dir = std::path::absolute(chemin)?.parent().context("dossier de la rencontre")?.to_path_buf();
    m.dossier = dir.to_string_lossy().into_owned();
    Ok(m)
}

/// Lit `rencontre.json` ; `None` pour tout autre dossier (l'utilisateur range ce qu'il veut dans Musique).
fn charger(dir: &Path) -> Option<Meeting> {
    let json = std::fs::read(dir.join("rencontre.json")).ok()?;
    let mut m: Meeting = serde_json::from_slice(&json).map_err(|e| log::warn!("{} illisible : {e}", dir.display())).ok()?;
    // Le dossier fait foi : ancien format sans `dossier`, ou dossier renommé à la main.
    m.dossier = dir.file_name()?.to_string_lossy().into_owned();
    Some(m)
}

/// Nom du dossier d'une nouvelle rencontre : `AAAA-MM-JJ-rencontre-<slug du titre>`, ou
/// `AAAA-MM-JJ-rencontre` pour un titre par défaut, suivi de -2, -3… s'il est déjà pris.
fn nom_dossier(racine: &Path, date: &DateTime<Local>, titre: &str) -> String {
    let mut base = date.format("%Y-%m-%d-rencontre").to_string();
    if chrono::NaiveDateTime::parse_from_str(titre.trim(), TITRE_PAR_DEFAUT).is_err() {
        let slug = slug(titre);
        // « Rencontre du CA » donne …-rencontre-du-ca, pas …-rencontre-rencontre-du-ca.
        let slug = if slug == "rencontre" { "" } else { slug.strip_prefix("rencontre-").unwrap_or(&slug) };
        if !slug.is_empty() {
            base.push('-');
            base.push_str(slug);
        }
    }
    (1..)
        .map(|n| if n == 1 { base.clone() } else { format!("{base}-{n}") })
        .find(|nom| !racine.join(nom).exists())
        .expect("suite infinie")
}

/// Titre réduit à des minuscules ASCII sans accents séparées par des tirets, 40 caractères au plus.
pub(crate) fn slug(titre: &str) -> String {
    let mut s = String::new();
    for c in titre.chars().flat_map(char::to_lowercase) {
        let lettre = match c {
            'à' | 'â' | 'ä' | 'á' | 'ã' | 'å' => "a",
            'ç' => "c",
            'é' | 'è' | 'ê' | 'ë' => "e",
            'î' | 'ï' | 'í' | 'ì' => "i",
            'ô' | 'ö' | 'ó' | 'ò' | 'õ' | 'ø' => "o",
            'ù' | 'û' | 'ü' | 'ú' => "u",
            'ÿ' | 'ý' => "y",
            'ñ' => "n",
            'œ' => "oe",
            'æ' => "ae",
            'ß' => "ss",
            c if c.is_ascii_alphanumeric() => {
                s.push(c);
                continue;
            }
            _ => "-",
        };
        s.push_str(lettre);
    }
    let mut slug = s.split('-').filter(|m| !m.is_empty()).collect::<Vec<_>>().join("-");
    if slug.len() > 40 {
        // Coupe au dernier mot entier qui tient, sauf s'il n'y en a qu'un.
        let coupe = if slug.as_bytes()[40] == b'-' { 40 } else { slug[..40].rfind('-').unwrap_or(40) };
        slug.truncate(coupe);
    }
    slug
}

/// Déplace les rencontres de l'ancien emplacement (`ancienne`) vers `racine`, chacune sous un
/// nom de dossier tiré de sa date et de son titre. Un échec laisse l'original en place.
pub fn migrer(ancienne: &Path, racine: &Path) {
    let Ok(dossiers) = std::fs::read_dir(ancienne) else { return };
    for dir in dossiers.flatten().map(|e| e.path()) {
        let Some(mut m) = charger(&dir) else {
            log::warn!("Migration : {} n'est pas une rencontre lisible, laissé en place", dir.display());
            continue;
        };
        if let Err(e) = std::fs::create_dir_all(racine) {
            log::error!("Migration des rencontres : impossible de créer {} : {e}", racine.display());
            return;
        }
        m.dossier = nom_dossier(racine, &m.started_at, &m.title);
        let dest = m.dir_dans(racine);
        match deplacer(&dir, &dest) {
            Ok(()) => {
                log::info!("Rencontre déplacée : {} → {}", dir.display(), dest.display());
                if let Err(e) = m.save_dans(racine) {
                    log::warn!("Migration : enregistrement de {} : {e:#}", dest.display());
                }
            }
            Err(e) => log::error!("Migration : rencontre laissée en place dans {} : {e:#}", dir.display()),
        }
    }
    // Retire l'ancien dossier s'il est vide (remove_dir échoue sinon).
    let _ = std::fs::remove_dir(ancienne);
}

/// `rename`, ou copie puis suppression quand la destination est sur un autre disque.
fn deplacer(de: &Path, vers: &Path) -> Result<()> {
    if std::fs::rename(de, vers).is_ok() {
        return Ok(());
    }
    deplacer_par_copie(de, vers)
}

/// En cas d'échec de la copie, la copie partielle est retirée et l'original reste intact.
fn deplacer_par_copie(de: &Path, vers: &Path) -> Result<()> {
    // Ne jamais nettoyer (donc supprimer) un dossier qu'on n'a pas créé.
    anyhow::ensure!(!vers.exists(), "{} existe déjà", vers.display());
    let copie = copier(de, vers).with_context(|| format!("Copie vers {}", vers.display()));
    // Sans son rencontre.json, un reste de l'original ne sera plus pris pour une rencontre.
    let copie = copie.and_then(|()| std::fs::remove_file(de.join("rencontre.json")).context("Suppression de l'original"));
    if let Err(e) = copie {
        let _ = std::fs::remove_dir_all(vers);
        return Err(e);
    }
    if let Err(e) = std::fs::remove_dir_all(de) {
        log::warn!("Copie faite, mais {} n'a pas pu être supprimé : {e}", de.display());
    }
    Ok(())
}

fn copier(de: &Path, vers: &Path) -> std::io::Result<()> {
    std::fs::create_dir(vers)?;
    for e in std::fs::read_dir(de)? {
        let e = e?;
        let cible = vers.join(e.file_name());
        if e.file_type()?.is_dir() {
            copier(&e.path(), &cible)?;
        } else {
            std::fs::copy(e.path(), &cible)?;
        }
    }
    Ok(())
}

pub fn delete(m: &Meeting) -> Result<()> {
    // Corbeille de l'application plutôt qu'une suppression définitive : l'utilisateur
    // peut récupérer le dossier. Elle reste dans le dossier de données, parfois sur un
    // autre disque que Musique, d'où le repli sur une copie.
    let dir = m.dir();
    let trash = crate::config::data_dir().join("corbeille");
    std::fs::create_dir_all(&trash)?;
    deplacer(&dir, &trash.join(&m.id)).context("Déplacement vers la corbeille de Tour de table")
}

pub fn clock(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

fn srt_time(ms: u64) -> String {
    format!("{:02}:{:02}:{:02},{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

/// Ouvre un dossier dans l'explorateur de fichiers du système.
pub fn reveal(path: &std::path::Path) {
    let cmd = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    if let Err(e) = std::process::Command::new(cmd).arg(path).spawn() {
        log::warn!("Ouverture de {} : {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::Source;

    fn seg(start_ms: u64, source: Source, text: &str) -> Segment {
        Segment { start_ms, end_ms: start_ms + 1000, source, text: text.into() }
    }

    #[test]
    fn ordre_et_exports() {
        let mut m = Meeting::new("test.bin");
        m.insert(seg(5000, Source::Distance, "Allo, vous m'entendez-tu?"));
        m.insert(seg(1000, Source::Salle, "Bon ben, on commence."));
        m.insert(seg(2500, Source::Salle, "Bienvenue à tous."));
        assert_eq!(m.segments[0].start_ms, 1000);
        assert_eq!(
            m.plain_text(),
            "[00:01] Salle : Bon ben, on commence. Bienvenue à tous.\n\n[00:05] À distance : Allo, vous m'entendez-tu?"
        );
        assert!(m.to_markdown().contains("[00:01] **Salle** : Bon ben"));
        assert!(m.to_srt().starts_with("1\n00:00:01,000 --> 00:00:02,000\nSalle : Bon ben"));
    }

    fn rencontre_avec_points() -> Meeting {
        let mut m = Meeting::new("test.bin");
        m.agenda = Some(Agenda::new("od.docx".into(), "Ordre du jour\n1. Ouverture\n2. Budget\n3. Varia".into()));
        m
    }

    #[test]
    fn segment_attribue_a_sa_periode() {
        let mut m = rencontre_avec_points();
        m.insert(seg(500, Source::Salle, "Bon, on attend Julie."));
        m.debuter_point(Some(0), 1000, Declenchement::Manuel);
        m.insert(seg(1000, Source::Salle, "Bienvenue."));
        m.insert(seg(3000, Source::Salle, "On passe au budget?"));
        m.debuter_point(Some(1), 4000, Declenchement::Manuel);
        // Recliquer sur le point actif ne crée pas de période.
        m.debuter_point(Some(1), 4500, Declenchement::Manuel);
        m.insert(seg(4000, Source::Distance, "Le budget est équilibré."));
        assert_eq!(m.periodes.len(), 2);
        assert_eq!(m.periodes[0].fin_ms, Some(4000));
        assert_eq!([500, 1000, 3999, 4000, 99_000].map(|t| m.point_de(t)), [None, Some(0), Some(0), Some(1), Some(1)]);
        assert_eq!(m.point_actif(), Some(1));
        let groupes: Vec<_> = m.segments_par_point().into_iter().map(|(p, v)| (p, v.len())).collect();
        assert_eq!(groupes, vec![(None, 1), (Some(0), 2), (Some(1), 1)]);
        assert_eq!(m.duree_point(1, 10_000), 6000);
    }

    #[test]
    fn retour_sur_un_point_et_hors_point() {
        let mut m = rencontre_avec_points();
        m.debuter_point(Some(0), 0, Declenchement::Manuel);
        m.debuter_point(Some(1), 2000, Declenchement::Manuel);
        m.debuter_point(None, 5000, Declenchement::Manuel); // Hors point
        assert_eq!(m.point_actif(), None);
        m.debuter_point(Some(0), 8000, Declenchement::Manuel); // retour sur l'ouverture
        m.fermer_periode(9000);
        for (t, texte) in [(100, "Ouverture."), (2100, "Budget."), (6000, "Pause café."), (8100, "Retour à l'ouverture.")] {
            m.insert(seg(t, Source::Salle, texte));
        }
        assert_eq!(m.periodes.len(), 3);
        assert_eq!(m.duree_point(0, 20_000), 3000);
        assert_eq!(m.point_de(6000), None);
        assert_eq!(m.point_de(9500), None);
        let ordre: Vec<_> = m.segments_par_point().into_iter().map(|(p, _)| p).collect();
        assert_eq!(ordre, vec![Some(0), Some(1), None, Some(0)]);
        assert_eq!(
            m.plain_text(),
            "— 1. Ouverture —\n\n[00:00] Salle : Ouverture.\n\n— 2. Budget —\n\n[00:02] Salle : Budget.\n\n\
             — Hors point —\n\n[00:06] Salle : Pause café.\n\n— 1. Ouverture —\n\n[00:08] Salle : Retour à l'ouverture."
        );
        let md = m.to_markdown();
        assert!(md.contains("## Transcription\n\n### 1. Ouverture\n\n[00:00] **Salle** : Ouverture.\n\n### 2. Budget\n\n"));
        assert!(m.to_srt().starts_with("1\n00:00:00,100 --> 00:00:01,100\n[1. Ouverture]\nSalle : Ouverture.\n\n2\n"));
        assert!(m.to_srt().contains("3\n00:00:06,000 --> 00:00:07,000\n[Hors point]\nSalle : Pause café."));

        // Suppression du budget : ses périodes passent hors point, la suite est renumérotée.
        m.renumeroter_periodes(|i| match i {
            1 => None,
            i if i > 1 => Some(i - 1),
            i => Some(i),
        });
        assert_eq!(m.point_de(2100), None);
        assert_eq!(m.point_de(100), Some(0));
    }

    #[test]
    fn transcription_nettoyee_exports_et_compte_rendu() {
        let mut m = rencontre_avec_points();
        m.insert(seg(500, Source::Salle, "Euh bon, on attend Julie."));
        m.debuter_point(Some(1), 1000, Declenchement::Manuel);
        m.insert(seg(1000, Source::Salle, "Le le budget est adopté."));
        m.fermer_periode(3000);
        // Sans nettoyage terminé, le compte rendu lit la transcription brute.
        assert_eq!(m.texte_compte_rendu(), m.plain_text());

        let n = crate::nettoyage::executer(&m, None, &crate::quebec::Glossary::parse(""), &Default::default(), &mut |_| {});
        m.nettoyage = Some(n);
        assert_eq!(m.texte_compte_rendu(), "— Hors point —\n\n[00:00] Salle : Bon, on attend Julie.\n\n— 2. Budget —\n\n[00:01] Salle : Le budget est adopté.");
        assert!(m.plain_text().contains("Le le budget"), "la version brute reste intacte");
        let md = m.to_markdown_nettoye();
        assert!(md.contains("## 2. Budget\n\n[00:01] **Salle** : Le budget est adopté."), "{md}");

        // transcription.md de chaque dossier de point.
        for point in [None, Some(0), Some(1)] {
            let md = m.markdown_du_point(point);
            assert_eq!(md.is_some(), point != Some(0), "point non abordé : aucun fichier");
        }
        assert!(m.markdown_du_point(Some(1)).unwrap().starts_with("# 2. Budget\n\n"));
        // Entrée du compte rendu par point : sans horodatages.
        assert_eq!(m.texte_du_point(Some(1)).as_deref(), Some("Le budget est adopté."));
        assert_eq!(m.texte_du_point(Some(0)), None);

        // Points renumérotés : les phrases suivent.
        m.renumeroter_periodes(|i| Some(i + 1));
        assert_eq!(m.nettoyage.as_ref().unwrap().phrases[1].point, Some(2));
        // Ancien rencontre.json : aucun nettoyage.
        assert!(serde_json::from_str::<Meeting>(ANCIEN_FORMAT).unwrap().nettoyage.is_none());
    }

    /// Dossier temporaire vide, propre au test : le vrai dossier Musique n'est jamais touché.
    fn temporaire(nom: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tourdetable-test-{}-{nom}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn le_30_octobre() -> DateTime<Local> {
        use chrono::TimeZone;
        Local.with_ymd_and_hms(2026, 10, 30, 9, 15, 0).unwrap()
    }

    #[test]
    fn slug_sans_accents_ni_ponctuation() {
        assert_eq!(slug("CA mensuel"), "ca-mensuel");
        assert_eq!(slug("Réunion du C.A. — budget 2027 !"), "reunion-du-c-a-budget-2027");
        assert_eq!(slug("  « Œuvres » : Ça déménage à Rouyn-Noranda?  "), "oeuvres-ca-demenage-a-rouyn-noranda");
        assert_eq!(slug("Comité d'été / Ève & Zoë"), "comite-d-ete-eve-zoe");
        assert_eq!(slug("!!! ??? 🎤"), "");

        let long = slug("Assemblée générale annuelle des membres de la coopérative régionale");
        assert_eq!(long, "assemblee-generale-annuelle-des-membres");
        assert!(long.len() <= 40);
        let mot = slug(&"a".repeat(60));
        assert_eq!(mot.len(), 40);
    }

    #[test]
    fn nom_du_dossier() {
        let racine = temporaire("nom");
        let date = le_30_octobre();
        assert_eq!(nom_dossier(&racine, &date, "CA mensuel"), "2026-10-30-rencontre-ca-mensuel");
        assert_eq!(nom_dossier(&racine, &date, "Rencontre : CA mensuel"), "2026-10-30-rencontre-ca-mensuel");
        assert_eq!(nom_dossier(&racine, &date, "Rencontre"), "2026-10-30-rencontre");
        assert_eq!(nom_dossier(&racine, &date, "…"), "2026-10-30-rencontre");
        // Titre par défaut, même d'une autre date que celle du dossier.
        assert_eq!(nom_dossier(&racine, &date, "Rencontre du 2026-10-30 à 09 h 15"), "2026-10-30-rencontre");
        assert_eq!(nom_dossier(&racine, &date, "Rencontre du 2025-01-02 à 14 h 30"), "2026-10-30-rencontre");
        assert_eq!(Meeting::nouvelle_dans(&racine, "m", None, date).dossier, "2026-10-30-rencontre");
        let _ = std::fs::remove_dir_all(&racine);
    }

    #[test]
    fn collisions() {
        let racine = temporaire("collisions");
        let date = le_30_octobre();
        for attendu in ["2026-10-30-rencontre-ca", "2026-10-30-rencontre-ca-2", "2026-10-30-rencontre-ca-3"] {
            let m = Meeting::nouvelle_dans(&racine, "m", Some("CA".into()), date);
            assert_eq!(m.dossier, attendu);
            m.save_dans(&racine).unwrap();
        }
        // Un fichier quelconque du même nom compte aussi.
        std::fs::write(racine.join("2026-10-30-rencontre"), "").unwrap();
        assert_eq!(Meeting::nouvelle_dans(&racine, "m", None, date).dossier, "2026-10-30-rencontre-2");
        let _ = std::fs::remove_dir_all(&racine);
    }

    #[test]
    fn renommer_ne_deplace_pas_le_dossier() {
        let racine = temporaire("renommer");
        let mut m = Meeting::nouvelle_dans(&racine, "m", Some("Budget".into()), le_30_octobre());
        m.save_dans(&racine).unwrap();
        m.title = "Budget révisé".into();
        m.save_dans(&racine).unwrap();
        let v = list_dans(&racine);
        assert_eq!(v.len(), 1);
        assert_eq!((v[0].title.as_str(), v[0].dossier.as_str()), ("Budget révisé", "2026-10-30-rencontre-budget"));
        let _ = std::fs::remove_dir_all(&racine);
    }

    #[test]
    fn liste_ignore_ce_qui_n_est_pas_une_rencontre() {
        let racine = temporaire("liste");
        Meeting::nouvelle_dans(&racine, "m", Some("Vraie".into()), le_30_octobre()).save_dans(&racine).unwrap();
        std::fs::create_dir_all(racine.join("Mes chansons")).unwrap();
        std::fs::write(racine.join("Mes chansons/piste.mp3"), "").unwrap();
        std::fs::write(racine.join("notes.txt"), "").unwrap();
        std::fs::create_dir_all(racine.join("abime")).unwrap();
        std::fs::write(racine.join("abime/rencontre.json"), "{pas du JSON").unwrap();
        let v = list_dans(&racine);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].title, "Vraie");
        let _ = std::fs::remove_dir_all(&racine);
    }

    /// rencontre.json d'avant la 0.1.2, sans champ `dossier`.
    const ANCIEN_FORMAT: &str = r#"{
  "id": "2026-10-30_09-15-00",
  "title": "Comité de liaison : Témiscamingue",
  "started_at": "2026-10-30T09:15:00-04:00",
  "duration_s": 3600,
  "model": "ggml-large-v3-turbo-q5_0.bin",
  "segments": [],
  "summary": "Tout va bien."
}"#;

    #[test]
    fn ancien_format_sans_dossier() {
        let m: Meeting = serde_json::from_str(ANCIEN_FORMAT).unwrap();
        assert_eq!(m.dossier, "");
        assert!(m.periodes.is_empty());
        assert_eq!(m.point_de(1000), None);
        // Jamais la racine elle-même.
        assert_eq!(m.dir_dans(Path::new("/racine")), Path::new("/racine/2026-10-30_09-15-00"));

        let racine = temporaire("ancien-format");
        std::fs::create_dir_all(racine.join("ailleurs")).unwrap();
        std::fs::write(racine.join("ailleurs/rencontre.json"), ANCIEN_FORMAT).unwrap();
        let v = list_dans(&racine);
        assert_eq!(v[0].dossier, "ailleurs");
        assert_eq!(v[0].dir_dans(&racine), racine.join("ailleurs"));
        let _ = std::fs::remove_dir_all(&racine);
    }

    #[test]
    fn ancien_format_ordre_du_jour_sans_points() {
        let json = ANCIEN_FORMAT.replace(
            r#""segments": [],"#,
            r#""segments": [{"start_ms": 1000, "end_ms": 2000, "source": "Salle", "text": "Bonjour."}],
  "agenda": {"file": "od.txt", "text": "1. Ouverture"},"#,
        );
        let m: Meeting = serde_json::from_str(&json).unwrap();
        assert_eq!(m.agenda.as_ref().unwrap().points, vec![]);
        assert!(m.periodes.is_empty());
        // Sans périodes, les exports restent comme avant (aucun intertitre).
        assert_eq!(m.plain_text(), "[00:01] Salle : Bonjour.");
        assert_eq!(serde_json::from_str::<Periode>(r#"{"point": 2, "debut_ms": 10}"#).unwrap().declenchement, Declenchement::Manuel);
    }

    #[test]
    fn migration_d_un_ancien_dossier() {
        let donnees = temporaire("migration");
        let (ancienne, racine) = (donnees.join("rencontres"), donnees.join("Musique/Tour de table"));
        let vieux = ancienne.join("2026-10-30_09-15-00");
        std::fs::create_dir_all(&vieux).unwrap();
        std::fs::write(vieux.join("rencontre.json"), ANCIEN_FORMAT).unwrap();
        std::fs::write(vieux.join("salle.wav"), "RIFF").unwrap();
        // Ni un dossier quelconque ni la corbeille ne bougent.
        std::fs::create_dir_all(ancienne.join("vide")).unwrap();
        std::fs::create_dir_all(donnees.join("corbeille/2026-01-01_10-00-00")).unwrap();
        std::fs::write(donnees.join("corbeille/2026-01-01_10-00-00/rencontre.json"), ANCIEN_FORMAT).unwrap();
        // Nom déjà pris dans la nouvelle racine.
        std::fs::create_dir_all(racine.join("2026-10-30-rencontre-comite-de-liaison-temiscamingue")).unwrap();

        migrer(&ancienne, &racine);

        let nouveau = racine.join("2026-10-30-rencontre-comite-de-liaison-temiscamingue-2");
        assert!(!vieux.exists());
        assert_eq!(std::fs::read(nouveau.join("salle.wav")).unwrap(), b"RIFF");
        let json = std::fs::read_to_string(nouveau.join("rencontre.json")).unwrap();
        let m: Meeting = serde_json::from_str(&json).unwrap();
        assert_eq!(m.dossier, "2026-10-30-rencontre-comite-de-liaison-temiscamingue-2");
        assert_eq!((m.id.as_str(), m.summary.as_deref()), ("2026-10-30_09-15-00", Some("Tout va bien.")));
        assert!(ancienne.join("vide").exists());
        assert!(donnees.join("corbeille/2026-01-01_10-00-00/rencontre.json").exists());
        assert_eq!(list_dans(&racine).len(), 1);

        // Relancée au démarrage suivant : rien à refaire.
        migrer(&ancienne, &racine);
        assert_eq!(list_dans(&racine).len(), 1);
        let _ = std::fs::remove_dir_all(&donnees);
    }

    #[test]
    fn deplacement_par_copie() {
        let base = temporaire("copie");
        let (de, vers) = (base.join("de"), base.join("vers"));
        std::fs::create_dir_all(de.join("sous")).unwrap();
        std::fs::write(de.join("rencontre.json"), ANCIEN_FORMAT).unwrap();
        std::fs::write(de.join("sous/distance.wav"), "RIFF").unwrap();
        deplacer_par_copie(&de, &vers).unwrap();
        assert!(!de.exists());
        assert_eq!(std::fs::read(vers.join("sous/distance.wav")).unwrap(), b"RIFF");

        // Destination déjà présente : échec, l'original reste intact.
        std::fs::create_dir_all(&de).unwrap();
        std::fs::write(de.join("rencontre.json"), ANCIEN_FORMAT).unwrap();
        assert!(deplacer_par_copie(&de, &vers).is_err());
        assert!(de.join("rencontre.json").exists());
        assert!(vers.join("sous/distance.wav").exists());
        let _ = std::fs::remove_dir_all(&base);
    }
}
