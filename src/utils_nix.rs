use caps::{CapSet, Capability};
use libc::{c_char, c_int, c_void};
use std::{
    env,
    ffi::CString,
    fs, io,
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

pub fn check_cap_net_admin() -> anyhow::Result<bool> {
    let effective = caps::read(None, CapSet::Effective)?;

    Ok(effective.contains(&Capability::CAP_NET_ADMIN))
}

type CapT = *mut c_void;

unsafe extern "C" {
    fn cap_from_text(text: *const c_char) -> CapT;
    fn cap_set_file(path: *const c_char, cap: CapT) -> c_int;
    fn cap_free(cap: *mut c_void) -> c_int;
}

pub fn handle_selfcap() -> std::io::Result<()> {
    if !std::env::args().any(|arg| arg == "--selfcap") {
        return Ok(());
    }

    let path = env::current_exe()?;
    let path = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();

    let caps = CString::new("cap_net_admin=eip").unwrap();

    let cap = unsafe { cap_from_text(caps.as_ptr()) };
    if cap.is_null() {
        return Err(io::Error::last_os_error());
    }

    let rc = unsafe { cap_set_file(path.as_ptr(), cap) };
    let free_rc = unsafe { cap_free(cap) };

    if free_rc != 0 {
        return Err(io::Error::last_os_error());
    }

    if rc != 0 {
        return Err(io::Error::last_os_error());
    }

    std::process::exit(0);
}

pub fn run_with_pkexec(path: PathBuf, arg: &str) -> std::io::Result<()> {
    let output = Command::new("pkexec").arg(path).arg(arg).output()?;

    if output.status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "{}",
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}
