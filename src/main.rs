#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use eframe::egui::{self, UiKind};
use egui_extras::{Column, TableBuilder};
use regex::Regex;
use rfd::FileDialog;
use std::fmt;
use std::sync::OnceLock;
use std::{fs, path::PathBuf};

mod app_config;
mod config;
mod fetch;
mod generic_dialog_box;
mod tunnel;
mod update_dialog;
#[cfg(not(target_os = "windows"))]
mod utils_nix;
#[cfg(target_os = "windows")]
mod utils_win;
use app_config::{AppConfig, Keys, TunnelMode, load_config, save_config};
use fetch::fetch_keys_data;
use generic_dialog_box::{DialogAction, DialogReply, GenericDialogBox};
use update_dialog::UpdateDialog;

static WG_KEY_REGEX: OnceLock<regex::Regex> = OnceLock::new();
static APPDATA_PATH: OnceLock<PathBuf> = OnceLock::new();

pub fn appdata_path() -> &'static PathBuf {
    APPDATA_PATH.get_or_init(|| {
        #[cfg(target_os = "windows")]
        {
            if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
                return PathBuf::from(local_app_data).join("mbtunnel");
            }
        }

        #[cfg(target_os = "linux")]
        {
            if let Some(home) = std::env::var_os("HOME") {
                return PathBuf::from(home)
                    .join(".local")
                    .join("share")
                    .join("mbtunnel");
            }
        }

        PathBuf::from("./")
    })
}

struct Opt<T>(Option<T>);

impl<T: fmt::Display> fmt::Display for Opt<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Some(v) => write!(f, "{v}"),
            None => write!(f, ""),
        }
    }
}

struct MyApp {
    config: AppConfig,
    current_page: Page,
    wgkey_dialog_input: String,
    wgkey_dialog_show: bool,
    wgkey_dialog_text_hidden: bool,
    dialog_box: Option<GenericDialogBox>,
    pending_delete_key_index: Option<usize>,
    ignore_save_error: bool,
    load_config_error: Option<Box<dyn std::error::Error>>,
    update_dialog: Option<UpdateDialog>,
    dir_creation_error: Option<String>,
}

impl MyApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        _cc.egui_ctx.set_pixels_per_point(1.25);

        let mut dir_creation_error = None;
        let mut load_config_error: Option<Box<dyn std::error::Error>> = None;

        let config = match fs::create_dir_all(appdata_path()) {
            Ok(_) => match load_config(appdata_path().join("config.dat")) {
                Ok(cfg) => cfg,
                Err(err) => {
                    load_config_error = Some(err);
                    AppConfig::default()
                }
            },
            Err(e) => {
                dir_creation_error = Some(e.to_string());
                AppConfig::default()
            }
        };

        if config.dark_mode {
            _cc.egui_ctx.set_visuals(egui::Visuals::dark());
        } else {
            _cc.egui_ctx.set_visuals(egui::Visuals::light());
        }

        let update_dialog = if dir_creation_error.is_none() {
            Some(UpdateDialog::new(_cc.egui_ctx.clone()))
        } else {
            None
        };

        Self {
            current_page: Page::Tunnel,
            wgkey_dialog_show: false,
            wgkey_dialog_input: String::new(),
            wgkey_dialog_text_hidden: true,
            dialog_box: None,
            config,
            load_config_error,
            ignore_save_error: false,
            pending_delete_key_index: None,
            update_dialog,
            dir_creation_error,
        }
    }

    fn handle_dialog_reply(&mut self, action: DialogAction, reply: DialogReply) {
        match (action, reply) {
            (DialogAction::ClearAllKeys, DialogReply::Secondary) => {
                self.config.keys.clear();
            }
            (DialogAction::ClearSpecificKey, DialogReply::Secondary) => {
                if let Some(index) = self.pending_delete_key_index.take() {
                    if index < self.config.keys.len() {
                        self.config.keys.remove(index);
                    }
                }
            }
            (DialogAction::IgnoreSaveError, DialogReply::Primary) => {
                self.ignore_save_error = true;
            }
            (DialogAction::Exit, _) => {
                std::process::exit(1);
            }
            _ => {}
        }

        if action == DialogAction::ClearSpecificKey {
            self.pending_delete_key_index = None;
        }
    }

    fn refresh_keys_data(&mut self) {
        match fetch_keys_data(self.config.keys.clone()) {
            Ok(keys) => {
                self.config.keys = keys;
            }
            Err(e) => {
                self.dialog_box = Some(GenericDialogBox::info(
                    "Error",
                    format!("Failed to fetch keys data: {:#?}", e),
                    "Close",
                ));
            }
        }
    }

    fn show_tunnel_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("🚀 Tunnel");
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Tunnel Mode:");
            ui.radio_value(&mut self.config.tunnel_mode, TunnelMode::Auto, "Auto");
            ui.radio_value(&mut self.config.tunnel_mode, TunnelMode::TCP, "TCP");
            ui.radio_value(&mut self.config.tunnel_mode, TunnelMode::UDP, "UDP");
        });

        ui.add_space(12.0);
        if ui.button("Go to Settings").clicked() {
            self.current_page = Page::Settings;
        }
    }

    fn show_config_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("📁 Configs");
        ui.separator();
        ui.add_space(8.0);

        ui.menu_button("Add config", |ui| {
            if ui.button("From file").clicked() {
                let path = FileDialog::new()
                    .set_title("Select your Wireguard config")
                    .add_filter("Config", &["conf"])
                    .add_filter("All Files", &["*"])
                    .pick_file();

                if let Some(path) = path {
                    match parse_wg_config(path) {
                        Ok(key) => {
                            if !key.is_empty() {
                                self.config.keys.push(Keys::new(key));
                                self.dialog_box = Some(GenericDialogBox::info(
                                    "Config added",
                                    "The Wireguard key was imported successfully.",
                                    "OK",
                                ));
                            }

                            self.refresh_keys_data();
                        }
                        Err(err) => {
                            self.dialog_box = Some(GenericDialogBox::info(
                                "Invalid config",
                                format!("{err}"),
                                "Close",
                            ));
                        }
                    }
                }
            }

            if ui.button("Using Key").clicked() {
                self.wgkey_dialog_show = true;
                self.wgkey_dialog_input.clear();
                ui.close_kind(UiKind::Menu);
            }
        });

        TableBuilder::new(ui)
            .striped(true)
            .column(Column::auto())
            .column(Column::auto())
            .column(Column::remainder())
            .column(Column::auto().at_least(60.0))
            .column(Column::auto().at_least(40.0))
            .header(20.0, |mut header| {
                header.col(|ui| {
                    ui.label("NetID");
                });
                header.col(|ui| {
                    ui.label("Subscription");
                });
                header.col(|ui| {
                    ui.label("End Date");
                });
                header.col(|ui| {
                    ui.label("        ");
                });
                header.col(|ui| {
                    ui.label("      ");
                });
            })
            .body(|mut body| {
                for (index, key) in self.config.keys.iter().enumerate() {
                    body.row(20.0, |mut row| {
                        row.col(|ui| {
                            ui.label(format!("{}", Opt(key.id.as_deref())));
                        });
                        row.col(|ui| {
                            ui.label(format!("{}", key.get_subscription()));
                        });
                        row.col(|ui| {
                            ui.label(format!("{}", key.get_expiry()));
                        });
                        row.col(|ui| {
                            if ui.button("Show Key").clicked() {
                                self.dialog_box = Some(GenericDialogBox::info(
                                    "Wireguard Key",
                                    format!("{}", key.key),
                                    "Close",
                                ));
                            }
                        });
                        row.col(|ui| {
                            if ui.button("Delete").clicked() {
                                self.pending_delete_key_index = Some(index);
                                self.dialog_box = Some(GenericDialogBox::two_buttons(
                                    "Delete config",
                                    format!(
                                        "Are you sure you want to delete this config?\n {:?}",
                                        key.id
                                    ),
                                    "Cancel",
                                    "Delete",
                                    DialogAction::ClearSpecificKey,
                                ));
                            }
                        });
                    });
                }
            });

        ui.add_space(12.0);
        if ui.button("Clear all saved keys").clicked() {
            self.dialog_box = Some(GenericDialogBox::two_buttons(
                "Clear saved keys",
                "This will remove all saved keys from your settings.",
                "Cancel",
                "Clear",
                DialogAction::ClearAllKeys,
            ));
        }
    }

    fn show_pathfinder_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("🧭 Pathfinder");
        ui.separator();
        ui.add_space(8.0);
    }

    fn show_settings_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("⚙ Settings");
        ui.separator();
        ui.add_space(8.0);

        let is_dark = ui.style().visuals.dark_mode;
        ui.horizontal(|ui| {
            ui.label("Theme: ");
            if ui.selectable_label(is_dark, "🌙 Dark").clicked() {
                ui.set_visuals(egui::Visuals::dark());
                self.config.dark_mode = true;
            }
            if ui.selectable_label(!is_dark, "☀ Light").clicked() {
                ui.set_visuals(egui::Visuals::light());
                self.config.dark_mode = false;
            }
        });
    }

    fn show_about_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("ℹ About");
        ui.separator();
        ui.add_space(8.0);
        ui.label("This app was built with:");
        ui.label("• Rust 🦀");
        ui.label("• egui 0.34");
        ui.label("• eframe 0.34");
        ui.add_space(12.0);
        ui.hyperlink_to("egui on GitHub", "https://github.com/emilk/egui");
    }

    fn show_wgkey_dialog(&mut self, ui: &mut egui::Ui) {
        let text_id = ui.make_persistent_id("dialog_text_edit");

        let had_focus = ui.memory(|mem| mem.has_focus(text_id));

        let show_hide = if self.wgkey_dialog_text_hidden {
            "Show"
        } else {
            "Hide"
        };

        egui::Window::new("Add config from key")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                ui.label("Enter key:");

                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.wgkey_dialog_input)
                            .id(text_id)
                            .hint_text("Paste key here")
                            .password(self.wgkey_dialog_text_hidden)
                            .return_key(egui::KeyboardShortcut::new(
                                egui::Modifiers::NONE,
                                egui::Key::Enter,
                            )),
                    );

                    if ui
                        .add_sized(
                            [60.0, ui.spacing().interact_size.y],
                            egui::Button::new(show_hide),
                        )
                        .clicked()
                    {
                        self.wgkey_dialog_text_hidden = !self.wgkey_dialog_text_hidden;
                        if had_focus {
                            ui.memory_mut(|mem| mem.request_focus(text_id));
                        }
                    }
                });

                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                let enter = enter && ui.memory(|mem| mem.has_focus(text_id));

                let re = WG_KEY_REGEX
                    .get_or_init(|| regex::Regex::new(r"^[A-Za-z0-9+/]{43}=$").unwrap());

                let is_key_empty = self.wgkey_dialog_input.trim().is_empty();
                let is_valid_key = re.is_match(self.wgkey_dialog_input.trim());

                if is_key_empty {
                    ui.label(egui::RichText::new("Please enter key").weak());
                } else if !is_valid_key {
                    ui.label(egui::RichText::new("Please enter a valid key").weak());
                } else {
                    ui.label("");
                }

                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        self.wgkey_dialog_show = false;
                        self.wgkey_dialog_input.clear();
                    }

                    let clicked = ui
                        .add_enabled(is_valid_key, egui::Button::new("Save"))
                        .clicked();

                    if is_valid_key && (clicked || enter) {
                        let final_input = self.wgkey_dialog_input.trim().to_owned();
                        if !final_input.is_empty() {
                            self.config.keys.push(Keys::new(final_input));
                        }
                        self.wgkey_dialog_show = false;
                        self.wgkey_dialog_input.clear();
                        self.refresh_keys_data();
                    }
                })
            });
    }
}

#[derive(PartialEq, Clone)]
enum Page {
    Tunnel,
    Configs,
    Pathfinder,
    Settings,
    About,
}

fn parse_wg_config(path: PathBuf) -> Result<String, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(&path)?;
    let re = Regex::new(r"(?m)^\s*PrivateKey\s*=\s*(\S+)\s*$").unwrap();
    let mut priv_key_b64 = String::new();
    let mut found = false;
    for cap in re.captures_iter(&content) {
        if let Some(m) = cap.get(1) {
            priv_key_b64 = m.as_str().to_string();
            found = true;
        }
    }

    if !found {
        return Err(format!("{} Not a valid Wireguard config file", path.display()).into());
    }

    Ok(priv_key_b64)
}

// This is the main trait you implement — eframe calls `ui()` every frame.
impl eframe::App for MyApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if let Some(err) = &self.dir_creation_error {
            if self.dialog_box.is_none() {
                let mut dialog = GenericDialogBox::info(
                    "Fatal Error",
                    format!("Failed to create AppData directory:\n{}", err),
                    "Exit",
                );
                dialog.action = DialogAction::Exit;
                self.dialog_box = Some(dialog);
            }

            let mut dialog_event = None;
            if let Some(dialog) = self.dialog_box.as_mut() {
                if let Some(reply) = dialog.show(ui.ctx()) {
                    dialog_event = Some((dialog.action, reply));
                }
            }

            if let Some((action, reply)) = dialog_event {
                self.dialog_box = None;
                self.handle_dialog_reply(action, reply);
            }
            return;
        }

        if let Some(dialog) = &mut self.update_dialog {
            if dialog.show(ui.ctx()) {
                self.update_dialog = None;
            }
            return;
        }

        let config_before_frame = self.config.clone();
        let previous_page = self.current_page.clone();

        egui::Panel::left("sidebar")
            .resizable(false)
            .exact_size(160.0)
            .show(ui, |ui| {
                ui.add_space(8.0);
                ui.vertical_centered(|ui| {
                    ui.heading("📋 Menu");
                });
                ui.separator();
                ui.add_space(4.0);

                ui.selectable_value(&mut self.current_page, Page::Tunnel, "🚀 Tunnel");
                ui.selectable_value(&mut self.current_page, Page::Configs, "📁 Configs");
                ui.selectable_value(&mut self.current_page, Page::Pathfinder, "🧭 Pathfinder");
                ui.selectable_value(&mut self.current_page, Page::Settings, "⚙ Settings");
                ui.selectable_value(&mut self.current_page, Page::About, "ℹ About");
            });

        if self.current_page == Page::Configs && previous_page != Page::Configs {
            self.refresh_keys_data();
        }

        egui::CentralPanel::default().show(ui, |ui| match &self.current_page {
            Page::Tunnel => self.show_tunnel_page(ui),
            Page::Configs => self.show_config_page(ui),
            Page::Pathfinder => self.show_pathfinder_page(ui),
            Page::Settings => self.show_settings_page(ui),
            Page::About => self.show_about_page(ui),
        });

        if self.wgkey_dialog_show {
            self.show_wgkey_dialog(ui);
        }

        let mut dialog_event = None;
        if let Some(dialog) = self.dialog_box.as_mut() {
            if let Some(reply) = dialog.show(ui.ctx()) {
                dialog_event = Some((dialog.action, reply));
            }
        }

        if let Some((action, reply)) = dialog_event {
            self.dialog_box = None;
            self.handle_dialog_reply(action, reply);
        }

        if let Some(err) = self.load_config_error.take() {
            self.dialog_box = Some(GenericDialogBox::info(
                "Error loading settings",
                format!("Failed to load mbtunnel settings file: {err}"),
                "Continue",
            ));
        }

        if self.config != config_before_frame {
            if let Err(err) = save_config(&self.config, appdata_path().join("config.dat")) {
                if !self.ignore_save_error {
                    self.dialog_box = Some(GenericDialogBox::two_buttons(
                        "Error saving config",
                        format!("Failed to save config: {err}"),
                        "Don't show again",
                        "Continue",
                        DialogAction::IgnoreSaveError,
                    ));
                }
            }
        }
    }
}

fn handle_update_state() {
    let current_exec = std::env::current_exe().unwrap();
    let exec_name = current_exec.file_name().unwrap().to_str().unwrap();
    let exec_path = current_exec.parent().unwrap();

    let needle = ".updatefile";

    let new_exec_name = if let Some(pos) = exec_name.rfind(needle) {
        format!("{}{}", &exec_name[..pos], &exec_name[pos + needle.len()..])
    } else {
        #[cfg(target_os = "windows")]
        let updatefile_path = current_exec.with_extension("updatefile.exe");
        #[cfg(not(target_os = "windows"))]
        let updatefile_path = current_exec.with_added_extension("updatefile");

        let _ = fs::remove_file(updatefile_path);
        return;
    };

    let new_exec = exec_path.join(new_exec_name);

    let _ = fs::remove_file(&new_exec);
    let _ = fs::copy(current_exec, &new_exec);

    #[cfg(target_os = "windows")]
    crate::utils_win::spawn_detached_process(new_exec).unwrap();

    #[cfg(not(target_os = "windows"))]
    crate::utils_nix::spawn_detached_process(new_exec).unwrap();

    std::process::exit(0);
}

fn main() -> eframe::Result {
    #[cfg(target_os = "windows")]
    utils_win::request_elevation();

    handle_update_state();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([800.0, 600.0])
            .with_title("mbtunnel"),
        ..Default::default()
    };

    eframe::run_native(
        "mbtunnel",
        native_options,
        Box::new(|cc| Ok(Box::new(MyApp::new(cc)))),
    )
}
