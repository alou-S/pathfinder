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

        *key_item = parsed;
    }
    Ok(keys)
}

pub fn fetch_config_data(
    key: String,
) -> Result<
    (
        String,
        Vec<defguard_wireguard_rs::net::IpAddrMask>,
        Option<u32>,
    ),
    Box<dyn std::error::Error>,
> {
    let url = format!("{}/config", API_URL);
    let response = reqwest::blocking::Client::new()
        .get(&url)
        .header("MBTUNNEL-KEY", key)
        .send()?;

    let status = response.status();

    if status == reqwest::StatusCode::FORBIDDEN {
        return Err("Invalid Key".into());
    }

    let body = response
        .text()
        .context(format!("Failed to read API response (status {})", status))?;

    let json: serde_json::Value =
        serde_json::from_str(&body).context("Failed to parse API response")?;

    let server_public_key = json["server_public_key"]
        .as_str()
        .ok_or("Missing server_public_key in response")?
        .to_string();

    let allowed_ips: Vec<defguard_wireguard_rs::net::IpAddrMask> = json["allowed_ips"]
        .as_str()
        .ok_or("Missing allowed_ips in response")?
        .split(',')
        .map(|ip| {
            ip.trim()
                .parse()
                .map_err(|e| format!("Invalid allowed IP '{}': {}", ip.trim(), e))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mtu = u32::try_from(json["mtu"].as_u64().ok_or("Missing mtu in response")?)
        .map_err(|_| "mtu is too large for u32")?;

    Ok((server_public_key, allowed_ips, Some(mtu)))
}
