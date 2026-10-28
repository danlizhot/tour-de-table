//! Rangement par point de l'audio d'une rencontre déjà enregistrée : import (blocs en attente
//! dans `_import/`) ou rencontre découpée après coup (« Découper par points »).
//!
//! Les frontières validées deviennent les périodes de la rencontre ; les blocs passent de
//! `_import/` aux dossiers des points. Un bloc qui chevauche une frontière est recoupé au
//! silence le plus proche, et la période commence exactement à cette coupe. L'audio est
//! ensuite consolidé en un fichier par source (`blocs::consolider`).

use std::path::Path;

use anyhow::{Context, Result};

use super::SAMPLE_RATE;
use super::blocs::{self, Bloc, INDEX, Index};
use super::import::TRAVAIL;
use crate::decoupage::Frontiere;
use crate::storage::{Declenchement, Meeting, Periode};

const SR: u64 = SAMPLE_RATE as u64;
/// Une frontière à moins d'une seconde d'une coupe de bloc tombe sur cette coupe.
const ACCROCHE: u64 = SR;
/// Où chercher le silence : surtout avant la frontière, qui est le début d'un énoncé.
const AVANT: u64 = 3 * SR / 2;
const APRES: u64 = SR / 2;
/// Trames de 10 ms pour mesurer l'énergie.
const TRAME: usize = SAMPLE_RATE as usize / 100;

/// Les blocs d'un même instant, une source chacun.
struct Groupe {
    decalage: u64,
    echantillons: u64,
    blocs: Vec<Bloc>,
}

impl Groupe {
    fn fin(&self) -> u64 {
        self.decalage + self.echantillons
    }
}

fn groupes(index: &Index) -> Vec<Groupe> {
    let mut v: Vec<Bloc> = index.blocs.clone();
    v.sort_by_key(|b| (b.decalage, b.source as u8));
    let mut out: Vec<Groupe> = Vec::new();
    for b in v {
        match out.last_mut() {
            Some(g) if g.decalage == b.decalage => {
                g.echantillons = g.echantillons.max(b.echantillons);
                g.blocs.push(b);
            }
            _ => out.push(Groupe { decalage: b.decalage, echantillons: b.echantillons, blocs: vec![b] }),
        }
    }
    out
}

/// Audio d'un groupe, une piste par source, à la longueur du groupe. Un bloc illisible est
/// remplacé par du silence.
fn charger(dir: &Path, g: &Groupe) -> Vec<Vec<i16>> {
    g.blocs
        .iter()
        .map(|b| {
            let mut v = blocs::lire_bloc(dir, b).unwrap_or_else(|e| {
                log::warn!("{} illisible ({e:#}), remplacé par du silence", b.fichier);
                Vec::new()
            });
            v.resize(g.echantillons as usize, 0);
            v
        })
        .collect()
}

/// Coupe la plus proche de `x` dans un silence : la trame la plus calme autour de `x` (à 3 dB
/// près, la plus proche l'emporte), ou la coupe de bloc voisine si elle est à moins d'une seconde.
fn coupe_au_silence(dir: &Path, groupes: &[Groupe], x: u64) -> u64 {
    let Some(g) = groupes.iter().find(|g| g.decalage <= x && x < g.fin()) else { return x };
    if x - g.decalage < ACCROCHE {
        return g.decalage;
    }
    if g.fin() - x < ACCROCHE {
        return g.fin();
    }
    let pistes = charger(dir, g);
    let rel = (x - g.decalage) as usize;
    let de = rel.saturating_sub(AVANT as usize).max(ACCROCHE as usize);
    let a = (rel + APRES as usize).min((g.echantillons - ACCROCHE) as usize);
    let energies: Vec<(usize, f64)> = (de..a.saturating_sub(TRAME))
        .step_by(TRAME)
        .map(|i| {
            let e = (i..i + TRAME)
                .map(|j| {
                    let s: f64 = pistes.iter().map(|p| p[j] as f64).sum();
                    s * s
                })
                .sum::<f64>();
            (i, e)
        })
        .collect();
    let Some(min) = energies.iter().map(|&(_, e)| e).reduce(f64::min) else { return x };
    let choix = energies.iter().filter(|&&(_, e)| e <= 2.0 * min + 1.0).min_by_key(|&&(i, _)| i.abs_diff(rel)).map_or(rel, |&(i, _)| i);
    g.decalage + choix as u64
}

/// Suite de l'audio consacrée à un point (ou hors point).
#[derive(Debug)]
struct Troncon {
    debut: u64,
    point: Option<usize>,
    declenchement: Declenchement,
}

/// Range les blocs de `_import/` par point d'après les frontières validées : crée les périodes,
/// déplace les blocs (en recoupant au silence ceux qui chevauchent une frontière), renumérote,
/// réécrit `index-audio.json`, supprime `_import/` puis consolide l'audio. La rencontre reste à
/// sauvegarder.
pub fn appliquer(dir: &Path, rencontre: &mut Meeting, frontieres: &[Frontiere]) -> Result<()> {
    let travail = dir.join(TRAVAIL);
    let index = blocs::lire_index(&travail)?;
    let groupes = groupes(&index);
    let fin = groupes.last().map_or(0, Groupe::fin);

    let mut tri = frontieres.to_vec();
    tri.sort_by_key(|f| f.debut_ms);
    let mut troncons = vec![Troncon { debut: 0, point: None, declenchement: Declenchement::Manuel }];
    for f in tri {
        let x = (f.debut_ms * SR / 1000).min(fin);
        let debut = coupe_au_silence(&travail, &groupes, x);
        if debut >= fin {
            continue;
        }
        // Une frontière remplace celle qui tombe au même endroit ; un même point qui continue
        // ne forme qu'un tronçon.
        if troncons.last().is_some_and(|t| t.debut == debut) {
            troncons.pop();
        }
        if troncons.last().is_some_and(|t| t.point == f.point) {
            continue;
        }
        troncons.push(Troncon { debut, point: f.point, declenchement: f.declenchement });
    }
    let fin_troncon = |i: usize| troncons.get(i + 1).map_or(fin, |t| t.debut);

    // Un passage par tronçon : revenir sur un point ouvre son passage suivant.
    let titres: Vec<String> = rencontre.agenda.as_ref().map(|a| a.points.iter().map(|p| p.titre.clone()).collect()).unwrap_or_default();
    let passages: Vec<(u32, String)> = (0..troncons.len())
        .map(|i| {
            let n = troncons[..i].iter().filter(|t| t.point == troncons[i].point).count() as u32 + 1;
            (n, format!("{}/passage-{n:02}", blocs::prefixe(troncons[i].point, &titres)))
        })
        .collect();

    let mut nouvel = Index { frequence: index.frequence, blocs: Vec::new() };
    let mut numeros = vec![0u32; troncons.len()];
    for g in &groupes {
        let mut pistes = None;
        let mut a = g.decalage;
        while a < g.fin() {
            let t = troncons.partition_point(|t| t.debut <= a) - 1;
            let b = fin_troncon(t).min(g.fin());
            numeros[t] += 1;
            let (passage, rel) = &passages[t];
            std::fs::create_dir_all(dir.join(rel)).with_context(|| format!("Création de {rel}"))?;
            // Un bloc d'un fichier consolidé est toujours réécrit : le fichier est partagé.
            let entier = a == g.decalage && b == g.fin();
            for (s, bloc) in g.blocs.iter().enumerate() {
                let fichier = format!("{rel}/bloc-{:03}{}.wav", numeros[t], blocs::suffixe(bloc.source));
                let chemin = dir.join(&fichier);
                let sha256 = if entier && bloc.echantillons == g.echantillons && bloc.position.is_none() {
                    std::fs::rename(travail.join(&bloc.fichier), &chemin).with_context(|| format!("Déplacement de {}", bloc.fichier))?;
                    bloc.sha256.clone()
                } else {
                    let p: &Vec<Vec<i16>> = pistes.get_or_insert_with(|| charger(&travail, g));
                    let r = (a - g.decalage) as usize..(b - g.decalage) as usize;
                    blocs::ecrire_wav(&chemin, &p[s][r]).with_context(|| format!("Écriture de {fichier}"))?;
                    blocs::empreinte(&chemin)?
                };
                nouvel.blocs.push(Bloc {
                    point: troncons[t].point,
                    passage: *passage,
                    numero: numeros[t],
                    source: bloc.source,
                    decalage: a,
                    echantillons: b - a,
                    duree_ms: blocs::ms(b - a),
                    fichier,
                    sha256,
                    position: None,
                });
            }
            a = b;
        }
    }

    rencontre.periodes = troncons
        .iter()
        .enumerate()
        .filter_map(|(i, t)| {
            Some(Periode { point: Some(t.point?), debut_ms: blocs::ms(t.debut), fin_ms: Some(blocs::ms(fin_troncon(i))), declenchement: t.declenchement })
        })
        .collect();
    rencontre.duration_s = rencontre.duration_s.max(blocs::ms(fin) / 1000);
    blocs::ecrire_index(dir, &nouvel)?;
    std::fs::remove_dir_all(&travail).context("Suppression du dossier de travail de l'import")?;
    log::info!("Découpage par point : {} tronçons, {} blocs", troncons.len(), nouvel.blocs.len());
    if let Err(e) = blocs::consolider(dir, rencontre.started_at.date_naive()) {
        log::warn!("{e:#}");
    }
    Ok(())
}

/// Remet les blocs d'une rencontre dans `_import/` pour un nouveau découpage. Un découpage déjà en
/// attente (interrompu) est repris tel quel.
fn preparer(dir: &Path) -> Result<()> {
    let travail = dir.join(TRAVAIL);
    if blocs::existe(&travail) {
        return Ok(());
    }
    let index = blocs::lire_index(dir)?;
    std::fs::create_dir_all(&travail)?;
    blocs::ecrire_index(&travail, &index)?;
    // Les blocs d'un fichier consolidé partagent ce fichier : il n'est déplacé qu'une fois.
    let mut deplaces = std::collections::HashSet::new();
    for b in index.blocs.iter().filter(|b| deplaces.insert(b.fichier.as_str())) {
        let vers = travail.join(&b.fichier);
        if let Some(p) = vers.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::rename(dir.join(&b.fichier), &vers).with_context(|| format!("Déplacement de {}", b.fichier))?;
    }
    std::fs::remove_file(dir.join(INDEX))?;
    // Dossiers devenus vides : les passages, puis les points.
    for b in index.blocs.iter().filter(|b| b.position.is_none()) {
        if let Some(passage) = dir.join(&b.fichier).parent() {
            let _ = std::fs::remove_dir(passage);
            if let Some(point) = passage.parent() {
                let _ = std::fs::remove_dir(point);
            }
        }
    }
    Ok(())
}

/// « Découper par points » sur une rencontre déjà rangée : mêmes étapes qu'à la fin d'un import.
pub fn redecouper(dir: &Path, rencontre: &mut Meeting, frontieres: &[Frontiere]) -> Result<()> {
    preparer(dir)?;
    appliquer(dir, rencontre, frontieres)
}

/// Frontières d'une rencontre déjà découpée, pour les revoir : un début par période, et un retour
/// hors point entre deux périodes qui ne se touchent pas.
pub fn frontieres_de(rencontre: &Meeting) -> Vec<Frontiere> {
    let extrait = |ms: u64| rencontre.segments.iter().find(|s| s.end_ms > ms).map(|s| s.text.clone()).unwrap_or_default();
    let hors_point = |ms: u64| Frontiere { point: None, debut_ms: ms, extrait: extrait(ms), raison: String::new(), declenchement: Declenchement::Manuel };
    let mut v = Vec::new();
    let mut fin_precedente = None;
    for p in &rencontre.periodes {
        if let Some(f) = fin_precedente
            && f < p.debut_ms
        {
            v.push(hors_point(f));
        }
        v.push(Frontiere { point: p.point, debut_ms: p.debut_ms, extrait: extrait(p.debut_ms), raison: String::new(), declenchement: p.declenchement });
        fin_precedente = p.fin_ms;
    }
    if let Some(f) = fin_precedente
        && f < rencontre.duration_s * 1000
    {
        v.push(hors_point(f));
    }
    v
}

/// Audio mono (sources additionnées) de `debut` à `fin`, en échantillons, pour l'écoute.
pub fn extraire(dir: &Path, index: &Index, debut: u64, fin: u64) -> Vec<f32> {
    let groupes = groupes(index);
    let fin = fin.min(groupes.last().map_or(0, Groupe::fin));
    let mut out = vec![0.0f32; fin.saturating_sub(debut) as usize];
    for g in groupes.iter().filter(|g| g.decalage < fin && g.fin() > debut) {
        let pistes = charger(dir, g);
        for i in g.decalage.max(debut)..g.fin().min(fin) {
            let s: f32 = pistes.iter().map(|p| blocs::dequantifier(p[(i - g.decalage) as usize])).sum();
            out[(i - debut) as usize] = s.clamp(-1.0, 1.0);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::import::{self, Avancement};
    use crate::storage::Agenda;
    use crate::stt::Segment;
    use std::path::PathBuf;

    fn temporaire(nom: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tourdetable-frontieres-{}-{nom}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Enregistrement simulé de 10 minutes, 16 kHz mono : parole (sinus) de 10k+2 à 10k+8 s,
    /// bruit très faible ailleurs. Les blocs tombent donc pile aux 120 s, dans un silence.
    fn enregistrement(chemin: &Path) {
        let spec = hound::WavSpec { channels: 1, sample_rate: SAMPLE_RATE, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(chemin, spec).unwrap();
        let mut x = 7u32;
        for i in 0..600 * SR {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let bruit = ((x >> 16) as i32 % 32 - 16) as i16;
            let voix = (2..8).contains(&(i / SR % 10));
            let s = if voix { ((i as f32 * 300.0 * std::f32::consts::TAU / SR as f32).sin() * 9_000.0) as i16 } else { bruit };
            w.write_sample(s).unwrap();
        }
        w.finalize().unwrap();
    }

    const ANNONCES: [(u64, &str); 3] = [
        (4, "Bon, on passe au point 1, ouverture de la séance."),
        (25, "Parfait. On passe au point 2, l'adoption du budget."),
        (43, "Merci. On passe au point 3, varia."),
    ];

    fn agenda() -> Agenda {
        Agenda::new("odj.txt".into(), "1. Ouverture de la séance\n2. Adoption du budget 2027\n3. Varia".into())
    }

    /// Import jusqu'à la vérification : copie, blocs dans `_import/`, transcription simulée
    /// (texte inventé pour chaque énoncé). Retourne la rencontre et l'audio décodé.
    fn importer(dir: &Path) -> (Meeting, Vec<f32>) {
        let ailleurs = dir.with_extension("original");
        std::fs::create_dir_all(&ailleurs).unwrap();
        let original = ailleurs.join("reunion.wav");
        enregistrement(&original);
        let avant = std::fs::read(&original).unwrap();
        let source = import::copier_source(&original, dir).unwrap();
        assert_eq!(source, dir.join("source").join(original.file_name().unwrap()));
        let travail = dir.join(TRAVAIL);
        let avancement = Avancement::default();
        let index = import::ecrire_blocs(&source, &travail, &avancement).unwrap();
        let decoupe: Vec<(u64, u64)> = index.blocs.iter().map(|b| (b.decalage / SR, b.echantillons / SR)).collect();
        assert_eq!(decoupe, [(0, 120), (120, 120), (240, 120), (360, 120), (480, 120)]);
        assert!(import::en_attente(dir));

        let mut m = Meeting::nouvelle("modele", Some("Réunion".into()), chrono::Local::now());
        let mut appels = Vec::new();
        import::transcrire_blocs(&travail, &index, |k, enonces| {
            appels.push((k, enonces.len()));
            for u in enonces {
                let start_ms = u.start * 1000 / SR;
                let dizaine = (u.start / SR + 1) / 10;
                let text = ANNONCES.iter().find(|(d, _)| *d == dizaine).map_or("Merci, c'est noté.", |(_, t)| t);
                m.insert(Segment { start_ms, end_ms: start_ms + u.samples.len() as u64 * 1000 / SR, source: u.source, text: text.into() });
            }
            Ok(())
        })
        .unwrap();
        // Progression par bloc : 12 énoncés par bloc de 2 minutes.
        assert_eq!(appels, [(0, 12), (1, 12), (2, 12), (3, 12), (4, 12)]);
        m.duration_s = 600;

        let mut audio = Vec::new();
        import::decode(&source, |x| {
            audio.extend_from_slice(x);
            Ok(())
        })
        .unwrap();
        assert_eq!(std::fs::read(&original).unwrap(), avant, "fichier original modifié");
        let _ = std::fs::remove_dir_all(ailleurs);
        (m, audio)
    }

    /// Fichiers du dossier de la rencontre, relatifs, triés.
    fn arborescence(dir: &Path) -> Vec<String> {
        fn parcourir(racine: &Path, d: &Path, v: &mut Vec<String>) {
            for e in std::fs::read_dir(d).unwrap().flatten() {
                if e.path().is_dir() {
                    parcourir(racine, &e.path(), v);
                } else {
                    v.push(e.path().strip_prefix(racine).unwrap().to_string_lossy().replace('\\', "/"));
                }
            }
        }
        let mut v = Vec::new();
        parcourir(dir, dir, &mut v);
        v.sort();
        v
    }

    fn resume(index: &Index) -> Vec<(u64, u64)> {
        index.blocs.iter().map(|b| (b.decalage, b.echantillons)).collect()
    }

    /// Blocs rangés : point, passage et numéro de chacun.
    fn rangement(index: &Index) -> Vec<(Option<usize>, u32, u32)> {
        index.blocs.iter().map(|b| (b.point, b.passage, b.numero)).collect()
    }

    /// Après rangement, l'audio est consolidé : un seul fichier à côté de l'index.
    fn consolide(m: &Meeting) -> String {
        format!("{}.wav", blocs::nom_consolide(m.started_at.date_naive()))
    }

    fn verifier_rangement(dir: &Path, m: &Meeting, audio: &[f32]) -> Index {
        let index = blocs::lire_index(dir).unwrap();
        assert!(!dir.join(TRAVAIL).exists(), "_import/ non supprimé");
        assert!(!import::en_attente(dir));
        // Blocs contigus, et chaque période commence exactement à une coupe.
        let mut attendu = 0;
        for b in &index.blocs {
            assert_eq!(b.decalage, attendu, "{}", b.fichier);
            assert_eq!((b.fichier.as_str(), b.position), (consolide(m).as_str(), Some(b.decalage)));
            blocs::lire_bloc(dir, b).unwrap();
            attendu += b.echantillons;
        }
        assert_eq!(arborescence(dir), [consolide(m), "index-audio.json".into(), "source/reunion.wav".into()]);
        assert_eq!(attendu, 600 * SR);
        for p in &m.periodes {
            assert!(index.blocs.iter().any(|b| blocs::ms(b.decalage) == p.debut_ms && b.point == p.point && b.numero == 1), "{p:?}");
        }
        let sortie = dir.join("tout.wav");
        assert!(blocs::reconstituer_dans(dir, m, blocs::Portee::Tout, crate::audio::Source::Mixte, &sortie).unwrap().is_empty());
        let relu: Vec<i16> = hound::WavReader::open(&sortie).unwrap().into_samples::<i16>().map(Result::unwrap).collect();
        assert_eq!(relu, audio.iter().map(|&x| blocs::quantifier(x)).collect::<Vec<_>>());
        std::fs::remove_file(sortie).unwrap();
        index
    }

    #[test]
    fn import_de_dix_minutes_avec_un_ordre_du_jour_de_trois_points() {
        let dir = temporaire("trois-points");
        let (mut m, audio) = importer(&dir);
        m.agenda = Some(agenda());
        let points = &m.agenda.as_ref().unwrap().points;
        assert_eq!(points.len(), 3);

        // Détection hors ligne : chaque annonce, au début de son énoncé (300 ms de préambule).
        let frontieres = crate::decoupage::detecter_hors_ligne(&m.segments, points);
        let trouve: Vec<(Option<usize>, u64)> = frontieres.iter().map(|f| (f.point, f.debut_ms)).collect();
        assert_eq!(trouve, [(Some(0), 41_700), (Some(1), 251_700), (Some(2), 431_700)]);
        assert_eq!(frontieres[1].extrait, ANNONCES[1].1);

        appliquer(&dir, &mut m, &frontieres).unwrap();
        let index = verifier_rangement(&dir, &m, &audio);
        assert_eq!(rangement(&index), [(None, 1, 1), (Some(0), 1, 1), (Some(0), 1, 2), (Some(0), 1, 3), (Some(1), 1, 1), (Some(1), 1, 2), (Some(2), 1, 1), (Some(2), 1, 2)]);
        // Coupes au silence juste avant chaque annonce (parole à 42, 252 et 432 s).
        let periodes: Vec<(Option<usize>, Option<u64>)> = m.periodes.iter().map(|p| (p.point, p.fin_ms)).collect();
        let debuts: Vec<u64> = m.periodes.iter().map(|p| p.debut_ms).collect();
        assert_eq!(periodes, [(Some(0), Some(debuts[1])), (Some(1), Some(debuts[2])), (Some(2), Some(600_000))]);
        for (d, f) in debuts.iter().zip(&frontieres) {
            assert!((f.debut_ms - 1500..=f.debut_ms + 300).contains(d), "coupe à {d} pour la frontière {}", f.debut_ms);
            assert!(audio[(d * SR / 1000) as usize].abs() < 0.01, "coupe en pleine parole à {d}");
        }
        // Les blocs entiers gardent leurs limites, les autres sont recoupés.
        assert_eq!((index.blocs[2].decalage, index.blocs[2].echantillons), (120 * SR, 120 * SR));
        assert!(m.segments.iter().filter(|s| s.text == ANNONCES[2].1).all(|s| m.point_de(s.start_ms) == Some(2)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_sans_ordre_du_jour_puis_decoupage_par_points() {
        let dir = temporaire("sans-ordre");
        let (mut m, audio) = importer(&dir);
        let avant = blocs::lire_index(&dir.join(TRAVAIL)).unwrap();
        appliquer(&dir, &mut m, &[]).unwrap();
        assert!(m.periodes.is_empty());
        let index = verifier_rangement(&dir, &m, &audio);
        // Simplement rangés hors point : mêmes blocs.
        assert_eq!(rangement(&index), [(None, 1, 1), (None, 1, 2), (None, 1, 3), (None, 1, 4), (None, 1, 5)]);
        assert_eq!(resume(&index), resume(&avant));

        // Plus tard : ordre du jour ajouté, « Découper par points » sur l'audio consolidé. Même
        // résultat qu'un import découpé directement.
        m.agenda = Some(agenda());
        let frontieres = crate::decoupage::detecter_hors_ligne(&m.segments, &m.agenda.as_ref().unwrap().points);
        redecouper(&dir, &mut m, &frontieres).unwrap();
        let index = verifier_rangement(&dir, &m, &audio);
        let points: Vec<Option<usize>> = index.blocs.iter().map(|b| b.point).collect();
        assert_eq!(points, [None, Some(0), Some(0), Some(0), Some(1), Some(1), Some(2), Some(2)]);

        // Revoir le découpage : les frontières reviennent des périodes, une frontière retirée
        // fusionne deux points, puis on revient hors point en fin de rencontre.
        let mut revues = frontieres_de(&m);
        assert_eq!(revues.iter().map(|f| f.point).collect::<Vec<_>>(), [Some(0), Some(1), Some(2)]);
        revues.remove(1);
        revues.push(Frontiere { point: None, debut_ms: 590_000, extrait: String::new(), raison: String::new(), declenchement: Declenchement::Manuel });
        redecouper(&dir, &mut m, &revues).unwrap();
        let index = verifier_rangement(&dir, &m, &audio);
        assert_eq!(m.periodes.iter().map(|p| p.point).collect::<Vec<_>>(), [Some(0), Some(2)]);
        assert_eq!(index.blocs.last().map(|b| (b.point, b.passage)), Some((None, 2)));
        assert_eq!(frontieres_de(&m).last().map(|f| f.point), Some(None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extrait_a_ecouter() {
        let dir = temporaire("extrait");
        let (m, audio) = importer(&dir);
        drop(m);
        let travail = dir.join(TRAVAIL);
        let index = blocs::lire_index(&travail).unwrap();
        // 10 s autour de la coupe de 120 s : sur deux blocs.
        let x = extraire(&travail, &index, 115 * SR, 125 * SR);
        assert_eq!(x.len(), (10 * SR) as usize);
        let attendu: Vec<i16> = audio[(115 * SR) as usize..(125 * SR) as usize].iter().map(|&v| blocs::quantifier(v)).collect();
        assert_eq!(x.iter().map(|&v| blocs::quantifier(v)).collect::<Vec<_>>(), attendu);
        assert_eq!(extraire(&travail, &index, 595 * SR, 605 * SR).len(), (5 * SR) as usize);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
