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
        Self { tours, noms: Vec::new() }
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
fn morceaux_du_tour(debut: u64, fin: u64) -> Vec<(u64, u64)> {
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
    use sherpa_onnx::{OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig, SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig};
    if !modeles_installes() {
        bail!("Modèles de reconnaissance des voix absents");
    }
    let empreinte = Some(crate::models::path(EMPREINTE).to_string_lossy().into_owned());
    let mut c = OfflineSpeakerDiarizationConfig::default();
    c.segmentation.pyannote.model = Some(crate::models::path(SEGMENTATION).to_string_lossy().into_owned());
    c.segmentation.pyannote.window_shift_ratio = FENETRE;
    c.segmentation.num_threads = fils();
    c.embedding.model = empreinte.clone();
    c.embedding.num_threads = fils();
    c.clustering.num_clusters = -1;
    c.clustering.threshold = SEUIL_TOURS;
    let moteur = OfflineSpeakerDiarization::create(&c).context("Chargement des modèles de reconnaissance des voix")?;
    let debut = std::time::Instant::now();
    let res = moteur.process(audio).context("Reconnaissance des voix")?;
    let tours: Vec<(u64, u64, i32)> =
        res.sort_by_start_time().iter().map(|s| ((s.start * 1000.0) as u64, (s.end * 1000.0) as u64, s.speaker)).collect();
    let duree_tours = debut.elapsed().as_secs_f32();

    let ec = SpeakerEmbeddingExtractorConfig { model: empreinte, num_threads: fils(), ..Default::default() };
    let extracteur = SpeakerEmbeddingExtractor::create(&ec).context("Chargement du modèle d'empreinte")?;
    let echantillon = |ms: u64| ((ms * crate::audio::SAMPLE_RATE as u64 / 1000) as usize).min(audio.len());
    let mut morceaux = Vec::new();
    for (i, &(d, f, _)) in tours.iter().enumerate() {
        for (md, mf) in morceaux_du_tour(d, f) {
            let flux = extracteur.create_stream().context("Flux d'empreinte")?;
            flux.accept_waveform(crate::audio::SAMPLE_RATE as i32, &audio[echantillon(md)..echantillon(mf)]);
            flux.input_finished();
            if !extracteur.is_ready(&flux) {
                continue;
            }
            let Some(mut e) = extracteur.compute(&flux) else { continue };
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
    fn meme_nom_meme_voix() {
        let mut v = Voix {
            tours: vec![Tour { debut_ms: 0, fin_ms: 1000, voix: 0 }, Tour { debut_ms: 1000, fin_ms: 2000, voix: 1 }, Tour { debut_ms: 2000, fin_ms: 3000, voix: 2 }],
            noms: vec!["Julie".into(), String::new(), " julie ".into()],
        };
        assert_eq!(v.voix_entre(2000, 3000), Some(0));
        assert_eq!(v.voix_entre(1000, 2000), Some(1));
        v.noms[0].clear();
        assert_eq!(v.voix_entre(2000, 3000), Some(2));
    }
}
