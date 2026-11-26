//! Icône de Tour de table près de l'horloge (Windows), montrée seulement pendant un travail en arrière-plan : son
//! infobulle donne l'avancement ; un clic ramène la fenêtre ; son menu offre l'avancement, « Ouvrir Tour de table » et
//! « Quitter ». Une fenêtre de messages invisible, dans son propre fil, reçoit les clics.
//!
//! Ne dépend que de `std` et de `windows` : vérifiable hors ligne depuis Linux (`--target x86_64-pc-windows-gnu`).

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos,
    GetMessageW, HICON, HWND_MESSAGE, IDI_APPLICATION, IDYES, LoadIconW, MB_ICONWARNING, MB_YESNO, MF_GRAYED, MF_SEPARATOR,
    MF_STRING, MSG, MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassW, SetForegroundWindow, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_CONTEXTMENU,
    WM_DESTROY, WM_LBUTTONUP, WM_RBUTTONUP, WNDCLASSW,
};
use windows::core::{PCWSTR, w};

/// Ce que l'utilisateur demande depuis l'icône.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Demande {
    Ouvrir,
    /// « Quitter », déjà confirmé si un travail était en cours.
    Quitter,
}

const MESSAGE_ICONE: u32 = WM_APP + 1;
const CMD_OUVRIR: usize = 1;
const CMD_QUITTER: usize = 2;
const ID_ICONE: u32 = 1;

/// État partagé avec la procédure de la fenêtre (une seule icône à la fois).
struct Partage {
    demandes: Sender<Demande>,
    reveil: Box<dyn Fn() + Send>,
    avancement: String,
    /// Travail encore en cours : « Quitter » demande confirmation.
    travail: bool,
}

static PARTAGE: OnceLock<Mutex<Option<Partage>>> = OnceLock::new();

fn partage() -> &'static Mutex<Option<Partage>> {
    PARTAGE.get_or_init(|| Mutex::new(None))
}

/// Icône affichée ; retirée quand elle est abandonnée.
pub struct Icone {
    /// Fenêtre de messages (HWND n'est pas `Send` : gardée comme entier).
    fenetre: isize,
}

impl Icone {
    /// Montre l'icône avec l'infobulle `avancement`. `reveil` fait passer l'application quand une demande arrive.
    pub fn montrer(avancement: &str, reveil: impl Fn() + Send + 'static) -> Option<(Self, Receiver<Demande>)> {
        let (tx, rx) = channel();
        *partage().lock().ok()? = Some(Partage { demandes: tx, reveil: Box::new(reveil), avancement: avancement.to_string(), travail: true });
        let (pret_tx, pret_rx) = channel();
        let infobulle = avancement.to_string();
        std::thread::spawn(move || boucle(&infobulle, &pret_tx));
        let fenetre = pret_rx.recv().ok()??;
        Some((Self { fenetre }, rx))
    }

    /// Nouvelle infobulle (et première ligne du menu) ; `travail` : un travail est encore en cours.
    pub fn avancement(&self, texte: &str, travail: bool) {
        if let Ok(mut p) = partage().lock()
            && let Some(p) = p.as_mut()
        {
            if p.avancement == texte && p.travail == travail {
                return;
            }
            p.avancement = texte.to_string();
            p.travail = travail;
        }
        let mut donnees = donnees(HWND(self.fenetre as *mut _));
        donnees.uFlags = NIF_TIP;
        copier(&mut donnees.szTip, &infobulle(texte));
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &donnees);
        }
    }
}

impl Drop for Icone {
    fn drop(&mut self) {
        unsafe {
            let _ = PostMessageW(Some(HWND(self.fenetre as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

fn infobulle(avancement: &str) -> String {
    format!("Tour de table · {avancement}")
}

/// Copie `texte` dans un tableau UTF-16 terminé par un zéro, coupé au besoin.
fn copier(dest: &mut [u16], texte: &str) {
    let (mut n, max) = (0, dest.len().saturating_sub(1));
    for (d, c) in dest.iter_mut().zip(texte.encode_utf16().take(max)) {
        *d = c;
        n += 1;
    }
    dest[n] = 0;
}

fn donnees(fenetre: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: fenetre, uID: ID_ICONE, ..Default::default() }
}

/// Icône de l'exécutable (ressource 1, voir `build.rs`), sinon celle de Windows.
fn icone_du_programme() -> HICON {
    unsafe {
        let module = GetModuleHandleW(None).ok();
        module
            .and_then(|m| LoadIconW(Some(m.into()), PCWSTR(1 as _)).ok())
            .or_else(|| LoadIconW(None, IDI_APPLICATION).ok())
            .unwrap_or_default()
    }
}

/// Fil de l'icône : fenêtre de messages, icône, puis boucle de messages jusqu'à `WM_CLOSE`.
fn boucle(avancement: &str, pret: &Sender<Option<isize>>) {
    unsafe {
        let module = GetModuleHandleW(None).ok();
        let classe = w!("TourDeTableZoneNotification");
        let wc = WNDCLASSW { lpfnWndProc: Some(procedure), hInstance: module.map(Into::into).unwrap_or_default(), lpszClassName: classe, ..Default::default() };
        RegisterClassW(&wc);
        let fenetre = match CreateWindowExW(
            WINDOW_EX_STYLE(0),
            classe,
            w!("Tour de table"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            module.map(Into::into),
            None,
        ) {
            Ok(f) => f,
            Err(_) => {
                let _ = pret.send(None);
                return;
            }
        };
        let mut d = donnees(fenetre);
        d.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        d.uCallbackMessage = MESSAGE_ICONE;
        d.hIcon = icone_du_programme();
        copier(&mut d.szTip, &infobulle(avancement));
        if !Shell_NotifyIconW(NIM_ADD, &d).as_bool() {
            let _ = DestroyWindow(fenetre);
            let _ = pret.send(None);
            return;
        }
        let _ = pret.send(Some(fenetre.0 as isize));
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn demander(d: Demande) {
    if let Ok(p) = partage().lock()
        && let Some(p) = p.as_ref()
    {
        let _ = p.demandes.send(d);
        (p.reveil)();
    }
}

unsafe extern "system" fn procedure(fenetre: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        MESSAGE_ICONE => {
            match (lparam.0 & 0xFFFF) as u32 {
                WM_LBUTTONUP => demander(Demande::Ouvrir),
                WM_RBUTTONUP | WM_CONTEXTMENU => unsafe { menu(fenetre) },
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &donnees(fenetre));
                let _ = DestroyWindow(fenetre);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(fenetre, message, wparam, lparam) },
    }
}

/// Menu de l'icône, à la position de la souris.
unsafe fn menu(fenetre: HWND) {
    let (avancement, travail) = partage()
        .lock()
        .ok()
        .and_then(|p| p.as_ref().map(|p| (p.avancement.clone(), p.travail)))
        .unwrap_or_default();
    let large = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (ligne, ouvrir, quitter) = (large(&avancement), large("Ouvrir Tour de table"), large("Quitter"));
    unsafe {
        let Ok(m) = CreatePopupMenu() else { return };
        let _ = AppendMenuW(m, MF_STRING | MF_GRAYED, 0, PCWSTR(ligne.as_ptr()));
        let _ = AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(m, MF_STRING, CMD_OUVRIR, PCWSTR(ouvrir.as_ptr()));
        let _ = AppendMenuW(m, MF_STRING, CMD_QUITTER, PCWSTR(quitter.as_ptr()));
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // Sans le premier plan, le menu ne se fermerait pas en cliquant ailleurs.
        let _ = SetForegroundWindow(fenetre);
        let choix = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, Some(0), fenetre, None).0 as usize;
        let _ = DestroyMenu(m);
        match choix {
            CMD_OUVRIR => demander(Demande::Ouvrir),
            CMD_QUITTER => {
                let confirme = !travail
                    || MessageBoxW(
                        None,
                        w!("Tour de table n'a pas fini son travail. S'il est arrêté maintenant, il le reprendra au prochain démarrage.\n\nQuitter quand même ?"),
                        w!("Tour de table"),
                        MB_YESNO | MB_ICONWARNING,
                    ) == IDYES;
                if confirme {
                    demander(Demande::Quitter);
                }
            }
            _ => {}
        }
    }
}
