use defguard_wireguard_rs::{
    InterfaceConfiguration, WGApi, WireguardInterfaceApi, key::Key, net::IpAddrMask, peer::Peer,
};
use owo_colors::OwoColorize;
use reqwest::Version;
use std::{
    io::{self, Read},
    net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

use crate::{
    app_config::TunnelMode,
    config::{Binary, SERVER_HOSTNAME},
    tunnel::TunnelStatus::Stopped,
    update_dialog::binary_path,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelStatus {
    DetectingMode,
    DetectingPort,
    Running,
    Stopping,
    Stopped,
    Failed(String),
    Exited(Option<i32>),
}

#[derive(Clone)]
pub struct TunnelState {
    pub status: TunnelStatus,
    pub mode: TunnelMode,
    pub port: Option<u16>,
    pub log: Vec<u8>,
}

pub struct Tunnel {
    pub state: Arc<Mutex<TunnelState>>,
    pub child: Arc<Mutex<Option<Child>>>,
}

impl Tunnel {
    pub fn default() -> Self {
        Tunnel {
            state: Arc::new(Mutex::new(TunnelState {
                status: Stopped,
                mode: TunnelMode::Auto,
                port: None,
                log: Vec::new(),
            })),
            child: Arc::new(Mutex::new(None)),
        }
    }
}
pub struct Wireguard {
    pub wgapi: Option<WGApi>,
    pub wgconfig: Option<WgConfig>,
    pub selected_key: Option<String>,
    pub waiting_for_tunnel: bool,
}

impl Wireguard {
    pub fn new() -> Self {
        Wireguard {
            wgapi: None,
            wgconfig: None,
            selected_key: None,
            waiting_for_tunnel: false,
        }
    }
}

pub struct WgConfig {
    pub private_key: String,
    pub ipv4_address: Vec<IpAddrMask>,
    pub server_public_key: String,
    pub endpoint_port: u16,
    pub allowed_ips: Vec<IpAddrMask>,
    pub mtu: Option<u32>,
}

impl WgConfig {
    pub fn new() -> Self {
        WgConfig {
            private_key: String::new(),
            ipv4_address: Vec::new(),
            server_public_key: String::new(),
            endpoint_port: 0,
            allowed_ips: Vec::new(),
            mtu: None,
        }
    }
}
enum LogType {
    Info,
    Error,
}

fn append_log(log: &mut Vec<u8>, log_type: LogType, msg: String) {
    let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true);

    if !log.is_empty() && !log.ends_with(b"\n") {
        log.push(b'\n');
    }

    let type_message = match log_type {
        LogType::Info => "INFO".green().to_string(),
        LogType::Error => "ERROR".red().to_string(),
    };

    let final_message = format!(
        "{}  {} {} {}\n",
        timestamp.bright_black(),
        "mbtunnel:".bright_black(),
        type_message,
        msg
    );
    log.extend_from_slice(final_message.as_bytes());
}

async fn test_udp() -> bool {
    let mut handles = vec![];

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
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    for handle in handles {
        if let Ok(true) = handle.await {
            return true;
        }
    }

    false
}

fn resolve_hostname_timeout(hostname: String, timeout: Duration) -> io::Result<IpAddr> {
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        let result = (hostname.as_str(), 0)
            .to_socket_addrs()
            .and_then(|mut addrs| {
                addrs
                    .next()
                    .map(|addr| addr.ip())
                    .ok_or_else(|| io::Error::other("No addresses found"))
            });

        let _ = tx.send(result);
    });

    match rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "DNS lookup timed out",
        )),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(io::Error::other("DNS worker thread terminated"))
        }
    }
}

pub fn start_tunnel(mode: TunnelMode, tunnel: &Tunnel) {
    let state = Arc::clone(&tunnel.state);
    let child = Arc::clone(&tunnel.child);

    thread::spawn(move || {
        if let Err(err) = run_tunnel_worker(mode, Arc::clone(&state), child) {
            let msg = err.to_string();
            if let Ok(mut s) = state.lock() {
                s.status = TunnelStatus::Failed(msg.clone());
                append_log(&mut s.log, LogType::Error, msg);
            }
        }
    });
}

fn run_tunnel_worker(
    mut mode: TunnelMode,
    state: Arc<Mutex<TunnelState>>,
    child_slot: Arc<Mutex<Option<Child>>>,
) -> std::io::Result<()> {
    let log_offset: usize;

    {
        let mut s = state.lock().unwrap();
        s.status = TunnelStatus::DetectingMode;
        s.mode = mode;
        s.port = None;
        log_offset = s.log.len();

        append_log(&mut s.log, LogType::Info, "Starting Tunnel...".to_string());
    }

    if mode == TunnelMode::Auto {
        {
            let mut s = state.lock().unwrap();
            append_log(
                &mut s.log,
                LogType::Info,
                "Testing if UDP/443 is open...".to_string(),
            );
        }

        let rt = tokio::runtime::Runtime::new().unwrap();
        mode = if rt.block_on(test_udp()) {
            TunnelMode::UDP
        } else {
            TunnelMode::TCP
        };

        let mut s = state.lock().unwrap();
        s.mode = mode;
    }

    let server_ip = resolve_hostname_timeout(SERVER_HOSTNAME.into(), Duration::from_secs(3))?;
    let wss = format!("wss://{}:443", SERVER_HOSTNAME);

    let (tunnel_path, tunnel_args) = match mode {
        TunnelMode::UDP => (
            binary_path(&Binary::Udpproxy),
            vec![
                "-b".to_string(),
                "127.0.0.1".to_string(),
                "-l".to_string(),
                "0".to_string(),
                "-h".to_string(),
                server_ip.to_string(),
                "-r".to_string(),
                "443".to_string(),
                "-d".to_string(),
            ],
        ),
        TunnelMode::TCP => (
            binary_path(&Binary::Wstunnel),
            vec![
                "client".to_string(),
                "--http-upgrade-path-prefix".to_string(),
                "/ws".to_string(),
                wss,
                "-L".to_string(),
                "udp://127.0.0.1:0:127.0.0.1:51280?timeout_sec=0".to_string(),
            ],
        ),
        TunnelMode::Auto => unreachable!(),
    };

    let mut cmd = Command::new(tunnel_path);
    cmd.args(&tunnel_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = ur_taking_me_with_you::spawn_dying_with_parent(cmd)?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    {
        let mut slot = child_slot.lock().unwrap();
        *slot = Some(child);
    }

    if let Some(mut out) = stdout {
        let state = Arc::clone(&state);
        thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            loop {
                match out.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Ok(mut s) = state.lock() {
                            s.log.extend_from_slice(&chunk[..n]);
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }

    if let Some(mut err) = stderr {
        let state = Arc::clone(&state);
        thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            loop {
                match err.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Ok(mut s) = state.lock() {
                            s.log.extend_from_slice(&chunk[..n]);
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }

    {
        let mut s = state.lock().unwrap();
        s.status = TunnelStatus::DetectingPort;
    }

    let re = regex::Regex::new(r"127\.0\.0\.1:(\d+)").unwrap();

    loop {
        {
            let mut exited = None;
            let mut stopped = false;

            if let Ok(mut slot) = child_slot.lock() {
                if slot.is_none() {
                    stopped = true;
                } else if let Some(child) = slot.as_mut() {
                    if let Ok(Some(status)) = child.try_wait() {
                        exited = Some(status.code());
                    }
                }
            }

            if stopped {
                break;
            }

            if let Some(code) = exited {
                let mut s = state.lock().unwrap();
                s.status = TunnelStatus::Exited(code);
                append_log(
                    &mut s.log,
                    LogType::Error,
                    format!("Tunnel Exited with code {}", crate::Opt(code)),
                );
                break;
            }
        }

        {
            let mut s = state.lock().unwrap();

            if s.port.is_none() {
                let new_log = &s.log[log_offset..];
                let log = String::from_utf8_lossy(new_log);

                s.port = log.lines().find_map(|line| {
                    re.captures(line)
                        .and_then(|caps| caps.get(1))
                        .and_then(|m| m.as_str().parse::<u16>().ok())
                });

                if s.port.is_some() {
                    s.status = TunnelStatus::Running;
                }
            }
        }

        thread::sleep(Duration::from_millis(50));
    }

    Ok(())
}

pub fn stop_tunnel(tunnel: &Tunnel) {
    {
        let mut state = tunnel.state.lock().unwrap();
        state.status = TunnelStatus::Stopping;
        append_log(
            &mut state.log,
            LogType::Info,
            "Stopping Tunnel...".to_string(),
        );
    }

    let maybe_child = tunnel.child.lock().unwrap().take();

    if let Some(mut child) = maybe_child {
        let kill_res = child.kill();
        let wait_res = child.wait();

        let mut state = tunnel.state.lock().unwrap();
        state.port = None;

        match (kill_res, wait_res) {
            (_, Ok(_)) => state.status = TunnelStatus::Stopped,
            (Err(e), _) => {
                state.status = TunnelStatus::Failed(format!("Kill Failed"));
                append_log(
                    &mut state.log,
                    LogType::Error,
                    format!("Kill Failed: {}", e),
                );
            }
            (_, Err(e)) => {
                state.status = TunnelStatus::Failed(format!("Wait Failed"));
                append_log(
                    &mut state.log,
                    LogType::Error,
                    format!("Wait Failed: {}", e),
                );
            }
        }
    } else {
        let mut state = tunnel.state.lock().unwrap();
        state.status = TunnelStatus::Stopped;
        state.port = None;
    }
}

pub fn start_wireguard(wgconfig: WgConfig) -> Result<WGApi, Box<dyn std::error::Error>> {
    let ifname = "mbtun0";

    #[cfg(not(target_os = "macos"))]
    let mut wgapi = WGApi::<defguard_wireguard_rs::Kernel>::new(ifname)?;
    #[cfg(target_os = "macos")]
    let mut wgapi = WGApi::<defguard_wireguard_rs::Userspace>::new(ifname.clone())?;

    wgapi.create_interface()?;

    let peer_key: Key = wgconfig.server_public_key.parse().map_err(|e| {
        format!(
            "Invalid peer key: {} Error :{}",
            wgconfig.server_public_key, e
        )
    })?;
    let mut peer = Peer::new(peer_key.clone());

    let endpoint: SocketAddr = format!("127.0.0.1:{}", wgconfig.endpoint_port)
        .parse()
        .map_err(|e| format!("Invalid port: {}\n Error :{}", wgconfig.endpoint_port, e))?;

    peer.endpoint = Some(endpoint);
    peer.persistent_keepalive_interval = Some(25);
    peer.allowed_ips = wgconfig.allowed_ips;

    let interface_config = InterfaceConfiguration {
        name: ifname.into(),
        prvkey: wgconfig.private_key,
        addresses: wgconfig.ipv4_address,
        port: 0,
        peers: vec![peer],
        mtu: wgconfig.mtu,
        fwmark: None,
    };

    #[cfg(not(windows))]
    wgapi.configure_interface(&interface_config)?;
    #[cfg(windows)]
    wgapi.configure_interface(&interface_config, &[], &[])?;
    wgapi.configure_peer_routing(&interface_config.peers)?;

    wgapi.configure_dns(
        &vec!["1.1.1.1".parse().unwrap(), "1.0.0.1".parse().unwrap()],
        &[],
    )?;

    Ok(wgapi)
}

pub fn stop_wireguard(wgapi: WGApi) -> Result<Option<WGApi>, Box<dyn std::error::Error>> {
    wgapi.remove_interface()?;
    Ok(None)
}
