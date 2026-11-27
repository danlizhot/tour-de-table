//! Version installée par le Microsoft Store : à l'ouverture, Windows est interrogé pour savoir si
//! une nouvelle version est offerte. Rien n'est vérifié ailleurs (version non empaquetée, Linux,
//! macOS). « Mettre à jour » la fait installer par le Store depuis l'application (`installer`) : Windows
//! montre sa fenêtre de mise à jour, télécharge, ferme Tour de table, installe et le relance. L'application
//! elle-même ne télécharge rien ; en cas d'échec, la page du Store s'ouvre comme avant.

use crossbeam_channel::Receiver;

/// Page de Tour de table dans l'application Microsoft Store, avec son bouton « Mettre à jour ».
#[cfg_attr(not(windows), allow(dead_code))]
const PAGE_STORE: &str = "ms-windows-store://pdp/?ProductId=9NZ4G5FG7L15";

/// Lance la vérification dans un fil ; le canal reçoit `true` si une mise à jour est offerte. Il
/// ne reçoit rien hors de la version du Store, ou si le Store ne répond pas.
pub fn verifier(reveil: impl Fn() + Send + 'static) -> Receiver<bool> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    #[cfg(windows)]
    std::thread::spawn(move || match offerte() {
        Ok(Some(oui)) => {
            log::info!("Mise à jour offerte par le Microsoft Store : {}", if oui { "oui" } else { "non" });
            let _ = tx.send(oui);
            reveil();
        }
        Ok(None) => {}
        Err(e) => log::warn!("Vérification des mises à jour du Microsoft Store : {e}"),
    });
    #[cfg(not(windows))]
    drop((tx, reveil));
    rx
}

/// `None` hors de la version empaquetée (rien à demander au Store).
#[cfg(windows)]
fn offerte() -> windows::core::Result<Option<bool>> {
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
    // SAFETY: initialisation COM du fil courant, créé pour cette seule vérification.
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if windows::ApplicationModel::Package::Current().is_err() {
        return Ok(None);
    }
    let store = windows::Services::Store::StoreContext::GetDefault()?;
    let mises_a_jour = store.GetAppAndOptionalStorePackageUpdatesAsync()?.join()?;
    Ok(Some(mises_a_jour.Size()? > 0))
}

/// Où en est l'installation lancée par `installer`.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Debug, PartialEq)]
pub enum Etat {
    /// En attente de Windows (fenêtre de confirmation, file du Store).
    Attente,
    /// Téléchargement, part faite de 0 à 1.
    Telechargement(f32),
    /// Installation : Windows va fermer Tour de table, puis le relancer.
    Installation,
    /// Refusée dans la fenêtre de Windows : l'avis revient.
    Annulee,
    /// Échec (message pour le journal) : la page du Store a été ouverte à la place.
    Echec(String),
}

/// Fenêtre de l'application, pour que la fenêtre de mise à jour de Windows lui appartienne (`IInitializeWithWindow`).
pub fn fenetre(cc: &eframe::CreationContext<'_>) -> Option<isize> {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        match cc.window_handle().ok()?.as_raw() {
            RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
            _ => None,
        }
    }
    #[cfg(not(windows))]
    {
        let _ = cc;
        None
    }
}

/// Fait télécharger et installer la nouvelle version par le Microsoft Store, dans un fil. Le canal reçoit l'avancement ;
/// pendant l'installation, Windows ferme l'application (inscrite pour être relancée) et rien d'autre n'arrive.
/// En cas d'échec, la page du Store s'ouvre, où l'utilisateur peut mettre à jour lui-même.
pub fn installer(fenetre: Option<isize>, reveil: impl Fn() + Send + Sync + 'static) -> Receiver<Etat> {
    let (tx, rx) = crossbeam_channel::unbounded();
    #[cfg(windows)]
    std::thread::spawn(move || {
        let reveil = std::sync::Arc::new(reveil);
        let fin = match windows_impl::installer(fenetre, &tx, reveil.clone()) {
            Ok(etat) => etat,
            Err(e) => Etat::Echec(e.to_string()),
        };
        match &fin {
            Etat::Echec(e) => {
                log::warn!("Mise à jour par le Microsoft Store : {e} ; ouverture de sa page");
                ouvrir_store();
            }
            Etat::Annulee => log::info!("Mise à jour refusée dans la fenêtre de Windows"),
            _ => {}
        }
        let _ = tx.send(fin);
        reveil();
    });
    #[cfg(not(windows))]
    {
        let _ = (fenetre, reveil);
        let _ = tx.send(Etat::Echec("version du Microsoft Store seulement".into()));
    }
    rx
}

#[cfg(windows)]
mod windows_impl {
    use super::Etat;
    use crossbeam_channel::Sender;
    use std::sync::Arc;
        use windows::Services::Store::{StoreContext, StorePackageUpdateResult, StorePackageUpdateState, StorePackageUpdateStatus};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
    use windows::Win32::System::Recovery::{RESTART_NO_CRASH, RESTART_NO_HANG, RESTART_NO_REBOOT, RegisterApplicationRestart};
    use windows::Win32::UI::Shell::IInitializeWithWindow;
    use windows::core::{Interface, PCWSTR};
    use windows_future::AsyncOperationProgressHandler;

    /// État final : `Annulee`, `Echec`, ou rien du tout si Windows ferme l'application pour l'installer.
    pub fn installer(fenetre: Option<isize>, tx: &Sender<Etat>, reveil: Arc<dyn Fn() + Send + Sync>) -> windows::core::Result<Etat> {
        // SAFETY: initialisation COM du fil courant, créé pour cette seule mise à jour.
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let store = StoreContext::GetDefault()?;
        if let Some(h) = fenetre {
            let init: IInitializeWithWindow = store.cast()?;
            // SAFETY: `h` est la fenêtre de l'application, vivante tant que l'application tourne.
            unsafe { init.Initialize(HWND(h as *mut _)) }?;
        }
        let mises_a_jour = store.GetAppAndOptionalStorePackageUpdatesAsync()?.join()?;
        if mises_a_jour.Size()? == 0 {
            return Ok(Etat::Echec("aucune mise à jour offerte".into()));
        }
        // Relancée par Windows après l'installation (pas après un plantage ni un gel).
        // SAFETY: appel sans pointeur conservé ; ligne de commande vide.
        if let Err(e) = unsafe { RegisterApplicationRestart(PCWSTR::null(), RESTART_NO_CRASH | RESTART_NO_HANG | RESTART_NO_REBOOT) } {
            log::warn!("Relance après la mise à jour : {e}");
        }
        log::info!("Mise à jour par le Microsoft Store : demande à Windows");
        let operation = store.RequestDownloadAndInstallStorePackageUpdatesAsync(&mises_a_jour)?;
        let (tx2, reveil2) = (tx.clone(), reveil.clone());
        operation.SetProgress(&AsyncOperationProgressHandler::<StorePackageUpdateResult, StorePackageUpdateStatus>::new(
            move |_, statut| {
                let s: &StorePackageUpdateStatus = &statut;
                let etat = if s.PackageUpdateState == StorePackageUpdateState::Deploying {
                    Etat::Installation
                } else if s.PackageUpdateState == StorePackageUpdateState::Downloading {
                    // Le téléchargement couvre les quatre premiers cinquièmes de l'avancement total.
                    Etat::Telechargement((s.TotalDownloadProgress / 0.8).clamp(0.0, 1.0) as f32)
                } else {
                    return Ok(());
                };
                let _ = tx2.send(etat);
                reveil2();
                Ok(())
            },
        ))?;
        let resultat = operation.join()?;
        let etat = resultat.OverallState()?;
        log::info!("Mise à jour par le Microsoft Store : état final {}", etat.0);
        Ok(match etat {
            StorePackageUpdateState::Canceled => Etat::Annulee,
            // Installée sans que Windows ferme l'application : rare, il suffit alors de la rouvrir.
            StorePackageUpdateState::Completed => Etat::Installation,
            StorePackageUpdateState::ErrorLowBattery => Etat::Echec("batterie trop faible".into()),
            StorePackageUpdateState::ErrorWiFiRequired | StorePackageUpdateState::ErrorWiFiRecommended => {
                Etat::Echec("connexion Wi-Fi demandée par Windows".into())
            }
            autre => Etat::Echec(format!("état {}", autre.0)),
        })
    }
}

/// Ouvre la page de Tour de table dans le Microsoft Store, où l'utilisateur lance la mise à jour.
///
/// Par `Windows.System.Launcher`, dans un fil : lancé depuis l'application empaquetée, `explorer` ne reconnaissait
/// pas l'adresse `ms-windows-store:` et ouvrait le dossier Documents (0.3.0, 21 novembre 2026).
#[cfg_attr(not(windows), allow(dead_code))]
pub fn ouvrir_store() {
    #[cfg(windows)]
    std::thread::spawn(|| {
        let lance = || -> windows::core::Result<bool> {
            use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
            // SAFETY: initialisation COM du fil courant, créé pour cette seule ouverture.
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            let uri = windows::Foundation::Uri::CreateUri(&windows::core::HSTRING::from(PAGE_STORE))?;
            windows::System::Launcher::LaunchUriAsync(&uri)?.join()
        };
        match lance() {
            Ok(true) => log::info!("Page du Microsoft Store ouverte"),
            Ok(false) => log::warn!("Ouverture du Microsoft Store refusée par Windows"),
            Err(e) => log::warn!("Ouverture du Microsoft Store : {e}"),
        }
    });
}
