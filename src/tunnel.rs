#![allow(unused)]
use crate::app_config::TunnelMode;
use reqwest::Version;
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;
use tokio::time::sleep;

struct TunnelState {
    mode: TunnelMode,
}

async fn test_udp() -> bool {
    let mut handles = vec![];

    println!("Testing if UDP/443 is open...");
    for i in 1..=3 {
        let handle = tokio::spawn(async move {
            let client = match reqwest::Client::builder()
                .http3_prior_knowledge()
                .local_address(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
                .timeout(Duration::from_secs(2))
                .build()
            {
                Ok(c) => c,
                Err(_) => return false,
            };

            let resp = match client
                .get("https://cloudflare.com/cdn-cgi/trace")
                .version(Version::HTTP_3)
                .send()
                .await
            {
                Ok(r) => r,
                Err(_) => return false,
            };

            match resp.text().await {
                Ok(body) => body.contains("http=http/3"),
                Err(_) => false,
            }
        });

        handles.push(handle);

        if i < 3 {
            sleep(Duration::from_millis(500)).await;
        }
    }

    for handle in handles {
        if let Ok(true) = handle.await {
            return true;
        }
    }

    false
}

fn start_tunnel_tcp(local_port: u16, remote_addr: &str) {}

fn start_tunnel_udp(local_port: u16, remote_addr: &str) {}

pub fn start_tunnel(mut mode: TunnelMode) {}

pub fn stop_tunnel() {}

pub fn start_wireguard() {}

pub fn stop_wireguard() {}
