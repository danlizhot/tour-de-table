//! Tour de table — transcription locale de rencontres en français québécois.
//! « Tout le monde parle, rien ne se perd. »
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agenda;
mod app;
mod audio;
mod cli;
mod config;
mod diagnostic;
mod models;
mod quebec;
mod recorder;
mod storage;
mod stt;
mod summary;

use eframe::Renderer;

fn main() {
    diagnostic::init();
    whisper_rs::install_logging_hooks();
    config::migrate_from_noter();

    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|a| a == "--transcrire") {
        let Some(wav) = args.get(2) else {
            eprintln!("Usage : tourdetable --transcrire enregistrement.wav [modele.bin]");
            std::process::exit(2);
        };
        if let Err(e) = cli::transcribe_file(wav, args.get(3).map(String::as_str)) {
            eprintln!("Erreur : {e:#}");
            std::process::exit(1);
        }
        return;
    }
    if args.get(1).is_some_and(|a| a == "--compte-rendu") {
        let (Some(agenda), Some(source)) = (args.get(2), args.get(3)) else {
            eprintln!("Usage : tourdetable --compte-rendu ordre-du-jour.docx (transcription.txt | enregistrement.wav)");
            std::process::exit(2);
        };
        if let Err(e) = cli::compte_rendu(agenda, source) {
            eprintln!("Erreur : {e:#}");
            std::process::exit(1);
        }
        return;
    }

    // On essaie les moteurs d'affichage l'un après l'autre : une machine virtuelle
    // sans carte graphique (Winboat, VirtualBox, bureau à distance…) n'offre souvent
    // pas OpenGL 3, mais Windows y fournit toujours Direct3D 12 en rendu logiciel (WARP).
    let mut order = if cfg!(windows) || cfg!(target_os = "macos") {
        [Renderer::Wgpu, Renderer::Glow]
    } else {
        [Renderer::Glow, Renderer::Wgpu]
    };
    // Dépannage : TOURDETABLE_AFFICHAGE=glow (OpenGL) ou wgpu (Direct3D 12 / Metal) en premier.
    match std::env::var("TOURDETABLE_AFFICHAGE").as_deref() {
        Ok("glow") => order = [Renderer::Glow, Renderer::Wgpu],
        Ok("wgpu") => order = [Renderer::Wgpu, Renderer::Glow],
        _ => {}
    }
    let [first, second] = order;
    log::info!("Démarrage de l'interface avec {first:?}");
    // Certains pilotes paniquent au lieu de renvoyer une erreur : on intercepte
    // aussi ces plantages.
    diagnostic::set_quiet_panics(true);
    let result = std::panic::catch_unwind(|| run_gui(first));
    diagnostic::set_quiet_panics(false);
    let error = match result {
        Ok(Ok(())) => return,
        Ok(Err(e)) => e.to_string(),
        Err(panic) => diagnostic::panic_message(&*panic),
    };
    log::error!("Moteur d'affichage {first:?} indisponible : {error}");

    // Après un échec, la boucle d'événements de la fenêtre n'est plus réutilisable :
    // on relance l'application dans un processus neuf avec l'autre moteur.
    match std::env::var("TOURDETABLE_ERREUR_AFFICHAGE") {
        Err(_) => {
            let relaunch = std::env::current_exe().and_then(|exe| {
                std::process::Command::new(exe)
                    .args(&args[1..])
                    .env("TOURDETABLE_AFFICHAGE", if second == Renderer::Glow { "glow" } else { "wgpu" })
                    .env("TOURDETABLE_ERREUR_AFFICHAGE", format!("{first:?} : {error}"))
                    .spawn()
            });
            match relaunch {
                Ok(_) => log::info!("Relance avec {second:?}"),
                Err(e) => fatal_display(&[format!("{first:?} : {error}"), format!("relance impossible : {e}")]),
            }
        }
        Ok(previous) => fatal_display(&[previous, format!("{first:?} : {error}")]),
    }
}

fn fatal_display(errors: &[String]) {
    diagnostic::fatal(&format!(
        "Tour de table n'a pas pu ouvrir sa fenêtre : aucun moteur d'affichage n'est disponible \
         sur cet ordinateur.\n\n{}\n\nSur une machine virtuelle, activez l'accélération 3D \
         ou installez le pilote graphique de l'hôte.",
        errors.join("\n")
    ));
}

fn run_gui(renderer: Renderer) -> eframe::Result {
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-256.png")).ok();
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title("Tour de table")
        .with_app_id("ca.tourdetable.TourDeTable")
        .with_inner_size([1100.0, 720.0])
        .with_min_inner_size([760.0, 480.0]);
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        renderer,
        wgpu_options: wgpu_options(),
        ..Default::default()
    };
    eframe::run_native(
        "Tour de table",
        options,
        Box::new(|cc| Ok(Box::new(app::TourDeTableApp::new(cc)))),
    )
}

/// Choix de la carte graphique pour wgpu : la meilleure disponible, et à défaut le
/// rendu logiciel (WARP sous Windows) que wgpu écarte normalement.
fn wgpu_options() -> egui_wgpu::WgpuConfiguration {
    use egui_wgpu::{WgpuSetup, WgpuSetupCreateNew};
    use wgpu::DeviceType;

    let selector: egui_wgpu::NativeAdapterSelectorMethod = std::sync::Arc::new(|adapters, surface| {
        let rank = |t: DeviceType| match t {
            DeviceType::DiscreteGpu => 0,
            DeviceType::IntegratedGpu => 1,
            DeviceType::VirtualGpu => 2,
            DeviceType::Other => 3,
            DeviceType::Cpu => 4,
        };
        for a in adapters {
            let i = a.get_info();
            log::info!("Carte graphique vue : {} ({:?}, {:?})", i.name, i.device_type, i.backend);
        }
        adapters
            .iter()
            .filter(|a| surface.is_none_or(|s| a.is_surface_supported(s)))
            .min_by_key(|a| rank(a.get_info().device_type))
            .cloned()
            .ok_or_else(|| "aucune carte graphique compatible, même logicielle".to_string())
    });
    egui_wgpu::WgpuConfiguration {
        wgpu_setup: WgpuSetup::CreateNew(WgpuSetupCreateNew {
            native_adapter_selector: Some(selector),
            ..WgpuSetupCreateNew::without_display_handle()
        }),
        ..Default::default()
    }
}
