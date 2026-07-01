//! The one-screen security posture and the LAN serve-gateway guard.
//!
//! The repo's invisible security decisions become one visible screen:
//! permission/bypass gate status, the loopback-only agent proxy, the serve
//! gateway's exposure and auth, and the binary download-pin posture. The
//! serve guard refuses the one genuinely dangerous shape — open (no-auth)
//! HTTP on a public-looking address — unless the operator opts in explicitly;
//! everything else is visible, not blocked.

use crate::env::EnvStore;
use crate::permissions::{gate_status_text, AgentGate, SettingsStore};

/// Whether a base URL is *public-looking* plain HTTP: `http://` on anything
/// that is not loopback, RFC-1918/link-local private space, or `localhost`.
/// HTTPS is never flagged (transport is protected); unparseable URLs are not
/// flagged here (they fail elsewhere).
#[must_use]
pub fn is_public_http(base_url: &str) -> bool {
    let Some(rest) = base_url.strip_prefix("http://") else {
        return false; // https:// or not a URL — not the open-HTTP hazard
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // Strip credentials and port; tolerate a bracketed IPv6 literal.
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let host = if let Some(v6) = host.strip_prefix('[') {
        v6.split(']').next().unwrap_or("")
    } else {
        host.split(':').next().unwrap_or("")
    };
    if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
        return false;
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v4) => {
                let o = v4.octets();
                !(o[0] == 10
                    || o[0] == 127
                    || (o[0] == 192 && o[1] == 168)
                    || (o[0] == 172 && (16..=31).contains(&o[1]))
                    || (o[0] == 169 && o[1] == 254))
            }
            std::net::IpAddr::V6(v6) => !v6.is_loopback(),
        };
    }
    // A DNS name over plain HTTP: treat as public-looking.
    true
}

/// The serve-gateway exposure decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeGuard {
    /// The base URLs that look publicly reachable over plain HTTP.
    pub public_urls: Vec<String>,
    /// The launch must be refused (open no-auth HTTP on a public address,
    /// with no explicit opt-in).
    pub refuse: bool,
    /// The operator explicitly opted into open public HTTP.
    pub opted_in: bool,
    /// The operator-facing refusal reason; empty when allowed.
    pub reason: String,
}

/// Decide the serve-gateway exposure: open (no-auth) HTTP on a public-looking
/// address is refused unless explicitly opted into; password-protected public
/// HTTP stays allowed (with the posture screen making it visible).
#[must_use]
pub fn evaluate_serve_guard(
    base_urls: &[String],
    password: &str,
    allow_public_no_auth: bool,
) -> ServeGuard {
    let public_urls: Vec<String> = base_urls
        .iter()
        .filter(|u| is_public_http(u))
        .cloned()
        .collect();
    let no_auth = password.trim().is_empty();
    let refuse = !public_urls.is_empty() && no_auth && !allow_public_no_auth;
    let reason = if refuse {
        format!(
            "open (no auth) HTTP on a public-looking address: {}. Set a password \
             (-Password or LOCAL_LLM_SERVE_PASS), bind a private address \
             (-ListenHost/-AdvertiseHost), or opt in explicitly with -AllowPublicNoAuth.",
            public_urls.join(", ")
        )
    } else {
        String::new()
    };
    ServeGuard {
        opted_in: allow_public_no_auth && no_auth && !public_urls.is_empty(),
        public_urls,
        refuse,
        reason,
    }
}

/// The download-pin posture inputs.
#[derive(Debug, Clone, Default)]
pub struct PinPosture {
    /// Number of pinned asset hashes configured.
    pub pin_count: usize,
    /// Whether unpinned downloads are blocked.
    pub require_pins: bool,
    /// The pinned llama.cpp release tag, when set.
    pub pinned_tag: Option<String>,
}

/// Inputs to the posture screen.
pub struct PostureInputs<'a> {
    pub env: &'a dyn EnvStore,
    pub settings: &'a dyn SettingsStore,
    pub proxy_port: u16,
    /// Whether a serve-gateway token is currently set.
    pub serve_token_set: bool,
    pub pins: PinPosture,
}

/// Render the one-screen security posture.
#[must_use]
pub fn security_posture(inputs: &PostureInputs) -> String {
    let permission = gate_status_text(
        AgentGate::ClaudeSkipPermissions,
        inputs.env,
        inputs.settings,
    );
    let localpilot = gate_status_text(AgentGate::LocalPilotBypass, inputs.env, inputs.settings);
    let codex = gate_status_text(AgentGate::CodexBypass, inputs.env, inputs.settings);
    let serve_auth = if inputs.serve_token_set {
        "token set"
    } else {
        "no token set (gateway would be open)"
    };
    let tag = inputs
        .pins
        .pinned_tag
        .clone()
        .unwrap_or_else(|| "none (latest, unpinned)".to_string());
    let pin_line = if inputs.pins.require_pins {
        format!(
            "{} asset pin(s), unpinned downloads blocked, llama.cpp tag {tag}",
            inputs.pins.pin_count
        )
    } else {
        format!(
            "{} asset pin(s), unpinned downloads ALLOWED (trust-on-first-use), llama.cpp tag {tag}",
            inputs.pins.pin_count
        )
    };

    format!(
        "=== LocalBox security posture ===\n\
         \x20 Agent permission prompts : {permission}\n\
         \x20 LocalPilot bypass        : {localpilot}\n\
         \x20 Codex bypass             : {codex}\n\
         \x20 Agent proxy              : 127.0.0.1:{} (local only)\n\
         \x20 Serve gateway (if used)  : listens on 0.0.0.0; auth: {serve_auth}\n\
         \x20                            LAN/VPN only; HTTPS in front for off-LAN; public no-auth HTTP is refused.\n\
         \x20 Binary download pins     : {pin_line}",
        inputs.proxy_port
    )
}

/// The managed default GGUF root under a home directory — discovery returns
/// this when nothing is configured, never a hardcoded machine path.
#[must_use]
pub fn default_gguf_root(home: &std::path::Path) -> std::path::PathBuf {
    home.join(".local-llm").join("gguf")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::permissions::JsonSettingsStore;
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

    #[test]
    fn the_public_http_classifier_spares_private_space() {
        for private in [
            "http://127.0.0.1:11435",
            "http://localhost:8080",
            "http://10.1.2.3:8080",
            "http://192.168.1.20:8080",
            "http://172.16.0.9:8080",
            "http://172.31.255.1:8080",
            "http://169.254.10.10:8080",
            "http://[::1]:8080",
            "https://93.184.216.34:8080",
        ] {
            assert!(!is_public_http(private), "{private} must not flag");
        }
        for public in [
            "http://93.184.216.34:8080",
            "http://172.32.0.1:8080",
            "http://myhost.example.com:8080",
            "http://[2001:db8::1]:8080",
        ] {
            assert!(is_public_http(public), "{public} must flag");
        }
    }

    #[test]
    fn no_token_public_serve_is_refused_with_the_remedy() {
        let guard = evaluate_serve_guard(
            &[
                "http://192.168.1.20:11436".to_string(),
                "http://93.184.216.34:11436".to_string(),
            ],
            "",
            false,
        );
        assert!(guard.refuse);
        assert_eq!(guard.public_urls, vec!["http://93.184.216.34:11436"]);
        assert!(guard.reason.contains("LOCAL_LLM_SERVE_PASS"));
        assert!(guard.reason.contains("-AllowPublicNoAuth"));
    }

    #[test]
    fn a_token_or_an_explicit_opt_in_allows_the_gateway() {
        let urls = vec!["http://93.184.216.34:11436".to_string()];
        // Password-protected public HTTP: allowed (visible, not blocked).
        let guard = evaluate_serve_guard(&urls, "s3cret", false);
        assert!(!guard.refuse);
        assert!(!guard.opted_in);
        // Explicit opt-in to open public HTTP: allowed and marked.
        let guard = evaluate_serve_guard(&urls, "", true);
        assert!(!guard.refuse);
        assert!(guard.opted_in);
        // Private-only, no auth: nothing to refuse.
        let guard = evaluate_serve_guard(&["http://192.168.1.2:1".to_string()], "", false);
        assert!(!guard.refuse);
        assert!(guard.public_urls.is_empty());
    }

    #[test]
    fn the_posture_screen_surfaces_every_gate_and_pin_state() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = JsonSettingsStore::open(dir.path().join("settings.json")).unwrap();
        crate::permissions::SettingsStore::persist_bool(&mut settings, "LocalPilotBypass", true);
        let env = FakeEnv::default();
        let posture = security_posture(&PostureInputs {
            env: &env,
            settings: &settings,
            proxy_port: 11_435,
            serve_token_set: false,
            pins: PinPosture {
                pin_count: 3,
                require_pins: true,
                pinned_tag: Some("b4988".to_string()),
            },
        });
        assert!(posture.contains("Agent permission prompts : undecided"));
        assert!(posture.contains("LocalPilot bypass        : ON"));
        assert!(posture.contains("Codex bypass             : undecided"));
        assert!(posture.contains("127.0.0.1:11435 (local only)"));
        assert!(posture.contains("no token set (gateway would be open)"));
        assert!(posture.contains("3 asset pin(s), unpinned downloads blocked, llama.cpp tag b4988"));
        // The trust-on-first-use spelling appears when pins are not required.
        let tofu = security_posture(&PostureInputs {
            env: &env,
            settings: &settings,
            proxy_port: 11_435,
            serve_token_set: true,
            pins: PinPosture::default(),
        });
        assert!(tofu.contains("ALLOWED (trust-on-first-use)"));
        assert!(tofu.contains("none (latest, unpinned)"));
        assert!(tofu.contains("auth: token set"));
    }

    #[test]
    fn discovery_defaults_are_managed_never_hardcoded() {
        // The default derives from the given home — no literal machine path.
        let root = default_gguf_root(std::path::Path::new("/home/alice"));
        assert!(root.ends_with(std::path::PathBuf::from(".local-llm").join("gguf")));
        assert!(root.starts_with("/home/alice"));
        let other = default_gguf_root(std::path::Path::new("D:/Users/bob"));
        assert!(other.starts_with("D:/Users/bob"));
        assert_ne!(root, other, "the root follows the home, not the machine");
    }
}
