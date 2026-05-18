use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::process::Command;

const CONFIG_DIR: &str = ".config/stm15";
const CONFIG_FILE: &str = "tunnels.yaml";
const HEALTH_CHECK_INTERVAL_SECS: u64 = 30;
const MAX_BACKOFF_SECS: u64 = 60;
const INITIAL_BACKOFF_SECS: u64 = 1;

// ── config / types ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelConfig {
    pub name: String,
    #[serde(rename = "type", default = "default_type")]
    pub tunnel_type: String,
    pub local_port: u16,
    pub ssh_host: String,
    #[serde(default = "default_ssh_port")]
    pub ssh_port: u16,
    #[serde(default)]
    pub ssh_user: String,
    #[serde(default)]
    pub ssh_key: String,
    #[serde(default = "default_localhost")]
    pub remote_host: String,
    #[serde(default)]
    pub remote_port: u16,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_type() -> String { "local".into() }
fn default_ssh_port() -> u16 { 22 }
fn default_localhost() -> String { "localhost".into() }
fn default_true() -> bool { true }

// ── SSH config parsing ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SshHost {
    pub name: String,
    pub hostname: Option<String>,
    pub port: u16,
    pub user: Option<String>,
    pub identity_file: Option<String>,
}

pub fn parse_ssh_config() -> Vec<SshHost> {
    let path = dirs::home_dir()
        .map(|h| h.join(".ssh/config"))
        .unwrap_or_default();
    if !path.exists() {
        return Vec::new();
    }
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let mut hosts = Vec::new();
    let mut current_names: Vec<String> = Vec::new();
    let mut current_hostname: Option<String> = None;
    let mut current_port: u16 = 22;
    let mut current_user: Option<String> = None;
    let mut current_idfile: Option<String> = None;
    let mut in_match_block = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let (key, value) = match trimmed.split_once(|c: char| c.is_whitespace()) {
            Some((k, v)) => (k.to_lowercase(), v.trim()),
            None => continue,
        };

        if key == "host" || key == "match" {
            // Flush the current Host entry before starting a new block.
            if !current_names.is_empty() {
                let hostname = current_hostname
                    .clone()
                    .unwrap_or_else(|| current_names[0].clone());
                for name in current_names.drain(..) {
                    if name == "*" {
                        continue;
                    }
                    hosts.push(SshHost {
                        name,
                        hostname: Some(hostname.clone()),
                        port: current_port,
                        user: current_user.clone(),
                        identity_file: current_idfile.clone(),
                    });
                }
            }
            current_hostname = None;
            current_port = 22;
            current_user = None;
            current_idfile = None;
            if key == "host" {
                current_names = value.split_whitespace().map(|s| s.to_string()).collect();
                in_match_block = false;
            } else {
                // Match blocks have conditional directives; skip their contents.
                current_names = Vec::new();
                in_match_block = true;
            }
        } else if !in_match_block {
            match key.as_str() {
                "hostname" => current_hostname = Some(value.to_string()),
                "port" => current_port = value.parse().unwrap_or(22),
                "user" => current_user = Some(value.to_string()),
                "identityfile" => current_idfile = Some(value.to_string()),
                _ => {}
            }
        }
    }

    if !current_names.is_empty() {
        let hostname = current_hostname
            .clone()
            .unwrap_or_else(|| current_names[0].clone());
        for name in current_names.drain(..) {
            if name == "*" {
                continue;
            }
            hosts.push(SshHost {
                name,
                hostname: Some(hostname.clone()),
                port: current_port,
                user: current_user.clone(),
                identity_file: current_idfile.clone(),
            });
        }
    }

    hosts
}

#[derive(Debug, Clone, PartialEq)]
pub enum TunnelStatus {
    Stopped,
    Running,
    Reconnecting { failures: u32, backoff: u64 },
}

impl TunnelStatus {
    pub fn symbol(&self) -> &str {
        match self {
            TunnelStatus::Stopped => "◆",
            TunnelStatus::Running => "●",
            TunnelStatus::Reconnecting { .. } => "↻",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConfigFile {
    tunnels: Vec<TunnelConfig>,
}

// ── tunnel ───────────────────────────────────────────────────────────────────

/// Shared state updated by the background SSH task, readable synchronously.
struct TunnelState {
    status: TunnelStatus,
    start_time: Option<Instant>,
    last_error: String,
}

pub struct Tunnel {
    pub config: TunnelConfig,
    pub health_port: Mutex<u16>,
    state: Arc<Mutex<TunnelState>>,
    stop_flag: Arc<AtomicBool>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Tunnel {
    fn from_config(config: TunnelConfig) -> Self {
        Tunnel {
            config,
            health_port: Mutex::new(0),
            state: Arc::new(Mutex::new(TunnelState {
                status: TunnelStatus::Stopped,
                start_time: None,
                last_error: String::new(),
            })),
            stop_flag: Arc::new(AtomicBool::new(false)),
            task: Mutex::new(None),
        }
    }

    pub fn snapshot(&self) -> (TunnelStatus, Option<Instant>, String) {
        let s = self.state.lock().unwrap();
        (s.status.clone(), s.start_time, s.last_error.clone())
    }

    fn build_ssh_args(&self) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "-N".into(),
            "-o".into(), "StrictHostKeyChecking=accept-new".into(),
            "-o".into(), "ServerAliveInterval=30".into(),
            "-o".into(), "ExitOnForwardFailure=yes".into(),
            "-o".into(), "BatchMode=yes".into(),
        ];

        if !self.config.ssh_key.is_empty() {
            args.push("-i".into());
            let expanded = if self.config.ssh_key.starts_with("~/") {
                if let Some(home) = dirs::home_dir() {
                    format!("{}{}", home.to_string_lossy(), &self.config.ssh_key[1..])
                } else {
                    self.config.ssh_key.clone()
                }
            } else {
                self.config.ssh_key.clone()
            };
            args.push(expanded);
        }

        if self.config.ssh_port != 22 {
            args.push("-p".into());
            args.push(self.config.ssh_port.to_string());
        }

        match self.config.tunnel_type.as_str() {
            "local" => {
                let target = if self.config.remote_port > 0 {
                    format!("{}:{}", self.config.remote_host, self.config.remote_port)
                } else {
                    self.config.remote_host.clone()
                };
                args.push("-L".into());
                args.push(format!("{}:{}", self.config.local_port, target));
            }
            "remote" => {
                let target = if self.config.remote_port > 0 {
                    format!("{}:{}", self.config.remote_host, self.config.remote_port)
                } else {
                    self.config.remote_host.clone()
                };
                args.push("-R".into());
                args.push(format!("{}:{}", self.config.local_port, target));
            }
            "dynamic" => {
                args.push("-D".into());
                args.push(self.config.local_port.to_string());
            }
            _ => {}
        }

        let dest = if self.config.ssh_user.is_empty() {
            self.config.ssh_host.clone()
        } else {
            format!("{}@{}", self.config.ssh_user, self.config.ssh_host)
        };
        args.push(dest);
        args
    }
}

// ── manager ─────────────────────────────────────────────────────────────────

pub struct TunnelManager {
    pub tunnels: Vec<Tunnel>,
    config_path: PathBuf,
}

impl TunnelManager {
    pub fn new(config_path: Option<PathBuf>) -> Self {
        let config_path = config_path.unwrap_or_else(|| {
            let mut p = dirs::home_dir().expect("no home dir");
            p.push(CONFIG_DIR);
            p.push(CONFIG_FILE);
            p
        });
        let mut mgr = TunnelManager { tunnels: Vec::new(), config_path };
        mgr.load_config();
        mgr
    }

    fn load_config(&mut self) {
        if !self.config_path.exists() { return; }
        if let Ok(content) = std::fs::read_to_string(&self.config_path) {
            if let Ok(cfg) = serde_yaml::from_str::<ConfigFile>(&content) {
                self.tunnels = cfg.tunnels.into_iter().map(Tunnel::from_config).collect();
            }
        }
    }

    pub fn save_config(&self) -> anyhow::Result<()> {
        if let Some(parent) = self.config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tunnels: Vec<TunnelConfig> = self.tunnels.iter().map(|t| t.config.clone()).collect();
        let yaml = serde_yaml::to_string(&ConfigFile { tunnels })?;
        std::fs::write(&self.config_path, yaml)?;
        Ok(())
    }

    pub fn add_tunnel(&mut self, config: TunnelConfig) {
        self.tunnels.push(Tunnel::from_config(config));
    }

    pub fn remove_tunnel(&mut self, index: usize) -> Option<Tunnel> {
        if index < self.tunnels.len() {
            Some(self.tunnels.remove(index))
        } else {
            None
        }
    }

    // ── lifecycle ────────────────────────────────────────────────────────

    pub fn start(&self, index: usize) {
        if let Some(t) = self.tunnels.get(index) {
            start_tunnel(t);
        }
    }

    pub fn stop(&self, index: usize) {
        if let Some(t) = self.tunnels.get(index) {
            stop_tunnel(t);
        }
    }

    pub fn restart(&self, index: usize) {
        if let Some(t) = self.tunnels.get(index) {
            stop_tunnel(t);
            start_tunnel(t);
        }
    }

    pub fn start_all_enabled(&self) {
        for t in &self.tunnels {
            if t.config.enabled {
                start_tunnel(t);
            }
        }
    }

    pub fn stop_all(&self) {
        for t in &self.tunnels {
            stop_tunnel(t);
        }
    }

    // ── snapshot (for UI) ────────────────────────────────────────────────

    pub fn snapshot(&self, index: usize) -> Option<(TunnelStatus, Option<Instant>, String)> {
        self.tunnels.get(index).map(|t| t.snapshot())
    }

    pub fn status_summary(&self) -> String {
        let mut running = 0;
        for t in &self.tunnels {
            if let Ok(s) = t.state.lock() {
                if s.status == TunnelStatus::Running {
                    running += 1;
                }
            }
        }
        format!("Active: {running}/{}", self.tunnels.len())
    }
}

// ── process helpers ─────────────────────────────────────────────────────────

fn start_tunnel(t: &Tunnel) {
    {
        let mut task_lock = t.task.lock().unwrap();
        t.stop_flag.store(true, Ordering::SeqCst);
        if let Some(old) = task_lock.take() {
            old.abort();
        }
        *task_lock = None;
    }

    t.stop_flag.store(false, Ordering::SeqCst);

    {
        let mut s = t.state.lock().unwrap();
        s.status = TunnelStatus::Reconnecting { failures: 0, backoff: INITIAL_BACKOFF_SECS };
        s.start_time = None;
        s.last_error.clear();
    }
    *t.health_port.lock().unwrap() = t.config.local_port;

    let args = t.build_ssh_args();
    let state = t.state.clone();
    let stop_flag = t.stop_flag.clone();

    let handle = tokio::spawn(async move {
        // Async delay so the previous SSH process has time to release the port.
        // Does not block the UI thread or the manager lock.
        tokio::time::sleep(Duration::from_millis(300)).await;
        if stop_flag.load(Ordering::SeqCst) {
            return;
        }
        run_loop(args, stop_flag, state).await;
    });

    *t.task.lock().unwrap() = Some(handle);
}

fn stop_tunnel(t: &Tunnel) {
    t.stop_flag.store(true, Ordering::SeqCst);
    if let Some(handle) = t.task.lock().unwrap().take() {
        handle.abort();
    }
    let mut s = t.state.lock().unwrap();
    s.status = TunnelStatus::Stopped;
    s.start_time = None;
    s.last_error.clear();
}

// ── background run loop ─────────────────────────────────────────────────────

async fn run_loop(
    args: Vec<String>,
    stop_flag: Arc<AtomicBool>,
    state: Arc<Mutex<TunnelState>>,
) {
    let mut failures: u32 = 0;
    let mut backoff: u64 = INITIAL_BACKOFF_SECS;

    while !stop_flag.load(Ordering::SeqCst) {
        let mut child = match Command::new("ssh")
            .args(&args)
            .kill_on_drop(true)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                {
                    let mut s = state.lock().unwrap();
                    s.last_error = format!("spawn: {e}");
                    failures += 1;
                    s.status = TunnelStatus::Reconnecting { failures, backoff };
                }
                sleep_if_not_stopped(&stop_flag, backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF_SECS);
                continue;
            }
        };

        {
            let mut s = state.lock().unwrap();
            s.status = if failures == 0 {
                TunnelStatus::Running
            } else {
                TunnelStatus::Reconnecting { failures, backoff }
            };
            s.start_time = Some(Instant::now());
            s.last_error.clear();
        }

        let mut stderr = child.stderr.take().unwrap();
        let stderr_handle = tokio::spawn(async move {
            let mut buf = Vec::new();
            let _ = stderr.read_to_end(&mut buf).await;
            String::from_utf8_lossy(&buf).into_owned()
        });

        // Poll child status so we can check stop_flag while running
        let exit_status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {
                    if stop_flag.load(Ordering::SeqCst) {
                        let _ = child.kill().await;
                        stderr_handle.abort();
                        break None;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(_) => {
                    stderr_handle.abort();
                    break None;
                }
            }
        };

        // Drain stderr if we didn't abort it
        let err_text = if !stderr_handle.is_finished() {
            stderr_handle.await.unwrap_or_default()
        } else {
            String::new()
        };

        if stop_flag.load(Ordering::SeqCst) {
            break;
        }

        failures += 1;
        let msg = match exit_status {
            Some(status) => {
                if !err_text.trim().is_empty() {
                    err_text.trim().lines().last().unwrap_or("").to_string()
                } else {
                    format!("exit: {status:?}")
                }
            }
            None => "killed".to_string(),
        };

        {
            let mut s = state.lock().unwrap();
            s.status = TunnelStatus::Reconnecting { failures, backoff };
            s.last_error = msg.chars().take(200).collect();
        }

        sleep_if_not_stopped(&stop_flag, backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF_SECS);
    }

    let mut s = state.lock().unwrap();
    s.status = TunnelStatus::Stopped;
    s.start_time = None;
}

async fn sleep_if_not_stopped(stop_flag: &Arc<AtomicBool>, secs: u64) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if stop_flag.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// ── health checks ───────────────────────────────────────────────────────────

pub async fn health_check_loop(manager: Arc<Mutex<TunnelManager>>) {
    loop {
        tokio::time::sleep(Duration::from_secs(HEALTH_CHECK_INTERVAL_SECS)).await;
        let checks: Vec<(usize, u16)> = {
            let mgr = manager.lock().unwrap();
            mgr.tunnels
                .iter()
                .enumerate()
                .filter_map(|(idx, t)| {
                    let s = t.state.lock().unwrap();
                    if s.status == TunnelStatus::Running {
                        let port = *t.health_port.lock().unwrap();
                        if port > 0 { Some((idx, port)) } else { None }
                    } else {
                        None
                    }
                })
                .collect()
        };
        for (idx, port) in checks {
            let healthy = check_port(port).await;
            if !healthy {
                let mgr = manager.lock().unwrap();
                // Re-check status: user may have manually stopped the tunnel
                // between when we collected the check list and now.
                if let Some(t) = mgr.tunnels.get(idx) {
                    let still_running = t.state.lock()
                        .map(|s| s.status == TunnelStatus::Running)
                        .unwrap_or(false);
                    if still_running {
                        mgr.start(idx);
                    }
                }
            }
        }
    }
}

async fn check_port(port: u16) -> bool {
    tokio::time::timeout(Duration::from_secs(3), async {
        TcpStream::connect(("127.0.0.1", port)).await.map(|s| { drop(s); true }).unwrap_or(false)
    })
    .await
    .unwrap_or(false)
}

pub async fn force_health_check(manager: Arc<Mutex<TunnelManager>>) -> Vec<(String, bool)> {
    let ports: Vec<(String, u16)> = {
        let mgr = manager.lock().unwrap();
        mgr.tunnels
            .iter()
            .filter_map(|t| {
                let s = t.state.lock().unwrap();
                if s.status == TunnelStatus::Running {
                    let port = *t.health_port.lock().unwrap();
                    if port > 0 {
                        Some((t.config.name.clone(), port))
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect()
    };
    let mut results = Vec::new();
    for (name, port) in ports {
        let healthy = check_port(port).await;
        results.push((name, healthy));
    }
    results
}
