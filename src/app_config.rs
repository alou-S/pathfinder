use chacha20::ChaCha20;
use chacha20::cipher::{KeyIvInit, StreamCipher};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use chrono::{NaiveDate, Local};
use xxhash_rust::xxh3::xxh3_64;
use zstd::bulk::{compress, decompress};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Keys {
    pub id: Option<String>,
    pub ip: Option<String>,
    pub key: String,
    pub key_type: Option<String>,
    pub is_active: Option<bool>,
    pub expiry: Option<NaiveDate>,
}

impl Keys {
    pub fn new(key: String) -> Self {
        Self {
            id: None,
            ip: None,
            key: key.to_string(),
            key_type: None,
            is_active: None,
            expiry: None,
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
                let diff  = date.signed_duration_since(today);
                let days  = diff.num_days();
                let date_formatted = date.format("%b %e, %Y");

                if days == 0 {
                    let mins = diff.num_minutes().unsigned_abs();
                    if diff.num_minutes() >= 0 {
                        format!("{mins} minutes left ({date_formatted})")
                    } else {
                        format!("{mins} minutes ago ({date_formatted})")
                    }
                } else if days > 0 {
                    format!("{days} days left ({date_formatted})")
                } else {
                    format!("{} days ago ({date_formatted})", days.unsigned_abs())
                }
            }
        }
    }
}

#[derive(PartialEq, Clone, Serialize, Deserialize)]
pub enum TunnelMode {
    Auto,
    TCP,
    UDP
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct AppConfig {
    pub keys: Vec<Keys>,
    pub dark_mode: bool,
    pub tunnel_mode: TunnelMode,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self { 
            keys: Vec::new(), 
            dark_mode: true,
            tunnel_mode: TunnelMode::Auto,
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
