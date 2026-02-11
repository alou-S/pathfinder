use indicatif::{ProgressBar, ProgressStyle};
use reqwest::Client;
use futures_util::StreamExt;
use std::cmp::min;
use std::env::{current_exe, var};

use std::process::{exit, Command};
use std::fs::{self, File};
use std::io::{self, Write}; 
use std::path::Path;

use std::time::Duration;
use sha2::{Sha256, Digest};
use tokio::time::timeout;
use windows_sys::Win32::System::Console::{
    GetConsoleMode, SetConsoleMode, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    GetStdHandle, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use colored::*;
use dont_disappear::any_key_to_continue;

use crate::config::{SERVER_HOSTNAME, PKGREL, BINARY_CHECKSUMS};

async fn download_file(client: &Client, url: &str, path: &str) -> Result<(), String> {
    // Reqwest setup
    let res = client
        .get(url)
        .send()
        .await
        .or(Err(format!("Failed to GET from '{}'", &url)))?;
    let total_size = res
        .content_length()
        .ok_or(format!("Failed to get content length from '{}'", &url))?;

    // Indicatif setup
    let pb = ProgressBar::new(total_size);
    pb.set_style(ProgressStyle::default_bar()
        .template("{msg}\n{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})")
        .progress_chars("#>-"));
    pb.set_message(&format!("Downloading {}", url));

    // download chunks
    let mut file = File::create(path).or(Err(format!("Failed to create file '{}'", path)))?;
    let mut downloaded: u64 = 0;
    let mut stream = res.bytes_stream();

    while let Some(item) = stream.next().await {
        let chunk = item.or(Err(format!("Error while downloading file")))?;
        file.write_all(&chunk)
            .or(Err(format!("Error while writing to file")))?;
        let new = min(downloaded + (chunk.len() as u64), total_size);
        downloaded = new;
        pb.set_position(new);
    }

    pb.finish_with_message(&format!("Download Complete"));
    return Ok(());
}

pub fn enable_ansi_support() -> io::Result<()> {
    unsafe {
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }

        let mut original_mode: u32 = 0;
        if GetConsoleMode(handle, &mut original_mode) == 0 {
            return Err(io::Error::last_os_error());
        }

        if SetConsoleMode(handle, original_mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn sha256sum(file_path: &Path, exec_name: &str) -> Result<bool, Box<dyn std::error::Error>> {
    let data = fs::read(file_path)?;
    let digest = Sha256::digest(&data);
    let sha256sum = BINARY_CHECKSUMS.iter()
        .find(|(name, _)| *name == exec_name)
        .map(|(_, sum)| *sum)
        .unwrap();

    Ok(format!("{:x}", digest) == sha256sum)
}

fn decompress_zstd<P: AsRef<Path>>(input_path: P, output_path: P) -> io::Result<()> {
    let input_file = File::open(input_path)?;
    let mut output_file = File::create(output_path)?;

    let mut decoder = zstd::Decoder::new(input_file)?;
    io::copy(&mut decoder, &mut output_file)?;

    Ok(())
}

pub async fn startup(base_name: &str) {
    let pkgrel = PKGREL;
    let appdata_binding = var("LOCALAPPDATA").unwrap();
    let workingdir = Path::new(&appdata_binding).join("mbtunnel");
    
    let exec_path = current_exe().unwrap();
    let exec_name = exec_path.to_str().unwrap();
    let orig_name = format!("{}.exe", base_name);
    let temp_name = format!("{}-temp.exe", base_name);

    let udpproxy_path = workingdir.join("udpproxy.exe");
    let udpproxy_zst_path = workingdir.join("udpproxy.exe.zst");

    let wstunnel_path = workingdir.join("wstunnel.exe");
    let wstunnel_zst_path = workingdir.join("wstunnel.exe.zst");

    let quiche_path = workingdir.join("quiche-client.exe");
    let quiche_zst_path = workingdir.join("quiche-client.exe.zst");

    let librespeed_path = workingdir.join("librespeed-cli.exe");
    let librespeed_zst_path = workingdir.join("librespeed-cli.exe.zst");

    if exec_name.contains(&temp_name) {
        let orig_path_name = exec_name.replace(&temp_name, &orig_name);
        fs::copy(exec_name, &orig_path_name).unwrap();
        Command::new("cmd")
            .arg("/C")
            .arg("start")
            .arg("/B")
            .arg(orig_path_name)
            .spawn()
            .expect(format!("Failed to start {}", &orig_name).as_str());
        exit(0);
    } else {
        let temp_path_name = exec_name.replace(&orig_name, &temp_name);
        let _ = fs::remove_file(temp_path_name);
    }

    // Tests internet connectivity
    if let Err(e) = timeout(
        Duration::from_secs(5),
        reqwest::get("https://cloudflare.com/cdn-cgi/trace"),
    )
    .await
    {
        eprintln!("{} {}", "Err: Unable to connect to internet".bright_red(), e);
        any_key_to_continue::custom_msg("Press any key to exit...");
        exit(1);
    }

    // Fetches latest release and tests connectivity with VPN server
    let currentrel = match reqwest::get(&format!("https://{}:80/pkgrel/windows-pf", SERVER_HOSTNAME)).await {
        Ok(response) => match response.text().await {
            Ok(text) => text,
            Err(e) => {
                eprintln!(
                    "{} ({})",
                    "Err: Unable to reach VPN server".bright_red(),
                    e.to_string().bright_red()
                );
                any_key_to_continue::custom_msg("Press any key to exit...");
                exit(1);
            }
        },
        Err(e) => {
            eprintln!(
                "{} ({})",
                "Err: Unable to reach VPN server".bright_red(),
                e.to_string().bright_red()
            );
            any_key_to_continue::custom_msg("Press any key to exit...");
            exit(1);
        }
    };

    if !workingdir.exists() {
        if let Err(_) = fs::create_dir(&workingdir) {
            eprintln!("{}", "Err: Unable to create working directory".bright_red());
            any_key_to_continue::custom_msg("Press any key to exit...");
            exit(1);
        }
    }

    // Updates the tunnel
    if pkgrel != currentrel {
        let temp_path_name = exec_name.replace(&orig_name, &temp_name);
        println!("Updating {}", &base_name);
        download_file(
            &Client::new(),
            &format!("https://{}:80/bin/pathfinder.exe", SERVER_HOSTNAME),
            &temp_path_name,
        )
        .await
        .unwrap();
        Command::new("cmd")
            .arg("/C")
            .arg("start")
            .arg("/B")
            .arg(temp_path_name)
            .spawn()
            .expect(format!("Failed to start {}", &temp_name).as_str());
        exit(0);
    }

    // Downloads/Updates udpproxy
    if !udpproxy_path.exists() || !sha256sum(&udpproxy_path, "udpproxy").unwrap() {
        println!("Updating udpproxy executable");
        download_file(
            &Client::new(),
            &format!("https://{}:80/bin/udpproxy/udpproxy-win-signed-amd64.zst", SERVER_HOSTNAME),
            udpproxy_zst_path.to_str().unwrap(),
        )
        .await
        .unwrap();
        decompress_zstd(&udpproxy_zst_path, &udpproxy_path).unwrap();
        fs::remove_file(udpproxy_zst_path).unwrap();
    }   

    // Downloads/Updates wstunnel
    if !wstunnel_path.exists() || !sha256sum(&wstunnel_path, "wstunnel").unwrap()
    {
        println!("Updating wstunnel executable");
        download_file(
            &Client::new(),
            &format!("https://{}:80/bin/wstunnel/wstunnel-win-signed-amd64.zst", SERVER_HOSTNAME),
            wstunnel_zst_path.to_str().unwrap(),
        )
        .await
        .unwrap();
        decompress_zstd(&wstunnel_zst_path, &wstunnel_path).unwrap();
        fs::remove_file(wstunnel_zst_path).unwrap();
    }

    // Downloads/Updates quiche
    if !quiche_path.exists() || !sha256sum(&quiche_path, "quiche").unwrap() {
        println!("Updating quiche client");
        download_file(
            &Client::new(),
            &format!("https://{}:80/bin/quiche/quiche-win-signed-amd64.zst", SERVER_HOSTNAME),
            quiche_zst_path.to_str().unwrap(),
        )
        .await
        .unwrap();
        decompress_zstd(&quiche_zst_path, &quiche_path).unwrap();
        fs::remove_file(quiche_zst_path).unwrap();
    }

    // Downloads/Updates librespeed
    if !librespeed_path.exists() || !sha256sum(&librespeed_path, "librespeed").unwrap() {
        println!("Updating librespeed executable");
        download_file(
            &Client::new(),
            &format!("https://{}:80/bin/librespeed-cli/librespeed-cli-win-signed-amd64.zst", SERVER_HOSTNAME),
            librespeed_zst_path.to_str().unwrap(),
        )
        .await
        .unwrap();
        decompress_zstd(&librespeed_zst_path, &librespeed_path).unwrap();
        fs::remove_file(librespeed_zst_path).unwrap();
    }

    println!("Everything seems upto date.");
}