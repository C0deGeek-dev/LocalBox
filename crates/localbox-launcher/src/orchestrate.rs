//! Launch planning: resolve model → GGUF → argv → ports → provider config →
//! env, producing one inspectable [`LaunchPlan`].
//!
//! Planning is **read-only by construction**: it downloads nothing, spawns
//! nothing, and commits no session state — the same plan object serves DryRun
//! (print it) and the live launch (execute it). Vision is opt-in and honest:
//! `--mmproj` enters the argv only when a projector actually resolved, and
//! only then is `supports_vision` declared to the agent.

use std::path::PathBuf;

use localx_llama_core::args::{build_llama_server_args, LaunchParams};
use localx_llama_core::{LauncherError, Mode};

use crate::env::{claude_env_plan, EnvPlanInputs};
use crate::launcher::LlamaLauncher;
use crate::localpilot_config::{localpilot_config_toml, LocalPilotConfigInputs, ProviderKind};
use crate::proxy::EnsureProxyConfig;

/// What a launch was asked to do.
#[derive(Debug, Clone)]
pub struct LaunchRequest {
    pub key: String,
    pub context_key: String,
    pub mode: Mode,
    /// Quant override; `None` uses the catalog default.
    pub quant: Option<String>,
    /// Vision opt-in: the projector loads only when requested AND resolvable.
    pub use_vision: bool,
    /// Drafter opt-in: classic speculative decoding loads only when requested
    /// AND the catalog configures a `DraftModule`.
    pub use_draft: bool,
    /// Route thinking through the model instead of the no-think strip.
    pub keep_thinking: bool,
    /// The (persisted, opt-in) LocalPilot bypass decision.
    pub bypass: bool,
    /// The no-think proxy listen port.
    pub proxy_port: u16,
    /// The server port search start.
    pub server_port_start: u16,
    /// Tunable launch parameters (AutoBest overrides re-hydrate into these).
    pub params: LaunchParams,
}

impl LaunchRequest {
    /// The standard proxied launch for a model key.
    #[must_use]
    pub fn new(key: impl Into<String>, context_key: impl Into<String>, mode: Mode) -> Self {
        Self {
            key: key.into(),
            context_key: context_key.into(),
            mode,
            quant: None,
            use_vision: false,
            use_draft: false,
            keep_thinking: false,
            bypass: false,
            proxy_port: 11_435,
            server_port_start: 8080,
            params: LaunchParams::default(),
        }
    }

    /// Fold the settings-file launch params under any explicit/AutoBest values,
    /// then default to a single-session server — one slot plus a
    /// prompt-cache-reuse window — when nothing above chose otherwise.
    ///
    /// Every launch path (agent, guided, headless serve) must call this.
    /// llama-server's own `--parallel` default became `-1 = auto`, which
    /// allocates the **full configured context per slot** (4 slots on auto),
    /// multiplying KV-cache memory several-fold over the single slot these
    /// launches were sized for — enough to OOM a model tuned to fit the GPU.
    /// A launch that wants the server's auto slot count can set the
    /// `LlamaCppAgentParallel` setting to `-1`: a non-positive value is kept
    /// here and the argv builder then omits `--parallel`, leaving the server
    /// default in charge.
    pub fn apply_session_defaults(&mut self, overlay: &LaunchParams) {
        let params = &mut self.params;
        params.parallel = params.parallel.or(overlay.parallel);
        params.cache_reuse = params.cache_reuse.or(overlay.cache_reuse);
        params.n_cpu_moe = params.n_cpu_moe.or(overlay.n_cpu_moe);
        params.mlock = params.mlock.or(overlay.mlock);
        params.no_mmap = params.no_mmap.or(overlay.no_mmap);
        if params.parallel.is_none() {
            params.parallel = Some(1);
        }
        if params.cache_reuse.is_none() {
            params.cache_reuse = Some(256);
        }
    }
}

/// Everything a launch resolved, ready to print (DryRun) or execute (live).
#[derive(Debug, Clone)]
pub struct LaunchPlan {
    pub key: String,
    /// The canonical context key.
    pub context_key: String,
    pub context_tokens: u32,
    /// The GGUF's expected on-disk path (no download happened).
    pub gguf_path: PathBuf,
    /// Whether the GGUF is already on disk (a live launch downloads first
    /// when false; DryRun just reports it).
    pub gguf_downloaded: bool,
    /// The resolved or planned projector when vision was requested.
    pub vision_module: Option<PathBuf>,
    /// Whether the planned projector is already present locally.
    pub vision_module_downloaded: bool,
    /// The resolved or planned drafter when draft speculation was requested.
    pub draft_module: Option<PathBuf>,
    /// Whether the planned drafter is already present locally.
    pub draft_module_downloaded: bool,
    /// The full llama-server argv.
    pub argv: Vec<String>,
    pub server_port: u16,
    /// The proxy the agent routes through.
    pub proxy: EnsureProxyConfig,
    /// The agent-facing endpoint origin.
    pub base_url: String,
    /// The generated `.localpilot.toml` content.
    pub provider_toml: String,
    /// The agent env plan (also the DryRun env preview).
    pub env_plan: Vec<(&'static str, String)>,
    /// Human-readable resolution notes (e.g. vision requested but absent).
    pub notes: Vec<String>,
}

/// Resolve a launch request into a plan. Read-only: no download, no spawn, no
/// session mutation — safe to call for DryRun and reused verbatim by the live
/// path.
///
/// # Errors
/// A [`LauncherError`] when the model/context/quant cannot be resolved or the
/// argv cannot be built.
pub fn plan_launch(
    launcher: &LlamaLauncher,
    request: &LaunchRequest,
) -> Result<LaunchPlan, LauncherError> {
    use localx_llama_core::Launcher;

    let def = launcher.model_def(&request.key)?;
    if let Some(required) = launcher.required_mode(&request.key) {
        if request.mode != required {
            let label = |mode: Mode| {
                if mode == Mode::PrismMl {
                    "prism"
                } else {
                    mode.as_str()
                }
            };
            return Err(LauncherError::Unavailable(format!(
                "{} requires the '{}' engine; requested '{}'",
                request.key,
                label(required),
                label(request.mode)
            )));
        }
    }
    let context_key = launcher.resolve_context_key(&def, &request.context_key)?;
    let context_tokens = launcher.context_value(&def, &context_key).unwrap_or(0);

    let gguf_path = launcher.expected_gguf_path(&def, request.quant.as_deref())?;
    let gguf_downloaded = gguf_path.is_file();

    let mut notes = Vec::new();
    let vision_module = if request.use_vision {
        let resolved = launcher.expected_vision_module_path(&request.key, &def);
        if resolved.is_none() {
            notes.push(format!(
                "vision requested but no mmproj found for {}; launching text-only",
                request.key
            ));
        }
        resolved
    } else {
        None
    };
    let vision_module_downloaded = vision_module.as_ref().is_some_and(|path| path.is_file());

    let draft_module = if request.use_draft {
        let resolved = launcher.expected_draft_module_path(&request.key, &def);
        if resolved.is_none() {
            notes.push(format!(
                "draft speculation requested but {} configures no DraftModule; launching without it",
                request.key
            ));
        }
        resolved
    } else {
        None
    };
    let draft_module_downloaded = draft_module.as_ref().is_some_and(|path| path.is_file());

    let server_port = launcher.free_port(request.server_port_start)?;

    // --mmproj enters argv for an existing projector or one planned for download.
    let mut params = request.params.clone();
    params.vision_module_path = vision_module
        .as_ref()
        .and_then(|p| p.to_str().map(str::to_string));
    // The drafter likewise enters argv when resolved or planned for download.
    params.draft_module_path = draft_module
        .as_ref()
        .and_then(|p| p.to_str().map(str::to_string));
    let argv = build_llama_server_args(
        &def,
        &context_key,
        request.mode,
        &gguf_path.to_string_lossy(),
        i64::from(server_port),
        &params,
    )
    .map_err(|e| LauncherError::Unavailable(e.to_string()))?;

    // `--keep-thinking` routes the agent straight at the server: the no-think
    // proxy strips `<think>` unconditionally, so keeping thinking means bypassing
    // it (the tradeoff is losing the proxy's system-message merge). Otherwise the
    // agent talks to the proxy, which strips and merges.
    let base_url = if request.keep_thinking {
        format!("http://127.0.0.1:{server_port}")
    } else {
        format!("http://127.0.0.1:{}", request.proxy_port)
    };
    let proxy = EnsureProxyConfig::new(request.proxy_port, server_port);

    let max_output_tokens = launcher.max_output_tokens();
    let provider_toml = localpilot_config_toml(&LocalPilotConfigInputs {
        provider_kind: ProviderKind::Anthropic,
        base_url: base_url.clone(),
        model: request.key.clone(),
        // Vision is declared to the agent only when the projector resolved.
        supports_vision: vision_module.is_some(),
        max_tokens: max_output_tokens,
        context_tokens,
        bypass: request.bypass,
    });

    let mut env_inputs = EnvPlanInputs::new(base_url.clone(), request.key.clone());
    env_inputs.keep_thinking = request.keep_thinking;
    env_inputs.context_tokens = context_tokens;
    env_inputs.max_output_tokens = max_output_tokens;
    let env_plan = claude_env_plan(&env_inputs);

    Ok(LaunchPlan {
        key: request.key.clone(),
        context_key,
        context_tokens,
        gguf_path,
        gguf_downloaded,
        vision_module,
        vision_module_downloaded,
        draft_module,
        draft_module_downloaded,
        argv,
        server_port,
        proxy,
        base_url,
        provider_toml,
        env_plan,
        notes,
    })
}

/// What to do after a failed smoke test: a non-native mode retries the whole
/// launch on native llama.cpp (with AutoBest off — its overrides were tuned
/// for the failing fork); native failing is a hard stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmokeFallback {
    /// Retry on native llama.cpp, AutoBest disabled.
    RetryNative,
    /// Nothing left to fall back to — fail with the smoke detail.
    Fail,
}

/// The fallback decision for a failed smoke on `mode`.
#[must_use]
pub fn smoke_fallback(mode: Mode) -> SmokeFallback {
    match mode {
        Mode::Native => SmokeFallback::Fail,
        Mode::Turboquant | Mode::Mtpturbo => SmokeFallback::RetryNative,
        Mode::PrismMl => SmokeFallback::Fail,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;
    use serde_json::{Map, Value};

    fn launcher(dir: &std::path::Path) -> LlamaLauncher {
        let catalog: Map<String, Value> = serde_json::from_str(
            r#"{
            "Models": {
                "q36apex": {
                    "Root": "q36apex",
                    "Repo": "mudler/apex",
                    "Quants": { "apex-i-quality": "APEX-I-Quality.gguf" },
                    "Quant": "apex-i-quality",
                    "Contexts": { "": 32768, "64k": 65536 }
                },
                "bonsai": {
                    "Root": "bonsai",
                    "Repo": "prism-ml/bonsai",
                    "File": "bonsai.gguf",
                    "RequiredMode": "prism",
                    "VisionModule": "mmproj-Q8_0.gguf",
                    "DraftModule": "bonsai-dspark-Q4_1.gguf",
                    "Contexts": { "": 65536 }
                }
            }
        }"#,
        )
        .unwrap();
        // Scalars ride the settings layer — the catalog file is Models-only.
        let settings: Map<String, Value> = serde_json::from_str(&format!(
            r#"{{ "LlamaCppGgufRoot": {root} }}"#,
            root = Value::from(dir.to_str().unwrap())
        ))
        .unwrap();
        let catalog = Catalog::from_layers(&Map::new(), &catalog, &settings).unwrap();
        LlamaLauncher::new(catalog, "1.2.1", dir.join("home"), 24)
    }

    #[test]
    fn planning_resolves_without_downloading_or_touching_session_state() {
        let dir = tempfile::tempdir().unwrap();
        let launcher = launcher(dir.path());
        let request = LaunchRequest::new("q36apex", "64k", Mode::Turboquant);
        let plan = plan_launch(&launcher, &request).expect("plan");

        // The GGUF path resolved WITHOUT a download (file absent, path known).
        assert!(plan.gguf_path.ends_with("APEX-I-Quality.gguf"));
        assert!(!plan.gguf_downloaded);
        assert!(!plan.gguf_path.exists(), "planning downloads nothing");
        // No session state was committed and no files were created.
        assert!(launcher.current_session().is_none());
        assert!(!dir.path().join("home").exists());

        // The plan is fully wired: argv carries the model and port, the
        // provider config carries the context carry-fix, the env plan the
        // paired caps.
        assert!(plan.argv.contains(&"-m".to_string()));
        assert_eq!(plan.context_tokens, 65_536);
        assert!(plan.provider_toml.contains("context_window = 65536"));
        assert!(plan
            .env_plan
            .iter()
            .any(|(n, v)| *n == "CLAUDE_CODE_MAX_CONTEXT_TOKENS" && v == "65536"));
        assert_eq!(plan.proxy.target_port, plan.server_port);
        assert!(plan.base_url.starts_with("http://127.0.0.1:"));
    }

    #[test]
    fn vision_is_honest_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let launcher = launcher(dir.path());
        let mut request = LaunchRequest::new("q36apex", "", Mode::Native);
        request.use_vision = true;

        // Requested but absent: text-only, noted, never declared.
        let plan = plan_launch(&launcher, &request).expect("plan");
        assert!(plan.vision_module.is_none());
        assert!(!plan.argv.contains(&"--mmproj".to_string()));
        assert!(!plan.provider_toml.contains("supports_vision"));
        assert!(plan.notes.iter().any(|n| n.contains("text-only")));

        // Projector on disk: --mmproj + supports_vision, together.
        let folder = dir.path().join("q36apex");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("mmproj-f16.gguf"), "x").unwrap();
        let plan = plan_launch(&launcher, &request).expect("plan");
        assert!(plan.vision_module.is_some());
        assert!(plan.argv.contains(&"--mmproj".to_string()));
        assert!(plan.provider_toml.contains("supports_vision = true"));

        // Not requested: a present projector stays unused (opt-in).
        request.use_vision = false;
        let plan = plan_launch(&launcher, &request).expect("plan");
        assert!(plan.vision_module.is_none());
        assert!(!plan.argv.contains(&"--mmproj".to_string()));
    }

    #[test]
    fn configured_projector_is_planned_for_download_and_required_mode_is_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let launcher = launcher(dir.path());

        let wrong = LaunchRequest::new("bonsai", "", Mode::Native);
        let err = plan_launch(&launcher, &wrong).unwrap_err();
        assert!(err.to_string().contains("requires the 'prism' engine"));

        let mut request = LaunchRequest::new("bonsai", "", Mode::PrismMl);
        request.use_vision = true;
        let plan = plan_launch(&launcher, &request).unwrap();
        assert!(plan
            .vision_module
            .as_ref()
            .is_some_and(|path| path.ends_with("mmproj-Q8_0.gguf")));
        assert!(!plan.vision_module_downloaded);
        assert!(plan.argv.contains(&"--mmproj".to_string()));
        assert!(plan.provider_toml.contains("supports_vision = true"));
    }

    #[test]
    fn draft_speculation_is_opt_in_and_honest() {
        let dir = tempfile::tempdir().unwrap();
        let launcher = launcher(dir.path());

        // Requested on a model with no DraftModule: launched without, noted.
        let mut request = LaunchRequest::new("q36apex", "", Mode::Native);
        request.use_draft = true;
        let plan = plan_launch(&launcher, &request).expect("plan");
        assert!(plan.draft_module.is_none());
        assert!(!plan.argv.contains(&"--spec-draft-model".to_string()));
        assert!(plan.notes.iter().any(|n| n.contains("no DraftModule")));

        // Configured drafter: planned for download, argv wired as draft-simple.
        let mut request = LaunchRequest::new("bonsai", "", Mode::PrismMl);
        request.use_draft = true;
        let plan = plan_launch(&launcher, &request).expect("plan");
        assert!(plan
            .draft_module
            .as_ref()
            .is_some_and(|path| path.ends_with("bonsai-dspark-Q4_1.gguf")));
        assert!(!plan.draft_module_downloaded);
        let argv = plan.argv.join(" ");
        assert!(argv.contains("--spec-type draft-simple"));
        assert!(argv.contains("--spec-draft-model"));

        // Not requested: a configured drafter stays unused (opt-in).
        let request = LaunchRequest::new("bonsai", "", Mode::PrismMl);
        let plan = plan_launch(&launcher, &request).expect("plan");
        assert!(plan.draft_module.is_none());
        assert!(!plan.argv.contains(&"--spec-draft-model".to_string()));
    }

    #[test]
    fn keep_thinking_routes_direct_to_the_server_not_the_proxy() {
        let dir = tempfile::tempdir().unwrap();
        let launcher = launcher(dir.path());

        // Default: the agent endpoint is the no-think proxy port.
        let request = LaunchRequest::new("q36apex", "64k", Mode::Turboquant);
        let proxied = plan_launch(&launcher, &request).unwrap();
        assert!(proxied
            .base_url
            .ends_with(&format!(":{}", proxied.proxy.listen_port)));

        // keep_thinking: the endpoint is the server port, so the stripping proxy
        // is bypassed (uses_proxy will be false at launch).
        let mut request = LaunchRequest::new("q36apex", "64k", Mode::Turboquant);
        request.keep_thinking = true;
        let direct = plan_launch(&launcher, &request).unwrap();
        assert_eq!(
            direct.base_url,
            format!("http://127.0.0.1:{}", direct.server_port)
        );
        assert!(!direct
            .base_url
            .ends_with(&format!(":{}", direct.proxy.listen_port)));
    }

    #[test]
    fn the_smoke_fallback_rule_is_native_or_nothing() {
        assert_eq!(smoke_fallback(Mode::Turboquant), SmokeFallback::RetryNative);
        assert_eq!(smoke_fallback(Mode::Mtpturbo), SmokeFallback::RetryNative);
        assert_eq!(smoke_fallback(Mode::Native), SmokeFallback::Fail);
        assert_eq!(smoke_fallback(Mode::PrismMl), SmokeFallback::Fail);
    }

    #[test]
    fn session_defaults_pin_one_slot_unless_something_chose_otherwise() {
        // Nothing chosen anywhere: the single-session defaults apply. Relying
        // on llama-server's own default instead is a several-fold KV-cache
        // memory multiplier now that its `--parallel` default is multi-slot
        // auto with the full context allocated per slot.
        let mut request = LaunchRequest::new("m", "", Mode::Native);
        request.apply_session_defaults(&LaunchParams::default());
        assert_eq!(request.params.parallel, Some(1));
        assert_eq!(request.params.cache_reuse, Some(256));

        // An explicit value (flag or AutoBest) outranks the settings overlay,
        // and the overlay outranks the default.
        let mut request = LaunchRequest::new("m", "", Mode::Native);
        request.params.parallel = Some(2);
        let overlay = LaunchParams {
            parallel: Some(4),
            cache_reuse: Some(512),
            n_cpu_moe: Some(9),
            ..LaunchParams::default()
        };
        request.apply_session_defaults(&overlay);
        assert_eq!(request.params.parallel, Some(2));
        assert_eq!(request.params.cache_reuse, Some(512));
        assert_eq!(request.params.n_cpu_moe, Some(9));

        // The escape hatch: a non-positive setting is kept, and the argv
        // builder omits `--parallel` for it, restoring the server's auto
        // slot count for the launch that genuinely wants it.
        let mut request = LaunchRequest::new("m", "", Mode::Native);
        let auto = LaunchParams {
            parallel: Some(-1),
            ..LaunchParams::default()
        };
        request.apply_session_defaults(&auto);
        assert_eq!(request.params.parallel, Some(-1));
    }

    #[test]
    fn unknown_keys_and_contexts_fail_the_plan_actionably() {
        let dir = tempfile::tempdir().unwrap();
        let launcher = launcher(dir.path());
        assert!(plan_launch(&launcher, &LaunchRequest::new("nope", "", Mode::Native)).is_err());
        let err = plan_launch(
            &launcher,
            &LaunchRequest::new("q36apex", "999k", Mode::Native),
        )
        .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("context"));
    }
}
