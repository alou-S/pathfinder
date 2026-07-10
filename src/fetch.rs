use crate::app_config::Key;
use crate::config::API_URL;
use anyhow::{Context, Result};

pub fn fetch_keys_data(mut keys: Vec<Key>) -> Result<Vec<Key>, Box<dyn std::error::Error>> {
    for key_item in &mut keys {
        let key = key_item.priv_key.clone();
        let url = format!("{}/key/info", API_URL);
        let response = match reqwest::blocking::Client::new()
            .get(&url)
            .header("MBTUNNEL-KEY", &key)
            .send()
        {
            Ok(resp) => resp,
            Err(e) => return Err(e.into()),
        };

        let status = response.status();

        if status == reqwest::StatusCode::FORBIDDEN {
            key_item.id = Some("Invalid Key".into());
            continue;
        }

        let body = response
            .text()
            .context(format!("Failed to read API response (status {})", status))?;

        let parsed: Key = serde_json::from_str(&body).context("Failed to parse API response")?;

        key_item.id = parsed.id;
        key_item.ip = parsed.ip;
        key_item.is_active = parsed.is_active;
        key_item.expiry = parsed.expiry;
    }
    Ok(keys)
}
