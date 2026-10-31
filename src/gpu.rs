//! Accélération par la puce graphique (Vulkan), décidée par l'application.
//!
//! Une puce récente (Iris Xe, Radeon 780M…) transcrit et rédige plus vite que le processeur ;
//! une vieille (UHD 630) est plus lente, donne du texte erroné ou plante. Un étalonnage, dans un
//! processus à part (un plantage du pilote ne ferme pas l'application), compare donc la puce au
//! processeur sur un court extrait connu : elle n'est retenue que si elle est nettement plus
//! rapide et donne le même texte. Un plantage en cours d'usage la désactive pour de bon.
//!
//! Sans la fonction `vulkan`, tout reste sur le processeur ; avec `metal` (Mac), la puce est
//! toujours utilisée, sans étalonnage.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use anyhow::{Context, Result};
use crossbeam_channel::Receiver;
use serde::{Deserialize, Serialize};

/// Extrait synthétique (voix espeak-ng, 20 s, 16 kHz) et son texte : il ne vient d'aucune rencontre.
const EXTRAIT: &[u8] = include_bytes!("../assets/etalonnage.wav");

/// La puce doit faire gagner au moins 15 % du temps : en deçà, le processeur reste plus sûr.
const GAIN_MINIMUM: f64 = 0.85;

/// Variable lue par ggml-vulkan : vide, aucune puce n'est utilisée (rien n'y est même copié).
const VARIABLE_PUCES: &str = "GGML_VK_VISIBLE_DEVICES";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Etat {
    /// Version de l'application étalonnée : une nouvelle version (nouveau whisper.cpp ou
    /// llama.cpp) refait l'étalonnage.
    version: String,
    whisper: Option<bool>,
    redaction: Option<bool>,
    /// Durées mesurées, pour le journal et le dépannage.
    mesures: String,
}

fn fichier_etat() -> PathBuf {
    crate::config::data_dir().join("puce-graphique.json")
}

/// Présent pendant une transcription sur la puce : s'il reste après un plantage, la puce est écartée.
fn sentinelle() -> PathBuf {
    crate::config::data_dir().join("puce-graphique-en-cours")
}

fn lire() -> Etat {
    std::fs::read_to_string(fichier_etat()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn ecrire(e: &Etat) {
    let _ = std::fs::create_dir_all(crate::config::data_dir());
    if let Err(err) = std::fs::write(fichier_etat(), serde_json::to_string_pretty(e).unwrap_or_default()) {
        log::warn!("{} : {err}", fichier_etat().display());
    }
}

fn a_jour(e: &Etat) -> bool {
    e.version == env!("CARGO_PKG_VERSION")
}

/// Au démarrage, avant tout calcul : écarte la puce après un plantage, et la cache entièrement
/// au processus quand Whisper ne doit pas s'en servir.
pub fn init() {
    if !cfg!(feature = "vulkan") {
        return;
    }
    if sentinelle().exists() {
        let mut e = lire();
        e.whisper = Some(false);
        e.version = env!("CARGO_PKG_VERSION").into();
        ecrire(&e);
        let _ = std::fs::remove_file(sentinelle());
        log::warn!("Plantage pendant une transcription sur la puce graphique : Whisper reste sur le processeur");
    }
    if !whisper() && std::env::var_os(VARIABLE_PUCES).is_none() {
        // SAFETY : appelé au tout début de `main`, avant le lancement de tout autre fil.
        unsafe { std::env::set_var(VARIABLE_PUCES, "") };
    }
}

/// Whisper sur la puce graphique ?
pub fn whisper() -> bool {
    cfg!(feature = "metal") || (cfg!(feature = "vulkan") && { let e = lire(); a_jour(&e) && e.whisper == Some(true) })
}

/// Rédaction sur la puce graphique ?
pub fn redaction() -> bool {
    cfg!(feature = "metal") || (cfg!(feature = "vulkan") && { let e = lire(); a_jour(&e) && e.redaction == Some(true) })
}

/// La rédaction a planté sur la puce : on n'y revient plus.
pub fn redaction_plantee() {
    let mut e = lire();
    e.redaction = Some(false);
    ecrire(&e);
    log::warn!("Le programme de rédaction a planté sur la puce graphique : il reste sur le processeur");
}

/// Encadre une transcription sur la puce (voir [`sentinelle`]).
pub fn debut_transcription() {
    let _ = std::fs::write(sentinelle(), b"");
}
pub fn fin_transcription() {
    let _ = std::fs::remove_file(sentinelle());
}

/// Un étalonnage reste à faire (puce Vulkan possible, jamais mesurée pour cette version).
pub fn a_etalonner() -> bool {
    cfg!(feature = "vulkan") && { let e = lire(); !a_jour(&e) || e.whisper.is_none() || e.redaction.is_none() }
}

// ─────────── Étalonnage, côté application ───────────

/// Étalonnage en cours : un processus `tourdetable --etalonner` par moteur, l'un après l'autre.
pub struct Etalonnage {
    enfant: Option<Child>,
    fin: Receiver<()>,
    /// Arrêté par l'application : les mesures manquantes ne disent rien de la puce.
    interrompu: Arc<AtomicBool>,
}

impl Etalonnage {
    /// Lancé en arrière-plan quand l'application est inoccupée. Seuls les moteurs sans verdict
    /// sont mesurés : un plantage de Whisper sur la puce ne se répète pas à chaque démarrage.
    pub fn lancer() -> Result<Self> {
        let e = lire();
        let mut cmd = Command::new(std::env::current_exe()?);
        cmd.arg("--etalonner").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        if !a_jour(&e) || e.whisper.is_none() {
            cmd.arg("whisper");
        }
        if !a_jour(&e) || e.redaction.is_none() {
            cmd.arg("redaction");
        }
        // Le processus de l'interface cache la puce quand Whisper ne s'en sert pas : pas l'étalonnage.
        cmd.env_remove(VARIABLE_PUCES);
        let mut enfant = cmd.spawn().context("Lancement de l'étalonnage de la puce graphique")?;
        let sortie = BufReader::new(enfant.stdout.take().expect("sortie redirigée"));
        let (tx, fin) = crossbeam_channel::bounded(1);
        let interrompu = Arc::new(AtomicBool::new(false));
        let drapeau = interrompu.clone();
        std::thread::spawn(move || {
            let lignes: Vec<String> = sortie.lines().map_while(Result::ok).collect();
            if !drapeau.load(Ordering::Relaxed) {
                conclure(&lignes);
            }
            let _ = tx.send(());
        });
        Ok(Self { enfant: Some(enfant), fin, interrompu })
    }

    /// Terminé (le verdict est enregistré) ?
    pub fn termine(&self) -> bool {
        !self.fin.is_empty()
    }

    /// Une rencontre commence : l'étalonnage fausserait les mesures et la ralentirait. Il sera
    /// repris à un autre moment.
    pub fn interrompre(&mut self) {
        if let Some(mut e) = self.enfant.take() {
            self.interrompu.store(true, Ordering::Relaxed);
            let _ = e.kill();
            let _ = e.wait();
        }
    }
}

impl Drop for Etalonnage {
    fn drop(&mut self) {
        self.interrompre();
    }
}

/// Une mesure rapportée par le processus d'étalonnage, une par ligne.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct Mesure {
    moteur: String,
    puce: bool,
    secondes: f64,
    texte: String,
}

/// Verdict d'après les mesures reçues. Une mesure sur la puce absente (plantage, interruption
/// avant la fin) : le moteur reste sur le processeur, sauf interruption avant même la mesure
/// sur le processeur (rien de conclu, on réessaiera).
fn conclure(lignes: &[String]) {
    let mesures: Vec<Mesure> = lignes.iter().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let mut e = lire();
    if !a_jour(&e) {
        e = Etat { version: env!("CARGO_PKG_VERSION").into(), ..Etat::default() };
    }
    for moteur in ["whisper", "redaction"] {
        let trouve = |puce: bool| mesures.iter().find(|m| m.moteur == moteur && m.puce == puce);
        let Some(cpu) = trouve(false) else { continue };
        let verdict = trouve(true).is_some_and(|gpu| meilleure(moteur, cpu, gpu));
        let detail = match trouve(true) {
            Some(gpu) => format!("{moteur} : processeur {:.1} s, puce {:.1} s", cpu.secondes, gpu.secondes),
            None => format!("{moteur} : processeur {:.1} s, puce en échec", cpu.secondes),
        };
        log::info!("Étalonnage de la puce graphique — {detail} → {}", if verdict { "puce" } else { "processeur" });
        e.mesures = if e.mesures.is_empty() { detail } else { format!("{} ; {detail}", e.mesures) };
        match moteur {
            "whisper" => e.whisper = Some(verdict),
            _ => e.redaction = Some(verdict),
        }
    }
    ecrire(&e);
}

/// La puce est retenue si elle est nettement plus rapide et que son texte ressemble à celui du
/// processeur. Whisper doit donner presque le même texte ; la rédaction, dont le texte varie
/// d'un calcul à l'autre, doit au moins répondre en français sur le même sujet.
fn meilleure(moteur: &str, cpu: &Mesure, gpu: &Mesure) -> bool {
    let seuil = if moteur == "whisper" { 0.8 } else { 0.3 };
    gpu.secondes < cpu.secondes * GAIN_MINIMUM && ressemblance(&cpu.texte, &gpu.texte) >= seuil && (moteur == "whisper" || francais(&gpu.texte))
}

fn mots(t: &str) -> Vec<String> {
    t.split(|c: char| !c.is_alphanumeric()).filter(|m| m.chars().count() > 1).map(str::to_lowercase).collect()
}

/// Part des mots du texte de référence retrouvés dans l'autre (0 à 1).
fn ressemblance(reference: &str, autre: &str) -> f64 {
    let r = mots(reference);
    let a = mots(autre);
    if r.is_empty() {
        return 0.0;
    }
    r.iter().filter(|m| a.contains(m)).count() as f64 / r.len() as f64
}

/// Une réponse en anglais (vue sur une puce Intel ancienne) n'a presque aucun de ces mots.
fn francais(t: &str) -> bool {
    let m = mots(t);
    let courants = ["le", "la", "les", "de", "des", "du", "et", "est", "une", "un", "pour", "au", "sur"];
    m.iter().filter(|x| courants.contains(&x.as_str())).count() * 10 >= m.len().max(1)
}

// ─────────── Étalonnage, côté processus `--etalonner` ───────────

/// Texte que l'extrait doit donner.
const TEXTE_EXTRAIT: &str = "Bonjour à tous et bienvenue à la rencontre du conseil d'administration. Nous passons au \
point trois de l'ordre du jour, le rapport budgétaire. Les dépenses de septembre s'élèvent à douze mille dollars, \
ce qui respecte les prévisions. Il est proposé d'adopter le rapport tel que présenté. La prochaine rencontre aura \
lieu le mardi vingt octobre à seize heures.";

fn rapporter(m: &Mesure) {
    println!("{}", serde_json::to_string(m).unwrap_or_default());
}

/// Point d'entrée de `tourdetable --etalonner [whisper] [redaction]` : mesure sur le processeur
/// puis sur la puce, chacune rapportée dès qu'elle est faite (un plantage n'efface pas les
/// précédentes). Une erreur sur la puce n'est pas rapportée : le moteur reste sur le processeur.
pub fn etalonner(moteurs: &[String]) -> Result<()> {
    sans_fenetre_de_plantage();
    if moteurs.iter().any(|m| m == "whisper") {
        let audio = extrait()?;
        let modele = crate::config::model_path();
        for puce in [false, true] {
            let t = Instant::now();
            match crate::stt::transcrire_extrait(&modele, puce, &audio) {
                Ok(texte) => rapporter(&Mesure { moteur: "whisper".into(), puce, secondes: t.elapsed().as_secs_f64(), texte }),
                Err(e) if puce => log::warn!("Whisper sur la puce graphique : {e:#}"),
                Err(e) => return Err(e),
            }
        }
    }
    if moteurs.iter().any(|m| m == "redaction") {
        let invite = format!(
            "Voici la transcription d'une rencontre. Résume-la en trois phrases, en français.\n\n{}",
            TEXTE_EXTRAIT.repeat(8)
        );
        for puce in [false, true] {
            let mut moteur = crate::config::moteur();
            moteur.gpu = puce;
            let t = Instant::now();
            match crate::summary::Session::ouvrir(&moteur).and_then(|mut s| s.rediger(&invite, 60, None, None)) {
                Ok(texte) => rapporter(&Mesure { moteur: "redaction".into(), puce, secondes: t.elapsed().as_secs_f64(), texte }),
                Err(e) if puce => log::warn!("Rédaction sur la puce graphique : {e:#}"),
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}

/// Windows : un pilote qui plante pendant l'étalonnage ne doit pas afficher « a cessé de
/// fonctionner ». Hérité par le programme de rédaction lancé d'ici.
fn sans_fenetre_de_plantage() {
    #[cfg(windows)]
    {
        unsafe extern "system" {
            fn SetErrorMode(mode: u32) -> u32;
        }
        // SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX
        // SAFETY : fonction de kernel32 sans pointeur.
        unsafe { SetErrorMode(0x0001 | 0x0002) };
    }
}

/// Échantillons de l'extrait (WAV 16 bits mono 16 kHz).
fn extrait() -> Result<Vec<f32>> {
    let donnees = EXTRAIT.windows(4).position(|w| w == b"data").context("Extrait d'étalonnage invalide")? + 8;
    Ok(EXTRAIT[donnees..].as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b) as f32 / 32768.0).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(moteur: &str, puce: bool, secondes: f64, texte: &str) -> Mesure {
        Mesure { moteur: moteur.into(), puce, secondes, texte: texte.into() }
    }

    #[test]
    fn verdicts() {
        let cpu = m("whisper", false, 14.0, TEXTE_EXTRAIT);
        assert!(meilleure("whisper", &cpu, &m("whisper", true, 6.0, TEXTE_EXTRAIT)));
        // Plus lente, ou à peine plus rapide.
        assert!(!meilleure("whisper", &cpu, &m("whisper", true, 23.0, TEXTE_EXTRAIT)));
        assert!(!meilleure("whisper", &cpu, &m("whisper", true, 13.0, TEXTE_EXTRAIT)));
        // Charabia (vu sur une UHD 630 avec flash attention).
        assert!(!meilleure("whisper", &cpu, &m("whisper", true, 4.0, "aland")));

        let cpu = m("redaction", false, 60.0, "La rencontre porte sur le rapport budgétaire, qui est adopté. La prochaine rencontre aura lieu le 20 octobre.");
        let fr = m("redaction", true, 20.0, "Le conseil d'administration adopte le rapport budgétaire. La prochaine rencontre est prévue le 20 octobre.");
        let en = m("redaction", true, 20.0, "The meeting discussed the budget report, which was adopted. The next meeting is on October 20.");
        assert!(meilleure("redaction", &cpu, &fr));
        assert!(!meilleure("redaction", &cpu, &en));
        // Vu sur une UHD 630 : réponse en chinois.
        assert!(!meilleure("redaction", &cpu, &m("redaction", true, 20.0, "創新與發展部在2023年推動了多項關鍵計畫")));
    }

    #[test]
    fn extrait_lisible() {
        let a = extrait().unwrap();
        assert!((19.0..21.0).contains(&(a.len() as f32 / 16000.0)), "{} échantillons", a.len());
        assert!(a.iter().any(|x| x.abs() > 0.1));
    }
}
