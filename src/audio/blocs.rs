//! Audio d'une rencontre en blocs d'environ 2 minutes, rangés par point de l'ordre du jour :
//! écriture (fil audio), index, reconstitution et réparation après un plantage.
//!
//! ```text
//! <rencontre>/
//!   00-hors-point/passage-01/bloc-001-salle.wav     mode séparé : un fichier par source,
//!   00-hors-point/passage-01/bloc-001-distance.wav  coupés au même échantillon
//!   01-ouverture-de-la-seance/passage-01/bloc-001.wav   mode mixé
//!   01-ouverture-de-la-seance/passage-02/…              retour sur le point
//!   index-audio.json                                    ordre de tous les blocs
//! ```
//!
//! Chaque bloc est finalisé avant l'ouverture du suivant : un plantage n'abîme que les blocs
//! ouverts, dont `reprendre` répare l'en-tête. Les rencontres d'avant ce format (salle.wav,
//! distance.wav, reunion.wav pour toute la rencontre) restent telles quelles.

use std::collections::{HashSet, VecDeque};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{SAMPLE_RATE, Source};
use crate::storage::Meeting;

/// À partir de cette durée, le bloc est coupé dès qu'aucun découpeur n'est en cours d'énoncé.
pub const SEUIL: u64 = 120 * SAMPLE_RATE as u64;
/// Durée maximale d'un bloc : coupe forcée, même en pleine parole.
pub const MAXIMUM: u64 = 135 * SAMPLE_RATE as u64;

pub const INDEX: &str = "index-audio.json";

/// Un fichier de l'index : un bloc d'une source.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bloc {
    /// Indice du point (`Agenda::points`) à l'enregistrement ; `None` : hors point.
    pub point: Option<usize>,
    pub passage: u32,
    pub numero: u32,
    pub source: Source,
    /// Premier échantillon, compté depuis le début de la rencontre (horloge des segments).
    pub decalage: u64,
    pub echantillons: u64,
    pub duree_ms: u64,
    /// Chemin relatif au dossier de la rencontre, séparé par « / ».
    pub fichier: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Index {
    #[serde(default = "frequence")]
    pub frequence: u32,
    #[serde(default)]
    pub blocs: Vec<Bloc>,
}

fn frequence() -> u32 {
    SAMPLE_RATE
}

impl Default for Index {
    fn default() -> Self {
        Self { frequence: SAMPLE_RATE, blocs: Vec::new() }
    }
}

/// La rencontre est-elle enregistrée en blocs (et non dans l'ancien format) ?
pub fn existe(dir: &Path) -> bool {
    dir.join(INDEX).is_file()
}

pub fn lire_index(dir: &Path) -> Result<Index> {
    let json = std::fs::read(dir.join(INDEX)).context("index-audio.json introuvable")?;
    serde_json::from_slice(&json).context("index-audio.json illisible")
}

/// Écriture atomique, comme `Meeting::save`.
pub fn ecrire_index(dir: &Path, index: &Index) -> Result<()> {
    let tmp = dir.join(format!("{INDEX}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec_pretty(index)?)?;
    std::fs::rename(&tmp, dir.join(INDEX)).context("Sauvegarde de index-audio.json")
}

/// Dossier d'un point : rang dans l'ordre du jour (00 : hors point) et titre réduit.
pub fn prefixe(point: Option<usize>, titres: &[String]) -> String {
    let largeur = if titres.len() > 99 { 3 } else { 2 };
    let (rang, nom) = match point {
        None => (0, "hors-point".to_string()),
        Some(i) => (i + 1, crate::storage::slug(titres.get(i).map_or("", String::as_str))),
    };
    let nom = if nom.is_empty() { "point" } else { &nom };
    format!("{rang:0largeur$}-{nom}")
}

fn suffixe(source: Source) -> &'static str {
    match source {
        Source::Salle => "-salle",
        Source::Distance => "-distance",
        Source::Mixte => "",
    }
}

fn spec() -> hound::WavSpec {
    hound::WavSpec { channels: 1, sample_rate: SAMPLE_RATE, bits_per_sample: 16, sample_format: hound::SampleFormat::Int }
}

/// Écriture atomique d'un bloc (fichier temporaire puis renommage).
fn ecrire_wav(chemin: &Path, echantillons: &[i16]) -> Result<()> {
    let tmp = chemin.with_extension("wav.tmp");
    let mut w = hound::WavWriter::create(&tmp, spec())?;
    for &x in echantillons {
        w.write_sample(x)?;
    }
    w.finalize()?;
    std::fs::rename(&tmp, chemin)?;
    Ok(())
}

pub fn quantifier(x: f32) -> i16 {
    (x.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
}

/// Inverse exact de `quantifier` : la valeur relue redonne le même échantillon.
fn dequantifier(v: i16) -> f32 {
    (v as f32 + 0.5 * v.signum() as f32) / i16::MAX as f32
}

pub fn empreinte(chemin: &Path) -> Result<String> {
    let mut f = std::fs::File::open(chemin)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}

fn ms(echantillons: u64) -> u64 {
    echantillons * 1000 / SAMPLE_RATE as u64
}

type Wav = hound::WavWriter<std::io::BufWriter<std::fs::File>>;

/// Écrit les blocs d'une rencontre en cours ; vit dans le fil audio.
///
/// Les sources sont écrites au même rythme : seuls les échantillons reçus par toutes sont
/// écrits, ce qui garantit des coupes au même indice d'échantillon pour chaque fichier.
pub struct Ecrivain {
    dir: PathBuf,
    sources: Vec<Source>,
    titres: Vec<String>,
    index: Index,
    /// Échantillons reçus et pas encore écrits, dans l'ordre de `sources`.
    attente: Vec<Vec<f32>>,
    /// Échantillons écrits par source depuis le début de la rencontre.
    ecrits: u64,
    point: Option<usize>,
    /// Passage en cours : numéro et dossier relatif, fixés à l'ouverture de son premier bloc.
    passage: Option<(u32, String)>,
    numero: u32,
    /// Premier échantillon du bloc en cours.
    debut: u64,
    ouverts: Vec<(Wav, String)>,
    /// Premier échantillon du passage en cours (dernier changement de point).
    debut_passage: u64,
    /// Changements de point à appliquer quand l'écriture atteindra leur échantillon.
    coupes: VecDeque<Coupe>,
    /// De quoi défaire le dernier changement de point détecté dans la parole.
    annulable: Option<Annulation>,
}

struct Coupe {
    a: u64,
    point: Option<usize>,
    /// Changement détecté dans la parole : début de l'énoncé de l'annonce, déjà écrit dans
    /// les blocs du point précédent.
    frontiere: Option<u64>,
}

/// État de l'écrivain et fichiers tels qu'ils étaient juste avant une recoupe.
struct Annulation {
    /// Index avant la fermeture des blocs ouverts à la coupe.
    index: Index,
    /// Contenu des blocs raccourcis ou supprimés par la recoupe.
    fichiers: Vec<(String, Vec<u8>)>,
    /// Blocs ouverts à la coupe, dans l'ordre de `sources`.
    rouverts: Vec<String>,
    point: Option<usize>,
    passage: Option<(u32, String)>,
    numero: u32,
    debut: u64,
    debut_passage: u64,
    coupe: u64,
}

impl Ecrivain {
    pub fn new(dir: PathBuf, sources: Vec<Source>, titres: Vec<String>) -> Self {
        let attente = vec![Vec::new(); sources.len()];
        Self {
            dir,
            sources,
            titres,
            index: Index::default(),
            attente,
            ecrits: 0,
            point: None,
            passage: None,
            numero: 0,
            debut: 0,
            ouverts: Vec::new(),
            debut_passage: 0,
            coupes: VecDeque::new(),
            annulable: None,
        }
    }

    /// Titres des points, pour nommer les dossiers des passages à venir.
    pub fn nommer_points(&mut self, titres: Vec<String>) {
        self.titres = titres;
    }

    pub fn pousser(&mut self, source: Source, samples: &[f32]) {
        if let Some(i) = self.sources.iter().position(|&s| s == source) {
            self.attente[i].extend_from_slice(samples);
        }
    }

    /// Ferme les blocs en cours à l'échantillon `coupe` et ouvre ceux de `point`. `coupe` ne peut
    /// précéder ce qui est déjà écrit : c'est l'échantillon transmis aux découpeurs au moment du clic.
    pub fn changer_point(&mut self, point: Option<usize>, coupe: u64) {
        self.annulable = None;
        self.coupes.push_back(Coupe { a: coupe.max(self.ecrits), point, frontiere: None });
    }

    /// Changement de point détecté dans la parole, à l'échantillon `coupe` : l'annonce a commencé
    /// à `frontiere`, quelques secondes plus tôt. L'audio entre les deux, déjà écrit dans les
    /// blocs du point précédent, passe au début du premier bloc du nouveau point (voir `recouper`).
    pub fn changer_point_a_posteriori(&mut self, point: Option<usize>, frontiere: u64, coupe: u64) {
        self.annulable = None;
        self.coupes.push_back(Coupe { a: coupe.max(self.ecrits), point, frontiere: Some(frontiere) });
    }

    /// Le dernier changement détecté ne sera plus annulé : libère la copie des blocs recoupés.
    pub fn oublier_annulation(&mut self) {
        self.annulable = None;
    }

    /// Écrit ce que toutes les sources ont reçu. `parle` : un découpeur est en cours d'énoncé.
    pub fn avancer(&mut self, parle: bool) -> Result<()> {
        let n = self.attente.iter().map(Vec::len).min().unwrap_or(0);
        let r = self.ecrire_attente(n, parle);
        for a in &mut self.attente {
            a.drain(..n);
        }
        r
    }

    fn ecrire_attente(&mut self, n: usize, parle: bool) -> Result<()> {
        let mut fait = 0;
        loop {
            while self.coupes.front().is_some_and(|c| c.a <= self.ecrits) {
                let c = self.coupes.pop_front().expect("coupe en attente");
                match c.frontiere {
                    Some(f) if f < self.ecrits => self.recouper(c.point, f)?,
                    _ => {
                        self.fermer()?;
                        self.entrer(c.point);
                    }
                }
            }
            if fait == n {
                return Ok(());
            }
            let dans_bloc = self.ecrits - self.debut;
            if dans_bloc >= MAXIMUM || (dans_bloc >= SEUIL && !parle) {
                self.fermer()?;
                continue;
            }
            let mut lot = (n - fait) as u64;
            lot = lot.min(if dans_bloc < SEUIL { SEUIL - dans_bloc } else { MAXIMUM - dans_bloc });
            if let Some(c) = self.coupes.front() {
                lot = lot.min(c.a - self.ecrits);
            }
            let lot = lot as usize;
            self.ecrire(fait, lot)?;
            fait += lot;
        }
    }

    fn entrer(&mut self, point: Option<usize>) {
        self.point = point;
        self.passage = None;
        self.numero = 0;
        self.debut_passage = self.ecrits;
    }

    /// Ferme les blocs en cours et passe à `point` à partir de `frontiere`, déjà écrite : les blocs
    /// du passage en cours sont raccourcis à la frontière et leur fin ouvre le premier bloc du
    /// nouveau point. Un bloc illisible ramène à une coupe ordinaire, au moment de la détection.
    fn recouper(&mut self, point: Option<usize>, frontiere: u64) -> Result<()> {
        let mut annulation = Annulation {
            index: self.index.clone(),
            fichiers: Vec::new(),
            rouverts: self.ouverts.iter().map(|(_, f)| f.clone()).collect(),
            point: self.point,
            passage: self.passage.clone(),
            numero: self.numero,
            debut: self.debut,
            debut_passage: self.debut_passage,
            coupe: self.ecrits,
        };
        self.fermer()?;
        let coupe = self.ecrits;
        let f = frontiere.clamp(self.debut_passage, coupe);
        let passage = self.passage.as_ref().map(|(n, _)| *n);
        let touches: Vec<usize> = (0..self.index.blocs.len())
            .filter(|&i| {
                let b = &self.index.blocs[i];
                b.point == self.point && Some(b.passage) == passage && b.decalage + b.echantillons > f
            })
            .collect();
        // Tout est lu avant de toucher aux fichiers.
        let mut lus = Vec::new();
        for &i in &touches {
            let b = &self.index.blocs[i];
            let lu = std::fs::read(self.dir.join(&b.fichier)).map_err(anyhow::Error::from).and_then(|o| Ok((decoder(&o, b)?, o)));
            match lu {
                Ok(x) => lus.push(x),
                Err(e) => {
                    log::warn!("Recoupe impossible ({}) : {e:#} ; changement de point au moment de la détection", b.fichier);
                    self.entrer(point);
                    return Ok(());
                }
            }
        }
        let n = (coupe - f) as usize;
        let mut fin = vec![Vec::with_capacity(n); self.sources.len()];
        let mut supprimes = Vec::new();
        for (&i, (mut echantillons, octets)) in touches.iter().zip(lus) {
            let b = &mut self.index.blocs[i];
            echantillons.resize(b.echantillons as usize, 0);
            let garde = f.saturating_sub(b.decalage) as usize;
            if let Some(s) = self.sources.iter().position(|&s| s == b.source) {
                fin[s].extend_from_slice(&echantillons[garde..]);
            }
            annulation.fichiers.push((b.fichier.clone(), octets));
            let chemin = self.dir.join(&b.fichier);
            if garde == 0 {
                std::fs::remove_file(&chemin).with_context(|| format!("Suppression de {}", b.fichier))?;
                supprimes.push(i);
            } else {
                ecrire_wav(&chemin, &echantillons[..garde]).with_context(|| format!("Recoupe de {}", b.fichier))?;
                b.echantillons = garde as u64;
                b.duree_ms = ms(garde as u64);
                b.sha256 = empreinte(&chemin)?;
            }
        }
        let mut i = 0;
        self.index.blocs.retain(|_| {
            i += 1;
            !supprimes.contains(&(i - 1))
        });
        log::info!("Recoupe : {} s d'audio passent au nouveau point", ms(n as u64) / 1000);
        self.point = point;
        self.passage = None;
        self.numero = 0;
        self.debut = f;
        self.debut_passage = f;
        if n > 0 {
            self.ouvrir()?;
            for ((w, _), mut v) in self.ouverts.iter_mut().zip(fin) {
                v.resize(n, 0);
                for x in v {
                    w.write_sample(x)?;
                }
            }
        }
        ecrire_index(&self.dir, &self.index)?;
        self.annulable = Some(annulation);
        Ok(())
    }

    /// Défait le dernier changement de point détecté dans la parole : les blocs, l'index et
    /// l'écrivain reviennent à leur état d'avant, et l'audio enregistré depuis repart dans le
    /// passage d'origine. Retourne `false` s'il n'y a rien à défaire.
    pub fn annuler(&mut self) -> Result<bool> {
        // Pas encore appliqué : il suffit de le retirer.
        if self.coupes.back().is_some_and(|c| c.frontiere.is_some()) {
            self.coupes.pop_back();
            return Ok(true);
        }
        let Some(a) = self.annulable.take() else { return Ok(false) };
        self.fermer()?;
        let maintenant = self.ecrits;
        let mut rejoue = vec![Vec::new(); self.sources.len()];
        if let Some((n, rel)) = self.passage.clone() {
            let point = self.point;
            let nouveaux: Vec<Bloc> = self.index.blocs.iter().filter(|b| b.point == point && b.passage == n).cloned().collect();
            for b in &nouveaux {
                let mut v = lire_bloc(&self.dir, b).unwrap_or_else(|e| {
                    log::warn!("Annulation : {} illisible ({e:#}), remplacé par du silence", b.fichier);
                    Vec::new()
                });
                v.resize(b.echantillons as usize, 0);
                if let Some(s) = self.sources.iter().position(|&s| s == b.source) {
                    rejoue[s].extend(v);
                }
                let _ = std::fs::remove_file(self.dir.join(&b.fichier));
            }
            self.index.blocs.retain(|b| !(b.point == point && b.passage == n));
            // Dossiers devenus vides : le passage, puis le point.
            let d = self.dir.join(&rel);
            if std::fs::remove_dir(&d).is_ok()
                && let Some(p) = d.parent()
            {
                let _ = std::fs::remove_dir(p);
            }
        }
        // Le nouveau passage commence à la frontière : ce qui précède la coupe est déjà dans les
        // fichiers d'origine.
        let deja = (a.coupe - self.debut_passage) as usize;
        for (fichier, octets) in &a.fichiers {
            let chemin = self.dir.join(fichier);
            if let Some(p) = chemin.parent() {
                std::fs::create_dir_all(p)?;
            }
            let tmp = chemin.with_extension("wav.tmp");
            std::fs::write(&tmp, octets)?;
            std::fs::rename(&tmp, &chemin).with_context(|| format!("Rétablissement de {fichier}"))?;
        }
        self.index = a.index;
        let mut ouverts = Vec::new();
        for f in a.rouverts {
            ouverts.push((hound::WavWriter::append(self.dir.join(&f)).with_context(|| format!("Réouverture de {f}"))?, f));
        }
        self.ouverts = ouverts;
        self.point = a.point;
        self.passage = a.passage;
        self.numero = a.numero;
        self.debut = a.debut;
        self.debut_passage = a.debut_passage;
        self.ecrits = a.coupe;
        let n = (maintenant - a.coupe) as usize;
        for (attente, mut v) in self.attente.iter_mut().zip(rejoue) {
            v.drain(..deja.min(v.len()));
            v.resize(n, 0);
            attente.splice(0..0, v.into_iter().map(dequantifier));
        }
        ecrire_index(&self.dir, &self.index)?;
        Ok(true)
    }

    fn ecrire(&mut self, de: usize, lot: usize) -> Result<()> {
        // L'horloge avance même si l'écriture échoue : les blocs suivants restent à leur place.
        self.ecrits += lot as u64;
        if self.ouverts.is_empty() {
            self.ouvrir()?;
        }
        for ((w, _), a) in self.ouverts.iter_mut().zip(&self.attente) {
            for &x in &a[de..de + lot] {
                w.write_sample(quantifier(x))?;
            }
        }
        Ok(())
    }

    fn ouvrir(&mut self) -> Result<()> {
        if self.passage.is_none() {
            let dossier = prefixe(self.point, &self.titres);
            // Le numéro de passage vient du disque : un point renuméroté ne réécrit jamais un passage.
            let deja = std::fs::read_dir(self.dir.join(&dossier))
                .map(|d| {
                    d.flatten()
                        .filter_map(|e| e.file_name().to_str()?.strip_prefix("passage-")?.parse::<u32>().ok())
                        .max()
                        .unwrap_or(0)
                })
                .unwrap_or(0);
            let n = deja + 1;
            self.passage = Some((n, format!("{dossier}/passage-{n:02}")));
        }
        let rel = self.passage.as_ref().map(|(_, r)| r.clone()).unwrap_or_default();
        std::fs::create_dir_all(self.dir.join(&rel)).with_context(|| format!("Création de {rel}"))?;
        self.numero += 1;
        let mut ouverts = Vec::new();
        for &s in &self.sources {
            let chemin = format!("{rel}/bloc-{:03}{}.wav", self.numero, suffixe(s));
            let w = hound::WavWriter::create(self.dir.join(&chemin), spec()).with_context(|| format!("Création de {chemin}"))?;
            ouverts.push((w, chemin));
        }
        self.ouverts = ouverts;
        Ok(())
    }

    /// Finalise les blocs en cours, calcule leur empreinte et réécrit l'index.
    fn fermer(&mut self) -> Result<()> {
        let echantillons = self.ecrits - self.debut;
        let decalage = std::mem::replace(&mut self.debut, self.ecrits);
        if self.ouverts.is_empty() {
            return Ok(());
        }
        let passage = self.passage.as_ref().map_or(1, |(n, _)| *n);
        let mut erreur = None;
        for ((w, fichier), &source) in std::mem::take(&mut self.ouverts).into_iter().zip(&self.sources) {
            let sha = w.finalize().map_err(anyhow::Error::from).and_then(|()| empreinte(&self.dir.join(&fichier)));
            match sha {
                Ok(sha256) => self.index.blocs.push(Bloc {
                    point: self.point,
                    passage,
                    numero: self.numero,
                    source,
                    decalage,
                    echantillons,
                    duree_ms: ms(echantillons),
                    fichier,
                    sha256,
                }),
                Err(e) => erreur = Some(e.context(format!("Fermeture de {fichier}"))),
            }
        }
        ecrire_index(&self.dir, &self.index)?;
        erreur.map_or(Ok(()), Err)
    }

    /// Fin de l'enregistrement : complète les sources en retard par du silence et ferme les blocs.
    pub fn terminer(mut self) -> Result<Index> {
        let max = self.attente.iter().map(Vec::len).max().unwrap_or(0);
        for a in &mut self.attente {
            a.resize(max, 0.0);
        }
        let r = self.avancer(false);
        self.fermer()?;
        r.map(|()| self.index)
    }
}

// ───────────────────────── Reconstitution ─────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Portee {
    /// Tous les passages d'un point, mis bout à bout.
    Point(usize),
    Tout,
}

/// Sources enregistrées, dans l'ordre de l'index.
pub fn sources(index: &Index) -> Vec<Source> {
    let mut v: Vec<Source> = Vec::new();
    for b in &index.blocs {
        if !v.contains(&b.source) {
            v.push(b.source);
        }
    }
    v
}

/// Reconstitue un WAV à partir des blocs, dans l'ordre de l'index. Un bloc manquant, abîmé ou
/// absent de l'index est remplacé par du silence pour que les horodatages restent justes.
/// Retourne les avertissements.
pub fn reconstituer(rencontre: &Meeting, portee: Portee, source: Source, sortie: &Path) -> Result<Vec<String>> {
    reconstituer_dans(&rencontre.dir(), rencontre, portee, source, sortie)
}

pub fn reconstituer_dans(dir: &Path, rencontre: &Meeting, portee: Portee, source: Source, sortie: &Path) -> Result<Vec<String>> {
    let index = lire_index(dir)?;
    let mut blocs: Vec<&Bloc> = index.blocs.iter().filter(|b| b.source == source).collect();
    blocs.sort_by_key(|b| b.decalage);
    let mut w = hound::WavWriter::create(sortie, spec()).with_context(|| format!("Création de {}", sortie.display()))?;
    let mut avertissements = Vec::new();
    let silence = |w: &mut Wav, n: u64| -> Result<()> {
        for _ in 0..n {
            w.write_sample(0i16)?;
        }
        Ok(())
    };
    // Fin du dernier bloc retenu, et son passage : un trou n'est comblé qu'à l'intérieur d'une suite.
    let mut fin = 0u64;
    let mut precedent: Option<(Option<usize>, u32)> = None;
    for b in blocs {
        // Les périodes de la rencontre suivent les renumérotations des points ; leurs débuts sont
        // les échantillons exacts des coupes.
        if let Portee::Point(n) = portee
            && rencontre.point_de(ms(b.decalage)) != Some(n)
        {
            continue;
        }
        let suite = portee == Portee::Tout || precedent == Some((b.point, b.passage));
        if suite && b.decalage > fin {
            avertissements.push(format!("{} s d'audio absentes de l'index avant {} : remplacées par du silence", ms(b.decalage - fin) / 1000, b.fichier));
            silence(&mut w, b.decalage - fin)?;
        }
        match lire_bloc(dir, b) {
            Ok(samples) => {
                for &s in &samples {
                    w.write_sample(s)?;
                }
                if (samples.len() as u64) < b.echantillons {
                    avertissements.push(format!("{} est incomplet : la fin est remplacée par du silence", b.fichier));
                    silence(&mut w, b.echantillons - samples.len() as u64)?;
                }
            }
            Err(e) => {
                avertissements.push(format!("{} : {e:#} ; remplacé par {} s de silence", b.fichier, b.duree_ms / 1000));
                silence(&mut w, b.echantillons)?;
            }
        }
        fin = b.decalage + b.echantillons;
        precedent = Some((b.point, b.passage));
    }
    w.finalize()?;
    for a in &avertissements {
        log::warn!("Reconstitution : {a}");
    }
    Ok(avertissements)
}

fn lire_bloc(dir: &Path, b: &Bloc) -> Result<Vec<i16>> {
    let chemin = dir.join(&b.fichier);
    let octets = std::fs::read(&chemin).context("fichier manquant")?;
    decoder(&octets, b)
}

fn decoder(octets: &[u8], b: &Bloc) -> Result<Vec<i16>> {
    if format!("{:x}", Sha256::digest(octets)) != b.sha256 {
        bail!("empreinte différente de l'index (fichier modifié ou abîmé)");
    }
    let lecteur = hound::WavReader::new(std::io::Cursor::new(octets))?;
    let mut v: Vec<i16> = lecteur.into_samples::<i16>().collect::<Result<_, _>>()?;
    v.truncate(b.echantillons as usize);
    Ok(v)
}

/// Exporte l'audio d'une rencontre : un fichier par source (suffixé quand il y en a plusieurs).
/// Retourne les messages à afficher (fichiers écrits, puis avertissements).
pub fn exporter(rencontre: &Meeting, portee: Portee, chemin: &Path) -> Result<Vec<String>> {
    let index = lire_index(&rencontre.dir())?;
    let sources = sources(&index);
    if sources.is_empty() {
        bail!("Aucun audio enregistré pour cette rencontre");
    }
    let mut messages = Vec::new();
    let mut avertissements = Vec::new();
    for &s in &sources {
        let sortie = if sources.len() == 1 {
            chemin.to_path_buf()
        } else {
            let base = chemin.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            chemin.with_file_name(format!("{base}{}.wav", suffixe(s)))
        };
        avertissements.extend(reconstituer(rencontre, portee, s, &sortie)?);
        messages.push(format!("Audio exporté : {}", sortie.display()));
    }
    messages.extend(avertissements);
    Ok(messages)
}

// ───────────────────────── Reprise après un plantage ─────────────────────────

/// Recalcule les tailles de l'en-tête d'un WAV 16 bits mono interrompu (données jusqu'à la fin
/// du fichier) ; retourne le nombre d'échantillons.
pub fn reparer_entete(chemin: &Path) -> Result<u64> {
    let mut f = std::fs::OpenOptions::new().read(true).write(true).open(chemin)?;
    let taille = f.metadata()?.len();
    let mut entete = [0u8; 12];
    f.read_exact(&mut entete).context("en-tête WAV incomplet")?;
    if &entete[..4] != b"RIFF" || &entete[8..] != b"WAVE" {
        bail!("ce n'est pas un fichier WAV");
    }
    let mut pos = 12u64;
    loop {
        if pos + 8 > taille {
            bail!("aucune donnée audio");
        }
        f.seek(SeekFrom::Start(pos))?;
        let mut morceau = [0u8; 8];
        f.read_exact(&mut morceau)?;
        let longueur = u32::from_le_bytes([morceau[4], morceau[5], morceau[6], morceau[7]]) as u64;
        if &morceau[..4] == b"data" {
            // Un échantillon à moitié écrit est retiré.
            let donnees = ((taille - pos - 8) & !1).min(u32::MAX as u64 - pos);
            f.set_len(pos + 8 + donnees)?;
            f.seek(SeekFrom::Start(pos + 4))?;
            f.write_all(&(donnees as u32).to_le_bytes())?;
            f.seek(SeekFrom::Start(4))?;
            f.write_all(&((pos + donnees) as u32).to_le_bytes())?;
            f.sync_all()?;
            return Ok(donnees / 2);
        }
        pos += 8 + longueur + (longueur & 1);
    }
}

/// Fichier de bloc trouvé sur le disque.
struct Trouve {
    fichier: String,
    point: Option<usize>,
    passage: u32,
    numero: u32,
    source: Source,
    modifie: std::time::SystemTime,
}

/// Blocs présents sur le disque, d'après leur nom.
fn fichiers_blocs(dir: &Path) -> Vec<Trouve> {
    let mut v = Vec::new();
    let Ok(points) = std::fs::read_dir(dir) else { return v };
    for p in points.flatten().filter(|e| e.path().is_dir()) {
        let nom_point = p.file_name().to_string_lossy().into_owned();
        let Some(rang) = nom_point.split_once('-').and_then(|(r, _)| (r.len() >= 2).then(|| r.parse::<usize>().ok()).flatten()) else {
            continue;
        };
        let Ok(passages) = std::fs::read_dir(p.path()) else { continue };
        for pa in passages.flatten() {
            let nom_passage = pa.file_name().to_string_lossy().into_owned();
            let Some(passage) = nom_passage.strip_prefix("passage-").and_then(|n| n.parse::<u32>().ok()) else { continue };
            let Ok(fichiers) = std::fs::read_dir(pa.path()) else { continue };
            for f in fichiers.flatten() {
                let nom = f.file_name().to_string_lossy().into_owned();
                let Some(reste) = nom.strip_prefix("bloc-").and_then(|n| n.strip_suffix(".wav")) else { continue };
                let (numero, source) = match reste.split_once('-') {
                    None => (reste, Source::Mixte),
                    Some((n, "salle")) => (n, Source::Salle),
                    Some((n, "distance")) => (n, Source::Distance),
                    Some(_) => continue,
                };
                let Ok(numero) = numero.parse::<u32>() else { continue };
                v.push(Trouve {
                    fichier: format!("{nom_point}/{nom_passage}/{nom}"),
                    point: rang.checked_sub(1),
                    passage,
                    numero,
                    source,
                    modifie: f.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH),
                });
            }
        }
    }
    v
}

fn orphelins(dir: &Path, index: &Index) -> Vec<Trouve> {
    let connus: HashSet<&str> = index.blocs.iter().map(|b| b.fichier.as_str()).collect();
    fichiers_blocs(dir).into_iter().filter(|t| !connus.contains(t.fichier.as_str())).collect()
}

/// Une rencontre est incomplète si sa dernière période n'a pas de fin, ou si des blocs
/// n'ont jamais été inscrits à l'index (ils étaient ouverts lors d'un plantage).
pub fn interrompue(rencontre: &Meeting, dir: &Path) -> bool {
    rencontre.periodes.last().is_some_and(|p| p.fin_ms.is_none())
        || !orphelins(dir, &lire_index(dir).unwrap_or_default()).is_empty()
}

/// Répare les blocs absents de l'index (en-tête recalculé d'après la taille du fichier) et
/// reconstruit l'index à partir des fichiers présents. Retourne la fin de l'audio, en échantillons.
pub fn reconstruire_index(dir: &Path) -> Result<u64> {
    let mut index = lire_index(dir).unwrap_or_default();
    index.blocs.retain(|b| dir.join(&b.fichier).is_file());
    let mut fin = index.blocs.iter().map(|b| b.decalage + b.echantillons).max().unwrap_or(0);
    let mut orphelins = orphelins(dir, &index);
    if orphelins.is_empty() && index.blocs.is_empty() {
        return Ok(0);
    }
    // Les blocs d'un même numéro (une source chacun) partagent un même décalage ; les groupes
    // suivent l'ordre où ils ont été écrits.
    let cle = |t: &Trouve| (t.fichier.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default(), t.numero);
    orphelins.sort_by(|a, b| a.modifie.cmp(&b.modifie).then_with(|| cle(a).cmp(&cle(b))));
    let mut groupes: Vec<((String, u32), Vec<Trouve>)> = Vec::new();
    for t in orphelins {
        let k = cle(&t);
        match groupes.iter_mut().find(|(g, _)| *g == k) {
            Some((_, v)) => v.push(t),
            None => groupes.push((k, vec![t])),
        }
    }
    for (_, groupe) in groupes {
        let mut longueur = 0;
        for t in groupe {
            let chemin = dir.join(&t.fichier);
            let n = match reparer_entete(&chemin) {
                Ok(n) => n,
                Err(e) => {
                    log::warn!("Bloc {} irrécupérable : {e:#}", t.fichier);
                    continue;
                }
            };
            log::info!("Bloc {} réparé : {} s", t.fichier, ms(n) / 1000);
            index.blocs.push(Bloc {
                point: t.point,
                passage: t.passage,
                numero: t.numero,
                source: t.source,
                decalage: fin,
                echantillons: n,
                duree_ms: ms(n),
                sha256: empreinte(&chemin)?,
                fichier: t.fichier,
            });
            longueur = longueur.max(n);
        }
        fin += longueur;
    }
    index.blocs.sort_by_key(|b| (b.decalage, b.source as u8));
    ecrire_index(dir, &index)?;
    Ok(fin)
}

/// Reprise d'une rencontre interrompue : répare l'audio, ferme la période en cours et complète
/// la durée. La rencontre reste à sauvegarder.
pub fn reprendre(rencontre: &mut Meeting, dir: &Path) -> Result<()> {
    let fin_audio = ms(reconstruire_index(dir)?);
    let fin_texte = rencontre.segments.iter().map(|s| s.end_ms).max().unwrap_or(0);
    let fin = fin_audio.max(fin_texte);
    rencontre.fermer_periode(fin);
    rencontre.duration_s = rencontre.duration_s.max(fin / 1000);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::dsp::{Segmenter, Utterance};
    use crate::storage::Declenchement;

    const SR: u64 = SAMPLE_RATE as u64;

    fn temporaire(nom: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tourdetable-blocs-{}-{nom}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Signal déterministe : bruit très faible (silence pour le VAD), voix (sinus) sur les
    /// intervalles donnés, en secondes.
    fn signal(secs: u64, voix: &[(u64, u64)], graine: u32, freq: f32) -> Vec<f32> {
        let mut x = graine;
        (0..secs * SR)
            .map(|i| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let bruit = ((x >> 16) as f32 / 65_536.0 - 0.5) * 0.001;
                let t = i / SR;
                if voix.iter().any(|&(a, b)| (a..b).contains(&t)) {
                    bruit + 0.3 * (2.0 * std::f32::consts::PI * freq * i as f32 / SR as f32).sin()
                } else {
                    bruit
                }
            })
            .collect()
    }

    fn lire(chemin: &Path) -> Vec<i16> {
        hound::WavReader::open(chemin).unwrap().into_samples::<i16>().map(Result::unwrap).collect()
    }

    /// Flux de 7 minutes en mode séparé, livré comme par le fil audio : morceaux de 10 ms, la
    /// distance en retard de 0,3 s sur la salle. Hors point 0–50 s, point 0 de 50 à 230 s (voix
    /// continue de 160 à 200 s : coupe forcée à 135 s), point 1 de 230 à 290 s, retour sur le
    /// point 0 de 290 s à la fin (voix de 405 à 415 s : coupe au premier silence après 120 s).
    #[test]
    fn sept_minutes_en_mode_separe() {
        let dir = temporaire("sept-minutes");
        let duree = 420;
        let salle = signal(duree, &[(10, 14), (160, 200)], 1, 300.0);
        let distance = signal(duree, &[(60, 70), (405, 415)], 2, 220.0);
        let titres = vec!["Ouverture de la séance".to_string(), "Budget 2027 : adoption".to_string()];
        let mut e = Ecrivain::new(dir.clone(), vec![Source::Salle, Source::Distance], titres.clone());
        let mut segs = [Segmenter::new(Source::Salle, 0.5), Segmenter::new(Source::Distance, 0.5)];
        let mut utts: Vec<Utterance> = Vec::new();
        let mut m = Meeting::nouvelle("m", None, chrono::Local::now());

        let morceau = 160usize;
        let retard = (3 * SR / 10) as usize;
        let changements = [(50 * SR, Some(0)), (230 * SR, Some(1)), (290 * SR, Some(0))];
        let mut prochain = 0;
        let (mut ps, mut pd) = (0usize, 0usize);
        while ps < salle.len() || pd < distance.len() {
            if ps < salle.len() {
                let fin = (ps + morceau).min(salle.len());
                segs[0].push(&salle[ps..fin], &mut utts);
                e.pousser(Source::Salle, &salle[ps..fin]);
                ps = fin;
            }
            if ps > retard || ps >= salle.len() {
                let fin = (pd + morceau).min(distance.len());
                segs[1].push(&distance[pd..fin], &mut utts);
                e.pousser(Source::Distance, &distance[pd..fin]);
                pd = fin;
            }
            // Clic : coupe à l'échantillon transmis le plus avancé, comme dans le fil audio.
            let transmis = ps.max(pd) as u64;
            if let Some(&(t, p)) = changements.get(prochain)
                && transmis >= t
            {
                e.changer_point(p, transmis);
                m.debuter_point(p, ms(transmis), Declenchement::Manuel);
                prochain += 1;
            }
            let parle = segs.iter().any(Segmenter::en_enonce);
            e.avancer(parle).unwrap();
        }
        let index = e.terminer().unwrap();
        m.fermer_periode(ms(duree * SR));

        // 1 bloc hors point, 2 sur le point 0, 1 sur le point 1, 2 au retour sur le point 0 ; par source.
        let par_source: Vec<&Bloc> = index.blocs.iter().filter(|b| b.source == Source::Salle).collect();
        let decrits: Vec<(Option<usize>, u32, u32)> = par_source.iter().map(|b| (b.point, b.passage, b.numero)).collect();
        assert_eq!(decrits, [(None, 1, 1), (Some(0), 1, 1), (Some(0), 1, 2), (Some(1), 1, 1), (Some(0), 2, 1), (Some(0), 2, 2)]);
        assert_eq!(index.blocs.len(), 12);
        assert!(index.blocs.iter().all(|b| b.echantillons <= MAXIMUM), "bloc de plus de 135 s");
        // Coupe forcée en pleine parole, puis coupe au premier silence après 120 s.
        assert_eq!(par_source[1].echantillons, MAXIMUM);
        assert!((SEUIL..MAXIMUM).contains(&par_source[4].echantillons), "{}", par_source[4].echantillons);
        assert_eq!(par_source[0].fichier, "00-hors-point/passage-01/bloc-001-salle.wav");
        assert_eq!(par_source[3].fichier, "02-budget-2027-adoption/passage-01/bloc-001-salle.wav");
        assert_eq!(par_source[5].fichier, "01-ouverture-de-la-seance/passage-02/bloc-002-salle.wav");
        // Les deux sources sont coupées au même échantillon, et les blocs se suivent sans trou.
        let distance_blocs: Vec<&Bloc> = index.blocs.iter().filter(|b| b.source == Source::Distance).collect();
        let mut attendu = 0;
        for (s, d) in par_source.iter().zip(&distance_blocs) {
            assert_eq!((s.decalage, s.echantillons), (d.decalage, d.echantillons));
            assert_eq!(s.decalage, attendu);
            attendu += s.echantillons;
        }
        assert_eq!(attendu, duree * SR);
        // Les changements de point tombent sur des coupes.
        for p in &m.periodes {
            assert!(par_source.iter().any(|b| ms(b.decalage) == p.debut_ms), "{p:?}");
        }
        assert_eq!(lire_index(&dir).unwrap().blocs, index.blocs);

        // Reconstitution identique au flux d'entrée, à l'échantillon près.
        let sortie = dir.join("tout-salle.wav");
        assert!(reconstituer_dans(&dir, &m, Portee::Tout, Source::Salle, &sortie).unwrap().is_empty());
        assert_eq!(lire(&sortie), salle.iter().map(|&x| quantifier(x)).collect::<Vec<_>>());
        let sortie = dir.join("tout-distance.wav");
        reconstituer_dans(&dir, &m, Portee::Tout, Source::Distance, &sortie).unwrap();
        assert_eq!(lire(&sortie), distance.iter().map(|&x| quantifier(x)).collect::<Vec<_>>());

        // Un point : ses deux passages bout à bout.
        let sortie = dir.join("point-0.wav");
        reconstituer_dans(&dir, &m, Portee::Point(0), Source::Salle, &sortie).unwrap();
        let c = |s: u64| (s * SR) as usize;
        let mut attendu: Vec<i16> = salle[c(50)..c(230)].iter().map(|&x| quantifier(x)).collect();
        attendu.extend(salle[c(290)..].iter().map(|&x| quantifier(x)));
        assert_eq!(lire(&sortie), attendu);

        // Bloc manquant : remplacé par du silence, les horodatages ne bougent pas.
        std::fs::remove_file(dir.join(&par_source[3].fichier)).unwrap();
        let sortie = dir.join("trou.wav");
        let avert = reconstituer_dans(&dir, &m, Portee::Tout, Source::Salle, &sortie).unwrap();
        assert_eq!(avert.len(), 1, "{avert:?}");
        let relu = lire(&sortie);
        assert_eq!(relu.len() as u64, duree * SR);
        let b = par_source[3];
        assert!(relu[b.decalage as usize..(b.decalage + b.echantillons) as usize].iter().all(|&s| s == 0));
        assert_eq!(relu[(b.decalage + b.echantillons) as usize], quantifier(salle[(b.decalage + b.echantillons) as usize]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Plantage : le dernier bloc n'a jamais été finalisé (tailles de l'en-tête à zéro, dernier
    /// échantillon à moitié écrit) et n'est pas dans l'index.
    #[test]
    fn reparation_d_un_bloc_tronque() {
        let dir = temporaire("reparation");
        let flux = signal(150, &[], 3, 300.0);
        let mut e = Ecrivain::new(dir.clone(), vec![Source::Mixte], vec!["Ouverture".into()]);
        e.changer_point(Some(0), 0);
        e.pousser(Source::Mixte, &flux);
        e.avancer(false).unwrap();
        e.terminer().unwrap();
        let mut index = lire_index(&dir).unwrap();
        assert_eq!(index.blocs.len(), 2);
        let dernier = index.blocs.pop().unwrap();
        ecrire_index(&dir, &index).unwrap();
        let chemin = dir.join(&dernier.fichier);
        let mut octets = std::fs::read(&chemin).unwrap();
        let data = octets.windows(4).position(|w| w == b"data").unwrap();
        octets[4..8].fill(0);
        octets[data + 4..data + 8].fill(0);
        octets.push(0x7f); // demi-échantillon
        std::fs::write(&chemin, &octets).unwrap();
        assert!(hound::WavReader::open(&chemin).map_or(true, |r| r.len() == 0));

        let mut m = Meeting::nouvelle("m", None, chrono::Local::now());
        m.debuter_point(Some(0), 0, Declenchement::Manuel);
        assert!(interrompue(&m, &dir));
        reprendre(&mut m, &dir).unwrap();
        assert!(!interrompue(&m, &dir));
        assert_eq!(m.periodes[0].fin_ms, Some(150_000));
        assert_eq!(m.duration_s, 150);

        let index = lire_index(&dir).unwrap();
        assert_eq!(index.blocs.len(), 2);
        let repare = &index.blocs[1];
        assert_eq!((repare.decalage, repare.echantillons, repare.numero), (SEUIL, 30 * SR, 2));
        assert_eq!(repare.fichier, dernier.fichier);
        let sortie = dir.join("tout.wav");
        assert!(reconstituer_dans(&dir, &m, Portee::Tout, Source::Mixte, &sortie).unwrap().is_empty());
        assert_eq!(lire(&sortie), flux.iter().map(|&x| quantifier(x)).collect::<Vec<_>>());
        let _ = std::fs::remove_dir_all(&dir);
    }

    enum Ev {
        Point(Option<usize>),
        /// Point détecté et frontière, en secondes.
        Auto(Option<usize>, u64),
        Annuler,
    }

    /// 200 s en mode séparé, morceaux de 10 ms ; les événements tombent à la seconde donnée.
    fn simuler(dir: &Path, evenements: &[(u64, Ev)]) -> (Index, Vec<f32>) {
        let salle = signal(200, &[(20, 40), (100, 150)], 4, 300.0);
        let distance = signal(200, &[(60, 80)], 5, 220.0);
        let titres = vec!["Ouverture".to_string(), "Budget".to_string()];
        let mut e = Ecrivain::new(dir.to_path_buf(), vec![Source::Salle, Source::Distance], titres);
        let morceau = 160;
        let mut prochain = 0;
        for debut in (0..salle.len()).step_by(morceau) {
            while let Some((t, ev)) = evenements.get(prochain)
                && debut as u64 >= t * SR
            {
                match ev {
                    Ev::Point(p) => e.changer_point(*p, debut as u64),
                    Ev::Auto(p, f) => e.changer_point_a_posteriori(*p, f * SR, debut as u64),
                    Ev::Annuler => assert!(e.annuler().unwrap()),
                }
                prochain += 1;
            }
            e.pousser(Source::Salle, &salle[debut..debut + morceau]);
            e.pousser(Source::Distance, &distance[debut..debut + morceau]);
            e.avancer(false).unwrap();
        }
        (e.terminer().unwrap(), salle)
    }

    fn resume(index: &Index) -> Vec<(Option<usize>, u64, u64, String, String)> {
        index.blocs.iter().map(|b| (b.point, b.decalage / SR, b.echantillons / SR, b.fichier.clone(), b.sha256.clone())).collect()
    }

    /// Changement détecté à 140 s, annonce commencée à 125 s : la frontière tombe dans le bloc
    /// coupé à 130 s (10 s + 120 s). Les deux blocs sont recoupés, puis l'annulation redonne
    /// exactement les fichiers d'un enregistrement sans changement.
    #[test]
    fn recoupe_a_la_frontiere_puis_annulation() {
        let reference = temporaire("recoupe-reference");
        let (attendu, _) = simuler(&reference, &[(10, Ev::Point(Some(0)))]);
        let r: Vec<(Option<usize>, u64, u64)> = attendu.blocs.iter().map(|b| (b.point, b.decalage / SR, b.echantillons / SR)).collect();
        assert_eq!(r, [(None, 0, 10), (None, 0, 10), (Some(0), 10, 120), (Some(0), 10, 120), (Some(0), 130, 70), (Some(0), 130, 70)]);

        let dir = temporaire("recoupe");
        let (index, salle) = simuler(&dir, &[(10, Ev::Point(Some(0))), (140, Ev::Auto(Some(1), 125))]);
        let r: Vec<(Option<usize>, u64, u64, &str)> =
            index.blocs.iter().filter(|b| b.source == Source::Salle).map(|b| (b.point, b.decalage / SR, b.echantillons / SR, b.fichier.as_str())).collect();
        assert_eq!(r, [
            (None, 0, 10, "00-hors-point/passage-01/bloc-001-salle.wav"),
            (Some(0), 10, 115, "01-ouverture/passage-01/bloc-001-salle.wav"),
            (Some(1), 125, 75, "02-budget/passage-01/bloc-001-salle.wav"),
        ]);
        assert!(!dir.join("01-ouverture/passage-01/bloc-002-salle.wav").exists());
        assert_eq!(lire_index(&dir).unwrap().blocs, index.blocs);
        let mut m = Meeting::nouvelle("m", None, chrono::Local::now());
        m.debuter_point(Some(0), 10_000, Declenchement::Manuel);
        m.debuter_point(Some(1), 125_000, Declenchement::Auto);
        m.fermer_periode(200_000);
        let sortie = dir.join("tout.wav");
        assert!(reconstituer_dans(&dir, &m, Portee::Tout, Source::Salle, &sortie).unwrap().is_empty());
        assert_eq!(lire(&sortie), salle.iter().map(|&x| quantifier(x)).collect::<Vec<_>>());
        let sortie = dir.join("budget.wav");
        reconstituer_dans(&dir, &m, Portee::Point(1), Source::Salle, &sortie).unwrap();
        assert_eq!(lire(&sortie), salle[(125 * SR) as usize..].iter().map(|&x| quantifier(x)).collect::<Vec<_>>());

        // Annulation 5 s après : mêmes blocs, à l'octet près, que sans changement.
        let annule = temporaire("recoupe-annulee");
        let (index, _) = simuler(&annule, &[(10, Ev::Point(Some(0))), (140, Ev::Auto(Some(1), 125)), (145, Ev::Annuler)]);
        assert_eq!(resume(&index), resume(&attendu));
        assert!(!annule.join("02-budget").exists());
        for b in &index.blocs {
            assert_eq!(std::fs::read(annule.join(&b.fichier)).unwrap(), std::fs::read(reference.join(&b.fichier)).unwrap(), "{}", b.fichier);
        }
        for d in [reference, dir, annule] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn noms_des_dossiers() {
        let titres: Vec<String> = vec!["Ouverture de la séance".into(), "".into()];
        assert_eq!(prefixe(None, &titres), "00-hors-point");
        assert_eq!(prefixe(Some(0), &titres), "01-ouverture-de-la-seance");
        assert_eq!(prefixe(Some(1), &titres), "02-point");
        let cent: Vec<String> = (0..120).map(|i| format!("Point {i}")).collect();
        assert_eq!(prefixe(Some(104), &cent), "105-point-104");
        assert_eq!(prefixe(None, &cent), "000-hors-point");
    }
}
