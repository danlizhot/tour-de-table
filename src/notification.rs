//! Notifications Windows (version du Microsoft Store) : Tour de table passe en arrière-plan, puis son travail est
//! terminé. Le bouton « Ouvrir », comme un clic sur la notification, lance Tour de table ; s'il tourne encore, c'est
//! l'exemplaire déjà ouvert qui revient (`instance`). Sans identité d'application (version de développement), rien
//! n'est affiché : le journal le note.
//!
//! Ne dépend que de `std` et de `windows` : vérifiable hors ligne depuis Linux (`--target x86_64-pc-windows-gnu`).

use windows::ApplicationModel::Package;
use windows::Data::Xml::Dom::XmlDocument;
use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::core::HSTRING;

/// Affiche une notification, dans un fil (les appels WinRT n'attendent pas l'interface). Attendre le fil rendu
/// avant de quitter l'application, sinon la notification pourrait ne jamais partir.
pub fn afficher(titre: &str, texte: &str, ouvrir: bool) -> std::thread::JoinHandle<()> {
    let xml = gabarit(titre, texte, ouvrir);
    std::thread::spawn(move || {
        if let Err(e) = montrer(&xml) {
            log::warn!("Notification Windows : {e}");
        }
    })
}

fn montrer(xml: &str) -> windows::core::Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    if Package::Current().is_err() {
        log::info!("Notification non affichée (version sans identité d'application)");
        return Ok(());
    }
    let doc = XmlDocument::new()?;
    doc.LoadXml(&HSTRING::from(xml))?;
    let notification = ToastNotification::CreateToastNotification(&doc)?;
    ToastNotificationManager::CreateToastNotifier()?.Show(&notification)
}

fn echapper(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn gabarit(titre: &str, texte: &str, ouvrir: bool) -> String {
    let actions = if ouvrir { r#"<actions><action content="Ouvrir" arguments="ouvrir" activationType="foreground"/></actions>"# } else { "" };
    format!(
        r#"<toast launch="ouvrir"><visual><binding template="ToastGeneric"><text>{}</text><text>{}</text></binding></visual>{actions}</toast>"#,
        echapper(titre),
        echapper(texte)
    )
}
