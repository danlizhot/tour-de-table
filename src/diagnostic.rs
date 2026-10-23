//! Diagnostic : journal dans un fichier et message d'erreur visible.
//!
//! Sous Windows, une application graphique n'a pas de console : sans cela, une
//! erreur au démarrage ferme l'application sans aucune trace.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Pendant l'essai d'un moteur d'affichage, un plantage est journalisé mais
/// n'affiche pas de message : main.rs passe alors au moteur suivant.
static QUIET_PANICS: AtomicBool = AtomicBool::new(false);

pub fn set_quiet_panics(quiet: bool) {
    QUIET_PANICS.store(quiet, Ordering::Relaxed);
}

pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "erreur inconnue".into())
}

/// `journal.txt` dans le dossier de données ; l'exécution précédente est gardée
/// dans `journal-precedent.txt`.
pub fn log_path() -> PathBuf {
    crate::config::data_dir().join("journal.txt")
}

struct Tee(Mutex<Option<std::fs::File>>);

impl Write for &Tee {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = std::io::stderr().write_all(buf);
        if let Some(f) = self.0.lock().unwrap().as_mut() {
            let _ = f.write_all(buf);
            let _ = f.flush();
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub fn init() {
    let path = log_path();
    let file = path.parent().and_then(|dir| std::fs::create_dir_all(dir).ok()).and_then(|_| {
        let _ = std::fs::rename(&path, path.with_file_name("journal-precedent.txt"));
        std::fs::File::create(&path).ok()
    });
    let tee: &'static Tee = Box::leak(Box::new(Tee(Mutex::new(file))));

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,whisper_rs=warn,wgpu_hal=warn,wgpu_core=warn,naga=warn"))
        .target(env_logger::Target::Pipe(Box::new(tee)))
        .init();
    log::info!(
        "Tour de table {} · {} {} · {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        path.display()
    );

    std::panic::set_hook(Box::new(|info| {
        let where_ = info.location().map(|l| format!(" ({}:{})", l.file(), l.line())).unwrap_or_default();
        let what = panic_message(info.payload());
        let backtrace = std::backtrace::Backtrace::force_capture();
        log::error!("Plantage : {what}{where_}\n{backtrace}");
        if !QUIET_PANICS.load(Ordering::Relaxed) {
            fatal(&format!("Tour de table a rencontré une erreur et doit fermer :\n\n{what}{where_}"));
        }
    }));
}

/// Affiche l'erreur à l'écran (boîte de dialogue), avec l'emplacement du journal.
pub fn fatal(message: &str) {
    log::error!("{message}");
    let text = format!("{message}\n\nJournal : {}", log_path().display());
    eprintln!("{text}");
    rfd::MessageDialog::new()
        .set_title("Tour de table")
        .set_description(&text)
        .set_level(rfd::MessageLevel::Error)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}
