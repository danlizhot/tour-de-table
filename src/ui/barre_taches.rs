//! Avancement des travaux longs (import, rédaction du compte rendu…) dans l'icône de la barre des
//! tâches, comme le font les applications Tauri (Vibe) : `ITaskbarList3` sous Windows. Ailleurs,
//! rien pour l'instant.

/// Ce que montre l'icône.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Avancement {
    Aucun,
    /// Travail en cours dont la durée est inconnue (barre animée).
    Indetermine,
    /// Part faite, de 0 à 1.
    Fraction(f32),
}

pub struct BarreTaches {
    #[cfg(windows)]
    windows: Option<windows_impl::Icone>,
    affiche: Avancement,
}

impl BarreTaches {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        #[cfg(not(windows))]
        let _ = cc;
        Self {
            #[cfg(windows)]
            windows: windows_impl::Icone::new(cc),
            affiche: Avancement::Aucun,
        }
    }

    /// Met l'icône à jour ; n'appelle le système que si l'affichage change.
    pub fn afficher(&mut self, a: Avancement) {
        // Un pour cent près suffit : inutile d'appeler le système à chaque image.
        let a = match a {
            Avancement::Fraction(f) => Avancement::Fraction((f.clamp(0.0, 1.0) * 100.0).round() / 100.0),
            autre => autre,
        };
        if a == self.affiche {
            return;
        }
        self.affiche = a;
        #[cfg(windows)]
        if let Some(icone) = &self.windows {
            icone.afficher(a);
        }
    }
}

#[cfg(windows)]
mod windows_impl {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx};
    use windows::Win32::UI::Shell::{ITaskbarList3, TBPF_INDETERMINATE, TBPF_NOPROGRESS, TBPF_NORMAL, TaskbarList};

    use super::Avancement;

    pub struct Icone {
        liste: ITaskbarList3,
        fenetre: HWND,
    }

    impl Icone {
        pub fn new(cc: &eframe::CreationContext<'_>) -> Option<Self> {
            let RawWindowHandle::Win32(h) = cc.window_handle().ok()?.as_raw() else { return None };
            let fenetre = HWND(h.hwnd.get() as *mut _);
            // winit a déjà initialisé COM sur ce fil : l'appel ne fait alors rien.
            let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
            let liste: ITaskbarList3 = match unsafe { CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER) } {
                Ok(l) => l,
                Err(e) => {
                    log::warn!("Barre des tâches : {e}");
                    return None;
                }
            };
            unsafe { liste.HrInit() }.ok()?;
            Some(Self { liste, fenetre })
        }

        pub fn afficher(&self, a: Avancement) {
            let r = unsafe {
                match a {
                    Avancement::Aucun => self.liste.SetProgressState(self.fenetre, TBPF_NOPROGRESS),
                    Avancement::Indetermine => self.liste.SetProgressState(self.fenetre, TBPF_INDETERMINATE),
                    Avancement::Fraction(f) => self
                        .liste
                        .SetProgressState(self.fenetre, TBPF_NORMAL)
                        .and_then(|()| self.liste.SetProgressValue(self.fenetre, (f * 1000.0) as u64, 1000)),
                }
            };
            if let Err(e) = r {
                log::warn!("Barre des tâches : {e}");
            }
        }
    }
}
