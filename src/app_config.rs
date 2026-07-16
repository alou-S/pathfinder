use chacha20::ChaCha20;
use chacha20::cipher::{KeyIvInit, StreamCipher};
use chrono::{Local, NaiveDate};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use xxhash_rust::xxh3::xxh3_64;
use zstd::bulk::{compress, decompress};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Key {
    pub id: Option<String>,
    pub ip: Option<String>,
    pub priv_key: String,
    pub is_active: Option<bool>,
    pub expiry: Option<NaiveDate>,
    pub rx_bytes: Option<u64>,
    pub tx_bytes: Option<u64>,
}

impl Key {
    pub fn new(priv_key: String) -> Self {
        Self {
            id: None,
            ip: None,
            priv_key: priv_key.to_string(),
            is_active: None,
            expiry: None,
            rx_bytes: None,
            tx_bytes: None,
        }
    }

    pub fn get_subscription(&self) -> String {
        match self.is_active {
            Some(true) => "Active".to_string(),
            Some(false) => "Inactive".to_string(),
            None => "".to_string(),
        }
    }

    pub fn get_expiry(&self) -> String {
        match self.expiry {
            None => String::new(),
            Some(date) => {
                let today = Local::now().date_naive();
                let diff = date.signed_duration_since(today);
                let days = diff.num_days();

                if days == 0 {
                    let mins = diff.num_minutes().unsigned_abs();
                    if diff.num_minutes() >= 0 {
                        format!("{mins} minutes left")
                    } else {
                        format!("{mins} minutes ago")
                    }
                } else if days > 0 {
                    format!("{days} days left")
                } else {
                    format!("{} days ago", days.unsigned_abs())
                }
            }
        }
    }
}

#[derive(PartialEq, Clone, Copy, Serialize, Deserialize)]
pub enum TunnelMode {
    Auto,
    TCP,
    UDP,
}

impl TunnelMode {
    pub fn label(&self) -> &'static str {
        match self {
            TunnelMode::Auto => "Auto",
            TunnelMode::TCP => "TCP",
            TunnelMode::UDP => "UDP",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum CloseAction {
    Ask,
    MinimizeToTray,
    Exit,
}

impl CloseAction {
    pub fn label(&self) -> &'static str {
        match self {
            CloseAction::Ask => "Ask every time",
            CloseAction::MinimizeToTray => "Minimize to Tray",
            CloseAction::Exit => "Exit",
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub keys: Vec<Key>,
    pub dark_mode: bool,
    pub tunnel_mode: TunnelMode,
    pub ui_zoom: f32,
    pub font_zoom: f32,
    pub close_action: CloseAction,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            keys: Vec::new(),
            dark_mode: true,
            tunnel_mode: TunnelMode::Auto,
            ui_zoom: 1.3,
            font_zoom: 1.05,
            close_action: CloseAction::Ask,
        }
    }
}

const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;

pub fn save_config(config: &AppConfig, path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let json = serde_json::to_vec(config).unwrap();
    let compressed = compress(&json, 0).unwrap();

    let checksum = xxh3_64(&compressed);
    let orig_len = json.len() as u64;
    let mut payload = Vec::with_capacity(16 + compressed.len());
    payload.extend_from_slice(&checksum.to_le_bytes());
    payload.extend_from_slice(&orig_len.to_le_bytes());
    payload.extend_from_slice(&compressed);

    let mut key = [0u8; KEY_LEN];
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut key).unwrap();
    getrandom::fill(&mut nonce).unwrap();

    let mut cipher = ChaCha20::new(&key.into(), &nonce.into());
    cipher.apply_keystream(&mut payload);

    let mut out = Vec::with_capacity(KEY_LEN + NONCE_LEN + payload.len());
    out.extend_from_slice(&key);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&payload);

    std::fs::write(path, out)?;

    Ok(())
}

pub fn load_config(path: PathBuf) -> Result<AppConfig, Box<dyn std::error::Error>> {
    let data = match std::fs::read(&path) {
        Ok(data) => data,

        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let default = AppConfig::default();
            save_config(&default, path)?;
            return Ok(default);
        }

        Err(err) => return Err(err.into()),
    };

    if data.len() < KEY_LEN + NONCE_LEN + 16 {
        return Err("Configuration file corrupt. (Too Short)".into());
    }

    let key: [u8; KEY_LEN] = data[..KEY_LEN].try_into().unwrap();
    let nonce: [u8; NONCE_LEN] = data[KEY_LEN..KEY_LEN + NONCE_LEN].try_into().unwrap();
    let mut payload = data[KEY_LEN + NONCE_LEN..].to_vec();

    let mut cipher = ChaCha20::new(&key.into(), &nonce.into());
    cipher.apply_keystream(&mut payload);

    let checksum = u64::from_le_bytes(payload[..8].try_into().unwrap());
    let orig_len = u64::from_le_bytes(payload[8..16].try_into().unwrap()) as usize;
    let compressed = &payload[16..];

    if xxh3_64(compressed) != checksum {
        return Err("Configuration file corrupt. (Invalid Checksum)".into());
    }

    let json = decompress(compressed, orig_len)?;

    let appconfig = serde_json::from_slice(&json)?;

    Ok(appconfig)
}
