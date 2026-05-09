use crate::app_config::{Keys};
use crate::config::API_URL;
use anyhow::{Result, Context};

pub fn fetch_keys_data(mut keys: Vec<Keys>) -> Result<Vec<Keys>, Box<dyn std::error::Error>> {
    
    for key_item in &mut keys {
        let key = key_item.key.clone();
        let url = format!("{}/key/info", API_URL);
        let mut response = match ureq::get(&url)
            .header("MBTUNNEL-KEY", &key)
            .call()
        {
            Ok(resp) => resp,
            Err(ureq::Error::StatusCode(403)) => {
                key_item.id = Some("Invalid Key".into());
                continue;
            }
            Err(e) => return Err(e.into()), // propagate other errors
        };

        let body = response.body_mut().read_to_string().context("Failed to read API response")?;

        let parsed: Keys = serde_json::from_str(&body).context("Failed to parse API response")?;

        key_item.id = parsed.id;
        key_item.ip = parsed.ip;
        key_item.key_type = parsed.key_type;
        key_item.is_active = parsed.is_active;
        key_item.expiry = parsed.expiry;
    }
    Ok(keys)
}