use crate::app_config::Keys;
use crate::config::API_URL;
use anyhow::{Context, Result};

pub fn fetch_keys_data(mut keys: Vec<Keys>) -> Result<Vec<Keys>, Box<dyn std::error::Error>> {
    for key_item in &mut keys {
        let key = key_item.key.clone();
        let url = format!("{}/key/info", API_URL);
        let response = match reqwest::blocking::Client::new()
            .get(&url)
            .header("MBTUNNEL-KEY", &key)
            .send()
        {
            Ok(resp) => resp,
            Err(e) if e.status() == Some(reqwest::StatusCode::FORBIDDEN) => {
                key_item.id = Some("Invalid Key".into());
                continue;
            }
            Err(e) => return Err(e.into()),
        };

        let body = response.text().context("Failed to read API response")?;

        let parsed: Keys = serde_json::from_str(&body).context("Failed to parse API response")?;

        key_item.id = parsed.id;
        key_item.ip = parsed.ip;
        key_item.key_type = parsed.key_type;
        key_item.is_active = parsed.is_active;
        key_item.expiry = parsed.expiry;
    }
    Ok(keys)
}
