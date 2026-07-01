//! The generated `.localpilot.toml`: the provider block a LocalPilot launch
//! reads its model, endpoint, and permission profile from (the REPL is the
//! default no-arg command and resolves everything from config, not argv).
//!
//! Coupling rules, pinned by test:
//! - The provider `kind` and the endpoint move together: `anthropic` when the
//!   no-think proxy fronts the server (its `/v1` normalizes to
//!   `/v1/messages`), `openai-compatible` for the direct route (`/v1` →
//!   `/v1/chat/completions`). The emitted `base_url` always ends in `/v1`.
//! - `api_key_env` follows the kind: `ANTHROPIC_AUTH_TOKEN` behind the proxy,
//!   `LOCALPILOT_LOCAL_API_KEY` direct.
//! - The model's context is declared as `providers.local.context_window` —
//!   never `[harness] context_token_limit` (the harness key does not size the
//!   provider window; that mis-placement cost a live-run diagnosis).
//! - `supports_vision = true` is auto-declared ONLY when the launch actually
//!   loaded a projector.
//! - Bypass is a config block (`[permissions] profile = "bypass"`), never an
//!   argv flag.

use serde::{Deserialize, Serialize};

/// The provider adapter LocalPilot should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    /// Anthropic `/v1/messages` — the no-think proxy route.
    Anthropic,
    /// OpenAI-compatible `/v1/chat/completions` — the direct route.
    OpenaiCompatible,
}

impl ProviderKind {
    /// The wire name in the TOML.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::OpenaiCompatible => "openai-compatible",
        }
    }

    /// The API-key environment variable coupled to this kind.
    #[must_use]
    pub fn api_key_env(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "ANTHROPIC_AUTH_TOKEN",
            ProviderKind::OpenaiCompatible => "LOCALPILOT_LOCAL_API_KEY",
        }
    }
}

/// Everything the generated config needs.
#[derive(Debug, Clone)]
pub struct LocalPilotConfigInputs {
    /// The route: anthropic behind the no-think proxy, openai-compatible direct.
    pub provider_kind: ProviderKind,
    /// The endpoint origin (no `/v1`; the emitter appends it).
    pub base_url: String,
    /// The provider's default model (the REPL resolves it from config).
    pub model: String,
    /// Whether a vision projector was actually loaded for this launch.
    pub supports_vision: bool,
    /// Output-token cap; 0 omits the key (client default).
    pub max_tokens: u32,
    /// The model's usable context; 0 omits the key (conservative default).
    pub context_tokens: u32,
    /// Whether the (persisted, opt-in) bypass decision is on.
    pub bypass: bool,
}

impl LocalPilotConfigInputs {
    /// The standard proxied local launch.
    #[must_use]
    pub fn proxied(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider_kind: ProviderKind::Anthropic,
            base_url: base_url.into(),
            model: model.into(),
            supports_vision: false,
            max_tokens: 4096,
            context_tokens: 0,
            bypass: false,
        }
    }
}

/// Emit the `.localpilot.toml` content for a local launch.
#[must_use]
pub fn localpilot_config_toml(inputs: &LocalPilotConfigInputs) -> String {
    let base_url = format!("{}/v1", inputs.base_url.trim_end_matches('/'));
    let mut toml = format!(
        "[provider]\ndefault = \"local\"\n\n[providers.local]\nkind = \"{}\"\nbase_url = \"{}\"\napi_key_env = \"{}\"\nmodel = \"{}\"\n",
        inputs.provider_kind.as_str(),
        base_url,
        inputs.provider_kind.api_key_env(),
        inputs.model,
    );
    if inputs.supports_vision {
        toml.push_str("supports_vision = true\n");
    }
    if inputs.max_tokens > 0 {
        toml.push_str(&format!("max_tokens = {}\n", inputs.max_tokens));
    }
    if inputs.context_tokens > 0 {
        // The provider window, on the provider — not a harness key.
        toml.push_str(&format!("context_window = {}\n", inputs.context_tokens));
    }
    if inputs.bypass {
        toml.push_str("\n[permissions]\nprofile = \"bypass\"\n");
    }
    toml
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn parse(text: &str) -> toml::Value {
        text.parse::<toml::Value>().expect("valid TOML")
    }

    #[test]
    fn the_proxied_route_couples_kind_endpoint_and_key_env() {
        let mut inputs = LocalPilotConfigInputs::proxied("http://127.0.0.1:11435", "q36apex");
        inputs.context_tokens = 65_536;
        let toml = parse(&localpilot_config_toml(&inputs));
        assert_eq!(toml["provider"]["default"].as_str(), Some("local"));
        let local = &toml["providers"]["local"];
        assert_eq!(local["kind"].as_str(), Some("anthropic"));
        // /v1 appended exactly once; the adapter normalizes it to /v1/messages.
        assert_eq!(
            local["base_url"].as_str(),
            Some("http://127.0.0.1:11435/v1")
        );
        assert_eq!(local["api_key_env"].as_str(), Some("ANTHROPIC_AUTH_TOKEN"));
        assert_eq!(local["model"].as_str(), Some("q36apex"));
    }

    #[test]
    fn the_direct_route_switches_kind_and_key_env_together() {
        let mut inputs = LocalPilotConfigInputs::proxied("http://127.0.0.1:8080/", "m");
        inputs.provider_kind = ProviderKind::OpenaiCompatible;
        let toml = parse(&localpilot_config_toml(&inputs));
        let local = &toml["providers"]["local"];
        assert_eq!(local["kind"].as_str(), Some("openai-compatible"));
        assert_eq!(
            local["base_url"].as_str(),
            Some("http://127.0.0.1:8080/v1"),
            "trailing slash normalized before the /v1 suffix"
        );
        assert_eq!(
            local["api_key_env"].as_str(),
            Some("LOCALPILOT_LOCAL_API_KEY")
        );
    }

    #[test]
    fn context_rides_the_provider_window_never_a_harness_key() {
        let mut inputs = LocalPilotConfigInputs::proxied("http://x", "m");
        inputs.context_tokens = 32_768;
        let text = localpilot_config_toml(&inputs);
        let toml = parse(&text);
        assert_eq!(
            toml["providers"]["local"]["context_window"].as_integer(),
            Some(32_768)
        );
        assert!(toml.get("harness").is_none(), "no [harness] table");
        assert!(!text.contains("context_token_limit"));
        // Zero omits the key entirely (the conservative default stands).
        inputs.context_tokens = 0;
        let toml = parse(&localpilot_config_toml(&inputs));
        assert!(toml["providers"]["local"].get("context_window").is_none());
    }

    #[test]
    fn vision_is_declared_only_when_the_projector_loaded() {
        let mut inputs = LocalPilotConfigInputs::proxied("http://x", "m");
        let without = parse(&localpilot_config_toml(&inputs));
        assert!(without["providers"]["local"]
            .get("supports_vision")
            .is_none());
        inputs.supports_vision = true;
        let with = parse(&localpilot_config_toml(&inputs));
        assert_eq!(
            with["providers"]["local"]["supports_vision"].as_bool(),
            Some(true)
        );
    }

    #[test]
    fn bypass_is_a_config_block_and_only_when_opted_in() {
        let mut inputs = LocalPilotConfigInputs::proxied("http://x", "m");
        let off = parse(&localpilot_config_toml(&inputs));
        assert!(off.get("permissions").is_none());
        inputs.bypass = true;
        let on = parse(&localpilot_config_toml(&inputs));
        assert_eq!(on["permissions"]["profile"].as_str(), Some("bypass"));
    }

    #[test]
    fn max_tokens_is_conditional() {
        let mut inputs = LocalPilotConfigInputs::proxied("http://x", "m");
        inputs.max_tokens = 0;
        let toml = parse(&localpilot_config_toml(&inputs));
        assert!(toml["providers"]["local"].get("max_tokens").is_none());
        inputs.max_tokens = 4096;
        let toml = parse(&localpilot_config_toml(&inputs));
        assert_eq!(
            toml["providers"]["local"]["max_tokens"].as_integer(),
            Some(4096)
        );
    }
}
