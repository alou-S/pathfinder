use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

pub fn set_executable_bit(path: &Path) -> std::io::Result<()> {
    let mut perms = fs::metadata(path)?.permissions();
    let mode = perms.mode();
    perms.set_mode(mode | 0o111);
    fs::set_permissions(path, perms)?;
    Ok(())
}

pub fn spawn_detached_process(path: PathBuf) -> std::io::Result<Child> {
    use std::os::unix::process::CommandExt;

    let mut cmd = Command::new(path);

    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }

    cmd.spawn()
}
