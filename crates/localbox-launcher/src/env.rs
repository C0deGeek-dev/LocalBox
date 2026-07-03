//! The agent environment envelope: save → mutate → finally-restore.
//!
//! One function ([`claude_env_plan`]) computes the variables an agent launch
//! sets — it IS the DryRun snapshot AND the live setter's source, so the two
//! can never drift (the PS version kept a hand-mirrored duplicate). The
//! envelope snapshots every canonical name before mutating and restores on
//! drop: values present before come back, values absent before are removed —
//! never left behind for the next launch in the same shell.
//!
//! Environment access goes through [`EnvStore`] so the flow is testable
//! hermetically (process env is global state; tests must not race over it).

use std::collections::BTreeMap;

/// Every environment variable a launch may touch — the envelope's save/restore
/// set. A variable set by any setter below MUST be listed here.
pub const CLAUDE_ENV_NAMES: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "CLAUDE_CODE_DISABLE_THINKING",
    "MAX_THINKING_TOKENS",
    "CLAUDE_CODE_DISABLE_ADAPTIVE_THINKING",
    "CLAUDE_CODE_MAX_OUTPUT_TOKENS",
    "CLAUDE_CODE_MAX_CONTEXT_TOKENS",
    "CLAUDE_CODE_AUTO_COMPACT_WINDOW",
    "CLAUDE_CODE_ATTRIBUTION_HEADER",
    "DISABLE_PROMPT_CACHING",
    "API_TIMEOUT_MS",
    "CLAUDE_CODE_DISABLE_AUTO_MEMORY",
    "CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS",
    "ENABLE_TOOL_SEARCH",
    "CLAUDE_LOCAL_MAX_IMAGES",
    // Codex (OpenAI-protocol) agent target.
    "OPENAI_BASE_URL",
    "OPENAI_API_KEY",
];

/// Environment access seam.
pub trait EnvStore {
    /// The variable's value, when set.
    fn get(&self, name: &str) -> Option<String>;
    /// Set a variable.
    fn set(&mut self, name: &str, value: &str);
    /// Remove a variable.
    fn remove(&mut self, name: &str);
}

/// The real process environment.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessEnv;

impl EnvStore for ProcessEnv {
    fn get(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
    fn set(&mut self, name: &str, value: &str) {
        std::env::set_var(name, value);
    }
    fn remove(&mut self, name: &str) {
        std::env::remove_var(name);
    }
}

/// Inputs to the agent env plan.
#[derive(Debug, Clone)]
pub struct EnvPlanInputs {
    /// The Anthropic-compatible endpoint (usually the no-think proxy).
    pub base_url: String,
    /// The model id every model alias resolves to.
    pub model: String,
    /// Leave thinking enabled (skip the no-think toggle trio); the caller must
    /// arrange routing accordingly.
    pub keep_thinking: bool,
    /// The gateway token; `local` for the loopback no-auth proxy.
    pub auth_token: String,
    /// The model's context tokens; 0 leaves the client default.
    pub context_tokens: u32,
    /// Output-token cap; 0 leaves the client default.
    pub max_output_tokens: u32,
    /// Images-per-request ceiling; 0 leaves the client default of 1.
    pub max_images_per_request: u32,
}

impl EnvPlanInputs {
    /// The standard local launch: no-think routing, default caps.
    #[must_use]
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            keep_thinking: false,
            auth_token: "local".to_string(),
            context_tokens: 0,
            max_output_tokens: 4096,
            max_images_per_request: 0,
        }
    }
}

/// The variables an agent launch sets, in order — the single source consumed
/// by both the live setter and the DryRun snapshot.
#[must_use]
pub fn claude_env_plan(inputs: &EnvPlanInputs) -> Vec<(&'static str, String)> {
    let mut plan: Vec<(&'static str, String)> = vec![
        ("ANTHROPIC_BASE_URL", inputs.base_url.clone()),
        ("ANTHROPIC_AUTH_TOKEN", inputs.auth_token.clone()),
        // A non-empty ANTHROPIC_API_KEY would win over the auth token; blank it.
        ("ANTHROPIC_API_KEY", String::new()),
        // Every alias resolves to the one local model.
        ("ANTHROPIC_MODEL", inputs.model.clone()),
        ("ANTHROPIC_DEFAULT_OPUS_MODEL", inputs.model.clone()),
        ("ANTHROPIC_DEFAULT_SONNET_MODEL", inputs.model.clone()),
        ("ANTHROPIC_DEFAULT_HAIKU_MODEL", inputs.model.clone()),
    ];
    if !inputs.keep_thinking {
        // The no-think trio: the proxy strips <think> blocks; the client must
        // not request thinking budgets the local model then wastes.
        plan.push(("CLAUDE_CODE_DISABLE_THINKING", "1".to_string()));
        plan.push(("MAX_THINKING_TOKENS", "0".to_string()));
        plan.push(("CLAUDE_CODE_DISABLE_ADAPTIVE_THINKING", "1".to_string()));
    }
    if inputs.max_output_tokens > 0 {
        plan.push((
            "CLAUDE_CODE_MAX_OUTPUT_TOKENS",
            inputs.max_output_tokens.to_string(),
        ));
    }
    if inputs.context_tokens > 0 {
        // The context cap and the auto-compact window move together.
        plan.push((
            "CLAUDE_CODE_MAX_CONTEXT_TOKENS",
            inputs.context_tokens.to_string(),
        ));
        plan.push((
            "CLAUDE_CODE_AUTO_COMPACT_WINDOW",
            inputs.context_tokens.to_string(),
        ));
    }
    plan.push(("CLAUDE_CODE_ATTRIBUTION_HEADER", "0".to_string()));
    plan.push(("DISABLE_PROMPT_CACHING", "1".to_string()));
    // Local models prefill slowly on big prompts; raise the SDK timeout so the
    // client doesn't abort + retry mid-prefill (which restarts the work).
    plan.push(("API_TIMEOUT_MS", "1800000".to_string()));
    // Drop the auto-memory system-prompt block (and the turn-end extract
    // agent): several KB of input tokens per turn when prefill is the
    // bottleneck.
    plan.push(("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1".to_string()));
    // llama.cpp's Anthropic-compatible endpoint does not implement beta tool
    // shapes (defer_loading/tool_reference); without these the client may
    // withhold real tools behind ToolSearch or send schema fields local
    // proxies tolerate inconsistently.
    plan.push(("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS", "1".to_string()));
    plan.push(("ENABLE_TOOL_SEARCH", "false".to_string()));
    if inputs.max_images_per_request > 0 {
        // Per-model image ceiling for the local vision backend (llama.cpp +
        // mmproj typically degenerates beyond one image; the client default
        // of 1 stands unless the catalog raises it).
        plan.push((
            "CLAUDE_LOCAL_MAX_IMAGES",
            inputs.max_images_per_request.to_string(),
        ));
    }
    plan
}

/// The variables a **Codex** (OpenAI-protocol) agent launch sets. Codex reads
/// `OPENAI_BASE_URL` + `OPENAI_API_KEY`, so it must be pointed at the local
/// OpenAI-compatible endpoint — otherwise it silently talks to the cloud. Point
/// it at the no-think proxy's `/v1` (the proxy strips `<think>` from OpenAI SSE
/// deltas too), or at the raw server when thinking is kept.
#[must_use]
pub fn codex_env_plan(base_url: &str, auth_token: &str) -> Vec<(&'static str, String)> {
    let v1 = format!("{}/v1", base_url.trim_end_matches('/'));
    vec![
        ("OPENAI_BASE_URL", v1),
        ("OPENAI_API_KEY", auth_token.to_string()),
    ]
}

/// The saved pre-launch state of every canonical variable: `Some` = present
/// with that value, `None` = absent (and to be removed again on restore).
#[derive(Debug, Clone)]
pub struct EnvEnvelope {
    backup: BTreeMap<&'static str, Option<String>>,
}

impl EnvEnvelope {
    /// Snapshot every canonical variable's current state.
    #[must_use]
    pub fn save(env: &dyn EnvStore) -> Self {
        let backup = CLAUDE_ENV_NAMES
            .iter()
            .map(|name| (*name, env.get(name)))
            .collect();
        Self { backup }
    }

    /// Apply a plan (each pair set verbatim).
    pub fn apply(env: &mut dyn EnvStore, plan: &[(&'static str, String)]) {
        for (name, value) in plan {
            env.set(name, value);
        }
    }

    /// Restore the snapshot: values present before come back; values absent
    /// before are removed. This is the launch's `finally`.
    pub fn restore(&self, env: &mut dyn EnvStore) {
        for (name, previous) in &self.backup {
            match previous {
                Some(value) if !value.is_empty() => env.set(name, value),
                _ => env.remove(name),
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// A hermetic env (tests must not race over the real process env).
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

    #[test]
    fn every_plan_variable_is_inside_the_envelope() {
        // The envelope can only restore what it saved: any variable a setter
        // writes MUST be in the canonical list.
        let mut inputs = EnvPlanInputs::new("http://127.0.0.1:11435", "apex");
        inputs.context_tokens = 65_536;
        inputs.max_images_per_request = 2;
        for (name, _) in claude_env_plan(&inputs) {
            assert!(CLAUDE_ENV_NAMES.contains(&name), "{name} not in envelope");
        }
    }

    #[test]
    fn restore_unsets_variables_that_were_absent_before() {
        let mut env = FakeEnv::default();
        env.set("ANTHROPIC_BASE_URL", "https://api.anthropic.com");
        // API_TIMEOUT_MS and the rest are absent before the launch.
        let envelope = EnvEnvelope::save(&env);
        EnvEnvelope::apply(
            &mut env,
            &claude_env_plan(&EnvPlanInputs::new("http://127.0.0.1:11435", "apex")),
        );
        assert_eq!(env.get("API_TIMEOUT_MS").as_deref(), Some("1800000"));
        assert_eq!(
            env.get("ANTHROPIC_BASE_URL").as_deref(),
            Some("http://127.0.0.1:11435")
        );

        envelope.restore(&mut env);
        // The pre-existing value came back; the launch-only vars are GONE.
        assert_eq!(
            env.get("ANTHROPIC_BASE_URL").as_deref(),
            Some("https://api.anthropic.com")
        );
        assert_eq!(env.get("API_TIMEOUT_MS"), None);
        assert_eq!(env.get("CLAUDE_CODE_DISABLE_THINKING"), None);
        assert_eq!(env.get("ENABLE_TOOL_SEARCH"), None);
    }

    #[test]
    fn the_dry_run_snapshot_is_the_live_setter() {
        // Single source: applying the plan writes exactly the plan's pairs —
        // what DryRun previews IS what the live launch sets.
        let mut inputs = EnvPlanInputs::new("http://127.0.0.1:11435", "apex");
        inputs.context_tokens = 32_768;
        let plan = claude_env_plan(&inputs);
        let mut env = FakeEnv::default();
        EnvEnvelope::apply(&mut env, &plan);
        assert_eq!(env.vars.len(), plan.len(), "nothing beyond the plan");
        for (name, value) in &plan {
            assert_eq!(env.get(name).as_deref(), Some(value.as_str()));
        }
    }

    #[test]
    fn the_no_think_trio_is_skipped_when_thinking_is_kept() {
        let mut inputs = EnvPlanInputs::new("u", "m");
        inputs.keep_thinking = true;
        let plan = claude_env_plan(&inputs);
        let names: Vec<&str> = plan.iter().map(|(n, _)| *n).collect();
        assert!(!names.contains(&"CLAUDE_CODE_DISABLE_THINKING"));
        assert!(!names.contains(&"MAX_THINKING_TOKENS"));
        assert!(!names.contains(&"CLAUDE_CODE_DISABLE_ADAPTIVE_THINKING"));
        // The rest of the envelope still applies.
        assert!(names.contains(&"API_TIMEOUT_MS"));
        assert!(names.contains(&"CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS"));
    }

    #[test]
    fn caps_are_conditional_and_paired() {
        let base = claude_env_plan(&EnvPlanInputs::new("u", "m"));
        let names: Vec<&str> = base.iter().map(|(n, _)| *n).collect();
        // No context cap requested: neither var appears (client default).
        assert!(!names.contains(&"CLAUDE_CODE_MAX_CONTEXT_TOKENS"));
        assert!(!names.contains(&"CLAUDE_CODE_AUTO_COMPACT_WINDOW"));
        // No image cap: the client's own default of 1 stands.
        assert!(!names.contains(&"CLAUDE_LOCAL_MAX_IMAGES"));

        let mut inputs = EnvPlanInputs::new("u", "m");
        inputs.context_tokens = 65_536;
        inputs.max_images_per_request = 2;
        let plan = claude_env_plan(&inputs);
        let get = |name: &str| {
            plan.iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| v.clone())
        };
        // The context cap and the compact window move together.
        assert_eq!(
            get("CLAUDE_CODE_MAX_CONTEXT_TOKENS").as_deref(),
            Some("65536")
        );
        assert_eq!(
            get("CLAUDE_CODE_AUTO_COMPACT_WINDOW").as_deref(),
            Some("65536")
        );
        assert_eq!(get("CLAUDE_LOCAL_MAX_IMAGES").as_deref(), Some("2"));
    }

    #[test]
    fn model_aliases_all_resolve_to_the_local_model_and_the_key_is_blanked() {
        let plan = claude_env_plan(&EnvPlanInputs::new("u", "apex-i-quality"));
        let get = |name: &str| {
            plan.iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| v.clone())
        };
        for alias in [
            "ANTHROPIC_MODEL",
            "ANTHROPIC_DEFAULT_OPUS_MODEL",
            "ANTHROPIC_DEFAULT_SONNET_MODEL",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        ] {
            assert_eq!(get(alias).as_deref(), Some("apex-i-quality"));
        }
        assert_eq!(get("ANTHROPIC_AUTH_TOKEN").as_deref(), Some("local"));
        assert_eq!(get("ANTHROPIC_API_KEY").as_deref(), Some(""));
    }

    #[test]
    fn codex_plan_points_at_the_local_openai_endpoint() {
        // Without this, Codex reads no OPENAI_* vars and talks to the cloud.
        let plan = codex_env_plan("http://127.0.0.1:11435", "local");
        let get = |name: &str| {
            plan.iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(
            get("OPENAI_BASE_URL").as_deref(),
            Some("http://127.0.0.1:11435/v1")
        );
        assert_eq!(get("OPENAI_API_KEY").as_deref(), Some("local"));
        // Every codex var is inside the restore envelope.
        for (name, _) in &plan {
            assert!(CLAUDE_ENV_NAMES.contains(name), "{name} not in envelope");
        }
    }
}
