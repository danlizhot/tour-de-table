//! Version installée par le Microsoft Store : à l'ouverture, Windows est interrogé pour savoir si
//! une nouvelle version est offerte. Rien n'est vérifié ailleurs (version non empaquetée, Linux,
//! macOS) : c'est le Store qui installe les mises à jour, l'application ne télécharge rien.

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

/// Ouvre la page de Tour de table dans le Microsoft Store, où l'utilisateur lance la mise à jour.
///
/// Par `Windows.System.Launcher`, dans un fil : lancé depuis l'application empaquetée, `explorer` ne reconnaissait
/// pas l'adresse `ms-windows-store:` et ouvrait le dossier Documents (0.3.0, 21 novembre 2026).
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
