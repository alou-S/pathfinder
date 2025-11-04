use std::env::var;
use std::path::Path;
use std::fs;
use std::process::exit;
use dont_disappear::any_key_to_continue;
use reqwest::Client;

use rfd::FileDialog;
use regex::Regex;
use x25519_dalek::{PublicKey, StaticSecret};
use base64::{engine::general_purpose, Engine};

use crate::config::API_BASE_URL;

pub async fn validate() -> String {
    let appdata_binding = var("LOCALAPPDATA").unwrap();
    let workingdir = Path::new(&appdata_binding).join("mbtunnel");
    let config_path_file = workingdir.join("config.path");

    if !config_path_file.exists() {
        println!("Please select your Wireguard config file.");
        let path = FileDialog::new()
            .set_title("Select your Wireguard config")
            .add_filter("Config", &["conf"])
            .add_filter("All Files", &["*"])
            .pick_file()
            .unwrap();

        fs::write(&config_path_file, &path.display().to_string()).unwrap();
    }

    let path_str = fs::read_to_string(&config_path_file).unwrap();
    let path = Path::new(&path_str);

    let content = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to read {}: {}", &path.display(), e);
            fs::remove_file(&config_path_file).unwrap();
            any_key_to_continue::custom_msg("Press any key to exit...");
            exit(1);
        }
    };

    // Extract PrivateKey
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
        eprintln!("{} Not a valid Wireguard config file", path.display());
        fs::remove_file(&config_path_file).unwrap();
        any_key_to_continue::custom_msg("Press any key to exit...");
        exit(1);
    }

    // Convert to PublicKey
    let priv_key_bytes = general_purpose::STANDARD
        .decode(priv_key_b64)
        .expect("Invalid base64 private key");
    let priv_key = StaticSecret::from(<[u8; 32]>::try_from(&priv_key_bytes[..32]).unwrap());
    let pub_key = PublicKey::from(&priv_key);

    let pub_key_b64 = general_purpose::STANDARD.encode(pub_key.as_bytes());

    // Validate PublicKey
    let url = format!("{}/validate", API_BASE_URL);
    let client = Client::new();
    let res = client.post(url)
        .header("PATHFINDER-KEY", &pub_key_b64)
        .send()
        .await
        .unwrap();

    if res.status().is_success() {
        println!("Successfully validated key.");
        return pub_key_b64;
    } else {
        println!("Failed to validate key ({})", res.status());
        fs::remove_file(&config_path_file).unwrap();
        any_key_to_continue::custom_msg("Press any key to exit...");
        exit(1);
    }
}