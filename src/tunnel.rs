use colored::*;
use std::net::ToSocketAddrs;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use tokio::process::Command;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_SHIFT};

use crate::config::SERVER_HOSTNAME;

// A safe wrapper around GetAsyncKeyState to check if shift is pressed
fn is_shift_pressed() -> bool {
    let state = unsafe { GetAsyncKeyState(VK_SHIFT.0 as i32) };
    (state as i16) < 0
}

fn get_ip4_addr(hostname: &str) -> String {
    match (hostname, 0).to_socket_addrs() {
        Ok(mut addrs) => match addrs.next() {
            Some(addr) => addr.ip().to_string(),
            None => format!("{}", "Err: Unable to resolve hostname".bright_red()),
        },
        Err(_) => format!("{}", "Err: Unable to resolve hostname".bright_red()),
    }
}

async fn test_udp(quiche_path: PathBuf) -> bool {
    let mut tries = 1;

    loop {
        println!("Testing if UDP/443 is open (Try {}/3)", tries);

        let output = Command::new(&quiche_path)
            .args(vec![
                "--idle-timeout",
                "2", 
                "https://cloudflare.com/cdn-cgi/trace",
            ])
            .kill_on_drop(true)
            .output()
            .await;

        let output = match output {
            Ok(output) => output,
            Err(e) => {
                println!("Failed to execute quiche command: {}", e);
                if tries == 3 {
                    return false;
                }
                tries += 1;
                continue;
            }
        };

        let output_str = String::from_utf8_lossy(&output.stdout);

        if output_str.contains("http=http/3") {
            println!(
                "{} is open. Tunneling Wireguard via UDP.",
                "UDP/443".bright_blue()
            );
            return true;
        } else {
            if tries == 3 {
                println!(
                    "{} seems closed. Tunneling Wireguard via TCP using Websocket relay.",
                    "UDP/443".bright_blue()
                );
                return false;
            }
            tries += 1;
        }
    }
}

pub async fn start_tunnel() {
    let appdata_binding = std::env::var("LOCALAPPDATA").unwrap();
    let workingdir = std::path::Path::new(&appdata_binding).join("mbtunnel");

    let udpproxy_path = workingdir.join("udpproxy.exe");
    let wstunnel_path = workingdir.join("wstunnel.exe");
    let quiche_path = workingdir.join("quiche-client.exe");

    let server_ip = get_ip4_addr(SERVER_HOSTNAME);

    let mut shift_pressed = false;
    let start_time = std::time::Instant::now();

    // Check for shift key for one second
    while start_time.elapsed() < Duration::from_millis(1500) {
        if is_shift_pressed() {
            shift_pressed = true;
            println!("SHIFT detected! Will use TCP tunnel mode.\n");
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }

    // Tests UDP and starts tunnel
    if shift_pressed || !test_udp(quiche_path).await {
        println!("{}", "You may now connect to Wireguard\n".bright_green());
        Command::new(wstunnel_path)
            .args(vec![
                "client",
                &format!("wss://{}:443", server_ip),
                "-L",
                "udp://51280:127.0.0.1:51280\\?timeout_sec=0",
            ])
            .status()
            .await
            .unwrap();
    } else {
        println!("{}", "You may now connect to Wireguard\n".bright_green());
        Command::new(udpproxy_path)
            .args(vec![
                "-b",
                "127.0.0.1",
                "-l",
                "51280",
                "-h",
                &format!("{}", server_ip),
                "-r",
                "443",
                "-d",
            ])
            .status()
            .await
            .unwrap();
    }
}