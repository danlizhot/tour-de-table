//! Capture audio : micro de la salle + son système (participants à distance).
//!
//! * Windows : WASAPI, le son système est capté en mode « loopback » sur la sortie.
//! * macOS 14.6+ : CoreAudio, loopback via une « process tap » (cpal).
//! * Linux : micro via ALSA/PulseAudio, son système via la source « monitor »
//!   de PulseAudio/PipeWire (`parec`).

pub mod dsp;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::Sender;
use serde::{Deserialize, Serialize};

/// Fréquence attendue par Whisper.
pub const SAMPLE_RATE: u32 = 16_000;

/// D'où provient un segment de parole.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Source {
    /// Micro de la salle.
    Salle,
    /// Son système : Teams, Zoom, Meet, Webex…
    Distance,
    /// Les deux sources additionnées (mode mixé).
    Mixte,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Salle => "Salle",
            Source::Distance => "À distance",
            Source::Mixte => "Réunion",
        }
    }
}

#[derive(Clone, Debug)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
}

/// Un bloc d'échantillons mono à la fréquence native du périphérique.
pub struct Chunk {
    pub source: Source,
    pub samples: Vec<f32>,
}

/// Niveau crête (0..1) mis à jour par le fil audio, lu par l'interface.
#[derive(Default)]
pub struct Level(AtomicU32);

impl Level {
    pub fn set(&self, v: f32) {
        self.0.store(v.to_bits(), Ordering::Relaxed);
    }
    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
}

fn describe(dev: &cpal::Device) -> Option<DeviceInfo> {
    Some(DeviceInfo { id: dev.id().ok()?.to_string(), name: dev.to_string() })
}

pub fn list_microphones() -> Vec<DeviceInfo> {
    let host = cpal::default_host();
    host.input_devices()
        .map(|it| it.filter_map(|d| describe(&d)).collect())
        .unwrap_or_default()
}

/// Sources possibles pour le son des participants à distance.
pub fn list_system_sources() -> Vec<DeviceInfo> {
    #[cfg(target_os = "linux")]
    {
        linux::list_monitors()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let host = cpal::default_host();
        host.output_devices()
            .map(|it| it.filter_map(|d| describe(&d)).collect())
            .unwrap_or_default()
    }
}

fn find_device(devices: impl Iterator<Item = cpal::Device>, id: &str) -> Option<cpal::Device> {
    devices.into_iter().find(|d| d.id().is_ok_and(|i| i.to_string() == id))
}

/// Garde les flux ouverts ; les fermer = laisser tomber cette valeur.
/// `cpal::Stream` n'est pas `Send` partout : on le crée et on le détruit dans le même fil.
#[allow(dead_code)] // gardes : seule leur destruction compte
pub enum Capture {
    Cpal(cpal::Stream),
    #[cfg(target_os = "linux")]
    Process(linux::Parec),
}

/// Démarre une capture et retourne (garde, fréquence native).
pub fn start(
    source: Source,
    device_id: Option<&str>,
    tx: Sender<Chunk>,
    level: Arc<Level>,
) -> Result<(Capture, u32)> {
    #[cfg(target_os = "linux")]
    if source == Source::Distance {
        let parec = linux::Parec::spawn(device_id, tx, level)?;
        return Ok((Capture::Process(parec), SAMPLE_RATE));
    }

    let host = cpal::default_host();
    let device = match (source, device_id) {
        (Source::Salle, Some(id)) => host.input_devices().ok().and_then(|d| find_device(d, id)),
        (Source::Salle, None) => host.default_input_device(),
        (_, Some(id)) => host.output_devices().ok().and_then(|d| find_device(d, id)),
        (_, None) => host.default_output_device(),
    }
    .ok_or_else(|| anyhow!("Périphérique audio introuvable ({})", source.label()))?;

    let supported = if source == Source::Salle {
        device.default_input_config()
    } else {
        // Loopback : on enregistre la sortie avec son propre format.
        device.default_output_config()
    }
    .with_context(|| format!("Format audio de « {device} »"))?;

    let rate = supported.sample_rate();
    let channels = supported.channels() as usize;
    let config: cpal::StreamConfig = supported.config();
    let err_fn = move |e| log::error!("Flux audio {}: {e}", source.label());

    macro_rules! build {
        ($t:ty) => {{
            let tx = tx.clone();
            let level = level.clone();
            device.build_input_stream::<$t, _, _>(
                config.clone(),
                move |data: &[$t], _| {
                    let mono = dsp::downmix(data, channels);
                    level.set(dsp::peak(&mono));
                    let _ = tx.try_send(Chunk { source, samples: mono });
                },
                err_fn,
                None,
            )
        }};
    }

    use cpal::SampleFormat as F;
    let stream = match supported.sample_format() {
        F::F32 => build!(f32),
        F::I16 => build!(i16),
        F::I32 => build!(i32),
        F::U16 => build!(u16),
        F::U8 => build!(u8),
        F::I8 => build!(i8),
        F::F64 => build!(f64),
        other => return Err(anyhow!("Format d'échantillon non pris en charge : {other:?}")),
    }
    .with_context(|| format!("Ouverture de « {device} »"))?;
    stream.play().context("Démarrage du flux audio")?;
    log::info!("Capture {} : « {device} » {rate} Hz, {channels} canaux", source.label());
    Ok((Capture::Cpal(stream), rate))
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::io::Read;
    use std::process::{Child, Command, Stdio};

    /// Liste les sources « .monitor » (son qui sort des haut-parleurs).
    pub fn list_monitors() -> Vec<DeviceInfo> {
        let Ok(out) = Command::new("pactl").args(["list", "short", "sources"]).output() else {
            return Vec::new();
        };
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.split('\t').nth(1))
            .filter(|name| name.ends_with(".monitor"))
            .map(|name| DeviceInfo { id: name.to_string(), name: name.to_string() })
            .collect()
    }

    pub struct Parec {
        child: Child,
    }

    impl Parec {
        pub fn spawn(device: Option<&str>, tx: Sender<Chunk>, level: Arc<Level>) -> Result<Self> {
            let mut child = Command::new("parec")
                .arg(format!("--device={}", device.unwrap_or("@DEFAULT_MONITOR@")))
                .args(["--format=float32le", "--channels=1", "--raw", "--latency-msec=40"])
                .arg(format!("--rate={SAMPLE_RATE}"))
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .context("Lancement de « parec » (paquet pulseaudio-utils requis)")?;
            let mut stdout = child.stdout.take().expect("stdout redirigé");
            std::thread::Builder::new().name("parec".into()).spawn(move || {
                let mut buf = vec![0u8; 640 * 4];
                while let Ok(n) = stdout.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let samples: Vec<f32> = buf[..n - n % 4]
                        .chunks_exact(4)
                        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                        .collect();
                    level.set(dsp::peak(&samples));
                    if tx.send(Chunk { source: Source::Distance, samples }).is_err() {
                        break;
                    }
                }
            })?;
            Ok(Self { child })
        }
    }

    impl Drop for Parec {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
