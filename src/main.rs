mod speedtest;
use speedtest::*;
mod validate;
use validate::*;
mod pathfinder;
use pathfinder::*;
mod startup;
use startup::*;
mod tunnel;
use tunnel::*;
mod config;

use colored::*;
use cliclack::{select, intro};
use std::env;
use std::process::{exit, Command};
use dont_disappear::any_key_to_continue;
use std::os::windows::process::CommandExt;

fn clear_screen() {
    Command::new("cmd").args(&["/C", "cls"]).status().unwrap();
}

fn request_elevation() {
    let is_admin = Command::new("net")
        .args(&["session"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);

    if !is_admin {
        let exe_path = env::current_exe().unwrap();
        
        // Try Windows Terminal first, fallback to cmd.exe
        let result = Command::new("powershell")
            .args(&[
                "-Command", 
                &format!(
                    "if (Get-Command wt -ErrorAction SilentlyContinue) {{ Start-Process 'wt.exe' -ArgumentList 'powershell.exe', '-Command', '& \"{}\"' -Verb RunAs }} else {{ Start-Process 'cmd.exe' -ArgumentList '/k', '\"{}\"' -Verb RunAs }}",
                    exe_path.display(),
                    exe_path.display()
                )
            ])
            .spawn();
            
        match result {
            Ok(_) => std::process::exit(0),
            Err(_) => {
                // Fallback to basic cmd.exe if the above fails
                Command::new("powershell")
                    .args(&[
                        "-Command", 
                        &format!("Start-Process 'cmd.exe' -ArgumentList '/k', '\"{}\"' -Verb RunAs", exe_path.display())
                    ])
                    .spawn()
                    .expect("Failed to request elevation");
                std::process::exit(0);
            }
        }
    }
}


#[tokio::main]
async fn main() {
    // Fixes lack of color when run in conhost.exe
    enable_ansi_support().unwrap();

    let exec_path = env::current_exe().unwrap();
    let exec_name = exec_path.to_str().unwrap();
    let args: Vec<String> = env::args().collect();


    if exec_name.contains("mbtunnel.exe") || (args.len() > 1 && args[1] == "start_tunnel") {
        startup("mbtunnel").await;
        start_tunnel().await;
        return
    } else if exec_name.contains("pathfinder.exe") || exec_name.contains("pathfinder-temp.exe") {
        startup("pathfinder").await;
        request_elevation();
    } else {
        eprintln!("{}", "Err: Invalid executable name. Please do not rename executable".bright_red());
        any_key_to_continue::custom_msg("Press any key to exit...");
        exit(1);
    }

    let key = validate().await;

    loop {
        intro("Welcome to Pathfinder!").unwrap();

        let selection = select("What would you like to do?")
            .item("tunnel", "Start mbtunnel", "")
            .item("speedtest", "Test current route", "")
            .item("fetch_metrics", "List routes", "")
            .item("randomize_mac", "Randomize MAC address (Get new route)", "")
            .item("remove_adapter", "Remove randomized MAC address", "")
            .item("exit", "Exit Pathfinder", "")
            .interact()
            .unwrap();

        match selection {
            "tunnel" => {
                clear_screen();
                match Command::new(&exec_path)
                    .arg("start_tunnel")
                    .creation_flags(0x00000010 | 0x00000200)
                    .spawn()
                {
                    Ok(_) => println!("mbtunnel started in a new window."),
                    Err(e) => eprintln!("Failed to start mbtunnel: {}", e),
                }
            }
            "speedtest" => {
                clear_screen();
                println!("Testing current route...");
                speed_test(key.clone()).await;
            }
            "fetch_metrics" => {
                clear_screen();
                fetch_metrics(key.clone()).await;
            }
            "randomize_mac" => {
                clear_screen();
                println!("{}", "Testing current route...".bright_yellow());
                speed_test(key.clone()).await;
                println!("\n");
                if let Err(e) = randomize_mac() {
                    eprintln!("Error: {}", format!("{}", e).red());
                    continue;
                }
                println!("\n");
                if !wait_for_internet().await {
                    let _ = remove_adapter().map_err(|e| eprintln!("Error: {}", format!("{}", e).red()));
                    continue;
                }
                println!("\n");
                println!("{}", "Testing new route...".bright_yellow());
                speed_test(key.clone()).await;
            }
            "remove_adapter" => {
                clear_screen();
                let _ = remove_adapter().map_err(|e| eprintln!("Error: {}", format!("{}", e).red()));
            }
            "exit" => {
                std::thread::sleep(std::time::Duration::from_secs(1));
                clear_screen();
                println!("Goodbye!");
                std::thread::sleep(std::time::Duration::from_secs(1));
                return;
            }
            _ => {}
        }

        let _ = select("\n").item("continue", "Continue...", "").interact().unwrap();
        clear_screen();
    }
}