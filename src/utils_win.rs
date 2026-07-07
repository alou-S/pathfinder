use std::{
    env,
    os::windows::process::CommandExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
};

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

pub fn spawn_detached_process(path: PathBuf) -> std::io::Result<Child> {
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

    Command::new(path)
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}
