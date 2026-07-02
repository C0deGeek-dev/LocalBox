//! No-think proxy lifecycle management.
//!
//! The decisions come from the shared tri-state machine
//! (`localx_llama_runtime::health`); this module owns the *orchestration* the
//! launcher runs before routing an agent through the proxy, with every effect
//! behind [`ProxyOps`] so the whole flow is testable without sockets:
//!
//! 1. **Reap-before-probe** — an orphaned proxy whose upstream is dead still
//!    answers `/health`, so it is reaped before any target comparison (else it
//!    reads as a live match and strands every request behind a bare 502).
//! 2. **Reuse on match** — unless gateway logs were requested, in which case
//!    an owned proxy is restarted and a foreign one is refused (this shell
//!    cannot capture another process's output).
//! 3. **Repoint on mismatch** — tear the mismatched proxy down and start
//!    fresh at the wanted target, never fail the launch into a silent
//!    direct-route fallback.
//! 4. **Kill-stale-listener-first** — an unverifiable listener on the port is
//!    killed before binding, so it cannot compete with the new proxy under
//!    Windows `SO_REUSEADDR`.
//! 5. **Owned vs any teardown** — stopping *this* handle is different from
//!    `llm-stop`'s reap-anything-on-the-port; a proxy must never outlive a
//!    full teardown.

use serde::{Deserialize, Serialize};

pub use localx_llama_runtime::{HealthState, ProxyAction, ProxyTarget};

/// What a proxy reports from `/health`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyHealth {
    /// Health status label.
    #[serde(default)]
    pub status: String,
    /// The upstream host the proxy forwards to.
    #[serde(default)]
    pub target_host: String,
    /// The upstream port the proxy forwards to.
    #[serde(default)]
    pub target_port: u16,
}

/// Parse a `/health` body. A non-JSON body still counts as "up" (the legacy
/// proxy replied with a bare string), with no verifiable target.
#[must_use]
pub fn parse_proxy_health(body: &str) -> ProxyHealth {
    serde_json::from_str(body).unwrap_or_else(|_| ProxyHealth {
        status: body.trim().to_string(),
        target_host: String::new(),
        target_port: 0,
    })
}

/// Everything `ensure_proxy` needs to know about the wanted proxy.
#[derive(Debug, Clone)]
pub struct EnsureProxyConfig {
    pub listen_host: String,
    pub listen_port: u16,
    pub target_host: String,
    pub target_port: u16,
    /// Whether gateway logs were requested (forces an owned restart and
    /// refuses a foreign matching proxy).
    pub logs_requested: bool,
    /// The PID of a proxy this session started earlier, when known.
    pub owned_pid: Option<u32>,
    /// Ready-poll attempts after a start (150ms apart; ~10s at the default).
    pub ready_attempts: u32,
    /// Bearer key forwarding requests must carry (the LAN-gateway posture);
    /// `None` for the loopback agent proxy.
    pub api_key: Option<String>,
}

impl EnsureProxyConfig {
    /// A loopback proxy config with the standard poll budget.
    #[must_use]
    pub fn new(listen_port: u16, target_port: u16) -> Self {
        Self {
            listen_host: "127.0.0.1".to_string(),
            listen_port,
            target_host: "127.0.0.1".to_string(),
            target_port,
            logs_requested: false,
            owned_pid: None,
            ready_attempts: 66,
            api_key: None,
        }
    }
}

/// The effects the lifecycle needs — implemented over real sockets/processes
/// by the app, and by a script in tests.
pub trait ProxyOps {
    /// `GET /health` on the loopback proxy port; `None` when unreachable.
    fn health(&mut self, listen_port: u16) -> Option<ProxyHealth>;
    /// Whether something is listening on the (loopback) port.
    fn port_listening(&mut self, port: u16) -> bool;
    /// The PIDs listening on a local port (socket→PID).
    fn listener_pids(&mut self, port: u16) -> Vec<u32>;
    /// Force-kill a process.
    fn kill(&mut self, pid: u32);
    /// Start the proxy; returns its PID.
    ///
    /// # Errors
    /// A human-readable reason when the proxy cannot start.
    fn start(&mut self, config: &EnsureProxyConfig) -> Result<u32, String>;
    /// Sleep between poll attempts / after kills.
    fn sleep_ms(&mut self, ms: u64);
}

/// A proxy-lifecycle failure.
#[derive(Debug, thiserror::Error)]
pub enum ProxyLifecycleError {
    /// A matching foreign proxy blocks a logs-capturing start.
    #[error(
        "no-think proxy port {port} is already serving target {target}, but gateway logs \
         were requested and this session does not own that process. Stop it with llm-stop \
         or free the port, then start Serve again."
    )]
    ForeignMatchingProxy { port: u16, target: String },
    /// The proxy could not start.
    #[error("failed to start the no-think proxy: {0}")]
    StartFailed(String),
    /// The proxy started but never became ready for the wanted target.
    #[error("no-think proxy did not become ready on 127.0.0.1:{port} for target {target}")]
    NeverReady { port: u16, target: String },
}

/// What `ensure_proxy` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnsureOutcome {
    /// The serving proxy's PID, when a fresh one was started (`None` = reused).
    pub started_pid: Option<u32>,
    /// A dead-upstream orphan was reaped before the target probe.
    pub reaped_stale: bool,
    /// A mismatched proxy was torn down and repointed.
    pub repointed: bool,
    /// An unverifiable listener was killed before binding.
    pub killed_stale_listener: bool,
}

/// The tri-state target test: `Some(true)` match, `Some(false)` mismatch,
/// `None` unverifiable (no listener, or `/health` unreadable).
fn target_matches(
    ops: &mut dyn ProxyOps,
    listen_port: u16,
    target_host: &str,
    target_port: u16,
) -> Option<bool> {
    let health = ops.health(listen_port)?;
    if health.target_port == 0 {
        return None;
    }
    Some(health.target_host == target_host && health.target_port == target_port)
}

/// Reap an orphaned proxy whose upstream is dead (it still answers `/health`,
/// so it must go before any target comparison). Returns whether it reaped.
fn reap_stale(ops: &mut dyn ProxyOps, listen_port: u16) -> bool {
    let Some(health) = ops.health(listen_port) else {
        return false;
    };
    if health.target_port == 0 || ops.port_listening(health.target_port) {
        return false;
    }
    let pids = ops.listener_pids(listen_port);
    let Some(pid) = pids.first().copied() else {
        return false;
    };
    ops.kill(pid);
    ops.sleep_ms(300);
    true
}

/// Ensure a proxy serves `target` on `listen_port`, applying the lifecycle
/// rules above. Returns what was done.
///
/// # Errors
/// [`ProxyLifecycleError`] when a foreign matching proxy blocks a
/// logs-capturing start, the start fails, or readiness times out.
pub fn ensure_proxy(
    ops: &mut dyn ProxyOps,
    config: &EnsureProxyConfig,
) -> Result<EnsureOutcome, ProxyLifecycleError> {
    let target = format!("{}:{}", config.target_host, config.target_port);
    let mut outcome = EnsureOutcome {
        started_pid: None,
        reaped_stale: false,
        repointed: false,
        killed_stale_listener: false,
    };

    // (1) Reap-before-probe.
    outcome.reaped_stale = reap_stale(ops, config.listen_port);

    // (2)/(3) The tri-state target test drives the flow.
    match target_matches(
        ops,
        config.listen_port,
        &config.target_host,
        config.target_port,
    ) {
        Some(true) => {
            if !config.logs_requested {
                return Ok(outcome); // reuse as-is
            }
            match config.owned_pid {
                Some(pid) => {
                    // Restart the owned proxy so gateway logs are captured.
                    ops.kill(pid);
                    ops.sleep_ms(300);
                }
                None => {
                    return Err(ProxyLifecycleError::ForeignMatchingProxy {
                        port: config.listen_port,
                        target,
                    });
                }
            }
        }
        Some(false) => {
            // Repoint: tear the mismatched proxy down, then start fresh.
            outcome.repointed = true;
            if let Some(pid) = config.owned_pid {
                ops.kill(pid);
                ops.sleep_ms(300);
            }
        }
        None => {}
    }

    // (4) Kill any remaining (unverifiable/mismatched-foreign) listener before
    // binding, so it cannot compete under SO_REUSEADDR.
    let stale = ops.listener_pids(config.listen_port);
    if !stale.is_empty() {
        for pid in stale {
            ops.kill(pid);
        }
        ops.sleep_ms(300);
        outcome.killed_stale_listener = true;
    }

    // Start and wait until the proxy answers for the wanted target.
    let pid = ops
        .start(config)
        .map_err(ProxyLifecycleError::StartFailed)?;
    outcome.started_pid = Some(pid);

    for _ in 0..config.ready_attempts {
        ops.sleep_ms(150);
        if target_matches(
            ops,
            config.listen_port,
            &config.target_host,
            config.target_port,
        ) == Some(true)
        {
            return Ok(outcome);
        }
    }
    ops.kill(pid);
    Err(ProxyLifecycleError::NeverReady {
        port: config.listen_port,
        target,
    })
}

/// Stop this session's owned proxy handle only (a foreign proxy survives).
pub fn stop_owned(ops: &mut dyn ProxyOps, owned_pid: Option<u32>) {
    if let Some(pid) = owned_pid {
        ops.kill(pid);
    }
}

/// Kill whatever listens on the proxy port, regardless of owner — the
/// `llm-stop` teardown, so a proxy never outlives a full stop. Returns whether
/// anything was killed.
pub fn stop_any_on_port(ops: &mut dyn ProxyOps, listen_port: u16) -> bool {
    let mut pids = ops.listener_pids(listen_port);
    pids.sort_unstable();
    pids.dedup();
    if pids.is_empty() {
        return false;
    }
    for pid in pids {
        ops.kill(pid);
    }
    true
}

/// Parse the PIDs listening on `port` from `netstat -ano` output (Windows).
#[must_use]
pub fn parse_netstat_listeners(output: &str, port: u16) -> Vec<u32> {
    let needle = format!(":{port}");
    let mut pids: Vec<u32> = output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let proto = fields.next()?;
            if !proto.eq_ignore_ascii_case("tcp") {
                return None;
            }
            let local = fields.next()?;
            if !local.ends_with(&needle) {
                return None;
            }
            let rest: Vec<&str> = fields.collect();
            // TCP <local> <remote> LISTENING <pid>
            let state = rest.get(rest.len().checked_sub(2)?)?;
            if !state.eq_ignore_ascii_case("listening") {
                return None;
            }
            rest.last()?.parse::<u32>().ok()
        })
        .collect();
    pids.sort_unstable();
    pids.dedup();
    pids
}

/// Parse PIDs from `lsof -t` output (one PID per line; Unix).
#[must_use]
pub fn parse_lsof_pids(output: &str) -> Vec<u32> {
    let mut pids: Vec<u32> = output
        .lines()
        .filter_map(|line| line.trim().parse::<u32>().ok())
        .collect();
    pids.sort_unstable();
    pids.dedup();
    pids
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A scripted world: which ports listen, what /health says, what start does.
    struct MockOps {
        /// port -> listener pids.
        listeners: BTreeMap<u16, Vec<u32>>,
        /// port -> health payload.
        health: BTreeMap<u16, ProxyHealth>,
        /// What `start` returns; on success the mock wires the new proxy up.
        start_result: Result<u32, String>,
        /// The target the STARTED proxy will report (readiness simulation).
        started_reports: Option<ProxyHealth>,
        killed: Vec<u32>,
        started: u32,
        slept_ms: u64,
    }

    impl MockOps {
        fn new() -> Self {
            Self {
                listeners: BTreeMap::new(),
                health: BTreeMap::new(),
                start_result: Ok(4242),
                started_reports: None,
                killed: Vec::new(),
                started: 0,
                slept_ms: 0,
            }
        }

        fn with_proxy(mut self, listen: u16, pid: u32, target_port: u16) -> Self {
            self.listeners.insert(listen, vec![pid]);
            self.health.insert(
                listen,
                ProxyHealth {
                    status: "ok".to_string(),
                    target_host: "127.0.0.1".to_string(),
                    target_port,
                },
            );
            self
        }

        fn with_upstream(mut self, port: u16) -> Self {
            self.listeners.entry(port).or_default();
            // an entry with no PIDs still marks the port as listening below
            self.listeners.get_mut(&port).unwrap().push(0);
            self
        }
    }

    impl ProxyOps for MockOps {
        fn health(&mut self, listen_port: u16) -> Option<ProxyHealth> {
            // Health answers only while something listens on the port.
            if !self.port_listening(listen_port) {
                return None;
            }
            self.health.get(&listen_port).cloned()
        }
        fn port_listening(&mut self, port: u16) -> bool {
            self.listeners.get(&port).is_some_and(|p| !p.is_empty())
        }
        fn listener_pids(&mut self, port: u16) -> Vec<u32> {
            self.listeners
                .get(&port)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|p| *p != 0)
                .collect()
        }
        fn kill(&mut self, pid: u32) {
            self.killed.push(pid);
            for pids in self.listeners.values_mut() {
                pids.retain(|p| *p != pid);
            }
            let dead: Vec<u16> = self
                .listeners
                .iter()
                .filter(|(_, pids)| pids.is_empty())
                .map(|(port, _)| *port)
                .collect();
            for port in dead {
                self.health.remove(&port);
            }
        }
        fn start(&mut self, config: &EnsureProxyConfig) -> Result<u32, String> {
            self.started += 1;
            let pid = self.start_result.clone()?;
            self.listeners
                .entry(config.listen_port)
                .or_default()
                .push(pid);
            let report = self.started_reports.clone().unwrap_or(ProxyHealth {
                status: "ok".to_string(),
                target_host: config.target_host.clone(),
                target_port: config.target_port,
            });
            self.health.insert(config.listen_port, report);
            Ok(pid)
        }
        fn sleep_ms(&mut self, ms: u64) {
            self.slept_ms += ms;
        }
    }

    #[test]
    fn a_matching_live_proxy_is_reused() {
        let mut ops = MockOps::new()
            .with_proxy(11435, 77, 8080)
            .with_upstream(8080);
        let outcome = ensure_proxy(&mut ops, &EnsureProxyConfig::new(11435, 8080)).unwrap();
        assert_eq!(outcome.started_pid, None, "reused, not restarted");
        assert!(ops.killed.is_empty());
        assert_eq!(ops.started, 0);
    }

    #[test]
    fn a_dead_upstream_orphan_is_reaped_before_the_target_test() {
        // Proxy answers /health for target 8080, but nothing listens on 8080:
        // without the reap this would read as a live MATCH and be reused.
        let mut ops = MockOps::new().with_proxy(11435, 77, 8080);
        let outcome = ensure_proxy(&mut ops, &EnsureProxyConfig::new(11435, 8080)).unwrap();
        assert!(outcome.reaped_stale, "orphan reaped first");
        assert!(ops.killed.contains(&77));
        assert_eq!(outcome.started_pid, Some(4242), "fresh proxy started");
    }

    #[test]
    fn a_mismatched_proxy_is_repointed_not_failed() {
        // Live proxy pointed at 9090 (upstream alive there); we want 8080.
        let mut ops = MockOps::new()
            .with_proxy(11435, 77, 9090)
            .with_upstream(9090)
            .with_upstream(8080);
        let mut config = EnsureProxyConfig::new(11435, 8080);
        config.owned_pid = Some(77);
        let outcome = ensure_proxy(&mut ops, &config).unwrap();
        assert!(outcome.repointed);
        assert!(ops.killed.contains(&77), "mismatched proxy torn down");
        assert_eq!(outcome.started_pid, Some(4242));
    }

    #[test]
    fn logs_requested_restarts_an_owned_match_and_refuses_a_foreign_one() {
        let mut config = EnsureProxyConfig::new(11435, 8080);
        config.logs_requested = true;

        // Foreign matching proxy: refused with the remedy.
        let mut ops = MockOps::new()
            .with_proxy(11435, 77, 8080)
            .with_upstream(8080);
        let err = ensure_proxy(&mut ops, &config).unwrap_err();
        assert!(matches!(
            err,
            ProxyLifecycleError::ForeignMatchingProxy { port: 11435, .. }
        ));
        assert!(err.to_string().contains("llm-stop"));

        // Owned matching proxy: restarted so logs are captured.
        let mut ops = MockOps::new()
            .with_proxy(11435, 77, 8080)
            .with_upstream(8080);
        config.owned_pid = Some(77);
        let outcome = ensure_proxy(&mut ops, &config).unwrap();
        assert!(ops.killed.contains(&77));
        assert_eq!(outcome.started_pid, Some(4242));
    }

    #[test]
    fn an_unverifiable_listener_is_killed_before_binding() {
        // Something listens on the port but /health is unreadable (no health
        // entry): kill it first so it cannot compete under SO_REUSEADDR.
        let mut ops = MockOps::new().with_upstream(8080);
        ops.listeners.insert(11435, vec![55]);
        let outcome = ensure_proxy(&mut ops, &EnsureProxyConfig::new(11435, 8080)).unwrap();
        assert!(outcome.killed_stale_listener);
        assert!(ops.killed.contains(&55));
        assert_eq!(outcome.started_pid, Some(4242));
    }

    #[test]
    fn readiness_timeout_stops_the_started_proxy_and_errors() {
        let mut ops = MockOps::new().with_upstream(8080);
        // The started proxy reports the WRONG target forever.
        ops.started_reports = Some(ProxyHealth {
            status: "ok".to_string(),
            target_host: "127.0.0.1".to_string(),
            target_port: 9999,
        });
        let mut config = EnsureProxyConfig::new(11435, 8080);
        config.ready_attempts = 3;
        let err = ensure_proxy(&mut ops, &config).unwrap_err();
        assert!(matches!(err, ProxyLifecycleError::NeverReady { .. }));
        assert!(ops.killed.contains(&4242), "the failed start is torn down");
    }

    #[test]
    fn owned_vs_any_teardown_semantics() {
        // stop_owned kills only this session's handle.
        let mut ops = MockOps::new().with_proxy(11435, 77, 8080);
        stop_owned(&mut ops, Some(77));
        assert_eq!(ops.killed, vec![77]);
        stop_owned(&mut ops, None);
        assert_eq!(ops.killed, vec![77], "no owned handle, no kill");

        // stop_any_on_port reaps every owner — llm-stop's guarantee.
        let mut ops = MockOps::new();
        ops.listeners.insert(11435, vec![77, 88, 88]);
        assert!(stop_any_on_port(&mut ops, 11435));
        assert_eq!(ops.killed, vec![77, 88]);
        assert!(!stop_any_on_port(&mut ops, 11435), "already clear");
    }

    #[test]
    fn health_body_parses_json_and_legacy_string() {
        let health =
            parse_proxy_health(r#"{"status":"ok","target_host":"127.0.0.1","target_port":8080}"#);
        assert_eq!(health.target_port, 8080);
        let legacy = parse_proxy_health("OK");
        assert_eq!(legacy.status, "OK");
        assert_eq!(legacy.target_port, 0, "no verifiable target");
    }

    #[test]
    fn netstat_and_lsof_parsers_extract_listener_pids() {
        let netstat = "\
  Proto  Local Address          Foreign Address        State           PID
  TCP    0.0.0.0:11435          0.0.0.0:0              LISTENING       1234
  TCP    127.0.0.1:11435        0.0.0.0:0              LISTENING       1234
  TCP    127.0.0.1:11435        127.0.0.1:5000         ESTABLISHED     999
  TCP    0.0.0.0:8080           0.0.0.0:0              LISTENING       4321
  UDP    0.0.0.0:11435          *:*                                    777
";
        assert_eq!(parse_netstat_listeners(netstat, 11435), vec![1234]);
        assert_eq!(parse_netstat_listeners(netstat, 8080), vec![4321]);
        assert!(parse_netstat_listeners(netstat, 9999).is_empty());
        assert_eq!(parse_lsof_pids("4242\n4242\n77\n"), vec![77, 4242]);
        assert!(parse_lsof_pids("garbage\n").is_empty());
    }
}
