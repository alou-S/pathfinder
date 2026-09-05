use std::{
    env, mem,
    os::windows::ffi::OsStrExt,
    os::windows::process::CommandExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    ptr::null_mut,
};

use windows::{
    Win32::Foundation::{CloseHandle, HANDLE, HWND},
    Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
    Win32::System::Threading::{GetCurrentProcess, OpenProcessToken},
    Win32::UI::Shell::ShellExecuteW,
    Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
    core::PCWSTR,
};

pub fn is_elevated() -> bool {
    unsafe {
        let mut token: HANDLE = HANDLE(null_mut());
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }

        let mut elevation: TOKEN_ELEVATION = mem::zeroed();
        let mut ret_size: u32 = 0;

        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut _),
            mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret_size,
        );

        CloseHandle(token).ok();

        match ok {
            Ok(_) => elevation.TokenIsElevated != 0,
            Err(_) => false,
        }
    }
}

pub fn request_elevation() -> std::io::Result<()> {
    let current_exe = env::current_exe().unwrap();
    let exe_wide: Vec<u16> = current_exe
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let args: Vec<String> = env::args().skip(1).collect();
    let args_joined = args.join(" ");
    let args_wide: Vec<u16> = args_joined
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        let result = ShellExecuteW(
            Some(HWND(null_mut())),
            PCWSTR::from_raw(windows::core::w!("runas").as_ptr()),
            PCWSTR::from_raw(exe_wide.as_ptr()),
            PCWSTR::from_raw(args_wide.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );

        if (result.0 as isize) <= 32 {
            eprintln!("Elevation failed or was cancelled by the user.");
            std::process::exit(1);
        }
    }

    std::process::exit(0);
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
