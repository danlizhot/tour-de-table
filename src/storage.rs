//! Rencontres enregistrées : un dossier par rencontre, tout reste en local.
//!
//! ```text
//! rencontres/2026-10-22_14-30-05/
//!   rencontre.json   transcription complète (sauvée après chaque segment)
//!   salle.wav        micro (optionnel)
//!   distance.wav     son système (optionnel)
//! ```

use std::fmt::Write as _;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

use crate::stt::Segment;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Meeting {
    pub id: String,
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

impl Meeting {
    pub fn new(model: &str) -> Self {
        let now = Local::now();
        Self {
            id: now.format("%Y-%m-%d_%H-%M-%S").to_string(),
            title: format!("Rencontre du {}", now.format("%Y-%m-%d à %H h %M")),
            started_at: now,
            duration_s: 0,
            model: model.to_string(),
            segments: Vec::new(),
            summary: None,
            agenda: None,
        }
    }

    pub fn dir(&self) -> PathBuf {
        crate::config::meetings_dir().join(&self.id)
    }

    pub fn save(&self) -> Result<()> {
        let dir = self.dir();
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
    let mut v: Vec<Meeting> = std::fs::read_dir(crate::config::meetings_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| std::fs::read(e.path().join("rencontre.json")).ok())
        .filter_map(|b| serde_json::from_slice(&b).ok())
        .collect();
    v.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    v
}

pub fn delete(m: &Meeting) -> Result<()> {
    // Corbeille du système plutôt qu'une suppression définitive : on se contente
    // de renommer le dossier, l'utilisateur peut le récupérer.
    let dir = m.dir();
    let trash = crate::config::data_dir().join("corbeille");
    std::fs::create_dir_all(&trash)?;
    std::fs::rename(&dir, trash.join(&m.id)).context("Déplacement vers la corbeille de Noter")
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
}
