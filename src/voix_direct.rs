//! Voix en direct : qui parle, pendant l'enregistrement.
//!
//! Un processus à part (`tourdetable --voix-direct`) reçoit les énoncés envoyés à Whisper. Chaque énoncé est
//! coupé en morceaux de 4 s (comme `voix`), dont l'empreinte est calculée sur un seul fil : environ 4 % du
//! temps réel d'un cœur (44 min de rencontre, 24 novembre 2026). Chaque minute d'audio, tous les morceaux
//! reçus sont regroupés (`regroupement`), et les voix sont renvoyées en JSON, provisoires : les numéros déjà
//! montrés sont gardés d'un regroupement à l'autre, et une voix n'est montrée qu'après 30 s de parole. Il n'y
//! a pas de découpage en tours de parole (pyannote, le plus lent) : un énoncé est déjà un tour, à 4 s près.
//! Accord avec le calcul complet : environ 87 %. Après l'enregistrement, le calcul complet (`voix`)
//! remplace ces voix ; les noms donnés en direct passent aux voix qui parlent aux mêmes moments
//! (`Voix::reporter_noms`). Si ce processus s'arrête, l'enregistrement et la transcription n'en savent rien.
//!
//! Protocole : sur l'entrée standard, chaque énoncé est son début en échantillons (u64), le nombre
//! d'échantillons (u32), puis les échantillons (f32), en petit-boutiste ; sur la sortie, une ligne JSON
//! (`Voix`) après chaque regroupement.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result, bail};
use crossbeam_channel::{Receiver, Sender};

use crate::audio::SAMPLE_RATE;
use crate::audio::dsp::Utterance;
use crate::voix::{EMPREINTE, Tour, Voix, modeles_installes, morceaux_du_tour};

/// Seuil du regroupement en direct : un peu plus bas que celui du calcul complet (0,36), une voix coupée en
/// deux en cours de rencontre dérange plus que deux voix réunies pour quelques minutes. Simulation du
/// 24 novembre 2026 (44 min, 8 voix au calcul complet) : 0,30 → 8 voix, 87 % de pureté, 86 % de couverture ;
/// 0,36 → 9 voix, 82 % de couverture.
const SEUIL: f32 = 0.30;
/// Intervalle entre deux regroupements, sur l'horloge de la rencontre.
const INTERVALLE_MS: u64 = 60_000;
/// Parole avant qu'une voix soit montrée : comme `voix`, qui écarte les voix de moins de 30 s.
const PAROLE_MIN_MS: u64 = 30_000;
/// Énoncés en attente d'envoi au processus : au-delà (processus bloqué), les suivants sont laissés de côté.
const FILE: usize = 256;

fn ms(echantillons: u64) -> u64 {
    echantillons * 1000 / SAMPLE_RATE as u64
}

// ─────────── Processus `tourdetable --voix-direct` ───────────

/// Lit les énoncés sur l'entrée standard jusqu'à sa fermeture, et écrit les voix provisoires.
pub fn commande() -> Result<()> {
    use sherpa_onnx::{SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig};
    if !modeles_installes() {
        bail!("Modèles de reconnaissance des voix absents");
    }
    let ec = SpeakerEmbeddingExtractorConfig {
        model: Some(crate::models::path(EMPREINTE).to_string_lossy().into_owned()),
        num_threads: 1,
        ..Default::default()
    };
    let extracteur = SpeakerEmbeddingExtractor::create(&ec).context("Chargement du modèle d'empreinte")?;
    let mut entree = BufReader::new(std::io::stdin().lock());
    let mut sortie = std::io::stdout().lock();
    let mut suivi = Suivi::default();
    let mut prochain_regroupement = INTERVALLE_MS;
    let mut entete = [0u8; 12];
    loop {
        match entree.read_exact(&mut entete) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e.into()),
        }
        let debut = u64::from_le_bytes(entete[..8].try_into().expect("8 octets"));
        let n = u32::from_le_bytes(entete[8..].try_into().expect("4 octets")) as usize;
        let mut octets = vec![0u8; n * 4];
        entree.read_exact(&mut octets)?;
        let echantillons: Vec<f32> = octets.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();
        let debut_ms = ms(debut);
        let fin_ms = ms(debut + n as u64);
        for (d, f) in morceaux_du_tour(debut_ms, fin_ms) {
            let a = ((d - debut_ms) * SAMPLE_RATE as u64 / 1000) as usize;
            let b = (((f - debut_ms) * SAMPLE_RATE as u64 / 1000) as usize).min(echantillons.len());
            let Some(flux) = extracteur.create_stream() else { continue };
            flux.accept_waveform(SAMPLE_RATE as i32, &echantillons[a.min(b)..b]);
            flux.input_finished();
            if !extracteur.is_ready(&flux) {
                continue;
            }
            let Some(mut e) = extracteur.compute(&flux) else { continue };
            crate::regroupement::normaliser(&mut e);
            suivi.ajouter(d, f, e);
        }
        if fin_ms >= prochain_regroupement {
            prochain_regroupement = fin_ms - fin_ms % INTERVALLE_MS + INTERVALLE_MS;
            if let Some(v) = suivi.regrouper() {
                writeln!(sortie, "{}", serde_json::to_string(&v)?)?;
                sortie.flush()?;
            }
        }
    }
}

/// Morceaux reçus et numéros provisoires montrés.
#[derive(Default)]
struct Suivi {
    /// (début, fin, empreinte normalisée), dans l'ordre d'arrivée.
    morceaux: Vec<(u64, u64, Vec<f32>)>,
    /// Numéro montré de chaque morceau au dernier regroupement (les morceaux arrivés depuis : absents).
    numeros: Vec<Option<u16>>,
}

impl Suivi {
    fn ajouter(&mut self, debut: u64, fin: u64, empreinte: Vec<f32>) {
        self.morceaux.push((debut, fin, empreinte));
    }

    /// Regroupe tous les morceaux ; `None` tant qu'aucune voix n'a assez parlé.
    fn regrouper(&mut self) -> Option<Voix> {
        use crate::regroupement::{Morceau, regrouper};
        if self.morceaux.is_empty() {
            return None;
        }
        let m: Vec<Morceau> = self.morceaux.iter().map(|(d, f, e)| Morceau { empreinte: e.clone(), poids: (f - d) as f32 }).collect();
        let groupes = regrouper(&m, SEUIL);
        let durees: Vec<u64> = self.morceaux.iter().map(|(d, f, _)| f - d).collect();
        self.numeros = numeroter(&groupes, &durees, &self.numeros);
        let mut tours: Vec<Tour> =
            self.morceaux.iter().zip(&self.numeros).filter_map(|((d, f, _), n)| n.map(|voix| Tour { debut_ms: *d, fin_ms: *f, voix })).collect();
        if tours.is_empty() {
            return None;
        }
        tours.sort_by_key(|t| t.debut_ms);
        Some(Voix { tours, noms: Vec::new(), provisoire: true })
    }
}

/// Numéro montré de chaque morceau : un groupe de moins de [`PAROLE_MIN_MS`] n'en a pas ; un groupe reprend
/// le numéro avec lequel il partage le plus de parole au regroupement précédent (chaque numéro une fois au
/// plus), sinon il reçoit le plus petit numéro libre.
fn numeroter(groupes: &[usize], durees: &[u64], anciens: &[Option<u16>]) -> Vec<Option<u16>> {
    let mut parole = HashMap::<usize, u64>::new();
    for (g, d) in groupes.iter().zip(durees) {
        *parole.entry(*g).or_default() += d;
    }
    let mut rec = HashMap::<(usize, u16), u64>::new();
    for ((g, d), a) in groupes.iter().zip(durees).zip(anciens) {
        if let Some(a) = a {
            *rec.entry((*g, *a)).or_default() += d;
        }
    }
    let mut paires: Vec<_> = rec.into_iter().collect();
    paires.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut vers = HashMap::<usize, u16>::new();
    let mut pris = HashSet::<u16>::new();
    for ((g, a), _) in paires {
        if parole[&g] >= PAROLE_MIN_MS && !vers.contains_key(&g) && pris.insert(a) {
            vers.insert(g, a);
        }
    }
    // Nouveaux groupes, dans l'ordre où ils prennent la parole : le plus petit numéro libre (celui d'un
    // groupe réuni à un autre, s'il y en a), pour ne pas montrer « Voix 10 » quand il y en a huit.
    groupes
        .iter()
        .map(|g| {
            if parole[g] < PAROLE_MIN_MS {
                return None;
            }
            Some(*vers.entry(*g).or_insert_with(|| {
                let n = (0..).find(|n| !pris.contains(n)).expect("numéro libre");
                pris.insert(n);
                n
            }))
        })
        .collect()
}

// ─────────── Côté application ───────────

/// Processus des voix en direct, pendant un enregistrement.
pub struct Direct {
    enfant: Option<Child>,
    envoi: Sender<(u64, Vec<f32>)>,
    /// Voix provisoires, après chaque regroupement.
    pub voix: Receiver<Voix>,
}

impl Direct {
    pub fn lancer() -> Result<Self> {
        let mut enfant = Command::new(std::env::current_exe()?)
            .arg("--voix-direct")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("Lancement des voix en direct")?;
        let mut entree = BufWriter::new(enfant.stdin.take().expect("entrée redirigée"));
        let sortie = BufReader::new(enfant.stdout.take().expect("sortie redirigée"));
        let erreurs = BufReader::new(enfant.stderr.take().expect("erreurs redirigées"));
        let (envoi, a_envoyer) = crossbeam_channel::bounded::<(u64, Vec<f32>)>(FILE);
        let (tx, voix) = crossbeam_channel::unbounded();
        std::thread::spawn(move || {
            for (debut, echantillons) in a_envoyer {
                let mut octets = Vec::with_capacity(12 + echantillons.len() * 4);
                octets.extend_from_slice(&debut.to_le_bytes());
                octets.extend_from_slice(&(echantillons.len() as u32).to_le_bytes());
                for e in echantillons {
                    octets.extend_from_slice(&e.to_le_bytes());
                }
                if entree.write_all(&octets).and_then(|_| entree.flush()).is_err() {
                    break;
                }
            }
        });
        std::thread::spawn(move || {
            for l in sortie.lines().map_while(Result::ok) {
                match serde_json::from_str::<Voix>(&l) {
                    Ok(v) => {
                        if tx.send(v).is_err() {
                            break;
                        }
                    }
                    Err(e) => log::warn!("Voix en direct : réponse illisible ({e})"),
                }
            }
        });
        std::thread::spawn(move || {
            for l in erreurs.lines().map_while(Result::ok).filter(|l| l.contains("Erreur")) {
                log::error!("Voix en direct : {l}");
            }
        });
        Ok(Self { enfant: Some(enfant), envoi, voix })
    }

    /// Transmet un énoncé ; laissé de côté si le processus ne suit plus.
    pub fn envoyer(&self, u: &Utterance) {
        let _ = self.envoi.try_send((u.start, u.samples.clone()));
    }
}

impl Drop for Direct {
    fn drop(&mut self) {
        if let Some(mut e) = self.enfant.take() {
            let _ = e.kill();
            let _ = e.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeros_gardes_d_un_regroupement_a_l_autre() {
        // Premier regroupement : groupe 5 (40 s) puis groupe 2 (35 s) ; le groupe 9 (10 s) n'est pas montré.
        let n1 = numeroter(&[5, 2, 9, 5], &[20_000, 35_000, 10_000, 20_000], &[]);
        assert_eq!(n1, vec![Some(0), Some(1), None, Some(0)]);
        // Deuxième : les étiquettes du regroupement changent, les numéros montrés non ; le groupe 9 a assez parlé.
        let n2 = numeroter(&[1, 0, 3, 1, 3], &[20_000, 35_000, 10_000, 20_000, 25_000], &n1);
        assert_eq!(n2, vec![Some(0), Some(1), Some(2), Some(0), Some(2)]);
        // Deux groupes réunis : un seul garde son numéro.
        let n3 = numeroter(&[0, 0, 1, 0, 1], &[20_000, 35_000, 10_000, 20_000, 25_000], &n2);
        assert_eq!(n3, vec![Some(0), Some(0), Some(2), Some(0), Some(2)]);
        // Une nouvelle voix reprend le numéro libéré (1).
        let n4 = numeroter(&[0, 0, 1, 0, 1, 2], &[20_000, 35_000, 10_000, 20_000, 25_000, 30_000], &n3);
        assert_eq!(n4, vec![Some(0), Some(0), Some(2), Some(0), Some(2), Some(1)]);
    }
}
