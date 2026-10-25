//! Rencontres enregistrées : un dossier par rencontre dans Musique/Tour de table, tout reste en local.
//!
//! ```text
//! Musique/Tour de table/2026-10-30-rencontre-ca-mensuel/
//!   rencontre.json   transcription complète (sauvée après chaque segment)
//!   salle.wav        micro (optionnel)
//!   distance.wav     son système (optionnel)
//! ```

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

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
}

/// Ordre du jour déposé avant ou après la rencontre.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Agenda {
    /// Nom du document d'origine (ou « Saisi à la main »).
    pub file: String,
    pub text: String,
}

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

    /// Regroupe les segments consécutifs d'une même source en paragraphes.
    fn paragraphs(&self) -> Vec<(u64, crate::audio::Source, String)> {
        let mut out: Vec<(u64, crate::audio::Source, String)> = Vec::new();
        for s in &self.segments {
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

    pub fn plain_text(&self) -> String {
        self.paragraphs()
            .iter()
            .map(|(t, src, text)| format!("[{}] {} : {text}", clock(*t), src.label()))
            .collect::<Vec<_>>()
            .join("\n\n")
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
        for (t, src, text) in self.paragraphs() {
            let _ = write!(md, "[{}] **{}** : {text}\n\n", clock(t), src.label());
        }
        md
    }

    pub fn to_srt(&self) -> String {
        let mut out = String::new();
        for (i, s) in self.segments.iter().enumerate() {
            let _ = write!(
                out,
                "{}\n{} --> {}\n{} : {}\n\n",
                i + 1,
                srt_time(s.start_ms),
                srt_time(s.end_ms.max(s.start_ms + 500)),
                s.source.label(),
                s.text
            );
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
        if let Some(sum) = &self.summary {
            std::fs::write(dir.join("compte-rendu.md"), sum)?;
        }
        Ok(dir)
    }
}

pub fn list() -> Vec<Meeting> {
    list_dans(&crate::config::meetings_dir())
}

fn list_dans(racine: &Path) -> Vec<Meeting> {
    let mut v: Vec<Meeting> = std::fs::read_dir(racine).into_iter().flatten().flatten().filter_map(|e| charger(&e.path())).collect();
    v.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    v
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
fn slug(titre: &str) -> String {
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
