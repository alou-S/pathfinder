use std::{env, process::Command};


#[cfg(windows)]
pub fn request_elevation() {
    let is_admin = Command::new("net")
        .args(["session"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !is_admin {
        let exe = env::current_exe().unwrap();
        let args: Vec<String> = env::args().skip(1).collect();

        Command::new("powershell")
            .args([
                "-Command",
                &format!(
                    "Start-Process -FilePath '{}' -ArgumentList @({}) -Verb RunAs",
                    exe.display(),
                    args.iter()
                        .map(|a| format!("'{}'", a.replace('\'', "''")))
                        .collect::<Vec<_>>()
                        .join(",")
                ),
            ])
            .spawn()
            .expect("Failed to request elevation");

        std::process::exit(0);
    }
}
