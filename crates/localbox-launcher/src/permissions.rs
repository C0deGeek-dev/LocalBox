//! Permission-skip and bypass gating — fail-closed, persisted, never
//! inherited.
//!
//! Permission prompts are the human-in-the-loop that breaks prompt-injection
//! and runaway tool calls, which matter MORE with smaller, less-aligned local
//! models. So skipping them — and every agent's bypass mode — is a conscious,
//! persisted decision:
//!
//! - Resolution order: environment override → persisted per-machine setting →
//!   a one-time first-run prompt that defaults **OFF**.
//! - A non-interactive session never silently enables anything and never
//!   persists a choice.
//! - A read-only resolve (preview/DryRun) answers the safe default without
//!   prompting or persisting.
//! - An explicit "no" persists a literal `false` — distinguishable from
//!   "never asked" forever after.

use std::path::PathBuf;

use serde_json::{Map, Value};

use crate::env::EnvStore;

/// The three per-agent gates, each with its env override, persisted setting,
/// and the flag/config it controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentGate {
    /// Claude Code `--dangerously-skip-permissions`.
    ClaudeSkipPermissions,
    /// LocalPilot bypass — written to `.localpilot.toml` as
    /// `[permissions] profile = "bypass"`, never passed on argv (the REPL
    /// takes no `--bypass` flag; clap would abort the launch).
    LocalPilotBypass,
    /// Codex `--dangerously-bypass-approvals-and-sandbox` — never defaulted.
    CodexBypass,
}

impl AgentGate {
    /// The environment variable that overrides this gate for one launch.
    #[must_use]
    pub fn env_var(self) -> &'static str {
        match self {
            AgentGate::ClaudeSkipPermissions => "LOCAL_LLM_SKIP_PERMISSIONS",
            AgentGate::LocalPilotBypass => "LOCAL_LLM_LOCALPILOT_BYPASS",
            AgentGate::CodexBypass => "LOCAL_LLM_CODEX_BYPASS",
        }
    }

    /// The persisted per-machine setting name.
    #[must_use]
    pub fn setting_name(self) -> &'static str {
        match self {
            AgentGate::ClaudeSkipPermissions => "LocalModelSkipPermissions",
            AgentGate::LocalPilotBypass => "LocalPilotBypass",
            AgentGate::CodexBypass => "CodexBypassApprovalsAndSandbox",
        }
    }

    /// What enabling this gate hands the model (shown in the prompt/summary).
    #[must_use]
    pub fn flag_summary(self) -> &'static str {
        match self {
            AgentGate::ClaudeSkipPermissions => "--dangerously-skip-permissions",
            AgentGate::LocalPilotBypass => "--bypass ([permissions] profile)",
            AgentGate::CodexBypass => "--dangerously-bypass-approvals-and-sandbox",
        }
    }

    /// The agent label used in prompts and the posture summary.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            AgentGate::ClaudeSkipPermissions => "Claude",
            AgentGate::LocalPilotBypass => "LocalPilot",
            AgentGate::CodexBypass => "Codex",
        }
    }
}

/// An env value that reads as "off" (anything else non-empty reads as "on").
fn env_is_falsey(value: &str) -> bool {
    matches!(value, "0" | "false" | "no" | "off")
}

/// The persisted-settings seam: read a boolean setting, persist a decision.
pub trait SettingsStore {
    /// The persisted boolean, when a decision was ever recorded.
    fn get_bool(&self, name: &str) -> Option<bool>;
    /// Persist a decision — an explicit `false` is stored literally.
    fn persist_bool(&mut self, name: &str, value: bool);
}

/// The first-run prompt seam. `None` = the session is non-interactive (no
/// prompt could be shown).
pub trait Prompter {
    /// Ask the one-time question; `None` when no interactive answer is
    /// possible.
    fn ask(&mut self, gate: AgentGate) -> Option<bool>;
}

/// A prompter for sessions with no interactive stdin.
#[derive(Debug, Clone, Copy, Default)]
pub struct NonInteractive;

impl Prompter for NonInteractive {
    fn ask(&mut self, _gate: AgentGate) -> Option<bool> {
        None
    }
}

/// How a gate decision was reached (surfaced in the posture summary).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateSource {
    /// The env var decided (one launch only, never persisted).
    Env(&'static str),
    /// The persisted per-machine setting decided.
    Setting,
    /// The first-run prompt decided (and persisted).
    Prompted,
    /// Nothing decided — the safe default (off) applied without persisting.
    DefaultOff,
}

/// A resolved gate decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateDecision {
    pub enabled: bool,
    pub source: GateSource,
}

/// Resolve a gate: env override → persisted setting → first-run prompt
/// (default OFF). `read_only` (preview/DryRun) resolves undecided to the safe
/// default without prompting or persisting; a non-interactive session does
/// the same.
pub fn resolve_gate(
    gate: AgentGate,
    env: &dyn EnvStore,
    settings: &mut dyn SettingsStore,
    prompter: &mut dyn Prompter,
    read_only: bool,
) -> GateDecision {
    if let Some(value) = env.get(gate.env_var()) {
        if !value.is_empty() {
            return GateDecision {
                enabled: !env_is_falsey(&value),
                source: GateSource::Env(gate.env_var()),
            };
        }
    }
    if let Some(persisted) = settings.get_bool(gate.setting_name()) {
        return GateDecision {
            enabled: persisted,
            source: GateSource::Setting,
        };
    }
    if read_only {
        return GateDecision {
            enabled: false,
            source: GateSource::DefaultOff,
        };
    }
    match prompter.ask(gate) {
        Some(answer) => {
            // The one-time decision persists — including an explicit false.
            settings.persist_bool(gate.setting_name(), answer);
            GateDecision {
                enabled: answer,
                source: GateSource::Prompted,
            }
        }
        None => GateDecision {
            enabled: false,
            source: GateSource::DefaultOff,
        },
    }
}

/// The launch arguments for a resolved Claude skip-permissions decision.
#[must_use]
pub fn claude_permission_args(decision: &GateDecision) -> Vec<String> {
    if decision.enabled {
        vec!["--dangerously-skip-permissions".to_string()]
    } else {
        Vec::new()
    }
}

/// The launch arguments for a resolved Codex bypass decision.
#[must_use]
pub fn codex_bypass_args(decision: &GateDecision) -> Vec<String> {
    if decision.enabled {
        vec!["--dangerously-bypass-approvals-and-sandbox".to_string()]
    } else {
        Vec::new()
    }
}

/// The `.localpilot.toml` block for a resolved LocalPilot bypass decision —
/// bypass rides the config file, never argv.
#[must_use]
pub fn localpilot_bypass_toml(decision: &GateDecision) -> String {
    if decision.enabled {
        "\n[permissions]\nprofile = \"bypass\"\n".to_string()
    } else {
        String::new()
    }
}

/// The human-readable posture line for a gate, read WITHOUT prompting.
#[must_use]
pub fn gate_status_text(
    gate: AgentGate,
    env: &dyn EnvStore,
    settings: &dyn SettingsStore,
) -> String {
    if let Some(value) = env.get(gate.env_var()) {
        if !value.is_empty() {
            return if env_is_falsey(&value) {
                format!("off via {} (agent permission gate on)", gate.env_var())
            } else {
                format!(
                    "ON via {} (bypass — no per-action approval)",
                    gate.env_var()
                )
            };
        }
    }
    match settings.get_bool(gate.setting_name()) {
        Some(true) => "ON (bypass — no per-action approval)".to_string(),
        Some(false) => "off (agent permission gate on)".to_string(),
        None => "undecided - this launch will ask (defaults off)".to_string(),
    }
}

/// The real per-machine settings file (`settings.json`), read and written
/// through the shared precedence engine's rules: catalog-only keys refused,
/// explicit `false` stored literally, UTF-8 without BOM.
#[derive(Debug, Clone)]
pub struct JsonSettingsStore {
    path: PathBuf,
    settings: Map<String, Value>,
}

impl JsonSettingsStore {
    /// Open (or start empty) the settings file at `path`.
    ///
    /// # Errors
    /// An I/O or parse error when the file exists but cannot be read.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, std::io::Error> {
        let path = path.into();
        let settings = if path.is_file() {
            let raw = std::fs::read_to_string(&path)?;
            let raw = raw.trim_start_matches('\u{feff}');
            serde_json::from_str(raw)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?
        } else {
            Map::new()
        };
        Ok(Self { path, settings })
    }

    fn write(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(&self.settings) {
            // Best-effort persist: a failed write must not abort a launch (the
            // decision still applies to this launch).
            let _ = std::fs::write(&self.path, json);
        }
    }

    /// A persisted setting's raw value, when present.
    #[must_use]
    pub fn get_value(&self, name: &str) -> Option<&Value> {
        self.settings.get(name)
    }

    /// Persist a structured setting (catalog-only keys are refused).
    pub fn persist_value(&mut self, name: &str, value: Value) {
        if localx_llama_core::config::set_setting(&mut self.settings, name, value).is_ok() {
            self.write();
        }
    }
}

impl SettingsStore for JsonSettingsStore {
    fn get_bool(&self, name: &str) -> Option<bool> {
        self.settings.get(name).and_then(Value::as_bool)
    }

    fn persist_bool(&mut self, name: &str, value: bool) {
        if localx_llama_core::config::set_setting(&mut self.settings, name, Value::Bool(value))
            .is_ok()
        {
            self.write();
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct FakeEnv {
        vars: BTreeMap<String, String>,
    }
    impl EnvStore for FakeEnv {
        fn get(&self, name: &str) -> Option<String> {
            self.vars.get(name).cloned()
        }
        fn set(&mut self, name: &str, value: &str) {
            self.vars.insert(name.to_string(), value.to_string());
        }
        fn remove(&mut self, name: &str) {
            self.vars.remove(name);
        }
    }

    struct ScriptedPrompter {
        answer: Option<bool>,
        asked: u32,
    }
    impl Prompter for ScriptedPrompter {
        fn ask(&mut self, _gate: AgentGate) -> Option<bool> {
            self.asked += 1;
            self.answer
        }
    }

    fn store(dir: &std::path::Path) -> JsonSettingsStore {
        JsonSettingsStore::open(dir.join("settings.json")).unwrap()
    }

    #[test]
    fn a_real_empty_settings_file_resolves_off_and_persists_nothing_non_interactive() {
        // The carry-fix class: build from the REAL (empty) settings store and
        // assert the observable gate decision, not a stubbed field.
        let dir = tempfile::tempdir().unwrap();
        let mut settings = store(dir.path());
        let env = FakeEnv::default();
        let decision = resolve_gate(
            AgentGate::ClaudeSkipPermissions,
            &env,
            &mut settings,
            &mut NonInteractive,
            false,
        );
        assert!(!decision.enabled, "fail-closed");
        assert_eq!(decision.source, GateSource::DefaultOff);
        assert!(claude_permission_args(&decision).is_empty());
        // Nothing persisted: the machine stays undecided (a later interactive
        // launch still gets its one-time ask).
        assert!(!dir.path().join("settings.json").exists());
    }

    #[test]
    fn the_first_run_prompt_persists_the_answer_once() {
        let dir = tempfile::tempdir().unwrap();
        let env = FakeEnv::default();
        let mut settings = store(dir.path());
        let mut prompter = ScriptedPrompter {
            answer: Some(true),
            asked: 0,
        };
        let decision = resolve_gate(
            AgentGate::ClaudeSkipPermissions,
            &env,
            &mut settings,
            &mut prompter,
            false,
        );
        assert!(decision.enabled);
        assert_eq!(decision.source, GateSource::Prompted);
        assert_eq!(prompter.asked, 1);

        // The persisted decision answers the next resolve — no second ask.
        let mut reopened = store(dir.path());
        let decision = resolve_gate(
            AgentGate::ClaudeSkipPermissions,
            &env,
            &mut reopened,
            &mut prompter,
            false,
        );
        assert!(decision.enabled);
        assert_eq!(decision.source, GateSource::Setting);
        assert_eq!(prompter.asked, 1, "asked exactly once, ever");
    }

    #[test]
    fn an_explicit_no_persists_a_literal_false() {
        let dir = tempfile::tempdir().unwrap();
        let env = FakeEnv::default();
        let mut settings = store(dir.path());
        let mut prompter = ScriptedPrompter {
            answer: Some(false),
            asked: 0,
        };
        let decision = resolve_gate(
            AgentGate::LocalPilotBypass,
            &env,
            &mut settings,
            &mut prompter,
            false,
        );
        assert!(!decision.enabled);
        // The FILE carries `false` literally — decided-no, not never-asked.
        let raw = std::fs::read_to_string(dir.path().join("settings.json")).unwrap();
        assert!(raw.contains("\"LocalPilotBypass\": false"));
        let reopened = store(dir.path());
        assert_eq!(reopened.get_bool("LocalPilotBypass"), Some(false));
    }

    #[test]
    fn the_env_override_wins_for_one_launch_and_never_persists() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = store(dir.path());
        let mut env = FakeEnv::default();
        env.set("LOCAL_LLM_SKIP_PERMISSIONS", "1");
        let decision = resolve_gate(
            AgentGate::ClaudeSkipPermissions,
            &env,
            &mut settings,
            &mut NonInteractive,
            false,
        );
        assert!(decision.enabled);
        assert_eq!(
            claude_permission_args(&decision),
            vec!["--dangerously-skip-permissions".to_string()]
        );
        assert!(
            !dir.path().join("settings.json").exists(),
            "env never persists"
        );

        // Falsey spellings keep the gate on (prompts stay).
        for falsey in ["0", "false", "no", "off"] {
            env.set("LOCAL_LLM_SKIP_PERMISSIONS", falsey);
            let decision = resolve_gate(
                AgentGate::ClaudeSkipPermissions,
                &env,
                &mut settings,
                &mut NonInteractive,
                false,
            );
            assert!(!decision.enabled, "{falsey} must read as off");
        }
    }

    #[test]
    fn codex_bypass_is_never_defaulted_and_localpilot_rides_the_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let env = FakeEnv::default();
        let mut settings = store(dir.path());
        // Preview (read-only): undecided resolves off with no prompt/persist.
        let mut prompter = ScriptedPrompter {
            answer: Some(true),
            asked: 0,
        };
        let codex = resolve_gate(
            AgentGate::CodexBypass,
            &env,
            &mut settings,
            &mut prompter,
            true,
        );
        assert!(!codex.enabled);
        assert_eq!(prompter.asked, 0, "read-only never prompts");
        assert!(codex_bypass_args(&codex).is_empty());

        // LocalPilot bypass emits a config block, never an argv flag.
        let on = GateDecision {
            enabled: true,
            source: GateSource::Setting,
        };
        assert_eq!(
            localpilot_bypass_toml(&on),
            "\n[permissions]\nprofile = \"bypass\"\n"
        );
        let off = GateDecision {
            enabled: false,
            source: GateSource::Setting,
        };
        assert!(localpilot_bypass_toml(&off).is_empty());
    }

    #[test]
    fn status_text_reads_without_prompting() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = store(dir.path());
        let mut env = FakeEnv::default();
        assert!(gate_status_text(AgentGate::CodexBypass, &env, &settings).contains("undecided"));
        settings.persist_bool("CodexBypassApprovalsAndSandbox", true);
        assert!(gate_status_text(AgentGate::CodexBypass, &env, &settings).starts_with("ON"));
        env.set("LOCAL_LLM_CODEX_BYPASS", "off");
        assert!(gate_status_text(AgentGate::CodexBypass, &env, &settings)
            .contains("off via LOCAL_LLM_CODEX_BYPASS"));
    }

    #[test]
    fn the_settings_store_refuses_catalog_only_keys() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = store(dir.path());
        settings.persist_bool("Models", true);
        assert_eq!(
            settings.get_bool("Models"),
            None,
            "catalog-only key refused"
        );
    }
}
