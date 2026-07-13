use eframe::egui;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DialogReply {
    Primary,
    Secondary,
    Closed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DialogAction {
    None,
    ClearAllKeys,
    ClearSpecificKey,
    IgnoreSaveError,
    RetryUpdate,
    Exit,
}

pub struct GenericDialogBox {
    dialog_box_title: String,
    dialog_box_body: Box<dyn FnMut(&mut egui::Ui)>,
    primary_button: String,
    secondary_button: Option<String>,
    pub action: DialogAction,
    is_open: bool,
}

impl GenericDialogBox {
    pub fn new(
        title: impl Into<String>,
        body: impl FnMut(&mut egui::Ui) + 'static,
        primary_button: impl Into<String>,
        secondary_button: Option<impl Into<String>>,
        action: DialogAction,
    ) -> Self {
        Self {
            dialog_box_title: title.into(),
            dialog_box_body: Box::new(body),
            primary_button: primary_button.into(),
            secondary_button: secondary_button.map(|button| button.into()),
            action,
            is_open: true,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<DialogReply> {
        if !self.is_open {
            return Some(DialogReply::Closed);
        }

        let mut open = self.is_open;
        let mut reply = None;

        egui::Window::new(self.dialog_box_title.as_str())
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                (self.dialog_box_body)(ui);
                ui.add_space(10.0);

                ui.horizontal(|ui| {
                    if ui.button(self.primary_button.as_str()).clicked() {
                        reply = Some(DialogReply::Primary);
                    }

                    if let Some(secondary) = &self.secondary_button {
                        if ui.button(secondary.as_str()).clicked() {
                            reply = Some(DialogReply::Secondary);
                        }
                    }
                });
            });

        if reply.is_some() {
            open = false;
        }

        let was_open = self.is_open;
        self.is_open = open;

        if !open && reply.is_none() && was_open {
            return Some(DialogReply::Closed);
        }

        reply
    }
}
