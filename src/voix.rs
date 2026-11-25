//! Reconnaissance des voix (diarisation) : qui parle, et quand.
//!
//! Après la transcription, sherpa-onnx découpe l'audio en tours de parole (pyannote 3.0, fenêtre décalée
//! de 10 %) ; chaque tour, en morceaux de 4 s, reçoit une empreinte (3D-Speaker ERes2Net), et les
//! empreintes sont regroupées par `regroupement` (liaison moyenne, seuil fixe). Le regroupement de
//! sherpa-onnx lui-même basculait pour un arrondi de l'audio ou ±10 % de volume ; celui-ci donne le même
//! nombre de voix à une près. Les voix de moins de 30 s de parole en tout (rires, deux personnes à la
//! fois, un mot isolé) sont écartées. Le calcul, qu'on ne peut pas interrompre de l'intérieur, tourne
//! dans un processus à part (`tourdetable --voix`), arrêté si un enregistrement démarre (`ui/voix.rs`).
//! Environ un cinquième de la durée de la rencontre sur processeur, moins de 1,5 Go de mémoire.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result, bail};
use crossbeam_channel::Receiver;
use serde::{Deserialize, Serialize};

/// Modèle de découpage en tours de parole (pyannote segmentation 3.0, licence MIT).
pub const SEGMENTATION: &str = "pyannote-segmentation-3.0.onnx";
/// Modèle d'empreinte de voix (3D-Speaker ERes2Net base, licence Apache 2.0).
pub const EMPREINTE: &str = "3dspeaker-eres2net-base-16k.onnx";

const FENETRE: f32 = 0.1;
const PAROLE_MIN_MS: u64 = 30_000;

/// Voix d'une rencontre : tours de parole dans l'ordre, et noms donnés par l'utilisateur.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Voix {
    pub tours: Vec<Tour>,
    /// Nom de chaque voix, par numéro ; vide : « Voix 3 ».
    #[serde(default)]
    pub noms: Vec<String>,
    /// Voix calculées pendant l'enregistrement (`voix_direct`), en attente du calcul complet.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub provisoire: bool,
}

/// Tour de parole, sur l'horloge des segments (`Segment::start_ms`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tour {
    pub debut_ms: u64,
    pub fin_ms: u64,
    /// Numéro de la voix, à partir de 0, dans l'ordre où elles prennent la parole.
    pub voix: u16,
}

impl Voix {
    /// Tours bruts (début, fin, voix du moteur) : les voix de moins de [`PAROLE_MIN_MS`] sont
    /// écartées, les autres renumérotées dans l'ordre où elles prennent la parole.
    pub fn depuis(mut bruts: Vec<(u64, u64, i32)>) -> Self {
        bruts.sort_by_key(|t| t.0);
        let mut parole = std::collections::HashMap::<i32, u64>::new();
        for &(d, f, v) in &bruts {
            *parole.entry(v).or_default() += f.saturating_sub(d);
        }
        let mut numeros = std::collections::HashMap::<i32, u16>::new();
        let mut tours = Vec::new();
        for (debut_ms, fin_ms, v) in bruts {
            if parole[&v] < PAROLE_MIN_MS {
                continue;
            }
            let n = numeros.len() as u16;
            let voix = *numeros.entry(v).or_insert(n);
            tours.push(Tour { debut_ms, fin_ms, voix });
        }
        Self { tours, noms: Vec::new(), provisoire: false }
    }

    pub fn nombre(&self) -> usize {
        self.tours.iter().map(|t| t.voix as usize + 1).max().unwrap_or(0)
    }

    /// Voix qui parle le plus entre `debut_ms` et `fin_ms` ; à défaut (aucun tour ne recouvre
    /// l'intervalle), la plus proche à moins de 2 s.
    pub fn voix_entre(&self, debut_ms: u64, fin_ms: u64) -> Option<u16> {
        self.voix_brute_entre(debut_ms, fin_ms).map(|v| self.canonique(v))
    }

    /// Première voix qui porte le même nom : deux voix nommées pareil n'en font qu'une (une
    /// personne coupée en deux par le regroupement).
    pub fn canonique(&self, voix: u16) -> u16 {
        let nom = self.noms.get(voix as usize).map(|n| n.trim().to_lowercase()).filter(|n| !n.is_empty());
        match nom {
            Some(n) => self.noms.iter().position(|m| m.trim().to_lowercase() == n).map_or(voix, |i| i as u16),
            None => voix,
        }
    }

    fn voix_brute_entre(&self, debut_ms: u64, fin_ms: u64) -> Option<u16> {
        let fin_ms = fin_ms.max(debut_ms + 1);
        // Tours triés par début : seuls ceux qui commencent avant la fin peuvent recouvrir.
        let n = self.tours.partition_point(|t| t.debut_ms < fin_ms);
        let mut parole = [0u64; 64];
        let mut proche: Option<(u64, u16)> = None;
        for t in self.tours[..n].iter().rev().take(200) {
            let r = t.fin_ms.min(fin_ms).saturating_sub(t.debut_ms.max(debut_ms));
            if r > 0 {
                parole[(t.voix as usize).min(63)] += r;
            } else {
                let ecart = debut_ms.saturating_sub(t.fin_ms);
                if proche.is_none_or(|(e, _)| ecart < e) {
                    proche = Some((ecart, t.voix));
                }
            }
        }
        // Le tour suivant peut aussi être le plus proche.
        if let Some(t) = self.tours.get(n) {
            let ecart = t.debut_ms - fin_ms;
            if proche.is_none_or(|(e, _)| ecart < e) {
                proche = Some((ecart, t.voix));
            }
        }
        match parole.iter().enumerate().max_by_key(|(i, p)| (**p, std::cmp::Reverse(*i))) {
            Some((v, p)) if *p > 0 => Some(v as u16),
            _ => proche.filter(|(e, _)| *e <= 2000).map(|(_, v)| v),
        }
    }

    pub fn nom(&self, voix: u16) -> String {
        match self.noms.get(voix as usize).map(|n| n.trim()) {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => format!("Voix {}", voix + 1),
        }
    }

    /// Donne aux voix de `self` les noms des voix de `ancien` (voix provisoires du direct, ou calcul
    /// précédent) qui parlent aux mêmes moments : les numéros peuvent changer d'un calcul à l'autre. Un nom
    /// passe à une voix dont il recouvre au moins le tiers de la parole ; avec `unique`, à une seule (celle qu'il
    /// recouvre le plus) : deux personnes réunies en direct ne doivent pas l'être de nouveau par leur nom. Sans
    /// `unique` (d'un regroupement en direct au suivant), deux voix réunies par un même nom le restent.
    pub fn reporter_noms(&mut self, ancien: &Voix, unique: bool) {
        let n = self.nombre();
        self.noms = vec![String::new(); n];
        let parole = self.parole();
        // Recouvrement (ms) entre chaque nom ancien et chaque voix nouvelle.
        let mut rec = std::collections::HashMap::<(String, u16), u64>::new();
        for a in &ancien.tours {
            let nom = ancien.nom_donne(a.voix);
            if nom.is_empty() {
                continue;
            }
            let fin = self.tours.partition_point(|t| t.debut_ms < a.fin_ms);
            for t in self.tours[..fin].iter().rev().take_while(|t| t.debut_ms + 600_000 > a.debut_ms) {
                let r = t.fin_ms.min(a.fin_ms).saturating_sub(t.debut_ms.max(a.debut_ms));
                if r > 0 {
                    *rec.entry((nom.clone(), t.voix)).or_default() += r;
                }
            }
        }
        let mut paires: Vec<_> = rec.into_iter().collect();
        paires.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let mut donnes = std::collections::HashSet::new();
        for ((nom, v), r) in paires {
            let i = v as usize;
            if self.noms[i].is_empty() && !(unique && donnes.contains(&nom)) && r * 3 >= parole[i] {
                self.noms[i] = nom.clone();
                donnes.insert(nom);
            }
        }
        if self.noms.iter().all(String::is_empty) {
            self.noms.clear();
        }
    }

    /// Nom donné par l'utilisateur (vide sinon).
    fn nom_donne(&self, voix: u16) -> String {
        self.noms.get(voix as usize).map(|n| n.trim().to_string()).unwrap_or_default()
    }

    /// Noms donnés aux voix qui ont parlé, dans l'ordre où elles prennent la parole la première fois, sans doublon
    /// (deux voix d'une même personne) : les présences du compte rendu.
    pub fn noms_entendus(&self) -> Vec<String> {
        let mut noms: Vec<String> = Vec::new();
        for t in &self.tours {
            let nom = self.nom_donne(t.voix);
            if !nom.is_empty() && t.fin_ms > t.debut_ms && !noms.contains(&nom) {
                noms.push(nom);
            }
        }
        noms
    }

    /// Temps de parole de chaque voix, en millisecondes.
    pub fn parole(&self) -> Vec<u64> {
        let mut p = vec![0; self.nombre()];
        for t in &self.tours {
            p[t.voix as usize] += t.fin_ms - t.debut_ms;
        }
        p
    }
}

/// Les deux modèles sont installés.
pub fn modeles_installes() -> bool {
    crate::models::installed(SEGMENTATION) && crate::models::installed(EMPREINTE)
}

// ─────────── Processus `tourdetable --voix <dossier>` ───────────

/// Seuil du regroupement de sherpa-onnx, qui ne sert qu'à trouver les tours de parole : trop de voix
/// n'y nuit pas, elles sont regroupées ensuite (`regroupement`).
const SEUIL_TOURS: f32 = 0.9;
/// Longueur des morceaux dont on calcule l'empreinte : un tour plus long est découpé (il peut
/// cacher un changement de voix), un reste trop court est rattaché au morceau précédent.
const MORCEAU_MS: u64 = 4000;
const MORCEAU_MIN_MS: u64 = 1500;
/// Similarité moyenne sous laquelle deux groupes de morceaux sont deux voix. Banc du 19 novembre 2026,
/// trois rencontres (voix gardées / personnes) : 0,33 → 7/8 pour celle de deux heures ; 0,36 → 8/8,
/// puis 9 pour 8 environ et 8 pour 6 environ. Une personne coupée en deux voix se répare (même nom aux deux),
/// deux personnes réunies non : d'où 0,36. Volume ±10 % : même nombre de voix, à une près.
const SEUIL_VOIX: f32 = 0.36;

/// Tours du moteur et empreintes des morceaux, avant le regroupement.
struct Releve {
    /// (début, fin, voix du moteur).
    tours: Vec<(u64, u64, i32)>,
    /// (début, fin, indice du tour, empreinte normalisée).
    morceaux: Vec<(u64, u64, usize, Vec<f32>)>,
}

/// Calcule les voix de la rencontre du dossier `dir` et les écrit sur la sortie standard, en JSON.
pub fn commande(dir: &Path) -> Result<()> {
    let index = crate::audio::blocs::lire_index(dir).context("Audio de la rencontre")?;
    let audio = crate::audio::frontieres::extraire(dir, &index, 0, u64::MAX);
    let voix = voix_du_releve(&releve(&audio)?);
    println!("{}", serde_json::to_string(&voix)?);
    Ok(())
}

fn fils() -> i32 {
    // La moitié des cœurs, au plus 4 par réseau : le poste reste utilisable.
    std::thread::available_parallelism().map_or(2, |n| n.get() / 2).clamp(1, 4) as i32
}

/// Morceaux d'un tour : `MORCEAU_MS` au plus, le reste trop court rattaché au précédent.
pub(crate) fn morceaux_du_tour(debut: u64, fin: u64) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    let mut d = debut;
    while fin.saturating_sub(d) >= MORCEAU_MIN_MS {
        let f = if fin - d < MORCEAU_MS + MORCEAU_MIN_MS { fin } else { d + MORCEAU_MS };
        out.push((d, f));
        d = f;
    }
    out
}

fn releve(audio: &[f32]) -> Result<Releve> {
    if !modeles_installes() {
        bail!("Modèles de reconnaissance des voix absents");
    }
    let debut = std::time::Instant::now();
    let tours = tours_de_parole(audio)?;
    let duree_tours = debut.elapsed().as_secs_f32();

    let mut empreintes = Empreintes::lancer(fils())?;
    let echantillon = |ms: u64| ((ms * crate::audio::SAMPLE_RATE as u64 / 1000) as usize).min(audio.len());
    let mut morceaux = Vec::new();
    for (i, &(d, f, _)) in tours.iter().enumerate() {
        for (md, mf) in morceaux_du_tour(d, f) {
            let Some(mut e) = empreintes.calculer(&audio[echantillon(md)..echantillon(mf)])? else { continue };
            crate::regroupement::normaliser(&mut e);
            morceaux.push((md, mf, i, e));
        }
    }
    eprintln!(
        "Reconnaissance des voix : {:.0} s (tours) + {:.0} s (empreintes) pour {:.0} s d'audio, {} tours, {} morceaux",
        duree_tours,
        debut.elapsed().as_secs_f32() - duree_tours,
        audio.len() as f32 / crate::audio::SAMPLE_RATE as f32,
        tours.len(),
        morceaux.len()
    );
    Ok(Releve { tours, morceaux })
}

// ─────────── Programme auxiliaire `tourdetable-voix` ───────────
//
// sherpa-onnx n'est pas lié à l'application : ses bibliothèques précompilées (GCC 11 sous Linux) font planter
// whisper.cpp (`std::regex`, voir `voix/src/main.rs`). Il tourne dans un exécutable à part, livré à côté.

fn auxiliaire(mode: &str) -> Result<Command> {
    let programme = std::env::current_exe()?.with_file_name(format!("tourdetable-voix{}", std::env::consts::EXE_SUFFIX));
    let mut cmd = Command::new(programme);
    cmd.arg(mode).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW : programme console, sans fenêtre
    }
    Ok(cmd)
}

/// Tours de parole (début, fin, voix du moteur) par `tourdetable-voix tours`.
fn tours_de_parole(audio: &[f32]) -> Result<Vec<(u64, u64, i32)>> {
    use std::io::{Read, Write};
    let mut enfant = auxiliaire("tours")?
        .arg(crate::models::path(SEGMENTATION))
        .arg(crate::models::path(EMPREINTE))
        .arg(fils().to_string())
        .arg(FENETRE.to_string())
        .arg(SEUIL_TOURS.to_string())
        .spawn()
        .context("Lancement de la reconnaissance des voix (tourdetable-voix)")?;
    // L'entrée reste ouverte jusqu'à la fin du calcul : si ce processus est tué, elle se ferme et
    // l'auxiliaire s'arrête aussi.
    let mut entree = enfant.stdin.take().expect("entrée redirigée");
    let mut envoi = || -> std::io::Result<()> {
        entree.write_all(&(audio.len() as u64).to_le_bytes())?;
        for morceau in audio.chunks(1 << 16) {
            entree.write_all(&morceau.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>())?;
        }
        entree.flush()
    };
    envoi().context("Envoi de l'audio à la reconnaissance des voix")?;
    let mut json = String::new();
    enfant.stdout.take().expect("sortie redirigée").read_to_string(&mut json)?;
    let statut = enfant.wait()?;
    drop(entree);
    if !statut.success() {
        bail!("La reconnaissance des voix a échoué ({statut})");
    }
    serde_json::from_str(json.trim()).context("Tours de parole")
}

/// Empreintes de morceaux d'audio par `tourdetable-voix empreintes`, une à la fois. Le programme s'arrête
/// quand son entrée se ferme : à la fin, ou si ce processus est tué.
pub(crate) struct Empreintes {
    enfant: Child,
    entree: Option<std::process::ChildStdin>,
    sortie: BufReader<std::process::ChildStdout>,
}

impl Empreintes {
    pub(crate) fn lancer(fils: i32) -> Result<Self> {
        let mut enfant = auxiliaire("empreintes")?
            .arg(crate::models::path(EMPREINTE))
            .arg(fils.to_string())
            .spawn()
            .context("Lancement du calcul des empreintes de voix (tourdetable-voix)")?;
        let entree = enfant.stdin.take();
        let sortie = BufReader::new(enfant.stdout.take().expect("sortie redirigée"));
        Ok(Self { enfant, entree, sortie })
    }

    /// Empreinte brute (non normalisée) d'un morceau ; `None` s'il est trop court.
    pub(crate) fn calculer(&mut self, echantillons: &[f32]) -> Result<Option<Vec<f32>>> {
        use std::io::Write;
        let entree = self.entree.as_mut().expect("entrée ouverte");
        let mut octets = Vec::with_capacity(4 + 4 * echantillons.len());
        octets.extend((echantillons.len() as u32).to_le_bytes());
        echantillons.iter().for_each(|x| octets.extend(x.to_le_bytes()));
        entree.write_all(&octets).and_then(|_| entree.flush()).context("Envoi d'un morceau à tourdetable-voix")?;
        let mut ligne = String::new();
        if self.sortie.read_line(&mut ligne)? == 0 {
            bail!("tourdetable-voix s'est arrêté");
        }
        serde_json::from_str(&ligne).context("Empreinte de voix")
    }
}

impl Drop for Empreintes {
    fn drop(&mut self) {
        self.entree.take();
        let _ = self.enfant.wait();
    }
}

/// Regroupe les morceaux ; un tour sans morceau (trop court) prend la voix la plus fréquente des
/// morceaux de la même voix du moteur, s'il y en a.
fn voix_du_releve(r: &Releve) -> Voix {
    use crate::regroupement::{Morceau, regrouper};
    let m: Vec<Morceau> = r.morceaux.iter().map(|(d, f, _, e)| Morceau { empreinte: e.clone(), poids: (f - d) as f32 }).collect();
    let etiq = regrouper(&m, SEUIL_VOIX);
    let mut par_moteur = std::collections::HashMap::<i32, std::collections::HashMap<usize, u64>>::new();
    for ((d, f, t, _), &e) in r.morceaux.iter().zip(&etiq) {
        *par_moteur.entry(r.tours[*t].2).or_default().entry(e).or_default() += f - d;
    }
    let mut bruts: Vec<(u64, u64, i32)> = r.morceaux.iter().zip(&etiq).map(|((d, f, _, _), &e)| (*d, *f, e as i32)).collect();
    let avec_morceau: std::collections::HashSet<usize> = r.morceaux.iter().map(|m| m.2).collect();
    for (i, &(d, f, v)) in r.tours.iter().enumerate() {
        if avec_morceau.contains(&i) {
            continue;
        }
        if let Some((&e, _)) = par_moteur.get(&v).and_then(|c| c.iter().max_by_key(|(_, p)| **p)) {
            bruts.push((d, f, e as i32));
        }
    }
    let voix = Voix::depuis(bruts);
    eprintln!("Regroupement des voix : {} groupes, {} voix gardées", etiq.iter().max().map_or(0, |e| e + 1), voix.nombre());
    voix
}

/// Calcul en cours dans un processus à part, côté application.
pub struct Calcul {
    pub id: String,
    enfant: Option<Child>,
    pub fin: Receiver<Result<Voix, String>>,
}

impl Calcul {
    pub fn lancer(id: String, dir: &Path) -> Result<Self> {
        let mut enfant = Command::new(std::env::current_exe()?)
            .arg("--voix")
            .arg(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("Lancement de la reconnaissance des voix")?;
        let sortie = BufReader::new(enfant.stdout.take().expect("sortie redirigée"));
        let erreurs = BufReader::new(enfant.stderr.take().expect("erreurs redirigées"));
        let (tx, fin) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || {
            let journal: Vec<String> = erreurs.lines().map_while(Result::ok).collect();
            for l in journal.iter().filter(|l| l.contains("voix") || l.contains("Erreur")) {
                log::info!("{l}");
            }
        });
        std::thread::spawn(move || {
            let lignes: Vec<String> = sortie.lines().map_while(Result::ok).collect();
            let resultat = lignes
                .last()
                .ok_or_else(|| "aucun résultat".to_string())
                .and_then(|l| serde_json::from_str::<Voix>(l).map_err(|e| e.to_string()));
            let _ = tx.send(resultat);
        });
        Ok(Self { id, enfant: Some(enfant), fin })
    }

    /// Arrête le processus : le calcul sera repris plus tard, depuis le début.
    pub fn interrompre(&mut self) {
        if let Some(mut e) = self.enfant.take() {
            let _ = e.kill();
            let _ = e.wait();
        }
    }

    /// Attend la fin du processus (déjà terminé quand le résultat est arrivé).
    pub fn conclure(&mut self) {
        if let Some(mut e) = self.enfant.take() {
            let _ = e.wait();
        }
    }
}

impl Drop for Calcul {
    fn drop(&mut self) {
        self.interrompre();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ecarte_les_voix_breves_et_renumerote() {
        // Voix 7 : 40 s ; voix 2 : 5 s seulement ; voix 4 : 35 s.
        let v = Voix::depuis(vec![(50_000, 85_000, 4), (0, 20_000, 7), (20_000, 25_000, 2), (25_000, 45_000, 7)]);
        assert_eq!(v.nombre(), 2);
        assert_eq!(v.tours.iter().map(|t| t.voix).collect::<Vec<_>>(), vec![0, 0, 1]);
        assert_eq!(v.parole(), vec![40_000, 35_000]);
    }

    #[test]
    fn voix_qui_parle_le_plus() {
        let v = Voix {
            tours: vec![
                Tour { debut_ms: 0, fin_ms: 10_000, voix: 0 },
                Tour { debut_ms: 10_000, fin_ms: 12_000, voix: 1 },
                Tour { debut_ms: 20_000, fin_ms: 30_000, voix: 1 },
            ],
            noms: vec![String::new(), "Julie".into()],
            ..Default::default()
        };
        assert_eq!(v.voix_entre(8_000, 12_000), Some(0));
        assert_eq!(v.voix_entre(9_500, 13_000), Some(1));
        // Silence entre deux tours : la voix la plus proche, à 2 s au plus.
        assert_eq!(v.voix_entre(13_000, 14_000), Some(1));
        assert_eq!(v.voix_entre(15_000, 16_000), None);
        assert_eq!(v.voix_entre(18_500, 19_000), Some(1));
        assert_eq!(v.nom(0), "Voix 1");
        assert_eq!(v.nom(1), "Julie");
    }

    #[test]
    fn presences_des_voix_nommees() {
        let t = |d: u64, f: u64, voix: u16| Tour { debut_ms: d, fin_ms: f, voix };
        // Marc parle en premier ; Julie a deux voix ; la voix 3 n'a pas de nom.
        let v = Voix {
            tours: vec![t(0, 5_000, 1), t(5_000, 9_000, 0), t(9_000, 12_000, 3), t(12_000, 20_000, 2)],
            noms: vec!["Julie Roy".into(), " Marc Côté ".into(), "Julie Roy".into(), String::new()],
            provisoire: false,
        };
        assert_eq!(v.noms_entendus(), vec!["Marc Côté".to_string(), "Julie Roy".into()]);
        assert!(Voix { tours: vec![t(0, 5_000, 0)], ..Default::default() }.noms_entendus().is_empty());
    }

    #[test]
    fn noms_reportes_par_recouvrement() {
        let t = |d: u64, f: u64, voix: u16| Tour { debut_ms: d, fin_ms: f, voix };
        // Direct : Julie (0) et Marc (1) ; une voix 2 sans nom.
        let ancien = Voix {
            tours: vec![t(0, 40_000, 0), t(40_000, 80_000, 1), t(80_000, 120_000, 2)],
            noms: vec!["Julie".into(), "Marc".into()],
            provisoire: true,
        };
        // Calcul complet : numéros changés, Marc coupé en deux (2 et 3).
        let mut v = Voix { tours: vec![t(0, 40_000, 1), t(40_000, 65_000, 2), t(65_000, 80_000, 3), t(80_000, 120_000, 0)], ..Default::default() };
        v.reporter_noms(&ancien, true);
        assert_eq!(v.noms, vec!["".to_string(), "Julie".into(), "Marc".into(), "".into()]);
        // Aucun nom : rien à reporter.
        let mut w = v.clone();
        w.reporter_noms(&Voix { tours: ancien.tours.clone(), ..Default::default() }, true);
        // Sans `unique` : Marc passe aux deux moitiés.
        let mut x = Voix { tours: v.tours.clone(), ..Default::default() };
        x.reporter_noms(&ancien, false);
        assert_eq!(x.noms, vec!["".to_string(), "Julie".into(), "Marc".into(), "Marc".into()]);
        assert!(w.noms.is_empty());
    }

    #[test]
    fn meme_nom_meme_voix() {
        let mut v = Voix {
            tours: vec![Tour { debut_ms: 0, fin_ms: 1000, voix: 0 }, Tour { debut_ms: 1000, fin_ms: 2000, voix: 1 }, Tour { debut_ms: 2000, fin_ms: 3000, voix: 2 }],
            noms: vec!["Julie".into(), String::new(), " julie ".into()],
            ..Default::default()
        };
        assert_eq!(v.voix_entre(2000, 3000), Some(0));
        assert_eq!(v.voix_entre(1000, 2000), Some(1));
        v.noms[0].clear();
        assert_eq!(v.voix_entre(2000, 3000), Some(2));
    }
}
