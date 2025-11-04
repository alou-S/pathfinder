use std::process::Command;
use std::path::{Path, PathBuf};
use std::env::var;
use std::fs;
use std::process::exit;
use dont_disappear::any_key_to_continue;
use regex::Regex;
use reqwest::Client;

use crate::config::{SERVER_HOSTNAME, API_BASE_URL};

async fn get_public_ip() -> String {
    let services = [
        "https://api.ipify.org",
        "https://ifconfig.me/ip",
        "https://checkip.amazonaws.com",
    ];

    let client = Client::new();

    for url in services {
        let resp = client.get(url).send().await;
        if resp.is_err() {
            continue;
        }

        let resp = resp.unwrap();
        if !resp.status().is_success() {
            continue;
        }

        let text = resp.text().await.unwrap_or_default();
        let ip = text.trim();
        if !ip.is_empty() {
            return ip.to_string();
        }
    }

    "unknown".to_string()
}

async fn get_client_isp(ip: String, key: String) -> String {
    let url = format!("{}/isp", API_BASE_URL);
    let client = Client::new();
    let resp = client.post(&url)
        .header("PATHFINDER-KEY", key)
        .json(&serde_json::json!({ "ip": ip }))
        .send()
        .await;
    if resp.is_err() {
        return "".to_string();
    }

    let resp = resp.unwrap();
    if !resp.status().is_success() {
        return "".to_string();
    }

    let text = resp.text().await.unwrap_or_default();
    if text.trim() != "" {
        return format!("({})", text.trim());
    }

    return "".to_string();
}


pub async fn speed_test(key: String) {
    let appdata_binding = var("LOCALAPPDATA").unwrap();
    let workingdir = Path::new(&appdata_binding).join("mbtunnel");

    let librespeed_path = workingdir.join("librespeed-cli.exe");
    let client_public_ip = get_public_ip().await;
    let client_isp = get_client_isp(client_public_ip.clone(), key.clone()).await;

    println!("Current Route: {} {}", client_public_ip, client_isp);
    Command::new(librespeed_path)
        .env("TERM", "xterm-256color")
        .env("COLORTERM", "truecolor")
        .args([
            "--no-icmp",
            "--duration", "3",
            "--ping-count", "75",
            "--server-json",
            &format!("https://{}:80/bin/librespeed-cli/config.json", SERVER_HOSTNAME),
            "--server", "1"
        ])
        .status()
        .unwrap();

    let mut path = PathBuf::from(std::env::var("TEMP").unwrap());
    path.push("librespeed-results.txt");
    let contents = fs::read_to_string(&path).unwrap();
    fs::remove_file(path).unwrap();

    let re = Regex::new(
        r"Ping:\s*([\d.]+)\s*ms\s*Jitter:\s*([\d.]+)\s*ms\s*Download rate:\s*([\d.]+)\s*Mbps\s*Upload rate:\s*([\d.]+)\s*Mbps"
    ).unwrap();

    let caps = match re.captures(&contents) {
        Some(caps) => caps,
        None => {
            eprintln!("No match found in file contents");
            any_key_to_continue::custom_msg("Press any key to exit...");
            exit(1);
        }
    };

    let ping: f64 = caps[1].parse().unwrap();
    let jitter: f64 = caps[2].parse().unwrap();
    let download: f64 = caps[3].parse().unwrap();
    let upload: f64 = caps[4].parse().unwrap();

    let url = format!("{}/metrics", API_BASE_URL);
    let client = Client::new();
    let res = client.put(url)
        .header("PATHFINDER-KEY", key)
        .json(&serde_json::json!({
            "ip": client_public_ip,
            "ping": ping,
            "jitter": jitter,
            "download": download,
            "upload": upload
        }))
        .send()
        .await
        .unwrap();

    if !res.status().is_success() {
        eprintln!("Failed to send metrics");
    }
}

pub async fn fetch_metrics(_key: String) {
    println!("This is not implemented yet :(")
}