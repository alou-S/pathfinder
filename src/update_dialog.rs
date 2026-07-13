use crate::appdata_path;
use crate::config::Binary::Mbtunnel;
use crate::config::{Binary, PKGREL, SERVER_HOSTNAME};
use crate::generic_dialog_box::{DialogAction, DialogReply, GenericDialogBox};
use eframe::egui;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::{env, fs, vec};

fn sha256sum(file_path: &Path, checksum: &str) -> bool {
    let Ok(data) = fs::read(file_path) else {
        return false;
    };

    let digest = Sha256::digest(&data);
    hex::encode(digest) == checksum
}

fn decompress_zstd<P: AsRef<Path>>(input_path: P, output_path: P) -> std::io::Result<()> {
    let result = (|| -> std::io::Result<()> {
        let input_file = fs::File::open(&input_path)?;
        let mut output_file = fs::File::create(output_path)?;

        let mut decoder = zstd::Decoder::new(input_file)?;
        std::io::copy(&mut decoder, &mut output_file)?;

        Ok(())
    })();

    let _ = fs::remove_file(input_path);

    return result;
}

#[derive(PartialEq)]
#[allow(dead_code)]
pub enum OS {
    Darwin,
    Linux,
    Windows,
}

#[derive(PartialEq)]
#[allow(dead_code)]
pub enum ARCH {
    Amd64,
    Arm64,
}

fn get_arch() -> ARCH {
    #[cfg(target_arch = "x86_64")]
    return ARCH::Amd64;
    #[cfg(target_arch = "aarch64")]
    return ARCH::Arm64;
}

fn get_arch_string(arch: ARCH) -> String {
    match arch {
        ARCH::Amd64 => "amd64".into(),
        ARCH::Arm64 => "arm64".into(),
    }
}

fn get_os() -> OS {
    #[cfg(target_os = "macos")]
    return OS::Darwin;
    #[cfg(target_os = "linux")]
    return OS::Linux;
    #[cfg(target_os = "windows")]
    return OS::Windows;
}

fn get_os_string(os: OS) -> String {
    match os {
        OS::Darwin => "darwin".into(),
        OS::Linux => "linux".into(),
        OS::Windows => "windows".into(),
    }
}

fn binary_url(binary: &Binary) -> String {
    format!(
        "https://{host}/bin/{name}/{name}-{os}-{arch}.zst",
        host = SERVER_HOSTNAME,
        name = binary.name(),
        os = get_os_string(get_os()),
        arch = get_arch_string(get_arch()),
    )
}

pub fn binary_path(binary: &Binary) -> PathBuf {
    if binary == &Binary::Mbtunnel {
        let mut exec_path = env::current_exe().unwrap();

        #[cfg(target_os = "windows")]
        {
            let stem = exec_path.file_stem().unwrap().to_string_lossy();
            let new_file_name = format!("{}.updatefile.exe", stem);
            exec_path.set_file_name(new_file_name);
            return exec_path;
        }

        #[cfg(not(target_os = "windows"))]
        {
            let exec_name = exec_path.file_name().unwrap().to_string_lossy();
            let new_file_name = format!("{}.updatefile", exec_name);
            exec_path.set_file_name(new_file_name);
            return exec_path;
        }
    }

    #[cfg(target_os = "windows")]
    if binary == &Binary::WireguardDll {
        return appdata_path().join(format!("{}.dll", binary.name()));
    } else {
        return appdata_path().join(format!("{}.exe", binary.name()));
    }

    #[cfg(not(target_os = "windows"))]
    return appdata_path().join(format!("{}", binary.name()));
}

enum DownloadMsg {
    Checking {
        index: usize,
        name: String,
    },
    Progress {
        binary_name: String,
        fraction: f32,
        downloaded: u64,
        total_bytes: u64,
        speed: f64,
    },
    Finished {
        had_updates: bool,
        updates_failed: bool,
    },
    ErrorRetryable {
        error_title: String,
        error_message: String,
    },
    ErrorNonRetryable {
        error_title: String,
        error_message: String,
    },
}

enum DialogState {
    Checking {
        current_name: String,
        current_index: usize,
        total: usize,
    },
    Downloading {
        current_name: String,
        fraction: f32,
        downloaded: u64,
        total_bytes: u64,
        speed: f64,
        done_count: usize,
        total: usize,
    },
    Done {
        had_updates: bool,
        updates_failed: bool,
    },
}

pub enum WorkerAction {
    Continue,
    Retry,
}

pub struct UpdateDialog {
    state: DialogState,
    rx: Receiver<DownloadMsg>,
    tx_worker: Sender<WorkerAction>,
    is_complete: bool,
    dialog_box: Option<GenericDialogBox>,
}

impl UpdateDialog {
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = mpsc::channel();
        let (tx_worker, rx_action) = mpsc::channel();
        let binaries_to_check = vec![
            Binary::Udpproxy,
            Binary::Wstunnel,
            Binary::Librespeed,
            Binary::WireguardDll,
        ];

        let total = binaries_to_check.len();

        std::thread::spawn(move || {
            worker(binaries_to_check, tx, rx_action, ctx);
        });

        Self {
            state: DialogState::Checking {
                current_name: String::new(),
                current_index: 0,
                total,
            },
            rx,
            tx_worker,
            is_complete: false,
            dialog_box: None,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> bool {
        let mut dialog_event = None;
        if let Some(dialog) = self.dialog_box.as_mut() {
            if let Some(reply) = dialog.show(ctx) {
                dialog_event = Some((dialog.action, reply));
            }
        }

        if let Some((action, reply)) = dialog_event {
            self.dialog_box = None;
            self.handle_dialog_reply(action, reply);
        }

        while let Ok(msg) = self.rx.try_recv() {
            self.handle_message(msg);
        }

        let mut dismiss = false;

        egui::Window::new("Updates")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .min_width(400.0)
            .show(ctx, |ui| {
                dismiss = self.draw_contents(ui);
            });

        dismiss
    }

    fn handle_message(&mut self, msg: DownloadMsg) {
        match msg {
            DownloadMsg::Checking { index, name } => {
                // Preserve the total we already have.
                let total = match &self.state {
                    DialogState::Checking { total, .. } => *total,
                    _ => 0,
                };
                self.state = DialogState::Checking {
                    current_name: name,
                    current_index: index,
                    total,
                };
            }

            DownloadMsg::Progress {
                binary_name,
                fraction,
                downloaded,
                total_bytes,
                speed,
            } => {
                let (done_count, total) = match &self.state {
                    DialogState::Checking { total, .. } => (0, *total),
                    DialogState::Downloading {
                        done_count, total, ..
                    } => (*done_count, *total),
                    _ => (0, 0),
                };

                let done_count = if fraction >= 1.0 {
                    done_count + 1
                } else {
                    done_count
                };

                self.state = DialogState::Downloading {
                    current_name: binary_name,
                    fraction: fraction.min(1.0),
                    downloaded,
                    total_bytes,
                    speed,
                    done_count,
                    total,
                };
            }

            DownloadMsg::Finished {
                had_updates,
                updates_failed,
            } => {
                self.state = DialogState::Done {
                    had_updates,
                    updates_failed,
                };
                self.is_complete = true;
            }
            DownloadMsg::ErrorRetryable {
                error_title,
                error_message,
            } => {
                let message = error_message;
                self.dialog_box = Some(GenericDialogBox::new(
                    error_title,
                    move |ui| {
                        ui.label(message.as_str());
                    },
                    "Continue",
                    Some("Retry"),
                    DialogAction::RetryUpdate,
                ));
            }
            DownloadMsg::ErrorNonRetryable {
                error_title,
                error_message,
            } => {
                let message = error_message;
                self.dialog_box = Some(GenericDialogBox::new(
                    error_title,
                    move |ui| {
                        ui.label(message.as_str());
                    },
                    "Continue",
                    None::<String>,
                    DialogAction::RetryUpdate,
                ));
            }
        }
    }

    fn handle_dialog_reply(&mut self, action: DialogAction, reply: DialogReply) {
        match (action, reply) {
            (DialogAction::RetryUpdate, DialogReply::Primary) => {
                let _ = self.tx_worker.send(WorkerAction::Continue);
            }

            (DialogAction::RetryUpdate, DialogReply::Secondary) => {
                let _ = self.tx_worker.send(WorkerAction::Retry);
            }

            _ => {}
        }
    }

    fn draw_contents(&self, ui: &mut egui::Ui) -> bool {
        match &self.state {
            DialogState::Checking {
                current_name,
                current_index,
                total,
            } => {
                ui.heading("Checking for updates…");
                ui.add_space(8.0);

                let label = if current_name.is_empty() {
                    "Starting…".to_string()
                } else {
                    format!("{} ({}/{})", current_name, current_index + 1, total)
                };
                ui.label(&label);

                ui.add(
                    egui::ProgressBar::new(0.0)
                        .animate(true)
                        .desired_width(ui.available_width()),
                );

                false
            }

            DialogState::Downloading {
                current_name,
                fraction,
                downloaded,
                total_bytes,
                speed,
                done_count,
                total,
            } => {
                ui.heading("Downloading updates…");
                ui.add_space(8.0);

                let speed_str = if *speed > 1_048_576.0 {
                    format!("{:.2} MB/s", speed / 1_048_576.0)
                } else {
                    format!("{:.2} KB/s", speed / 1024.0)
                };

                egui::Sides::new().show(
                    ui,
                    |ui| {
                        ui.label(format!("{} ({}/{})", current_name, done_count + 1, total));
                    },
                    |ui| {
                        ui.label(format!("{}", speed_str));
                    },
                );

                let dl_mb = *downloaded as f64 / 1_048_576.0;
                let tot_mb = *total_bytes as f64 / 1_048_576.0;

                egui::Sides::new().show(
                    ui,
                    |ui| {
                        ui.add_sized(
                            [ui.available_width() - 100.0, ui.available_height()],
                            egui::ProgressBar::new(*fraction)
                                .show_percentage()
                                .animate(*fraction < 1.0),
                        );
                    },
                    |ui| {
                        ui.label(format!("{:.1}/{:.1} MB", dl_mb, tot_mb));
                    },
                );

                false
            }

            DialogState::Done {
                had_updates,
                updates_failed,
            } => {
                if *updates_failed {
                    ui.heading("⚠ Updates completed with errors");
                    ui.label("Some binaries may have failed to update.");
                } else if *had_updates {
                    ui.heading("✅ Updates complete");
                    ui.label("All binaries have been updated successfully.");
                } else {
                    ui.heading("✅ Up to date");
                    ui.label("All binaries are already at the latest version.");
                }
                ui.add_space(8.0);
                ui.button("Continue").clicked()
            }
        }
    }
}

fn update_mbtunnel(
    tx: &Sender<DownloadMsg>,
    rx_action: &Receiver<WorkerAction>,
    ctx: &egui::Context,
    updates_failed: &mut bool,
) {
    loop {
        let _ = tx.send(DownloadMsg::Checking {
            index: 1,
            name: Mbtunnel.name().to_string(),
        });
        ctx.request_repaint();

        match reqwest::blocking::get(&format!("https://{}/pkgrel/pc-gui", SERVER_HOSTNAME))
            .and_then(|response| response.text())
        {
            Ok(text) => {
                if text == PKGREL {
                    break;
                }
            }
            Err(e) => {
                let _ = tx.send(DownloadMsg::ErrorRetryable {
                    error_title: format!("Unable to reach VPN server"),
                    error_message: format!("{}", e),
                });
                ctx.request_repaint();

                match rx_action.recv() {
                    Ok(WorkerAction::Retry) => continue,
                    Ok(WorkerAction::Continue) => {
                        *updates_failed = true;
                        continue;
                    }
                    Err(_) => break,
                }
            }
        }

        match download_with_progress(&Mbtunnel, 1, tx, ctx) {
            Err(e) => {
                let _ = tx.send(DownloadMsg::ErrorRetryable {
                    error_title: format!("Error downloading binary"),
                    error_message: format!("{}: {}", &Mbtunnel.name(), e),
                });
                ctx.request_repaint();

                match rx_action.recv() {
                    Ok(WorkerAction::Retry) => continue,
                    Ok(WorkerAction::Continue) => {
                        *updates_failed = true;
                        continue;
                    }
                    Err(_) => break,
                }
            }

            Ok(_) => {
                if let Err(e) = decompress_zstd(
                    binary_path(&Mbtunnel).with_added_extension("zst"),
                    binary_path(&Mbtunnel),
                ) {
                    let _ = tx.send(DownloadMsg::ErrorNonRetryable {
                        error_title: format!("Error decompressing binary"),
                        error_message: format!("{}: {}", &Mbtunnel.name(), e),
                    });
                    ctx.request_repaint();

                    let _ = rx_action.recv();
                    *updates_failed = true;
                    break;
                }

                let path = binary_path(&Mbtunnel);

                #[cfg(not(target_os = "windows"))]
                {
                    if let Err(e) = crate::utils_nix::set_executable_bit(&path) {
                        let _ = tx.send(DownloadMsg::ErrorNonRetryable {
                            error_title: format!("Error setting binary as executable"),
                            error_message: format!("{}: {}", &Mbtunnel.name(), e),
                        });
                        ctx.request_repaint();

                        let _ = rx_action.recv();
                        *updates_failed = true;
                        break;
                    }
                }

                #[cfg(target_os = "windows")]
                let spawn_result = crate::utils_win::spawn_detached_process(path);

                #[cfg(not(target_os = "windows"))]
                let spawn_result = crate::utils_nix::spawn_detached_process(path);

                if let Err(e) = spawn_result {
                    let _ = tx.send(DownloadMsg::ErrorNonRetryable {
                        error_title: format!("Unable to start executable"),
                        error_message: format!("{}: {}", Mbtunnel.name(), e),
                    });
                    ctx.request_repaint();

                    let _ = rx_action.recv();
                    *updates_failed = true;
                    break;
                } else {
                    std::process::exit(0)
                }
            }
        }
    }
}

fn worker(
    binaries: Vec<Binary>,
    tx: Sender<DownloadMsg>,
    rx_action: Receiver<WorkerAction>,
    ctx: egui::Context,
) {
    let mut updates_needed: Vec<Binary> = Vec::new();
    let mut updates_failed = false;

    update_mbtunnel(&tx, &rx_action, &ctx, &mut updates_failed);

    let _ = tx.send(DownloadMsg::Finished {
        had_updates: true,
        updates_failed: true,
    });
    ctx.request_repaint();

    for (i, binary) in binaries.into_iter().enumerate() {
        if binary == Binary::WireguardDll && get_os() != OS::Windows {
            continue;
        }

        let _ = tx.send(DownloadMsg::Checking {
            index: i,
            name: binary.name().to_string(),
        });
        ctx.request_repaint();

        if !sha256sum(
            &binary_path(&binary),
            &binary.checksum(get_os(), get_arch()),
        ) {
            updates_needed.push(binary);
        }
    }

    if updates_needed.is_empty() {
        let _ = tx.send(DownloadMsg::Finished {
            had_updates: false,
            updates_failed: false,
        });
        ctx.request_repaint();
        return;
    }

    ctx.request_repaint();

    let total_downloads = updates_needed.len();

    'outer: for binary in &updates_needed {
        loop {
            match download_with_progress(binary, total_downloads, &tx, &ctx) {
                Err(e) => {
                    let _ = tx.send(DownloadMsg::ErrorRetryable {
                        error_title: format!("Error downloading binary"),
                        error_message: format!("{}: {}", binary.name(), e),
                    });
                    ctx.request_repaint();

                    match rx_action.recv() {
                        Ok(WorkerAction::Retry) => continue,
                        Ok(WorkerAction::Continue) => {
                            updates_failed = true;
                            continue 'outer;
                        }
                        Err(_) => break 'outer,
                    }
                }

                Ok(_) => {
                    if let Err(e) = decompress_zstd(
                        &binary_path(&binary).with_added_extension("zst"),
                        &binary_path(&binary),
                    ) {
                        let _ = tx.send(DownloadMsg::ErrorNonRetryable {
                            error_title: format!("Error decompressing binary"),
                            error_message: format!("{}: {}", binary.name(), e),
                        });
                        ctx.request_repaint();

                        let _ = rx_action.recv();
                        updates_failed = true;
                        continue 'outer;
                    }

                    #[cfg(not(target_os = "windows"))]
                    if let Err(e) = crate::utils_nix::set_executable_bit(&binary_path(&binary)) {
                        let _ = tx.send(DownloadMsg::ErrorNonRetryable {
                            error_title: format!("Error setting binary as executable"),
                            error_message: format!("{}: {}", &binary.name(), e),
                        });
                        break;
                    }
                    break;
                }
            }
        }
    }

    let _ = tx.send(DownloadMsg::Finished {
        had_updates: true,
        updates_failed,
    });
    ctx.request_repaint();
}

fn download_with_progress(
    binary: &Binary,
    _total: usize,
    tx: &Sender<DownloadMsg>,
    ctx: &egui::Context,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::{Read, Write};
    use std::time::Instant;

    let binary_url: String = binary_url(binary);

    let response = reqwest::blocking::get(&binary_url)?;

    let total_bytes = response.content_length().unwrap_or(0);

    let mut downloaded: u64 = 0;
    let mut reader = response;

    let dest = fs::File::create(&binary_path(&binary).with_added_extension("zst"))?;
    let mut writer = std::io::BufWriter::new(dest);

    let mut buf = vec![0u8; 65_536];

    let mut last_report_time = Instant::now();
    let mut last_report_downloaded = 0u64;
    let mut speed: f64 = 0.0;

    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n])?;
        downloaded += n as u64;

        let fraction = if total_bytes > 0 {
            downloaded as f32 / total_bytes as f32
        } else {
            0.0
        };

        let now = Instant::now();
        let elapsed = now.duration_since(last_report_time).as_secs_f64();
        if elapsed >= 0.25 || n == 0 {
            speed = (downloaded - last_report_downloaded) as f64 / elapsed;
            last_report_time = now;
            last_report_downloaded = downloaded;
        }

        let _ = tx.send(DownloadMsg::Progress {
            binary_name: binary.name().to_string(),
            fraction,
            downloaded,
            total_bytes,
            speed,
        });

        ctx.request_repaint();
    }

    Ok(())
}
