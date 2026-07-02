//! Process and socket effects: the live [`ProxyOps`] implementation, server
//! spawning, VRAM probing, the agent-environment guard, and interactive
//! agent launch.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;

use localbox_launcher::env::{EnvEnvelope, ProcessEnv};
use localbox_launcher::proxy::{
    parse_lsof_pids, parse_netstat_listeners, parse_proxy_health, EnsureProxyConfig, ProxyHealth,
    ProxyOps,
};
use localx_llama_runtime::net::is_port_listening;
use localx_llama_runtime::probe::parse_nvidia_smi_vram_gb;
use localx_llama_runtime::spawn::spawn_detached;

/// The user home directory (`USERPROFILE` on Windows, `HOME` elsewhere).
#[must_use]
pub fn home_dir() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var(var)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
}

/// Extract the body from a raw HTTP/1.1 response.
#[must_use]
pub fn http_body(raw: &str) -> Option<&str> {
    raw.split_once("\r\n\r\n").map(|(_, body)| body)
}

/// Whether a raw HTTP/1.1 response is a 200.
#[must_use]
pub fn http_is_ok(raw: &str) -> bool {
    raw.lines()
        .next()
        .is_some_and(|line| line.contains(" 200 "))
}

/// Blocking loopback `GET` returning the body on a 200 answer.
#[must_use]
pub fn loopback_get(port: u16, path: &str, timeout: Duration) -> Option<String> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, timeout).ok()?;
    stream.set_read_timeout(Some(timeout)).ok()?;
    stream.set_write_timeout(Some(timeout)).ok()?;
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).ok()?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw).ok()?;
    if http_is_ok(&raw) {
        http_body(&raw).map(str::to_string)
    } else {
        None
    }
}

/// Probe total GPU VRAM in whole GB via `nvidia-smi`; `0` when unavailable.
#[must_use]
pub fn probe_vram_gb() -> u32 {
    let output = Command::new("nvidia-smi")
        .args(["--query-gpu=memory.total", "--format=csv,noheader,nounits"])
        .output();
    match output {
        Ok(out) if out.status.success() => {
            parse_nvidia_smi_vram_gb(&String::from_utf8_lossy(&out.stdout))
                .and_then(|gb| u32::try_from(gb).ok())
                .unwrap_or(0)
        }
        _ => 0,
    }
}

/// Probe the GPU's name and memory for the guided launcher's hardware
/// banner: `nvidia-smi` first, then AMD tools (`rocm-smi` where present,
/// the CIM video-controller table on Windows). `None` = no GPU tool
/// answered — the banner then says so in plain words.
#[must_use]
pub fn probe_gpu() -> Option<localbox_tui::vocab::GpuInfo> {
    if let Ok(out) = Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output()
    {
        if out.status.success() {
            if let Some(info) = parse_gpu_name_vram(&String::from_utf8_lossy(&out.stdout)) {
                return Some(info);
            }
        }
    }
    if let Ok(out) = Command::new("rocm-smi")
        .args(["--showproductname"])
        .output()
    {
        if out.status.success() {
            if let Some(name) = parse_rocm_product_name(&String::from_utf8_lossy(&out.stdout)) {
                return Some(localbox_tui::vocab::GpuInfo { name, vram_gb: 0 });
            }
        }
    }
    #[cfg(windows)]
    {
        // Last resort for AMD on Windows (no rocm-smi): the CIM table.
        // Only reached when neither vendor tool answered, so the extra
        // shell spawn never slows an NVIDIA machine.
        if let Ok(out) = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "(Get-CimInstance Win32_VideoController).Name",
            ])
            .output()
        {
            if out.status.success() {
                if let Some(name) = parse_amd_controller_name(&String::from_utf8_lossy(&out.stdout))
                {
                    return Some(localbox_tui::vocab::GpuInfo { name, vram_gb: 0 });
                }
            }
        }
    }
    None
}

/// Parse `nvidia-smi --query-gpu=name,memory.total` CSV: first card wins.
fn parse_gpu_name_vram(raw: &str) -> Option<localbox_tui::vocab::GpuInfo> {
    let line = raw.lines().find(|l| !l.trim().is_empty())?;
    let (name, mib) = line.rsplit_once(',')?;
    let mib: f64 = mib.trim().parse().ok()?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(localbox_tui::vocab::GpuInfo {
        name: name.to_string(),
        vram_gb: (mib / 1024.0).round().max(0.0) as u32,
    })
}

/// Pull the card name out of `rocm-smi --showproductname` output.
fn parse_rocm_product_name(raw: &str) -> Option<String> {
    raw.lines()
        .find_map(|line| line.split_once("Card series:").map(|(_, name)| name))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

/// The first AMD-looking name in a video-controller listing.
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_amd_controller_name(raw: &str) -> Option<String> {
    raw.lines()
        .map(str::trim)
        .find(|line| line.contains("AMD") || line.contains("Radeon"))
        .map(str::to_string)
}

/// The PIDs listening on a loopback port, via the OS socket table
/// (`netstat -ano` on Windows, `lsof -t` elsewhere).
#[must_use]
pub fn os_listener_pids(port: u16) -> Vec<u32> {
    if cfg!(windows) {
        let Ok(out) = Command::new("netstat").args(["-ano"]).output() else {
            return Vec::new();
        };
        parse_netstat_listeners(&String::from_utf8_lossy(&out.stdout), port)
    } else {
        let Ok(out) = Command::new("lsof")
            .args(["-t", "-i", &format!("tcp:{port}"), "-sTCP:LISTEN"])
            .output()
        else {
            return Vec::new();
        };
        parse_lsof_pids(&String::from_utf8_lossy(&out.stdout))
    }
}

/// Force-kill a process by PID.
pub fn kill_pid(pid: u32) {
    let mut system = sysinfo::System::new();
    let target = sysinfo::Pid::from_u32(pid);
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[target]), true);
    if let Some(process) = system.process(target) {
        process.kill();
    }
}

/// The live [`ProxyOps`]: real sockets, the OS socket table, and the proxy
/// hosted by re-invoking this executable's `nothink-proxy` command.
#[derive(Debug)]
pub struct LiveProxyOps {
    /// Where proxy logs land (`<home>/.local-llm/logs`).
    pub log_dir: PathBuf,
}

impl LiveProxyOps {
    /// Ops logging under `<home>/.local-llm/logs`.
    #[must_use]
    pub fn new(home: &Path) -> Self {
        Self {
            log_dir: home.join(".local-llm").join("logs"),
        }
    }
}

impl ProxyOps for LiveProxyOps {
    fn health(&mut self, listen_port: u16) -> Option<ProxyHealth> {
        loopback_get(listen_port, "/health", Duration::from_secs(2))
            .map(|body| parse_proxy_health(&body))
    }

    fn port_listening(&mut self, port: u16) -> bool {
        is_port_listening(port)
    }

    fn listener_pids(&mut self, port: u16) -> Vec<u32> {
        os_listener_pids(port)
    }

    fn kill(&mut self, pid: u32) {
        kill_pid(pid);
    }

    fn start(&mut self, config: &EnsureProxyConfig) -> Result<u32, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&self.log_dir).map_err(|e| e.to_string())?;
        let log = self.log_dir.join("no-think-proxy.log");
        let mut args = vec![
            "nothink-proxy".to_string(),
            "--listen".to_string(),
            config.listen_port.to_string(),
            "--listen-host".to_string(),
            config.listen_host.clone(),
            "--target-host".to_string(),
            config.target_host.clone(),
            "--target-port".to_string(),
            config.target_port.to_string(),
        ];
        if let Some(key) = &config.api_key {
            args.push("--api-key".to_string());
            args.push(key.clone());
        }
        let child = spawn_detached(&exe.to_string_lossy(), &args, None, Some(&log))
            .map_err(|e| e.to_string())?;
        Ok(child.id())
    }

    fn sleep_ms(&mut self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

/// Applies an env plan and restores the pre-launch environment on drop, so
/// an agent session can never leak launch variables into the parent shell's
/// process even on an error path.
#[derive(Debug)]
pub struct EnvGuard {
    envelope: EnvEnvelope,
}

impl EnvGuard {
    /// Save the current environment, then apply `plan`.
    #[must_use]
    pub fn apply(plan: &[(&'static str, String)]) -> Self {
        let mut env = ProcessEnv;
        let envelope = EnvEnvelope::save(&env);
        EnvEnvelope::apply(&mut env, plan);
        Self { envelope }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        let mut env = ProcessEnv;
        self.envelope.restore(&mut env);
    }
}

/// Spawn an interactive program (inherited stdio) and wait for it. On
/// Windows a plain-name miss retries through `cmd /c`, which resolves the
/// `.cmd`/`.bat` shims Node-based CLIs install.
///
/// # Errors
/// The underlying spawn error when the program cannot be started either way.
pub fn run_interactive(program: &str, args: &[String]) -> std::io::Result<ExitStatus> {
    let direct = Command::new(program)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();
    match direct {
        Err(e) if should_retry_via_cmd(program, e.kind()) => {
            let mut shim_args = vec!["/c".to_string(), program.to_string()];
            shim_args.extend_from_slice(args);
            Command::new("cmd")
                .args(&shim_args)
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()
        }
        other => other,
    }
}

/// Whether a spawn miss should retry through the Windows `cmd /c` shim.
#[must_use]
pub fn should_retry_via_cmd(program: &str, kind: std::io::ErrorKind) -> bool {
    cfg!(windows)
        && kind == std::io::ErrorKind::NotFound
        && !program.contains(['/', '\\'])
        && !program.to_ascii_lowercase().ends_with(".exe")
}

/// Spawn a llama-server with the resolved argv, logging to `log_path`, cwd at
/// the binary's directory (its DLLs/`.so`s live next to it).
///
/// # Errors
/// The spawn error when the server cannot start.
pub fn spawn_server(
    binary: &Path,
    argv: &[String],
    log_path: &Path,
) -> std::io::Result<std::process::Child> {
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    spawn_detached(
        &binary.to_string_lossy(),
        argv,
        binary.parent(),
        Some(log_path),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn http_helpers_parse_status_and_body() {
        let raw = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"a\":1}";
        assert!(http_is_ok(raw));
        assert_eq!(http_body(raw), Some("{\"a\":1}"));
        let missing = "HTTP/1.1 404 Not Found\r\n\r\nnope";
        assert!(!http_is_ok(missing));
    }

    #[test]
    fn cmd_shim_retry_is_windows_plain_names_only() {
        let not_found = std::io::ErrorKind::NotFound;
        assert_eq!(should_retry_via_cmd("claude", not_found), cfg!(windows));
        // Paths and explicit exes never reroute through the shell.
        assert!(!should_retry_via_cmd("C:\\tools\\claude", not_found));
        assert!(!should_retry_via_cmd("./claude", not_found));
        assert!(!should_retry_via_cmd("claude.exe", not_found));
        // Only a missing binary retries; a real failure surfaces.
        assert!(!should_retry_via_cmd(
            "claude",
            std::io::ErrorKind::PermissionDenied
        ));
    }

    #[test]
    fn gpu_probe_parsers_read_vendor_tool_output() {
        // nvidia-smi CSV: name may itself contain no comma; MiB → whole GB.
        let nvidia = parse_gpu_name_vram("NVIDIA GeForce RTX 4090, 24564\n").unwrap();
        assert_eq!(nvidia.name, "NVIDIA GeForce RTX 4090");
        assert_eq!(nvidia.vram_gb, 24);
        assert!(parse_gpu_name_vram("\n").is_none());
        assert!(parse_gpu_name_vram("garbage without commas\n").is_none());

        let rocm = "========== Product Info ==========\nGPU[0] : Card series: Radeon RX 7900 XTX\n";
        assert_eq!(parse_rocm_product_name(rocm).unwrap(), "Radeon RX 7900 XTX");
        assert!(parse_rocm_product_name("no card line").is_none());

        let cim = "Name\n----\nMicrosoft Basic Display\nAMD Radeon RX 7800 XT\n";
        assert_eq!(
            parse_amd_controller_name(cim).unwrap(),
            "AMD Radeon RX 7800 XT"
        );
        assert!(parse_amd_controller_name("Intel Arc A770").is_none());
    }

    #[test]
    fn env_guard_restores_on_drop_even_for_previously_unset_vars() {
        // A canonical envelope var that is unset in the test environment.
        std::env::remove_var("LOCALBOX_CONTEXT_KEY");
        {
            let _guard = EnvGuard::apply(&[("LOCALBOX_CONTEXT_KEY", "64k".to_string())]);
            assert_eq!(std::env::var("LOCALBOX_CONTEXT_KEY").unwrap(), "64k");
        }
        assert!(std::env::var("LOCALBOX_CONTEXT_KEY").is_err());
    }
}
