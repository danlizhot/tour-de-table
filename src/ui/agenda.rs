//! Ordre du jour : dépôt, saisie et modification, avant ou après la rencontre.

use eframe::egui::{self, Align, Layout, RichText, Ui};

use super::{TourDeTableApp, View};
use crate::storage::Agenda;

impl TourDeTableApp {
    /// Ordre du jour visé par un dépôt : la rencontre affichée, sinon la prochaine.
    fn agenda_slot(&mut self) -> &mut Option<Agenda> {
        match self.view {
            View::Past(i) if i < self.meetings.len() => &mut self.meetings[i].agenda,
            _ => match self.live.as_mut() {
                Some(m) => &mut m.agenda,
                None => &mut self.draft_agenda,
            },
        }
    }

    pub(super) fn attach_agenda(&mut self, path: &std::path::Path) {
        match crate::agenda::read(path) {
            Ok(text) => {
                let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                *self.agenda_slot() = Some(Agenda { file: file.clone(), text });
                self.agenda_changed();
                self.notify(format!("Ordre du jour « {file} » ajouté."), false);
            }
            Err(e) => self.notify(format!("Ordre du jour : {e:#}"), true),
        }
    }

    /// Sauvegarde et, pendant une rencontre, guide Whisper avec les nouveaux sujets.
    fn agenda_changed(&mut self) {
        match self.view {
            View::Past(i) if i < self.meetings.len() => {
                let _ = self.meetings[i].save();
            }
            _ => {
                if let Some(m) = &self.live {
                    let _ = m.save();
                    self.push_settings();
                }
            }
        }
    }

    pub(super) fn agenda_card(&mut self, ui: &mut Ui) {
        let mut browse = false;
        let mut remove = false;
        let mut edited = false;
        let mut manual = false;
        let slot = self.agenda_slot().clone();
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new("Ordre du jour").strong());
                match &slot {
                    None => {
                        ui.label(RichText::new("optionnel : glissez un document Word, PDF ou texte ici").weak());
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            manual = ui.small_button("Saisir").clicked();
                            browse = ui.small_button("Parcourir…").clicked();
                        });
                    }
                    Some(a) => {
                        ui.label(RichText::new(&a.file).weak());
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            remove = ui.small_button("Retirer").clicked();
                            browse = ui.small_button("Remplacer…").clicked();
                        });
                    }
                }
            });
            if slot.is_some() {
                egui::CollapsingHeader::new("Voir et modifier").id_salt("agenda_text").show(ui, |ui| {
                    if let Some(a) = self.agenda_slot().as_mut() {
                        edited = ui
                            .add(egui::TextEdit::multiline(&mut a.text).desired_rows(6).desired_width(f32::INFINITY))
                            .lost_focus();
                    }
                });
            }
        });
        if manual {
            *self.agenda_slot() = Some(Agenda { file: "Saisi à la main".into(), text: String::new() });
        }
        if remove {
            *self.agenda_slot() = None;
            self.agenda_changed();
        }
        if edited {
            self.agenda_changed();
        }
        if browse {
            let picked = rfd::FileDialog::new()
                .set_title("Choisir l'ordre du jour")
                .add_filter("Ordre du jour", crate::agenda::EXTENSIONS)
                .pick_file();
            if let Some(path) = picked {
                self.attach_agenda(&path);
            }
        }
    }
}
