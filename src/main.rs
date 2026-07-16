#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use defguard_wireguard_rs::WireguardInterfaceApi;
use eframe::egui::{self, UiKind};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use egui_extras::{Column, Size, StripBuilder, TableBuilder};
use regex::Regex;
use rfd::FileDialog;
use std::{
    fmt, fs,
    path::PathBuf,
    sync::{
        Arc, Once, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime},
};
use tray_icon::{
    TrayIcon, TrayIconBuilder,
    menu::{Menu, MenuEvent, MenuItem},
};

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
use crate::tunnel::{Tunnel, TunnelState, TunnelStatus, WgConfig, Wireguard};
use app_config::{AppConfig, CloseAction, Key, TunnelMode, load_config, save_config};
use fetch::{fetch_config_data, fetch_keys_data};
use generic_dialog_box::{DialogAction, DialogReply, GenericDialogBox};
use update_dialog::UpdateDialog;

static WG_KEY_REGEX: OnceLock<regex::Regex> = OnceLock::new();
static APPDATA_PATH: OnceLock<PathBuf> = OnceLock::new();
static CLEANUP_DONE: Once = Once::new();

const APP_WINDOW_SIZE: (f32, f32) = (678.0, 508.0);
const LICENSES_MD: &str = include_str!("licenses.md");

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
struct TrayObject {
    _tray: TrayIcon,
    showhide: MenuItem,
    quit: MenuItem,
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
    tunnel: Tunnel,
    base_style: egui::Style,
    wireguard: Wireguard,
    md_cache: CommonMarkCache,
    show_licenses: bool,
    privilege_checked: bool,
    can_run_wireguard: bool,
    ifdata_error: bool,
    close_app_dialog_open: bool,
    remember_close_action: bool,
    tray_object: Option<TrayObject>,
    window_visible: bool,
    allow_exit: bool,
}

fn show_ansi_log(ui: &mut egui::Ui, log: &[u8], font: f32) {
    let text = String::from_utf8_lossy(log);
    let height = ui.available_height();

    let theme = egui_sgr::EguiAnsiTheme::default();
    let mut job = egui_sgr::ansi_to_layout_job(&text, &theme);

    for section in &mut job.sections {
        section.format.font_id = egui::FontId::monospace(font);
    }

    egui::Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .inner_margin(egui::Margin::same(6))
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .max_height(height)
                .auto_shrink([false, false])
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.add(egui::Label::new(job));
                });
        });
}

fn update_zoom(ui_zoom: f32, font_zoom: f32, mut style: egui::Style, ctx: &egui::Context) {
    ctx.set_zoom_factor(ui_zoom);

    let (x, y) = APP_WINDOW_SIZE;
    let size = egui::Vec2::new(x, y);

    if (ctx.viewport_rect().size() - size).length() > 5.0 {
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
    }

    for font_id in style.text_styles.values_mut() {
        font_id.size *= font_zoom;
    }

    ctx.set_global_style(style);
}

fn humanize_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

fn cleanup() {
    CLEANUP_DONE.call_once(|| {
        let _ = crate::tunnel::remove_stale_iface();
    });
}

struct CleanupGuard;
impl Drop for CleanupGuard {
    fn drop(&mut self) {
        cleanup();
    }
}

fn setup_cleanup_guard() -> Arc<AtomicBool> {
    let running = Arc::new(AtomicBool::new(true));

    // Cross-platform: Ctrl+C on Windows (CTRL_C_EVENT) and Unix (SIGINT)
    let r = running.clone();
    ctrlc::set_handler(move || {
        cleanup();
        r.store(false, Ordering::SeqCst);
        std::process::exit(0);
    })
    .expect("Error setting Ctrl-C handler");

    // Unix-only: SIGTERM and SIGHUP
    #[cfg(unix)]
    {
        use signal_hook::consts::{SIGHUP, SIGTERM};
        use signal_hook::flag;

        let term = Arc::new(AtomicBool::new(false));
        flag::register(SIGTERM, term.clone()).expect("register SIGTERM failed");
        flag::register(SIGHUP, term.clone()).expect("register SIGHUP failed");

        let running_unix = running.clone();
        std::thread::spawn(move || {
            while running_unix.load(Ordering::SeqCst) {
                if term.load(Ordering::SeqCst) {
                    cleanup();
                    std::process::exit(0);
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        });
    }

    // Panic hook, covers unwind-mode panics
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        cleanup();
        default_hook(info);
    }));

    running
}

type GridCell<'a> = Box<dyn FnMut(&mut egui::Ui) + 'a>;
struct GridRow<'a>(GridCell<'a>, GridCell<'a>);

impl<'a> From<(&'a str, &'a str)> for GridRow<'a> {
    fn from((left, right): (&'a str, &'a str)) -> Self {
        GridRow(
            Box::new(move |ui| {
                ui.label(left);
            }),
            Box::new(move |ui| {
                ui.label(right);
            }),
        )
    }
}

fn render_egui_grid<'a, R>(ui: &mut egui::Ui, rows: impl IntoIterator<Item = R>, grid_name: &str)
where
    R: Into<GridRow<'a>>,
{
    egui::Grid::new(grid_name)
        .spacing([16.0, 8.0])
        .striped(true)
        .show(ui, |ui| {
            for GridRow(mut left, mut right) in rows.into_iter().map(Into::into) {
                left(ui);
                right(ui);
                ui.end_row();
            }
        });
}

impl MyApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let _ = crate::tunnel::remove_stale_iface();

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

        let mut base_style = egui::Style::default();
        base_style.text_styles.insert(
            egui::TextStyle::Name("subheading".into()),
            egui::FontId::new(15.0, egui::FontFamily::Proportional),
        );

        update_zoom(
            config.ui_zoom,
            config.font_zoom,
            base_style.clone(),
            &_cc.egui_ctx,
        );

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

        _cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        _cc.egui_ctx
            .send_viewport_cmd(egui::ViewportCommand::Resizable(false));

        let icon_bytes = include_bytes!("../assets/icon.png");
        let image = image::load_from_memory(icon_bytes).unwrap().into_rgba8();
        let (w, h) = image.dimensions();
        let icon = tray_icon::Icon::from_rgba(image.into_raw(), w, h).unwrap();

        let menu = Menu::new();
        let showhide = MenuItem::new("Show/Hide", true, None);
        let quit = MenuItem::new("Quit", true, None);

        menu.append(&showhide).unwrap();
        menu.append(&quit).unwrap();

        #[cfg(target_os = "linux")]
        gtk::init().unwrap();

        let _tray = TrayIconBuilder::new()
            .with_icon(icon)
            .with_menu(Box::new(menu))
            .with_tooltip("mbtunnel")
            .build()
            .unwrap();

        let tray_object = Some(TrayObject {
            _tray,
            showhide,
            quit,
        });

        Self {
            current_page: Page::Home,
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
            tunnel: Tunnel::default(),
            base_style,
            wireguard: Wireguard::new(),
            md_cache: CommonMarkCache::default(),
            show_licenses: false,
            privilege_checked: false,
            can_run_wireguard: true,
            ifdata_error: false,
            close_app_dialog_open: false,
            remember_close_action: false,
            tray_object,
            window_visible: true,
            allow_exit: false,
        }
    }

    fn handle_dialog_reply(&mut self, ui: &mut egui::Ui, action: DialogAction, reply: DialogReply) {
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
            (DialogAction::GracefulExit, DialogReply::Primary) => {
                if self.wireguard.wgapi.is_some() {
                    let wireguard = self.wireguard.wgapi.take().unwrap();
                    let _ = crate::tunnel::stop_wireguard(wireguard);
                }
                self.tray_object.take();
                self.allow_exit = true;
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
            _ => {}
        }

        if action == DialogAction::ClearSpecificKey {
            self.pending_delete_key_index = None;
        }
    }

    fn handle_zoom_shortcut(&mut self, ui: &mut egui::Ui) {
        let input = ui.ctx().input(|i| i.clone());

        if input.modifiers.ctrl && !input.modifiers.shift && !input.modifiers.alt {
            if input.key_pressed(egui::Key::Plus) || input.key_pressed(egui::Key::Equals) {
                let zoom_factor = self.config.ui_zoom + 0.05;
                self.config.ui_zoom = zoom_factor.clamp(0.5, 3.0);
            }

            if input.key_pressed(egui::Key::Minus) {
                let zoom_factor = self.config.ui_zoom - 0.05;
                self.config.ui_zoom = zoom_factor.clamp(0.5, 3.0);
            }

            if input.key_pressed(egui::Key::Num0) {
                self.config.ui_zoom = 1.3;
            }
        }

        if input.modifiers.ctrl && input.modifiers.shift && !input.modifiers.alt {
            if input.key_pressed(egui::Key::Plus) || input.key_pressed(egui::Key::Equals) {
                let zoom_factor = self.config.font_zoom + 0.025;
                self.config.font_zoom = zoom_factor.clamp(0.7, 1.3);
            }

            if input.key_pressed(egui::Key::Minus) {
                let zoom_factor = self.config.font_zoom - 0.025;
                self.config.font_zoom = zoom_factor.clamp(0.7, 1.3);
            }

            if input.key_pressed(egui::Key::Num0) {
                self.config.font_zoom = 1.05;
            }
        }
    }

    fn handle_exit(&mut self, ui: &mut egui::Ui, forced_action: Option<CloseAction>) {
        let close_action = forced_action.unwrap_or(self.config.close_action);
        match close_action {
            CloseAction::Ask => {
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.close_app_dialog_open = true;
                return;
            }
            CloseAction::MinimizeToTray => {
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::Visible(false));
                self.window_visible = false;
                return;
            }
            CloseAction::Exit => {}
        }

        if self.wireguard.wgapi.is_some() {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::CancelClose);
            let message = "The VPN is still running. Are you sure you want to exit?";
            self.dialog_box = Some(GenericDialogBox::new(
                "Confirm Exit",
                move |ui| {
                    ui.label(message);
                },
                "Exit",
                Some("Cancel"),
                DialogAction::GracefulExit,
            ));
        } else {
            self.tray_object.take();
            self.allow_exit = true;
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn refresh_keys_data(&mut self) {
        match fetch_keys_data(self.config.keys.clone()) {
            Ok(keys) => {
                self.config.keys = keys;
            }
            Err(e) => {
                let message = format!("Failed to fetch keys data: {:#?}", e);
                self.dialog_box = Some(GenericDialogBox::new(
                    "Error",
                    move |ui| {
                        ui.label(message.as_str());
                    },
                    "Close",
                    None::<String>,
                    DialogAction::None,
                ));
            }
        }

        self.wireguard.selected_key = self.config.keys.get(0).and_then(|k| k.id.clone());
    }

    fn show_vpn_frame(&mut self, ui: &mut egui::Ui) {
        let key = self
            .config
            .keys
            .iter()
            .find(|k| k.id.clone().unwrap() == self.wireguard.selected_key.clone().unwrap());

        let peer_hashmap = match self.wireguard.wgapi.as_ref() {
            Some(wgapi) => match wgapi.read_interface_data() {
                Ok(data) => Some(data.peers),
                Err(_) => {
                    if !self.ifdata_error {
                        self.dialog_box = Some(GenericDialogBox::new(
                            "Error",
                            |ui| {
                                ui.label("Failed to read Wireguard interface data.");
                            },
                            "Close",
                            None::<String>,
                            DialogAction::None,
                        ));
                        self.ifdata_error = true;
                    }
                    None
                }
            },
            None => None,
        };

        let if_data = match peer_hashmap.as_ref() {
            Some(peers) => peers.values().next(),
            None => None,
        };

        let (rx_bytes, tx_bytes, last_handshake) = match if_data {
            Some(data) => (
                humanize_bytes(data.rx_bytes),
                humanize_bytes(data.tx_bytes),
                data.last_handshake,
            ),
            None => ("".to_string(), "".to_string(), None),
        };

        let seconds_since = match last_handshake {
            Some(handshake) => {
                if handshake
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
                    == 0
                {
                    "".to_string()
                } else {
                    format!(
                        "{} seconds ago",
                        SystemTime::now()
                            .duration_since(handshake)
                            .unwrap_or(Duration::from_secs(0))
                            .as_secs()
                    )
                }
            }
            None => "".to_string(),
        };

        let (ipv4_addr, private_key, is_key_active) = if let Some(key) = key {
            (
                key.ip.clone().unwrap(),
                key.priv_key.clone(),
                key.is_active.unwrap(),
            )
        } else {
            ("".to_string(), "".to_string(), false)
        };

        let mut grid_rows: Vec<GridRow<'_>> = Vec::new();

        if self.wireguard.wgapi.is_none() && !self.wireguard.waiting_for_tunnel {
            grid_rows.push(GridRow(
                Box::new(|ui| {
                    ui.label("VPN State");
                }),
                Box::new(|ui| {
                    ui.label("Not Running");
                }),
            ));

            grid_rows.push(GridRow(
                Box::new(|ui| {
                    ui.label("VPN Key");
                }),
                Box::new(|ui| {
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("key_select")
                            .selected_text(
                                self.wireguard
                                    .selected_key
                                    .as_deref()
                                    .unwrap_or("Select a key"),
                            )
                            .show_ui(ui, |ui| {
                                for key in &self.config.keys {
                                    if let Some(id) = &key.id {
                                        let enabled = id != "Invalid Key";
                                        ui.add_enabled_ui(enabled, |ui| {
                                            ui.selectable_value(
                                                &mut self.wireguard.selected_key,
                                                Some(id.clone()),
                                                id,
                                            );
                                        });
                                    }
                                }

                                ui.separator();

                                if ui.selectable_label(false, "➕ Add key").clicked() {
                                    self.current_page = Page::Keys;
                                }
                            });

                        if ui.button("🔄").clicked() {
                            self.refresh_keys_data();
                        }
                    });
                }),
            ));
        } else {
            if self.wireguard.waiting_for_tunnel {
                grid_rows.push(GridRow(
                    Box::new(|ui| {
                        ui.label("VPN State");
                    }),
                    Box::new(|ui| {
                        ui.label("Waiting for Tunnel");
                    }),
                ));
            } else if seconds_since == "" {
                grid_rows.push(GridRow(
                    Box::new(|ui| {
                        ui.label("VPN State");
                    }),
                    Box::new(|ui| {
                        ui.label("Connecting...");
                    }),
                ));
            } else {
                grid_rows.push(GridRow(
                    Box::new(|ui| {
                        ui.label("VPN State");
                    }),
                    Box::new(|ui| {
                        ui.label("Connected");
                    }),
                ));
            }

            grid_rows.push(GridRow(
                Box::new(|ui| {
                    ui.label("VPN Key");
                }),
                Box::new(|ui| {
                    ui.label(format!(
                        "{}",
                        self.wireguard.selected_key.as_deref().unwrap()
                    ));
                }),
            ));
        }

        grid_rows.push(GridRow(
            Box::new(|ui| {
                ui.label("VPN IP");
            }),
            Box::new(|ui| {
                ui.label(ipv4_addr.clone());
            }),
        ));

        grid_rows.push(GridRow(
            Box::new(|ui| {
                ui.label("Handshake");
            }),
            Box::new(|ui| {
                ui.label(seconds_since.clone());
            }),
        ));

        grid_rows.push(GridRow(
            Box::new(|ui| {
                ui.label("Download");
            }),
            Box::new(|ui| {
                ui.label(rx_bytes.clone());
            }),
        ));

        grid_rows.push(GridRow(
            Box::new(|ui| {
                ui.label("Upload");
            }),
            Box::new(|ui| {
                ui.label(tx_bytes.clone());
            }),
        ));

        render_egui_grid(ui, grid_rows, "vpn_frame_grid");

        ui.add_space(8.0);

        if !self.can_run_wireguard && ui.button("Start VPN").clicked() {
            self.dialog_box = Some(GenericDialogBox::new(
                "Privilege Error",
                |ui| {
                    ui.label("App does not have the privileges to start the VPN.");
                },
                "Close",
                None::<String>,
                DialogAction::None,
            ));
        }

        if (self.wireguard.wgapi.is_none() && !self.wireguard.waiting_for_tunnel)
            && self.can_run_wireguard
            && ui.button("Start VPN").clicked()
        {
            if self.wireguard.selected_key.is_none() {
                self.dialog_box = Some(GenericDialogBox::new(
                    "Error",
                    |ui| {
                        ui.label("Please select a key before starting VPN.");
                    },
                    "Close",
                    None::<String>,
                    DialogAction::None,
                ));
                return;
            }

            if !is_key_active {
                self.dialog_box = Some(GenericDialogBox::new(
                    "Warning",
                    |ui| {
                        ui.label("Subscription for the selected key is inactive.");
                    },
                    "Continue",
                    None::<String>,
                    DialogAction::None,
                ));
            }

            let mut wgconfig = WgConfig::new();

            wgconfig.ipv4_address = vec![ipv4_addr.parse().unwrap()];
            wgconfig.private_key = private_key;

            (
                wgconfig.server_public_key,
                wgconfig.allowed_ips,
                wgconfig.mtu,
            ) = match fetch_config_data(wgconfig.private_key.clone()) {
                Ok(config) => config,
                Err(e) => {
                    let message = format!("Failed to fetch Wireguard config: {:#?}", e);
                    self.dialog_box = Some(GenericDialogBox::new(
                        "Error",
                        move |ui| {
                            ui.label(message.as_str());
                        },
                        "Close",
                        None::<String>,
                        DialogAction::None,
                    ));
                    return;
                }
            };

            let tunnel_status = self.tunnel.state.lock().unwrap().status.clone();

            if tunnel_status == TunnelStatus::Running {
                wgconfig.endpoint_port = self.tunnel.state.lock().unwrap().port.unwrap();
                match crate::tunnel::start_wireguard(wgconfig) {
                    Ok(wgapi) => {
                        self.wireguard.wgapi = Some(wgapi);
                    }
                    Err(e) => {
                        let message = format!("Failed to start Wireguard: {:#?}", e);
                        self.dialog_box = Some(GenericDialogBox::new(
                            "Error",
                            move |ui| {
                                ui.label(message.as_str());
                            },
                            "Close",
                            None::<String>,
                            DialogAction::None,
                        ));
                    }
                }
                return;
            }

            if tunnel_status == TunnelStatus::Stopped {
                crate::tunnel::start_tunnel(self.config.tunnel_mode, &self.tunnel);
            }

            self.wireguard.wgconfig = Some(wgconfig);
            self.wireguard.waiting_for_tunnel = true;
        }

        if self.wireguard.waiting_for_tunnel && ui.button("Stop VPN").clicked() {
            self.wireguard.waiting_for_tunnel = false;
            crate::tunnel::stop_tunnel(&self.tunnel);
        }

        if self.wireguard.wgapi.is_some() && ui.button("Stop VPN").clicked() {
            crate::tunnel::stop_tunnel(&self.tunnel);

            let wireguard = self.wireguard.wgapi.take();
            if wireguard.is_none() {
                self.dialog_box = Some(GenericDialogBox::new(
                    "Error",
                    |ui| {
                        ui.label("VPN is not running.");
                    },
                    "Close",
                    None::<String>,
                    DialogAction::None,
                ));
            } else {
                match crate::tunnel::stop_wireguard(wireguard.unwrap()) {
                    Ok(_) => {
                        self.wireguard.wgapi = None;
                    }
                    Err(e) => {
                        let message = format!("Failed to stop Wireguard: {:#?}", e);
                        self.dialog_box = Some(GenericDialogBox::new(
                            "Error",
                            move |ui| {
                                ui.label(message.as_str());
                            },
                            "Close",
                            None::<String>,
                            DialogAction::None,
                        ));
                    }
                }
            }

            self.ifdata_error = false;
        }
    }

    fn show_tunnel_frame(&mut self, ui: &mut egui::Ui, tunnel_state: TunnelState) {
        let state_text = match &tunnel_state.status {
            TunnelStatus::DetectingMode | TunnelStatus::DetectingPort => "Starting".to_owned(),
            TunnelStatus::Failed(err) => {
                format!("Failed to Start {}", err)
            }
            TunnelStatus::Exited(code) => {
                format!("Exited {}", Opt(*code))
            }
            TunnelStatus::Running => "Running".to_owned(),
            TunnelStatus::Stopping => "Stopping".to_owned(),
            TunnelStatus::Stopped => "Stopped".to_owned(),
        };

        let mode_text = match tunnel_state.mode {
            TunnelMode::Auto => "Detecting",
            TunnelMode::UDP => "UDP",
            TunnelMode::TCP => "TCP",
        };

        let mut grid_rows: Vec<GridRow<'_>> = Vec::new();

        grid_rows.push(GridRow(
            Box::new(|ui| {
                ui.label("Tunnel State");
            }),
            Box::new(|ui| {
                ui.label(state_text.clone());
            }),
        ));

        if tunnel_state.status == TunnelStatus::Stopped {
            grid_rows.push(GridRow(
                Box::new(|ui| {
                    ui.label("Tunnel Mode");
                }),
                Box::new(|ui| {
                    egui::ComboBox::from_id_salt("tunnel_mode_select")
                        .selected_text(self.config.tunnel_mode.label())
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.config.tunnel_mode,
                                TunnelMode::Auto,
                                "Auto",
                            );
                            ui.selectable_value(
                                &mut self.config.tunnel_mode,
                                TunnelMode::TCP,
                                "TCP",
                            );
                            ui.selectable_value(
                                &mut self.config.tunnel_mode,
                                TunnelMode::UDP,
                                "UDP",
                            );
                        });
                }),
            ));

            grid_rows.push(GridRow(
                Box::new(|ui| {
                    ui.label("Tunnel Port");
                }),
                Box::new(|ui| {
                    ui.label("");
                }),
            ));
        } else {
            grid_rows.push(GridRow(
                Box::new(|ui| {
                    ui.label("Tunnel Mode");
                }),
                Box::new(|ui| {
                    ui.label(mode_text);
                }),
            ));

            if tunnel_state.port.is_some() {
                grid_rows.push(GridRow(
                    Box::new(|ui| {
                        ui.label("Tunnel Port");
                    }),
                    Box::new(|ui| {
                        ui.label(format!("{}", tunnel_state.port.unwrap()));
                    }),
                ));
            } else {
                grid_rows.push(GridRow(
                    Box::new(|ui| {
                        ui.label("Tunnel Port");
                    }),
                    Box::new(|ui| {
                        ui.label("Detecting");
                    }),
                ));
            }
        }

        render_egui_grid(ui, grid_rows, "tunnel_frame_grid");
        ui.add_space(8.0);
        if tunnel_state.status == TunnelStatus::Stopped {
            if ui.button("Start Tunnel").clicked() {
                crate::tunnel::start_tunnel(self.config.tunnel_mode, &self.tunnel);
            }
        } else {
            if ui.button("Stop Tunnel").clicked() {
                crate::tunnel::stop_tunnel(&self.tunnel);
            }
        }

        ui.add_space(12.0);
    }

    fn show_home_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("🏠 Home");
        ui.separator();

        if self.wireguard.selected_key.is_none() {
            self.wireguard.selected_key = self.config.keys.get(0).and_then(|k| k.id.clone());
        }

        let tunnel_state = {
            let guard = self.tunnel.state.lock().unwrap();
            guard.clone()
        };

        ui.allocate_ui(egui::vec2(ui.available_width(), 200.0), |ui| {
            StripBuilder::new(ui)
                .size(Size::remainder())
                .size(Size::remainder())
                .horizontal(|mut strip| {
                    strip.cell(|ui| {
                        egui::Frame::group(ui.style()).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(
                                egui::RichText::new("🌍 VPN")
                                    .text_style(egui::TextStyle::Name("subheading".into())),
                            );
                            ui.separator();
                            self.show_vpn_frame(ui);
                            ui.allocate_space(ui.available_size());
                        });
                    });

                    strip.cell(|ui| {
                        egui::Frame::group(ui.style()).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(
                                egui::RichText::new("🚀 Tunnel")
                                    .text_style(egui::TextStyle::Name("subheading".into())),
                            );
                            ui.separator();
                            self.show_tunnel_frame(ui, tunnel_state.clone());
                            ui.allocate_space(ui.available_size());
                        });
                    });
                });
        });
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.label(
                egui::RichText::new("📃 Tunnel Log")
                    .text_style(egui::TextStyle::Name("subheading".into())),
            );
            ui.separator();
            show_ansi_log(ui, &tunnel_state.log, 10.0 * self.config.font_zoom);
        });
    }

    fn show_key_info_dialog(&mut self, key: Key) {
        let rx_bytes = match key.rx_bytes {
            Some(bytes) => humanize_bytes(bytes),
            None => "".to_string(),
        };

        let tx_bytes = match key.tx_bytes {
            Some(bytes) => humanize_bytes(bytes),
            None => "".to_string(),
        };

        let end_date = match key.expiry {
            Some(date) => date.to_string(),
            None => "".to_string(),
        };

        let key_name = key.id.clone().unwrap_or_default();
        let subscription = key.get_subscription();
        let ip_address = key.ip.clone().unwrap_or_default();

        self.dialog_box = Some(GenericDialogBox::new(
            "Key Info",
            move |ui| {
                let grid_rows = vec![
                    ("Key Name", key_name.as_str()),
                    ("Subscription", subscription.as_str()),
                    ("End Date", end_date.as_str()),
                    ("IP Address", ip_address.as_str()),
                    ("Download Usage", rx_bytes.as_str()),
                    ("Upload Usage", tx_bytes.as_str()),
                ];

                render_egui_grid(ui, grid_rows, "key_info_grid");
            },
            "Close",
            None::<String>,
            DialogAction::None,
        ));
    }

    fn show_key_table_frame(&mut self, ui: &mut egui::Ui) {
        TableBuilder::new(ui)
            .striped(true)
            .column(Column::auto().at_least(60.0))
            .column(Column::auto().at_least(80.0))
            .column(Column::remainder())
            .column(Column::auto().at_least(60.0))
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
                    ui.label("        ");
                });
                header.col(|ui| {
                    ui.label("      ");
                });
            })
            .body(|mut body| {
                let keys = self.config.keys.clone().into_iter().enumerate();

                for (index, key) in keys {
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
                            if ui.button("Show Info").clicked() {
                                self.show_key_info_dialog(key.clone());
                            }
                        });
                        row.col(|ui| {
                            if ui.button("Show Key").clicked() {
                                let private_key = format!("{}", key.priv_key);
                                self.dialog_box = Some(GenericDialogBox::new(
                                    "Wireguard Key",
                                    move |ui| {
                                        ui.label(private_key.as_str());
                                    },
                                    "Close",
                                    None::<String>,
                                    DialogAction::None,
                                ));
                            }
                        });
                        row.col(|ui| {
                            if ui.button("Delete").clicked() {
                                self.pending_delete_key_index = Some(index);
                                let message = format!(
                                    "Are you sure you want to delete this config?\n {}",
                                    key.id.as_deref().unwrap()
                                );
                                self.dialog_box = Some(GenericDialogBox::new(
                                    "Delete config",
                                    move |ui| {
                                        ui.label(message.as_str());
                                    },
                                    "Cancel",
                                    Some("Delete"),
                                    DialogAction::ClearSpecificKey,
                                ));
                            }
                        });
                    });
                }
            });
    }

    fn show_keys_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("🔑 Keys");
        ui.separator();
        ui.add_space(8.0);

        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("📋 Key Table")
                        .text_style(egui::TextStyle::Name("subheading".into())),
                );
                ui.add_space(ui.available_width() - 30.0);
                if ui
                    .button(
                        egui::RichText::new("🔄")
                            .text_style(egui::TextStyle::Name("subheading".into())),
                    )
                    .clicked()
                {
                    self.refresh_keys_data();
                }
            });
            ui.separator();
            self.show_key_table_frame(ui);
        });
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            ui.menu_button("➕ Add Key", |ui| {
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
                                    self.config.keys.push(Key::new(key));
                                    self.dialog_box = Some(GenericDialogBox::new(
                                        "Key added",
                                        |ui| {
                                            ui.label(
                                                "The Wireguard key was imported successfully.",
                                            );
                                        },
                                        "OK",
                                        None::<String>,
                                        DialogAction::None,
                                    ));
                                }

                                self.refresh_keys_data();
                            }
                            Err(err) => {
                                let message = format!("{err}");
                                self.dialog_box = Some(GenericDialogBox::new(
                                    "Invalid config file",
                                    move |ui| {
                                        ui.label(message.as_str());
                                    },
                                    "Close",
                                    None::<String>,
                                    DialogAction::None,
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
            ui.add_space(ui.available_width() - 104.0);
            if ui.button("🗑 Clear all keys").clicked() {
                self.dialog_box = Some(GenericDialogBox::new(
                    "Clear saved keys",
                    |ui| {
                        ui.label("This will remove all saved keys from your settings.");
                    },
                    "Cancel",
                    Some("Clear"),
                    DialogAction::ClearAllKeys,
                ));
            }
        });
    }

    fn show_pathfinder_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("📌 Pathfinder");
        ui.separator();
        ui.add_space(8.0);
    }

    fn show_settings_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("⚙ Settings");
        ui.separator();
        ui.add_space(8.0);

        let is_dark = ui.style().visuals.dark_mode;
        let mut grid_rows: Vec<GridRow<'_>> = Vec::new();

        grid_rows.push(GridRow(
            Box::new(|ui| {
                ui.label("Theme");
            }),
            Box::new(|ui| {
                ui.horizontal(|ui| {
                    if ui.selectable_label(is_dark, "🌙 Dark").clicked() {
                        ui.set_visuals(egui::Visuals::dark());
                    }
                    if ui.selectable_label(!is_dark, "☀ Light").clicked() {
                        ui.set_visuals(egui::Visuals::light());
                    }
                });
            }),
        ));

        grid_rows.push(GridRow(
            Box::new(|ui| {
                ui.label("UI Zoom");
            }),
            Box::new(|ui| {
                ui.horizontal(|ui| {
                    if ui.button("➖").clicked() {
                        self.config.ui_zoom -= 0.05;
                        self.config.ui_zoom = self.config.ui_zoom.clamp(0.5, 3.0);
                    }
                    ui.label(format!("{:.2}x", self.config.ui_zoom));
                    if ui.button("➕").clicked() {
                        self.config.ui_zoom += 0.05;
                        self.config.ui_zoom = self.config.ui_zoom.clamp(0.5, 3.0);
                    }
                    if ui.button("🔄").clicked() {
                        self.config.ui_zoom = 1.3;
                    }
                });
            }),
        ));

        grid_rows.push(GridRow(
            Box::new(|ui| {
                ui.label("Font Zoom");
            }),
            Box::new(|ui| {
                ui.horizontal(|ui| {
                    if ui.button("➖").clicked() {
                        self.config.font_zoom -= 0.025;
                        self.config.font_zoom = self.config.font_zoom.clamp(0.7, 1.3);
                    }
                    ui.label(format!("{:.2}x", self.config.font_zoom));
                    if ui.button("➕").clicked() {
                        self.config.font_zoom += 0.025;
                        self.config.font_zoom = self.config.font_zoom.clamp(0.7, 1.3);
                    }
                    if ui.button("🔄").clicked() {
                        self.config.font_zoom = 1.05;
                    }
                });
            }),
        ));

        grid_rows.push(GridRow(
            Box::new(|ui| {
                ui.label("Close Action");
            }),
            Box::new(|ui| {
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("close_action_select")
                        .selected_text(self.config.close_action.label())
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.config.close_action,
                                CloseAction::Ask,
                                "Ask every time",
                            );
                            ui.selectable_value(
                                &mut self.config.close_action,
                                CloseAction::MinimizeToTray,
                                "Minimize to Tray",
                            );
                            ui.selectable_value(
                                &mut self.config.close_action,
                                CloseAction::Exit,
                                "Exit",
                            );
                        });
                });
            }),
        ));

        render_egui_grid(ui, grid_rows, "settings_grid");
    }

    fn show_about_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("ℹ About");
        ui.separator();

        if self.show_licenses {
            egui::ScrollArea::vertical()
                .max_height(ui.available_height() - 40.0)
                .show(ui, |ui| {
                    let old_style = ui.style().clone();

                    let mut style = (*old_style).clone();
                    for font_id in style.text_styles.values_mut() {
                        font_id.size *= 0.8;
                    }

                    ui.set_style(style);
                    CommonMarkViewer::new().show(ui, &mut self.md_cache, LICENSES_MD);
                    ui.set_style(old_style);
                });

            ui.separator();
            if ui.button("Close").clicked() {
                self.show_licenses = false;
            }
        } else {
            ui.add_space(8.0);
            ui.label("This app was built with ❤ in Rust");
            ui.label("Using the eframe/egui GUI framework");
            ui.hyperlink_to("egui on GitHub", "https://github.com/emilk/egui");
            ui.add_space(8.0);

            if ui.button("View Licenses").clicked() {
                self.show_licenses = true
            }
        }
    }

    fn show_close_app_dialog(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx();
        let mut open = self.close_app_dialog_open;
        let mut should_close = false;

        egui::Window::new("Close App")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label("What would you like to do?");
                ui.add_space(8.0);
                ui.checkbox(&mut self.remember_close_action, "Remember my choice");
                ui.add_space(10.0);

                ui.horizontal(|ui| {
                    if ui.button("Minimize to Tray").clicked() {
                        if self.remember_close_action {
                            self.config.close_action = CloseAction::MinimizeToTray;
                            self.remember_close_action = false;
                        }
                        should_close = true;
                        self.handle_exit(ui, Some(CloseAction::MinimizeToTray));
                    }

                    if ui.button("Exit").clicked() {
                        if self.remember_close_action {
                            self.config.close_action = CloseAction::Exit;
                            self.remember_close_action = false;
                        }
                        should_close = true;
                        self.handle_exit(ui, Some(CloseAction::Exit));
                    }
                });
            });

        if should_close {
            open = false;
        }

        self.close_app_dialog_open = open;
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
                            self.config.keys.push(Key::new(final_input));
                        }
                        self.wgkey_dialog_show = false;
                        self.wgkey_dialog_input.clear();
                        self.refresh_keys_data();
                    }
                })
            });
    }

    #[cfg(target_os = "linux")]
    fn check_privilege(&mut self) {
        let has_cap = match crate::utils_nix::check_cap_net_admin() {
            Ok(value) => value,
            Err(e) => {
                let message = format!(
                    "Failed to check CAP_NET_ADMIN: {:#?}\n If this app doesn't have CAP_NET_ADMIN, you will not be able to start the VPN.",
                    e
                );
                self.dialog_box = Some(GenericDialogBox::new(
                    "Privilege Error",
                    move |ui| {
                        ui.label(message.as_str());
                    },
                    "Close",
                    None::<String>,
                    DialogAction::None,
                ));
                self.can_run_wireguard = false;
                return;
            }
        };

        if !has_cap {
            if !which::which("pkexec").is_ok() {
                self.dialog_box = Some(GenericDialogBox::new(
                    "Privilege Error",
                    |ui| {
                        ui.label("This app requires polkit (pkexec) to start the VPN. Please install polkit and try again.");
                    },
                    "Close",
                    None::<String>,
                    DialogAction::None,
                ));
                self.can_run_wireguard = false;
                return;
            }
            let path = std::env::current_exe().unwrap();
            let arg = "--selfcap";

            if let Err(err) = crate::utils_nix::run_with_pkexec(path.clone(), arg) {
                let message = format!("Failed to grant CAP_NET_ADMIN:\n {:#?}", err);
                self.dialog_box = Some(GenericDialogBox::new(
                    "Privilege Error",
                    move |ui| {
                        ui.label(message.as_str());
                    },
                    "Close",
                    None::<String>,
                    DialogAction::None,
                ));
                self.can_run_wireguard = false;
                return;
            }

            crate::utils_nix::spawn_detached_process(path).unwrap();
            std::process::exit(0);
        }
    }
}

#[derive(PartialEq, Clone)]
enum Page {
    Home,
    Keys,
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
                let message = format!("Failed to create AppData directory:\n{}", err);
                self.dialog_box = Some(GenericDialogBox::new(
                    "Fatal Error",
                    move |ui| {
                        ui.label(message.as_str());
                    },
                    "Exit",
                    None::<String>,
                    DialogAction::Exit,
                ));
            }

            let mut dialog_event = None;
            if let Some(dialog) = self.dialog_box.as_mut() {
                if let Some(reply) = dialog.show(ui.ctx()) {
                    dialog_event = Some((dialog.action, reply));
                }
            }

            if let Some((action, reply)) = dialog_event {
                self.dialog_box = None;
                self.handle_dialog_reply(ui, action, reply);
            }
            return;
        }

        if let Some(dialog) = &mut self.update_dialog {
            if dialog.show(ui.ctx()) {
                self.update_dialog = None;
            }
            return;
        }

        #[cfg(target_os = "linux")]
        if !self.privilege_checked {
            self.check_privilege();
            self.privilege_checked = true;
        }

        let config_before_frame = self.config.clone();
        let previous_page = self.current_page.clone();

        self.handle_zoom_shortcut(ui);
        update_zoom(
            self.config.ui_zoom,
            self.config.font_zoom,
            self.base_style.clone(),
            ui.ctx(),
        ); // TODO: Look into optimizing this. Performance ovedhead is minimal but the implementation is far from neat.

        if self.wireguard.waiting_for_tunnel {
            let tunnel_state = self.tunnel.state.lock().unwrap();

            if tunnel_state.status == TunnelStatus::Running {
                let mut wgconfig = self.wireguard.wgconfig.take().unwrap();
                wgconfig.endpoint_port = tunnel_state.port.unwrap();

                match crate::tunnel::start_wireguard(wgconfig) {
                    Ok(wgapi) => {
                        self.wireguard.wgapi = Some(wgapi);
                    }
                    Err(e) => {
                        let message = format!("Failed to start Wireguard: {:#?}", e);
                        self.dialog_box = Some(GenericDialogBox::new(
                            "Error",
                            move |ui| {
                                ui.label(message.as_str());
                            },
                            "Close",
                            None::<String>,
                            DialogAction::None,
                        ));
                        crate::tunnel::stop_tunnel(&self.tunnel);
                    }
                }
                self.wireguard.waiting_for_tunnel = false;
            }
        }

        if self.close_app_dialog_open {
            self.show_close_app_dialog(ui);
        }

        if ui.ctx().input(|i| i.viewport().close_requested()) && !self.allow_exit {
            self.handle_exit(ui, None);
        }

        #[cfg(target_os = "linux")]
        glib::MainContext::default().iteration(false);

        if let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == self.tray_object.as_ref().unwrap().showhide.id() {
                if self.window_visible {
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::Visible(false));
                    self.window_visible = false;
                } else {
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    self.window_visible = true;
                }
            }

            if event.id == self.tray_object.as_ref().unwrap().quit.id() {
                if !self.window_visible {
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::Visible(true));
                }
                self.window_visible = true;

                self.handle_exit(ui, None);
            }
        }

        egui::Panel::left("sidebar")
            .resizable(false)
            .exact_size(160.0)
            .show(ui, |ui| {
                ui.add_space(8.0);
                ui.vertical_centered(|ui| {
                    ui.heading("📋 Menu");
                });
                ui.separator();
                ui.add_space(6.0);

                let items = [
                    (Page::Home, "🏠 Home"),
                    (Page::Keys, "🔑 Keys"),
                    (Page::Pathfinder, "📌 Pathfinder"),
                    (Page::Settings, "⚙ Settings"),
                    (Page::About, "ℹ About"),
                ];

                ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| {
                    for (page, label) in items {
                        let text = egui::RichText::new(label)
                            .text_style(egui::TextStyle::Name("subheading".into()));

                        if ui
                            .add_sized(
                                [ui.available_width(), 26.0],
                                egui::Button::selectable(self.current_page == page, text),
                            )
                            .clicked()
                        {
                            self.current_page = page;
                        }
                    }
                });
            });

        if matches!(self.current_page, Page::Keys | Page::Home)
            && self.current_page != previous_page
        {
            self.refresh_keys_data();
        }

        egui::CentralPanel::default().show(ui, |ui| match &self.current_page {
            Page::Home => self.show_home_page(ui),
            Page::Keys => self.show_keys_page(ui),
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
            self.handle_dialog_reply(ui, action, reply);
        }

        if let Some(err) = self.load_config_error.take() {
            let message = format!("Failed to load mbtunnel settings file: {err}");
            self.dialog_box = Some(GenericDialogBox::new(
                "Error loading settings",
                move |ui| {
                    ui.label(message.as_str());
                },
                "Continue",
                None::<String>,
                DialogAction::None,
            ));
        }

        if self.config != config_before_frame {
            if let Err(err) = save_config(&self.config, appdata_path().join("config.dat")) {
                if !self.ignore_save_error {
                    let message = format!("Failed to save config: {err}");
                    self.dialog_box = Some(GenericDialogBox::new(
                        "Error saving config",
                        move |ui| {
                            ui.label(message.as_str());
                        },
                        "Don't show again",
                        Some("Continue"),
                        DialogAction::IgnoreSaveError,
                    ));
                }
            }
        }

        ui.ctx().request_repaint_after(Duration::from_millis(100));
    }

    fn on_exit(&mut self) {
        if self.wireguard.wgapi.is_some() {
            let wireguard = self.wireguard.wgapi.take().unwrap();
            let _ = crate::tunnel::stop_wireguard(wireguard);
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

    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
        AttachConsole(ATTACH_PARENT_PROCESS);
        use std::fs::OpenOptions;

        let _ = OpenOptions::new().write(true).open("CONOUT$");
    }

    #[cfg(target_os = "linux")]
    crate::utils_nix::handle_selfcap().unwrap();

    #[cfg(target_os = "linux")]
    if std::env::var_os("DISPLAY").is_some() {
        unsafe {
            std::env::remove_var("WAYLAND_DISPLAY");
        }
    }

    handle_update_state();

    let _guard = CleanupGuard;
    setup_cleanup_guard();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_resizable(true)
            .with_title("mbtunnel"),
        ..Default::default()
    };

    eframe::run_native(
        "mbtunnel",
        native_options,
        Box::new(|cc| Ok(Box::new(MyApp::new(cc)))),
    )
}
