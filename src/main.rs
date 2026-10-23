//! Noter — transcription locale de rencontres en français québécois.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agenda;
mod app;
mod audio;
mod cli;
mod config;
mod models;
mod quebec;
mod recorder;
mod storage;
mod stt;
mod summary;

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,whisper_rs=warn")).init();
    whisper_rs::install_logging_hooks();

    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|a| a == "--transcrire") {
        let Some(wav) = args.get(2) else {
            eprintln!("Usage : noter --transcrire enregistrement.wav [modele.bin]");
            std::process::exit(2);
        };
        if let Err(e) = cli::transcribe_file(wav, args.get(3).map(String::as_str)) {
            eprintln!("Erreur : {e:#}");
            std::process::exit(1);
        }
        return Ok(());
    }
    if args.get(1).is_some_and(|a| a == "--compte-rendu") {
        let (Some(agenda), Some(source)) = (args.get(2), args.get(3)) else {
            eprintln!("Usage : noter --compte-rendu ordre-du-jour.docx (transcription.txt | enregistrement.wav)");
            std::process::exit(2);
        };
        if let Err(e) = cli::compte_rendu(agenda, source) {
            eprintln!("Erreur : {e:#}");
            std::process::exit(1);
        }
        return Ok(());
    }

    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-256.png")).ok();
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title("Noter")
        .with_app_id("ca.noter.Noter")
        .with_inner_size([1100.0, 720.0])
        .with_min_inner_size([760.0, 480.0]);
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    eframe::run_native(
        "Noter",
        eframe::NativeOptions { viewport, ..Default::default() },
        Box::new(|cc| Ok(Box::new(app::NoterApp::new(cc)))),
    )
}
