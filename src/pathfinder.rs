use cliclack::{confirm, select};
use rand::Rng;
use std::env::var;
use std::fs;
use std::path::Path;
use std::process::{Command, exit};
use std::time::{Duration, Instant};
use command_error::CommandExt;
use colored::*;
use reqwest::Client;

#[derive(Debug, PartialEq)]
pub enum HyperVState {
    Enabled,
    Disabled,
    Unavailable,
}

impl std::fmt::Display for HyperVState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HyperVState::Enabled => write!(f, "Enabled"),
            HyperVState::Disabled => write!(f, "Disabled"),
            HyperVState::Unavailable => write!(f, "Unavailable"),
        }
    }
}

fn get_hyperv_state() -> Result<HyperVState, Box<dyn std::error::Error>> {
    let output = Command::new("powershell")
        .args(&[
            "-Command",
            "Get-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V-All"
        ])
        .output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    
    if stdout.trim().is_empty() || !stdout.contains("Microsoft-Hyper-V-All") {
        return Ok(HyperVState::Unavailable);
    }
    
    let state_output = Command::new("powershell")
        .args(&[
            "-Command",
            "(Get-WindowsOptionalFeature -FeatureName Microsoft-Hyper-V-All -Online).State",
        ])
        .output()?;

    let state_stdout = String::from_utf8_lossy(&state_output.stdout);
    let state = state_stdout.trim();

    match state {
        "Enabled" => Ok(HyperVState::Enabled),
        "Disabled" => Ok(HyperVState::Disabled),
        _ => Ok(HyperVState::Unavailable),
    }
}

fn random_mac() -> String {
    let mut rng = rand::rng();

    // Generate 6 random bytes
    let mut mac_bytes = [0u8; 6];
    rng.fill(&mut mac_bytes);

    mac_bytes[0] = (mac_bytes[0] & 0xFC) | 0x02;

    format!(
        "{:02x}-{:02x}-{:02x}-{:02x}-{:02x}-{:02x}",
        mac_bytes[0], mac_bytes[1], mac_bytes[2], mac_bytes[3], mac_bytes[4], mac_bytes[5]
    )
}

pub fn install_hyperv() -> Result<(), Box<dyn std::error::Error>> {
    if get_hyperv_state().unwrap() == HyperVState::Unavailable {
        let appdata_binding = var("LOCALAPPDATA").unwrap();
        let hyper_v_bat_path = Path::new(&appdata_binding)
            .join("mbtunnel")
            .join("hyperv.bat");

        let batch_content = [
        r#"pushd "%~dp0""#,
        r#"dir /b %SystemRoot%\servicing\Packages\*Hyper-V*.mum >hyper-v.txt"#,
        r#"for /f %%i in ('findstr /i . hyper-v.txt 2^>nul') do dism /online /norestart /add-package:"%SystemRoot%\servicing\Packages\%%i""#,
        r#"del hyper-v.txt"#,
        ].join("\n");

        println!("Hyper-V Unavailable. Installing Hyper-V...");
        fs::write(&hyper_v_bat_path, batch_content)?;
        Command::new("cmd")
            .args(&["/C", hyper_v_bat_path.to_str().unwrap()])
            .status_checked()?;
        fs::remove_file(&hyper_v_bat_path)?;
    }

    println!("Enabling Hyper-V...");
    Command::new("cmd")
        .args(&[
            "/C",
            "Dism /online /enable-feature /featurename:Microsoft-Hyper-V -All /LimitAccess /ALL",
        ])
        .status_checked()?;

    Ok(())
}

pub fn randomize_mac() -> Result<(), Box<dyn std::error::Error>> {
    println!("Fetching Hyper-V state...");
    let hyperv_state =
        get_hyperv_state().map_err(|e| format!("Failed to get Hyper-V state: {}", e))?;

    if hyperv_state != HyperVState::Enabled {
        println!("Hyper-V is not enabled. Hyper-V Virtual Switch is required to fetch new route.");
        let should_install = confirm("Would you like to install and enable Hyper-V now?")
            .interact()
            .unwrap();

        if should_install {
            install_hyperv().map_err(|e| format!("Failed to install Hyper-V: {}", e))?;
            exit(0);
        } else {
            return Err("Hyper-V is required to randomize MAC address.".into());
        }
    }

    let _ = remove_adapter();

    let adapters_command  = Command::new("powershell")
        .args(&["-Command", "Get-NetAdapter | Where-Object {$_.Status -eq 'Up'} | Select-Object -ExpandProperty Name"])
        .output()
        .map_err(|e| format!("Failed to get network adapters: {}", e))?;

    let adapters_utf8 = String::from_utf8_lossy(&adapters_command.stdout);

    let adapters: Vec<(&str, &str, &str)> =
        adapters_utf8.lines().map(|line| (line, line, "")).collect();

    let adapter = select("Please select adapter for MAC Randomization:")
        .items(adapters.as_slice())
        .interact()
        .unwrap();

    let mac = random_mac();

    Command::new("powershell")
        .args(&["-Command", &format!("New-VMSwitch -Name 'Pathfinder Adapter' -NetAdapterName '{}' -AllowManagementOS $true", adapter)])
        .status_checked()?;

    Command::new("powershell")
        .args(&[
            "-Command",
            &format!(
                "Set-NetAdapter -Name 'vEthernet (Pathfinder Adapter)' -MacAddress {} -Confirm:$false",
                mac
            ),
        ])
        .status_checked()?;

    println!(
        "{} '{}' to {}",
        "Successfully set MAC address for adapter".bright_green(),
        adapter,
        mac
    );

    Ok(())
}

pub fn remove_adapter() -> Result<(), Box<dyn std::error::Error>> {
    println!("Removing pathfinder adapter...");
    let output = Command::new("powershell")
        .args(&[
            "-Command",
            "Remove-VMSwitch -Name 'Pathfinder Adapter' -Force",
        ])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("Hyper-V was unable to find a virtual switch with name \"Pathfinder Adapter\"") {
            return Err("Pathfinder adapter does not exist.".into());
        } else {
            return Err(format!("Command failed: {}", stderr).into());
        }
    }

    println!("{}", "Successfully disabled MAC Randomization (Removed pathfinder adapter).".bright_green());

    Ok(())
}

pub async fn wait_for_internet() -> bool {
    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    loop {
        let deadline = Instant::now() + Duration::from_secs(30);
        println!("Checking for internet connectivity (30s max)...");
        println!("{}", "Please relogin to Captive Portal (Wi-Fi Login) if required.".bright_yellow());

        let mut last_status: Option<String> = None;

        while Instant::now() < deadline {
            match client.get("https://cloudflare.com/cdn-cgi/trace").send().await {
                Ok(resp) if resp.status().is_success() => {
                    println!("{}","Internet connectivity detected!".bright_green());
                    return true;
                }
                Ok(resp) => {
                    last_status = Some(format!("Got response but not success: {}", resp.status()));
                }
                Err(err) => {
                    last_status = Some(format!("Request failed: {}", err));
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        println!("{}", last_status.unwrap_or_else(|| "No response received".to_string()).bright_red());
        println!("No internet connectivity detected within 30 seconds.");
        let try_again = confirm("Do you want to retry?")
            .interact()
            .unwrap();

        if !try_again {
            println!("Exiting by user request.");
            return false;
        }
    }
}

