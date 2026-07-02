//! Launch orchestration: execute a resolved launch plan end to end —
//! download-on-miss → server spawn → readiness → proxy lifecycle → smoke
//! test → agent — plus stop-everything and a health status report.
//!
//! The plan itself is resolved read-only by the launcher library; this module
//! is the one place its effects actually happen.

use std::path::{Path, PathBuf};

use localbox_launcher::launcher::LlamaLauncher;
use localbox_launcher::orchestrate::{LaunchPlan, LaunchRequest};
use localbox_launcher::proxy::{ensure_proxy, stop_any_on_port, ProxyLifecycleError};
use localbox_launcher::smoke::{evaluate_smoke_reply, format_smoke_failure};
use localx_llama_core::{BackendSession, Launcher, LauncherError};
use localx_llama_runtime::health::{classify_health, health_description, remediation};
use localx_llama_runtime::net::is_port_listening;

use crate::exec::{run_interactive, spawn_server, EnvGuard, LiveProxyOps};
use crate::fetch::{download_with_resume, hf_download_url, FetchError};

/// What runs after the model is up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    /// Claude Code through the no-think proxy.
    Claude,
    /// LocalPilot (writes `.localpilot.toml` in the working directory).
    LocalPilot,
    /// Codex CLI.
    Codex,
    /// No agent: leave the server (and proxy) serving.
    ServeOnly,
}

impl AgentKind {
    /// The command this agent kind launches, when any.
    #[must_use]
    pub fn program(self) -> Option<&'static str> {
        match self {
            Self::Claude => Some("claude"),
            Self::LocalPilot => Some("localpilot"),
            Self::Codex => Some("codex"),
            Self::ServeOnly => None,
        }
    }
}

/// A live-launch failure, in plain user-facing terms.
#[derive(Debug, thiserror::Error)]
pub enum LiveError {
    /// Resolution failed (model, binary, port, ...).
    #[error("{0}")]
    Launcher(#[from] LauncherError),
    /// The model file could not be downloaded.
    #[error("{0}")]
    Fetch(#[from] FetchError),
    /// The no-think proxy could not be brought up.
    #[error("{0}")]
    Proxy(#[from] ProxyLifecycleError),
    /// The server process failed to start or become ready.
    #[error("the model server did not start: {0}")]
    Server(String),
    /// The model answered the smoke test with degenerate output.
    #[error("{0}")]
    Smoke(String),
    /// The agent could not be launched.
    #[error("could not start {agent}: {reason}")]
    Agent { agent: String, reason: String },
    /// Local file I/O failed.
    #[error("{0}")]
    Io(String),
}

/// What a live launch brought up.
#[derive(Debug, Clone, Default)]
pub struct LaunchOutcome {
    /// The spawned server PID.
    pub server_pid: Option<u32>,
    /// A freshly started proxy PID (`None` = reused or direct).
    pub proxy_pid: Option<u32>,
    /// The smoke reply's visible text (empty when the smoke was skipped).
    pub smoke_text: String,
}

/// Whether the plan routes the agent through the no-think proxy (the
/// endpoint origin points at the proxy listen port rather than the server).
#[must_use]
pub fn uses_proxy(plan: &LaunchPlan) -> bool {
    plan.base_url
        .ends_with(&format!(":{}", plan.proxy.listen_port))
}

/// The server log path for a launch.
#[must_use]
pub fn server_log_path(home: &Path, server_port: u16) -> PathBuf {
    home.join(".local-llm")
        .join("logs")
        .join(format!("llama-server-{server_port}.log"))
}

/// The GGUF download URL for the launched quant, from the catalog definition.
///
/// # Errors
/// [`LauncherError::Unavailable`] when the catalog names no file to fetch.
pub fn gguf_url(
    launcher: &LlamaLauncher,
    plan: &LaunchPlan,
    request: &LaunchRequest,
) -> Result<String, LauncherError> {
    let def = launcher.model_def(&plan.key)?;
    let quant_key = match request.quant.as_deref().or(def.quant.as_deref()) {
        Some(q) => Some(
            localx_llama_core::model::resolve_quant_key(&def, q)
                .map_err(|e| LauncherError::Unavailable(format!("quant for {}: {e}", plan.key)))?,
        ),
        None => None,
    };
    let file = quant_key
        .and_then(|k| def.quants.get(&k).map(|entry| entry.file.clone()))
        .or_else(|| def.file.clone())
        .filter(|f| !f.trim().is_empty())
        .ok_or_else(|| {
            LauncherError::Unavailable(format!("model {} names no GGUF file", plan.key))
        })?;
    Ok(hf_download_url(&def.repo, &file))
}

fn block_on<F: std::future::Future>(future: F) -> Result<F::Output, LiveError> {
    let runtime = tokio::runtime::Runtime::new().map_err(|e| LiveError::Io(e.to_string()))?;
    Ok(runtime.block_on(future))
}

async fn post_smoke(base_url: &str, model: &str) -> Result<String, String> {
    let body = serde_json::json!({
        "model": model,
        "max_tokens": 64,
        "messages": [{"role": "user", "content": "Reply with the single word: ready"}],
    });
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{base_url}/v1/messages"))
        .json(&body)
        .timeout(std::time::Duration::from_secs(120))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    response.text().await.map_err(|e| e.to_string())
}

/// Execute a resolved launch plan: download the model when missing, spawn
/// the server and wait for readiness, bring up the proxy when the plan
/// routes through it, smoke-test the reply path, then hand off to the agent
/// (environment applied for the session and restored afterwards).
///
/// # Errors
/// A [`LiveError`] naming the failed stage in plain language.
pub fn execute_launch(
    launcher: &LlamaLauncher,
    plan: &LaunchPlan,
    request: &LaunchRequest,
    agent: AgentKind,
    home: &Path,
) -> Result<LaunchOutcome, LiveError> {
    let mut outcome = LaunchOutcome::default();

    if !plan.gguf_downloaded {
        let url = gguf_url(launcher, plan, request)?;
        eprintln!("Downloading model ({url}) ...");
        let client = reqwest::Client::new();
        block_on(download_with_resume(&client, &url, &plan.gguf_path))??;
    }

    let binary = launcher.server_binary(request.mode, true)?;
    let log = server_log_path(home, plan.server_port);
    let child = spawn_server(&binary, &plan.argv, &log)
        .map_err(|e| LiveError::Server(format!("{}: {e}", binary.display())))?;
    outcome.server_pid = Some(child.id());

    if let Err(e) = launcher.wait_server(plan.server_port, 180) {
        return Err(LiveError::Server(format!(
            "{e} — the server log is at {}",
            log.display()
        )));
    }
    launcher.set_backend_session(&BackendSession {
        key: plan.key.clone(),
        mode: request.mode,
        port: plan.server_port,
        pid: outcome.server_pid,
    });

    if uses_proxy(plan) {
        let mut ops = LiveProxyOps::new(home);
        let ensured = ensure_proxy(&mut ops, &plan.proxy)?;
        outcome.proxy_pid = ensured.started_pid;

        let reply = block_on(post_smoke(&plan.base_url, &plan.key))?
            .map_err(|e| LiveError::Smoke(format!("the model did not answer: {e}")))?;
        let smoke = evaluate_smoke_reply(&reply);
        if !smoke.ok {
            return Err(LiveError::Smoke(format_smoke_failure(&smoke)));
        }
        outcome.smoke_text = smoke.visible_text;
    }

    if agent == AgentKind::LocalPilot {
        std::fs::write(".localpilot.toml", &plan.provider_toml)
            .map_err(|e| LiveError::Io(format!("could not write .localpilot.toml: {e}")))?;
    }

    if let Some(program) = agent.program() {
        let _env = EnvGuard::apply(&plan.env_plan);
        let status = run_interactive(program, &[]).map_err(|e| LiveError::Agent {
            agent: program.to_string(),
            reason: e.to_string(),
        })?;
        if !status.success() {
            eprintln!("{program} exited with {status}; the model is still serving.");
        }
    }

    Ok(outcome)
}

/// Whether a process name is a llama-server (any mode's fork included).
#[must_use]
pub fn is_server_process_name(name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    lowered
        .strip_suffix(".exe")
        .unwrap_or(&lowered)
        .starts_with("llama-server")
}

/// Stop every llama-server process and reap whatever serves the given
/// loopback ports (the no-think proxy, an embed server). Returns how many
/// processes were told to stop.
#[must_use]
pub fn stop_all(home: &Path, ports: &[u16]) -> usize {
    let mut stopped = 0;
    let mut system = sysinfo::System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    for process in system.processes().values() {
        if is_server_process_name(&process.name().to_string_lossy()) {
            process.kill();
            stopped += 1;
        }
    }
    let mut ops = LiveProxyOps::new(home);
    for &port in ports {
        if stop_any_on_port(&mut ops, port) {
            stopped += 1;
        }
    }
    stopped
}

/// A one-line serve health report: state, ports, and the remedy when down.
#[must_use]
pub fn status_report(proxy_port: u16, server_port: u16) -> String {
    let state = classify_health(
        is_port_listening(proxy_port),
        is_port_listening(server_port),
    );
    let description = health_description(state, proxy_port, server_port);
    let remedy = remediation(state);
    if remedy.is_empty() {
        description
    } else {
        format!("{description}\n  remedy: {remedy}")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use localbox_launcher::proxy::EnsureProxyConfig;

    fn plan_with(base_url: &str, listen_port: u16) -> LaunchPlan {
        LaunchPlan {
            key: "m".into(),
            context_key: String::new(),
            context_tokens: 65_536,
            gguf_path: PathBuf::from("m.gguf"),
            gguf_downloaded: true,
            vision_module: None,
            argv: vec![],
            server_port: 8080,
            proxy: EnsureProxyConfig::new(listen_port, 8080),
            base_url: base_url.to_string(),
            provider_toml: String::new(),
            env_plan: vec![],
            notes: vec![],
        }
    }

    #[test]
    fn proxy_use_is_read_off_the_endpoint_origin() {
        assert!(uses_proxy(&plan_with("http://127.0.0.1:11435", 11_435)));
        // Direct-to-server plans never touch the proxy lifecycle.
        assert!(!uses_proxy(&plan_with("http://127.0.0.1:8080", 11_435)));
    }

    #[test]
    fn server_process_matching_covers_forks_and_windows_suffix() {
        assert!(is_server_process_name("llama-server"));
        assert!(is_server_process_name("llama-server.exe"));
        assert!(is_server_process_name("LLAMA-SERVER.EXE"));
        assert!(!is_server_process_name("llama-bench.exe"));
        assert!(!is_server_process_name("localbox.exe"));
    }

    #[test]
    fn status_report_names_the_remedy_when_everything_is_down() {
        // Ports chosen from the dynamic range with nothing listening.
        let report = status_report(59_998, 59_999);
        assert!(!report.trim().is_empty());
        assert!(report.contains("remedy:"));
    }
}
