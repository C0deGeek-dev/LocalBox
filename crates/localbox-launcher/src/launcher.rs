//! [`LlamaLauncher`]: LocalBox's implementation of the shared launcher
//! contract. Domain resolution delegates to the shared crates; this type owns
//! only what is launcher-specific — the catalog, on-disk layout, per-mode
//! install roots, and the recorded backend session.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use localx_llama_core::{
    BackendSession, KvTypes, Launcher, LauncherError, LauncherVersion, Mode, ModelDef,
    RUNTIME_LLAMACPP, TARGET_LOCALBOX,
};
use localx_llama_runtime::is_port_free;
use localx_llama_runtime::server::server_exe_name;

use crate::catalog::Catalog;

/// The KV cache types every llama.cpp build accepts.
const STANDARD_KV_TYPES: &[&str] = &[
    "f16", "f32", "bf16", "q8_0", "q4_0", "q4_1", "q5_0", "q5_1", "iq4_nl",
];

/// LocalBox's launcher: the catalog plus the host facts the contract needs.
pub struct LlamaLauncher {
    catalog: Catalog,
    /// This product's version string (from `VERSION`).
    product_version: String,
    /// The user home the `~/.local-llm` tree hangs off (injected for tests).
    home: PathBuf,
    /// Detected device VRAM in GB (0 = unknown), probed by the app at startup.
    vram_gb: u32,
    session: Mutex<Option<BackendSession>>,
}

impl LlamaLauncher {
    /// A launcher over an assembled catalog and host facts.
    #[must_use]
    pub fn new(
        catalog: Catalog,
        product_version: impl Into<String>,
        home: impl Into<PathBuf>,
        vram_gb: u32,
    ) -> Self {
        Self {
            catalog,
            product_version: product_version.into(),
            home: home.into(),
            vram_gb,
            session: Mutex::new(None),
        }
    }

    /// The recorded backend session, when one is active.
    #[must_use]
    pub fn current_session(&self) -> Option<BackendSession> {
        self.session.lock().ok().and_then(|s| s.clone())
    }

    /// Launch params the user set directly in `settings.json` — the llama-server
    /// tunables the catalog documents (`LlamaCppMlock`, `LlamaCppNoMmap`,
    /// `LlamaCppAgentParallel`, `LlamaCppAgentCacheReuse`, `LlamaCppNCpuMoe`), as
    /// opposed to values that arrive via an AutoBest profile. Absent keys stay
    /// `None`, so an AutoBest profile or a caller default still wins over an
    /// unset setting.
    #[must_use]
    pub fn settings_launch_params(&self) -> localx_llama_core::args::LaunchParams {
        use serde_json::Value;
        let int = |k: &str| self.catalog.setting(k).and_then(Value::as_i64);
        let boolean = |k: &str| self.catalog.setting(k).and_then(Value::as_bool);
        localx_llama_core::args::LaunchParams {
            parallel: int("LlamaCppAgentParallel"),
            cache_reuse: int("LlamaCppAgentCacheReuse"),
            n_cpu_moe: int("LlamaCppNCpuMoe"),
            mlock: boolean("LlamaCppMlock"),
            no_mmap: boolean("LlamaCppNoMmap"),
            ..Default::default()
        }
    }

    /// The agent output-token cap (`LocalModelMaxOutputTokens`), defaulting to
    /// 16384 when unset. Documented in `settings.md`; fed to both the agent env
    /// plan and the LocalPilot provider config.
    #[must_use]
    pub fn max_output_tokens(&self) -> u32 {
        self.catalog
            .setting("LocalModelMaxOutputTokens")
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .filter(|&n| n > 0)
            .unwrap_or(16384)
    }

    fn timeout_setting(&self, key: &str) -> u32 {
        self.catalog
            .setting(key)
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .filter(|&n| n > 0)
            .unwrap_or(300)
    }

    /// Seconds to wait for `/health` readiness (`LlamaCppHealthCheckTimeoutSec`,
    /// default 300) — the shipped default the launcher previously ignored.
    #[must_use]
    pub fn health_check_timeout_secs(&self) -> u32 {
        self.timeout_setting("LlamaCppHealthCheckTimeoutSec")
    }

    /// Seconds to wait for the smoke reply (`LlamaCppSmokeTestTimeoutSec`,
    /// default 300).
    #[must_use]
    pub fn smoke_timeout_secs(&self) -> u32 {
        self.timeout_setting("LlamaCppSmokeTestTimeoutSec")
    }

    /// The GGUF's expected on-disk path — pure path math, no download, no
    /// existence requirement (the DryRun resolution).
    ///
    /// # Errors
    /// [`LauncherError::Unavailable`] when the catalog cannot name a file.
    pub fn expected_gguf_path(
        &self,
        def: &ModelDef,
        quant: Option<&str>,
    ) -> Result<PathBuf, LauncherError> {
        let folder = self.model_folder(def, "")?;
        let file = Self::model_file_name(def, quant)?;
        Ok(folder.join(file))
    }

    /// The model folder under the GGUF root (`<root>/<Def.Root or key>`).
    /// The root setting may be spelled with `~` or `%VAR%` — a child process
    /// never expands those, so they resolve here before entering any argv.
    fn model_folder(&self, def: &ModelDef, key_fallback: &str) -> Result<PathBuf, LauncherError> {
        let root = self.catalog.gguf_root().ok_or_else(|| {
            LauncherError::Unavailable(
                "LlamaCppGgufRoot is not configured; set it in settings.json".to_string(),
            )
        })?;
        let root = expand_path_with_home(&root.to_string_lossy(), &self.home);
        let folder = def.root.clone().unwrap_or_else(|| key_fallback.to_string());
        Ok(root.join(folder))
    }

    /// The GGUF filename for a definition and optional quant override.
    fn model_file_name(def: &ModelDef, quant: Option<&str>) -> Result<String, LauncherError> {
        if !def.quants.is_empty() {
            let quant_key = match quant {
                Some(q) => localx_llama_core::model::resolve_quant_key(def, q)
                    .map_err(|e| LauncherError::Unavailable(e.to_string()))?,
                None => def.quant.clone().ok_or_else(|| {
                    LauncherError::Unavailable(
                        "the model declares quants but no default Quant".to_string(),
                    )
                })?,
            };
            let entry = def.quants.get(&quant_key).ok_or_else(|| {
                LauncherError::Unavailable(format!("quant '{quant_key}' is not in the catalog"))
            })?;
            return Ok(entry.file.clone());
        }
        def.file.clone().ok_or_else(|| {
            LauncherError::Unavailable("the model declares neither Quants nor File".to_string())
        })
    }
}

/// Expand `~`-prefixed and `%VAR%`-style path spellings against a home and the
/// process environment. `%USERPROFILE%` and `%HOME%` always mean the given
/// home — even when that env var is unset on this OS — so a config authored on
/// Windows (`%USERPROFILE%\.local-llm\gguf`) still resolves on macOS/Linux
/// instead of surviving as a literal, relative path.
#[must_use]
pub fn expand_path_with_home(path: &str, home: &Path) -> PathBuf {
    let mut expanded = String::with_capacity(path.len());
    let mut rest = path;
    while let Some(start) = rest.find('%') {
        let Some(len) = rest[start + 1..].find('%') else {
            break;
        };
        let name = &rest[start + 1..start + 1 + len];
        expanded.push_str(&rest[..start]);
        if name.eq_ignore_ascii_case("USERPROFILE") || name.eq_ignore_ascii_case("HOME") {
            // Funnel the home markers into the `~` branch below so their tail
            // resolves against `home` regardless of the host's environment.
            expanded.push('~');
        } else {
            match std::env::var(name) {
                Ok(value) => expanded.push_str(&value),
                Err(_) => {
                    expanded.push('%');
                    expanded.push_str(name);
                    expanded.push('%');
                }
            }
        }
        rest = &rest[start + len + 2..];
    }
    expanded.push_str(rest);

    if let Some(tail) = expanded.strip_prefix('~') {
        // Join component-by-component, splitting on either separator, so a
        // `\`-spelled Windows tail does not become one literal filename.
        let mut out = home.to_path_buf();
        for part in tail.split(['/', '\\']).filter(|p| !p.is_empty()) {
            out.push(part);
        }
        return out;
    }
    PathBuf::from(expanded)
}

impl Launcher for LlamaLauncher {
    fn version(&self) -> LauncherVersion {
        LauncherVersion {
            version: self.product_version.clone(),
            api_version: 1,
            launcher_export_version: 1,
            supported_targets: vec![TARGET_LOCALBOX.to_string()],
            supported_runtimes: vec![RUNTIME_LLAMACPP.to_string()],
        }
    }

    fn model_def(&self, key: &str) -> Result<ModelDef, LauncherError> {
        self.catalog
            .model(key)
            .cloned()
            .ok_or_else(|| LauncherError::UnknownModel(key.to_string()))
    }

    fn gguf_path(&self, def: &ModelDef, quant: Option<&str>) -> Result<PathBuf, LauncherError> {
        let path = self.expected_gguf_path(def, quant)?;
        if path.is_file() {
            Ok(path)
        } else {
            Err(LauncherError::Unavailable(format!(
                "GGUF not downloaded: {} (install the model first)",
                path.display()
            )))
        }
    }

    fn context_value(&self, def: &ModelDef, context_key: &str) -> Result<u32, LauncherError> {
        let value = localx_llama_core::model::context_value(def, context_key)
            .map_err(|e| LauncherError::Unavailable(e.to_string()))?;
        value
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| LauncherError::Unavailable("no context tokens recorded".to_string()))
    }

    fn resolve_context_key(
        &self,
        def: &ModelDef,
        context_key: &str,
    ) -> Result<String, LauncherError> {
        localx_llama_core::model::resolve_context_key(def, context_key)
            .map_err(|e| LauncherError::Unavailable(e.to_string()))
    }

    fn vision_module_path(&self, key: &str, def: &ModelDef) -> Option<PathBuf> {
        let folder = self.model_folder(def, key).ok()?;
        // A configured module wins; otherwise auto-detect a local mmproj.
        if let Some(configured) = &def.vision_module {
            let path = folder.join(configured);
            return path.is_file().then_some(path);
        }
        let entries = std::fs::read_dir(&folder).ok()?;
        let mut candidates: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("mmproj") && n.ends_with(".gguf"))
            })
            .collect();
        candidates.sort();
        candidates.into_iter().next()
    }

    fn resolve_quant_key(&self, def: &ModelDef, quant: &str) -> Result<String, LauncherError> {
        localx_llama_core::model::resolve_quant_key(def, quant)
            .map_err(|e| LauncherError::Unavailable(e.to_string()))
    }

    fn vram_gb(&self) -> u32 {
        self.vram_gb
    }

    fn server_binary(&self, mode: Mode, _non_interactive: bool) -> Result<PathBuf, LauncherError> {
        let root = self.install_root(mode);
        let exe = server_exe_name();
        for candidate in [root.join(exe), root.join("bin").join(exe)] {
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        Err(LauncherError::Unavailable(format!(
            "no {exe} under {} (install llama.cpp for the {} mode first)",
            root.display(),
            mode.as_str()
        )))
    }

    fn bench_binary(&self, _non_interactive: bool) -> Option<PathBuf> {
        let exe = if cfg!(windows) {
            "llama-bench.exe"
        } else {
            "llama-bench"
        };
        let root = self.install_root(Mode::Native);
        [root.join(exe), root.join("bin").join(exe)]
            .into_iter()
            .find(|p| p.is_file())
    }

    fn perplexity_binary(&self, _non_interactive: bool, mode: Mode) -> Option<PathBuf> {
        let exe = if cfg!(windows) {
            "llama-perplexity.exe"
        } else {
            "llama-perplexity"
        };
        let root = self.install_root(mode);
        [root.join(exe), root.join("bin").join(exe)]
            .into_iter()
            .find(|p| p.is_file())
    }

    fn install_root(&self, mode: Mode) -> PathBuf {
        let dir = match mode {
            Mode::Native => "llama-cpp",
            Mode::Turboquant => "llama-cpp-turboquant",
            Mode::Mtpturbo => "llama-cpp-mtpturbo",
        };
        self.home.join(".local-llm").join(dir)
    }

    fn kv_types(&self, def: &ModelDef) -> KvTypes {
        let k = def.kv_cache_k.clone().unwrap_or_else(|| "q8_0".to_string());
        let v = def.kv_cache_v.clone().unwrap_or_else(|| k.clone());
        KvTypes { k, v }
    }

    fn kv_type_supported(&self, kv_type: &str, mode: Mode) -> bool {
        let lower = kv_type.to_ascii_lowercase();
        if lower.starts_with("turbo") {
            // The turbo cache types exist only in the turbo forks.
            return matches!(mode, Mode::Turboquant | Mode::Mtpturbo);
        }
        STANDARD_KV_TYPES.contains(&lower.as_str())
    }

    fn free_port(&self, start: u16) -> Result<u16, LauncherError> {
        (start..start.saturating_add(200))
            .find(|p| is_port_free(*p))
            .ok_or_else(|| {
                LauncherError::Unavailable(format!("no free TCP port in {start}..{}", start + 200))
            })
    }

    fn wait_server(&self, port: u16, timeout_secs: u32) -> Result<(), LauncherError> {
        // Readiness is /health answering 200: a llama-server listens
        // immediately and answers 503 while the model is still loading.
        if localx_llama_runtime::server::wait_for_ready(
            port,
            Duration::from_secs(u64::from(timeout_secs)),
        ) {
            Ok(())
        } else {
            Err(LauncherError::Unavailable(format!(
                "server on port {port} did not become ready within {timeout_secs}s"
            )))
        }
    }

    fn stop_server(&self, _quiet: bool) {
        let session = self.session.lock().ok().and_then(|mut s| s.take());
        if let Some(BackendSession { pid: Some(pid), .. }) = session {
            let mut system = sysinfo::System::new();
            let target = sysinfo::Pid::from_u32(pid);
            system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[target]), true);
            if let Some(process) = system.process(target) {
                process.kill();
            }
        }
    }

    fn set_backend_session(&self, session: &BackendSession) {
        if let Ok(mut slot) = self.session.lock() {
            *slot = Some(session.clone());
        }
    }

    fn expand_path(&self, path: &str) -> PathBuf {
        expand_path_with_home(path, &self.home)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::{Map, Value};

    fn catalog_with_root(gguf_root: &Path) -> Catalog {
        let catalog: Map<String, Value> = serde_json::from_str(
            r#"{
            "Models": {
                "q36apex": {
                    "Root": "q36apex",
                    "Repo": "mudler/apex",
                    "Quants": {
                        "apex-i-quality": "APEX-I-Quality.gguf",
                        "apex-compact": "APEX-Compact.gguf"
                    },
                    "Quant": "apex-i-quality",
                    "Contexts": { "": 32768, "64k": 65536 },
                    "KvCacheK": "q8_0"
                },
                "single": {
                    "Root": "single",
                    "Repo": "x/y",
                    "File": "single.gguf",
                    "Contexts": { "": 8192 }
                }
            }
        }"#,
        )
        .unwrap();
        let settings: Map<String, Value> = serde_json::from_str(&format!(
            r#"{{ "LlamaCppGgufRoot": {} }}"#,
            serde_json::Value::from(gguf_root.to_str().unwrap())
        ))
        .unwrap();
        Catalog::from_layers(&Map::new(), &catalog, &settings).unwrap()
    }

    fn launcher(dir: &Path) -> LlamaLauncher {
        LlamaLauncher::new(catalog_with_root(dir), "1.2.1", dir.join("home"), 24)
    }

    #[test]
    fn settings_launch_params_reads_the_documented_tunables() {
        let models: Map<String, Value> = serde_json::from_str(r#"{ "Models": {} }"#).unwrap();
        let settings: Map<String, Value> = serde_json::from_str(
            r#"{ "LlamaCppMlock": true, "LlamaCppNoMmap": false,
                 "LlamaCppAgentParallel": 2, "LlamaCppAgentCacheReuse": 512,
                 "LlamaCppNCpuMoe": 9 }"#,
        )
        .unwrap();
        let cat = Catalog::from_layers(&Map::new(), &models, &settings).unwrap();
        let p = LlamaLauncher::new(cat, "1.2.1", "/tmp/home", 24).settings_launch_params();
        assert_eq!(p.mlock, Some(true));
        assert_eq!(p.no_mmap, Some(false));
        assert_eq!(p.parallel, Some(2));
        assert_eq!(p.cache_reuse, Some(512));
        assert_eq!(p.n_cpu_moe, Some(9));

        // An unset key stays None so an AutoBest profile or a default still wins.
        let bare = Catalog::from_layers(&Map::new(), &models, &Map::new()).unwrap();
        let empty = LlamaLauncher::new(bare, "1.2.1", "/tmp/home", 24).settings_launch_params();
        assert_eq!(empty.mlock, None);
        assert_eq!(empty.parallel, None);
    }

    #[test]
    fn the_envelope_satisfies_the_shared_compatibility_gate() {
        let dir = tempfile::tempdir().unwrap();
        let launcher = launcher(dir.path());
        let version = launcher.version();
        localx_llama_core::assert_compatible(&version, TARGET_LOCALBOX, RUNTIME_LLAMACPP)
            .expect("LocalBox's own envelope must pass");
        assert_eq!(version.version, "1.2.1");
    }

    #[test]
    fn model_resolution_delegates_to_the_shared_domain() {
        let dir = tempfile::tempdir().unwrap();
        let launcher = launcher(dir.path());
        let def = launcher.model_def("q36apex").expect("known key");
        assert!(matches!(
            launcher.model_def("nope"),
            Err(LauncherError::UnknownModel(_))
        ));
        // Context aliases resolve through the shared rules (fast -> 32k... the
        // default context is the blank key here).
        assert_eq!(launcher.resolve_context_key(&def, "default").unwrap(), "");
        assert_eq!(launcher.context_value(&def, "64k").unwrap(), 65_536);
        assert_eq!(
            launcher.resolve_quant_key(&def, "APEX-I-QUALITY").unwrap(),
            "apex-i-quality"
        );
    }

    #[test]
    fn gguf_path_resolves_on_disk_and_fails_actionably_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let launcher = launcher(dir.path());
        let def = launcher.model_def("q36apex").unwrap();
        // Not downloaded yet: actionable error, not a phantom path.
        let err = launcher.gguf_path(&def, None).unwrap_err();
        assert!(err.to_string().contains("not downloaded"));
        // Once the file exists, the default quant resolves...
        let folder = dir.path().join("q36apex");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("APEX-I-Quality.gguf"), "x").unwrap();
        assert!(launcher
            .gguf_path(&def, None)
            .unwrap()
            .ends_with("APEX-I-Quality.gguf"));
        // ...and a quant override picks its own file.
        std::fs::write(folder.join("APEX-Compact.gguf"), "x").unwrap();
        assert!(launcher
            .gguf_path(&def, Some("apex-compact"))
            .unwrap()
            .ends_with("APEX-Compact.gguf"));
        // Single-file models resolve via File.
        let single = launcher.model_def("single").unwrap();
        let sfolder = dir.path().join("single");
        std::fs::create_dir_all(&sfolder).unwrap();
        std::fs::write(sfolder.join("single.gguf"), "x").unwrap();
        assert!(launcher
            .gguf_path(&single, None)
            .unwrap()
            .ends_with("single.gguf"));
    }

    #[test]
    fn vision_module_prefers_configured_then_auto_detects() {
        let dir = tempfile::tempdir().unwrap();
        let launcher = launcher(dir.path());
        let mut def = launcher.model_def("q36apex").unwrap();
        let folder = dir.path().join("q36apex");
        std::fs::create_dir_all(&folder).unwrap();
        // Nothing on disk: no projector.
        assert!(launcher.vision_module_path("q36apex", &def).is_none());
        // An auto-detected mmproj is found (sorted-first for determinism).
        std::fs::write(folder.join("mmproj-f16.gguf"), "x").unwrap();
        assert!(launcher
            .vision_module_path("q36apex", &def)
            .unwrap()
            .ends_with("mmproj-f16.gguf"));
        // A configured module wins — and only when it actually exists.
        def.vision_module = Some("mmproj-custom.gguf".to_string());
        assert!(launcher.vision_module_path("q36apex", &def).is_none());
        std::fs::write(folder.join("mmproj-custom.gguf"), "x").unwrap();
        assert!(launcher
            .vision_module_path("q36apex", &def)
            .unwrap()
            .ends_with("mmproj-custom.gguf"));
    }

    #[test]
    fn install_roots_and_binary_resolution_are_per_mode() {
        let dir = tempfile::tempdir().unwrap();
        let launcher = launcher(dir.path());
        let home = dir.path().join("home");
        assert_eq!(
            launcher.install_root(Mode::Native),
            home.join(".local-llm").join("llama-cpp")
        );
        assert_eq!(
            launcher.install_root(Mode::Turboquant),
            home.join(".local-llm").join("llama-cpp-turboquant")
        );
        assert_eq!(
            launcher.install_root(Mode::Mtpturbo),
            home.join(".local-llm").join("llama-cpp-mtpturbo")
        );
        // Missing binary: actionable per-mode error.
        let err = launcher.server_binary(Mode::Turboquant, true).unwrap_err();
        assert!(err.to_string().contains("turboquant"));
        // A binary in the root (or bin/) resolves.
        let root = launcher.install_root(Mode::Native);
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("bin").join(server_exe_name()), "x").unwrap();
        assert!(launcher.server_binary(Mode::Native, true).is_ok());
    }

    #[test]
    fn kv_capability_mirrors_the_fork_matrix() {
        let dir = tempfile::tempdir().unwrap();
        let launcher = launcher(dir.path());
        let def = launcher.model_def("q36apex").unwrap();
        let kv = launcher.kv_types(&def);
        assert_eq!(kv.k, "q8_0");
        assert_eq!(kv.v, "q8_0", "V defaults to the K type");
        assert!(launcher.kv_type_supported("q8_0", Mode::Native));
        assert!(!launcher.kv_type_supported("turbo3", Mode::Native));
        assert!(launcher.kv_type_supported("turbo3", Mode::Turboquant));
        assert!(launcher.kv_type_supported("TURBO4", Mode::Mtpturbo));
        assert!(!launcher.kv_type_supported("q9_9", Mode::Native));
    }

    #[test]
    fn sessions_record_and_stop_reads_the_recorded_pid() {
        let dir = tempfile::tempdir().unwrap();
        let launcher = launcher(dir.path());
        assert!(launcher.current_session().is_none());
        let session = BackendSession {
            key: "q36apex".to_string(),
            mode: Mode::Turboquant,
            port: 8080,
            pid: None,
        };
        launcher.set_backend_session(&session);
        assert_eq!(launcher.current_session(), Some(session));
        // Stopping with no PID recorded is a no-op that clears the session.
        launcher.stop_server(true);
        assert!(launcher.current_session().is_none());
    }

    #[test]
    fn path_expansion_covers_tilde_and_env_spellings() {
        let home = Path::new("/home/tester");
        assert_eq!(
            expand_path_with_home("~/models", home),
            PathBuf::from("/home/tester").join("models")
        );
        std::env::set_var("LOCALBOX_TEST_VAR", "expanded");
        assert_eq!(
            expand_path_with_home("%LOCALBOX_TEST_VAR%/x", home),
            PathBuf::from("expanded/x")
        );
        // An unknown var is preserved literally rather than eaten.
        assert_eq!(
            expand_path_with_home("%NOPE_UNSET%/x", home),
            PathBuf::from("%NOPE_UNSET%/x")
        );
        std::env::remove_var("LOCALBOX_TEST_VAR");
    }

    #[test]
    fn windows_home_spellings_resolve_against_home_on_any_os() {
        let home = Path::new("/home/tester");
        let want = home.join(".local-llm").join("gguf");
        // The Windows default spelling (backslash tail, `%USERPROFILE%`) must
        // resolve to `<home>/.local-llm/gguf`, not a literal relative path —
        // and does so without reading the environment, so it holds on macOS,
        // Linux, and Windows alike.
        assert_eq!(
            expand_path_with_home("%USERPROFILE%\\.local-llm\\gguf", home),
            want
        );
        // `%HOME%` is treated the same, and the cross-platform `~/` spelling
        // lands in the identical place.
        assert_eq!(
            expand_path_with_home("%HOME%\\.local-llm\\gguf", home),
            want
        );
        assert_eq!(expand_path_with_home("~/.local-llm/gguf", home), want);
    }

    #[test]
    fn free_port_returns_a_usable_port() {
        let dir = tempfile::tempdir().unwrap();
        let launcher = launcher(dir.path());
        let port = launcher.free_port(38_000).expect("a free port");
        assert!((38_000..38_200).contains(&port));
        assert!(is_port_free(port));
    }
}
