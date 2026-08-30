//! Catalog inspection and cleanup conveniences: the model overview and
//! per-model detail (`info`), downloaded-model purge (`purge`), and the
//! server-log tail (`log`).
//!
//! Everything here is pure rendering/planning over the loaded catalog; the
//! command layer performs the deletions and file reads.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use localbox_launcher::catalog::Catalog;
use localbox_launcher::profile::{resolve_run_profile, RunProfileQuery};
use localx_llama_core::ModelDef;
use serde::Serialize;

use crate::guided::model_tier;

/// Resolve a user-typed model name to its catalog key: the key itself, a
/// `CommandAliases` entry, or the model's on-disk folder name.
#[must_use]
pub fn resolve_model_key(catalog: &Catalog, name: &str) -> Option<String> {
    catalog.resolve_model_key(name)
}

/// Tier display order: the known tiers first, anything else after, sorted.
fn tier_order(catalog: &Catalog) -> Vec<String> {
    let mut order = vec![
        "recommended".to_string(),
        "experimental".to_string(),
        "legacy".to_string(),
    ];
    let mut extra: BTreeSet<String> = BTreeSet::new();
    for key in catalog.model_keys() {
        if let Some(def) = catalog.model(key) {
            let tier = model_tier(def);
            if !order.contains(&tier) {
                extra.insert(tier);
            }
        }
    }
    order.extend(extra);
    order
}

fn context_summary(def: &ModelDef) -> String {
    def.contexts
        .iter()
        .map(|(label, tokens)| {
            let label = if label.is_empty() { "default" } else { label };
            format!("{label}={tokens}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn quant_summary(def: &ModelDef) -> String {
    def.quants
        .iter()
        .map(|(key, entry)| {
            let mut rendered = key.clone();
            if def.quant.as_deref() == Some(key) {
                rendered.push_str(" [current]");
            }
            if let Some(size) = entry.size_gb {
                let _ = write!(rendered, " ({size} GB)");
            }
            rendered
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn push_model_block(out: &mut String, key: &str, def: &ModelDef) {
    let _ = writeln!(out, "{key}");
    if let Some(name) = def.display_name.as_deref().filter(|n| !n.trim().is_empty()) {
        let _ = writeln!(out, "  Name     : {name}");
    }
    if let Some(about) = def.description.as_deref().filter(|d| !d.trim().is_empty()) {
        let _ = writeln!(out, "  About    : {about}");
    }
    let _ = writeln!(out, "  Source   : GGUF: {}", def.repo);
    let _ = writeln!(out, "  Contexts : {}", context_summary(def));
    if !def.quants.is_empty() {
        let _ = writeln!(out, "  Quants   : {}", quant_summary(def));
    }
    if def.vision_module.is_some() {
        let _ = writeln!(out, "  Vision   : yes (--vision)");
    }
}

/// The full catalog overview, grouped by tier.
#[must_use]
pub fn render_model_overview(catalog: &Catalog) -> String {
    let mut out = String::from("Configured models (by tier)\n");
    for tier in tier_order(catalog) {
        let keys: Vec<&str> = catalog
            .model_keys()
            .into_iter()
            .filter(|key| {
                catalog
                    .model(key)
                    .is_some_and(|def| model_tier(def) == tier)
            })
            .collect();
        if keys.is_empty() {
            continue;
        }
        let _ = write!(out, "\n[{tier}]\n");
        for key in keys {
            if let Some(def) = catalog.model(key) {
                push_model_block(&mut out, key, def);
            }
        }
    }
    out.push_str("\nDetails: localbox info <model>\n");
    out
}

/// Version of the `localbox models --json` cross-process contract.
pub const MODELS_CATALOG_SCHEMA: u32 = 1;

/// Effective saved-profile state advertised for one catalog model.
#[derive(Debug, Clone, Serialize)]
pub struct RunProfileSummary {
    /// `tuned` when a saved entry resolves, otherwise `defaults`.
    pub source: String,
    /// `measured` or `adopted` when the source is tuned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// Original measurement generation for an adopted entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adopted_from_tuner_version: Option<i64>,
    /// UTC time at which LocalBox adopted the entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adopted_at: Option<String>,
    /// Exact inspected profile path, for diagnostics.
    pub source_path: String,
    /// Machine-readable reason when defaults would be used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Visible warning a parent process must show before fallback.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    /// Resolved tuned quant, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quant: Option<String>,
    /// Resolved tuned context key, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// Resolved tuned engine, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

/// One launchable catalog entry. The canonical accepted name is intentionally
/// the first field so both human and chat renderers put it first.
#[derive(Debug, Clone, Serialize)]
pub struct ModelCatalogEntry {
    /// Canonical key accepted by `localbox serve`.
    pub name: String,
    /// Additional accepted spellings owned by the LocalBox catalog.
    pub aliases: Vec<String>,
    /// Human label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Hugging Face model identity.
    pub repository: String,
    /// Catalog default quant identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_quant: Option<String>,
    /// Catalog-required engine, when any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_mode: Option<String>,
    /// Effective saved-profile state for a default launch.
    pub run_profile: RunProfileSummary,
    /// Whether this model is serving now, when LocalBox can establish it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
}

/// Stable JSON envelope for LocalPilot and other local callers.
#[derive(Debug, Clone, Serialize)]
pub struct ModelsCatalog {
    /// Contract schema version.
    pub schema: u32,
    /// Sorted launchable catalog entries.
    pub models: Vec<ModelCatalogEntry>,
}

fn mode_name(mode: localx_llama_core::Mode) -> String {
    if mode == localx_llama_core::Mode::PrismMl {
        "prism".to_string()
    } else {
        mode.as_str().to_string()
    }
}

/// Build the LocalBox-owned launch catalog and tuned/default diagnostics.
#[must_use]
pub fn models_catalog(catalog: &Catalog, home: &Path) -> ModelsCatalog {
    let models = catalog
        .model_keys()
        .into_iter()
        .filter_map(|key| {
            let def = catalog.model(key)?;
            let profile = resolve_run_profile(home, key, RunProfileQuery::default());
            let warning = profile.warning(key);
            let entry = profile.entry.as_ref();
            Some(ModelCatalogEntry {
                name: key.to_string(),
                aliases: catalog
                    .aliases_for(key)
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                display_name: def.display_name.clone(),
                repository: def.repo.clone(),
                default_quant: def.quant.clone(),
                required_mode: catalog.required_mode(key).map(mode_name),
                run_profile: RunProfileSummary {
                    source: if entry.is_some() { "tuned" } else { "defaults" }.to_string(),
                    origin: entry.map(|_| {
                        if profile.adoption.is_some() {
                            "adopted"
                        } else {
                            "measured"
                        }
                        .to_string()
                    }),
                    adopted_from_tuner_version: profile
                        .adoption
                        .as_ref()
                        .map(|adoption| adoption.from_tuner_version),
                    adopted_at: profile
                        .adoption
                        .as_ref()
                        .map(|adoption| adoption.adopted_at.clone()),
                    source_path: profile.path.display().to_string(),
                    reason: profile
                        .unavailable
                        .map(|reason| reason.as_str().to_string()),
                    warning,
                    quant: entry.map(|entry| entry.quant.clone()),
                    context: entry.map(|entry| entry.context_key.clone()),
                    mode: entry.map(|entry| mode_name(entry.mode)),
                },
                active: None,
            })
        })
        .collect();
    ModelsCatalog {
        schema: MODELS_CATALOG_SCHEMA,
        models,
    }
}

/// Concise human rendering of the same contract used by `--json`.
#[must_use]
pub fn render_models_catalog(catalog: &ModelsCatalog) -> String {
    let mut out = String::from("Launchable LocalBox models\n");
    for model in &catalog.models {
        let aliases = if model.aliases.is_empty() {
            String::new()
        } else {
            format!(" (aliases: {})", model.aliases.join(", "))
        };
        let display = model.display_name.as_deref().unwrap_or(&model.repository);
        let catalog_quant = model.default_quant.as_deref().unwrap_or("catalog default");
        let run = if model.run_profile.source == "tuned" {
            let adoption = if model.run_profile.origin.as_deref() == Some("adopted") {
                model.run_profile.adopted_from_tuner_version.map_or_else(
                    || " (adopted)".to_string(),
                    |version| format!(" (adopted from tuner v{version})"),
                )
            } else {
                String::new()
            };
            format!(
                "tuned{adoption} {} / {} / {}",
                model
                    .run_profile
                    .quant
                    .as_deref()
                    .unwrap_or("default quant"),
                model
                    .run_profile
                    .context
                    .as_deref()
                    .unwrap_or("default context"),
                model.run_profile.mode.as_deref().unwrap_or("default mode")
            )
        } else {
            format!(
                "defaults ({})",
                model.run_profile.reason.as_deref().unwrap_or("not tuned")
            )
        };
        let _ = writeln!(
            out,
            "{}{} — {} · {} · quant {} · {}",
            model.name, aliases, display, model.repository, catalog_quant, run
        );
    }
    out.push_str("\nStart one: localbox serve <name>\n");
    out
}

/// The detail view for one model, resolved by any of its names.
///
/// # Errors
/// A plain-language message naming the known keys when nothing matches.
pub fn render_model_detail(catalog: &Catalog, name: &str) -> Result<String, String> {
    let key = resolve_model_key(catalog, name).ok_or_else(|| {
        format!(
            "unknown model '{name}'. Known keys: {}",
            catalog.model_keys().join(", ")
        )
    })?;
    let def = catalog
        .model(&key)
        .ok_or_else(|| format!("unknown model '{name}'"))?;
    let mut out = String::new();
    push_model_block(&mut out, &key, def);
    let _ = writeln!(out, "  Tier     : {}", model_tier(def));
    if let Some(root) = def.root.as_deref() {
        let _ = writeln!(out, "  Folder   : {root}");
    }
    if let Some(policy) = def.thinking_policy.as_deref() {
        let _ = writeln!(out, "  Thinking : {policy}");
    }
    if let Some(mode) = catalog.required_mode(&key) {
        let label = if mode == localx_llama_core::Mode::PrismMl {
            "prism"
        } else {
            mode.as_str()
        };
        let _ = writeln!(out, "  Engine   : {label} (required)");
    }
    Ok(out)
}

/// A model folder is only deletable when it stays inside the GGUF root: a
/// bare folder name, no absolute path, no parent-directory traversal.
fn is_safe_folder_name(folder: &str) -> bool {
    let path = Path::new(folder);
    !folder.trim().is_empty()
        && path.is_relative()
        && path.components().all(|c| matches!(c, Component::Normal(_)))
}

/// The model folders `purge` would delete: one per catalog model, deduplicated,
/// each strictly under the GGUF root. Folder spellings that would escape the
/// root are skipped rather than deleted.
#[must_use]
pub fn purge_targets(catalog: &Catalog, gguf_root: &Path) -> Vec<PathBuf> {
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    for key in catalog.model_keys() {
        let Some(def) = catalog.model(key) else {
            continue;
        };
        let folder = def.root.as_deref().unwrap_or(key);
        if is_safe_folder_name(folder) {
            seen.insert(gguf_root.join(folder));
        }
    }
    seen.into_iter().collect()
}

/// The last `n` lines of a text, joined back with newlines.
#[must_use]
pub fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// The most recently modified file in a log directory, when any exists.
#[must_use]
pub fn newest_log(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    entries
        .filter_map(Result::ok)
        .filter(|e| e.path().is_file())
        .filter_map(|e| {
            let modified = e.metadata().ok()?.modified().ok()?;
            Some((modified, e.path()))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::{Map, Value};

    fn catalog() -> Catalog {
        let raw: Map<String, Value> = serde_json::from_str(
            r#"{
                "Models": {
                    "q36apex": {
                        "DisplayName": "Qwen 3.6 APEX",
                        "Description": "The recommended coding model.",
                        "Tier": "recommended",
                        "Root": "q36apex",
                        "Repo": "mudler/apex",
                        "Quants": {
                            "apex-i-quality": { "File": "a.gguf", "SizeGB": 21.3 },
                            "apex-compact": "b.gguf"
                        },
                        "Quant": "apex-i-quality",
                        "Contexts": { "": 32768, "64k": 65536 }
                    },
                    "oldie": {
                        "Repo": "x/oldie",
                        "Tier": "legacy",
                        "File": "oldie.gguf",
                        "Contexts": { "": 8192 }
                    },
                    "newbie": {
                        "Repo": "x/newbie",
                        "File": "newbie.gguf",
                        "Contexts": { "": 4096 }
                    }
                },
                "CommandAliases": { "apex": "q36apex" }
            }"#,
        )
        .unwrap();
        Catalog::from_layers(&Map::new(), &raw, &Map::new()).unwrap()
    }

    fn write_profile(home: &Path, entries: Value) {
        let dir = home.join(".local-llm").join("tuner");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("best-q36apex.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema": 1,
                "key": "q36apex",
                "entries": entries,
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn profile_entry(quant: &str, score: f64, tuner_version: i64) -> Value {
        serde_json::json!({
            "quant": quant,
            "contextKey": "64k",
            "mode": "native",
            "vramGB": 24,
            "prompt_length": "short",
            "profile": "balanced",
            "score": score,
            "scoreUnit": "tok/s",
            "args": [],
            "overrides": {},
            "measured_at": "2026-08-21T00:00:00Z",
            "tuner_version": tuner_version,
        })
    }

    #[test]
    fn any_name_resolves_key_alias_and_folder() {
        let catalog = catalog();
        assert_eq!(
            resolve_model_key(&catalog, "q36apex").as_deref(),
            Some("q36apex")
        );
        assert_eq!(
            resolve_model_key(&catalog, "apex").as_deref(),
            Some("q36apex")
        );
        // The on-disk folder name works too (the shortcut habit).
        assert_eq!(
            resolve_model_key(&catalog, "oldie").as_deref(),
            Some("oldie")
        );
        assert!(resolve_model_key(&catalog, "nope").is_none());
    }

    #[test]
    fn the_overview_groups_by_tier_and_marks_the_current_quant() {
        let overview = render_model_overview(&catalog());
        let recommended = overview.find("[recommended]").unwrap();
        let experimental = overview.find("[experimental]").unwrap();
        let legacy = overview.find("[legacy]").unwrap();
        assert!(recommended < experimental && experimental < legacy);
        // An untiered model reads as experimental, not hidden.
        assert!(overview[experimental..legacy].contains("newbie"));
        assert!(overview.contains("apex-i-quality [current] (21.3 GB)"));
        assert!(overview.contains("default=32768, 64k=65536"));
    }

    #[test]
    fn detail_resolves_aliases_and_an_unknown_name_lists_the_keys() {
        let catalog = catalog();
        let detail = render_model_detail(&catalog, "apex").unwrap();
        assert!(detail.contains("Qwen 3.6 APEX"));
        assert!(detail.contains("Tier     : recommended"));
        let err = render_model_detail(&catalog, "nope").unwrap_err();
        assert!(err.contains("newbie, oldie, q36apex"));
    }

    #[test]
    fn models_contract_is_versioned_key_first_and_reports_profile_state() {
        let home = tempfile::tempdir().unwrap();
        let contract = models_catalog(&catalog(), home.path());
        assert_eq!(contract.schema, MODELS_CATALOG_SCHEMA);
        let apex = contract
            .models
            .iter()
            .find(|model| model.name == "q36apex")
            .unwrap();
        assert_eq!(apex.aliases, vec!["apex"]);
        assert_eq!(apex.default_quant.as_deref(), Some("apex-i-quality"));
        assert_eq!(apex.run_profile.source, "defaults");
        assert_eq!(apex.run_profile.origin, None);
        assert_eq!(apex.run_profile.reason.as_deref(), Some("missing"));
        assert!(apex
            .run_profile
            .warning
            .as_deref()
            .unwrap()
            .contains("localbench"));
        let json = serde_json::to_string(&contract).unwrap();
        assert!(json.contains(r#""schema":1"#));
        let rendered = render_models_catalog(&contract);
        assert!(rendered.contains("q36apex (aliases: apex)"));
        assert!(rendered.contains("quant apex-i-quality"));
    }

    #[test]
    fn models_contract_reports_stale_profiles_and_mixed_store_selection() {
        let home = tempfile::tempdir().unwrap();
        let current = localx_llama_core::CURRENT_TUNER_VERSION;
        write_profile(
            home.path(),
            serde_json::json!([profile_entry("legacy", 999.0, current - 1)]),
        );

        let stale = models_catalog(&catalog(), home.path());
        let stale_profile = &stale
            .models
            .iter()
            .find(|model| model.name == "q36apex")
            .unwrap()
            .run_profile;
        assert_eq!(stale_profile.source, "defaults");
        assert_eq!(
            stale_profile.reason.as_deref(),
            Some("unsupported_tuner_version")
        );
        assert!(stale_profile
            .warning
            .as_deref()
            .unwrap()
            .contains("localbench findbest q36apex"));

        write_profile(
            home.path(),
            serde_json::json!([
                profile_entry("legacy", 999.0, current - 1),
                profile_entry("current", 10.0, current),
            ]),
        );
        let mixed = models_catalog(&catalog(), home.path());
        let mixed_profile = &mixed
            .models
            .iter()
            .find(|model| model.name == "q36apex")
            .unwrap()
            .run_profile;
        assert_eq!(mixed_profile.source, "tuned");
        assert_eq!(mixed_profile.origin.as_deref(), Some("measured"));
        assert_eq!(mixed_profile.quant.as_deref(), Some("current"));
        assert_eq!(mixed_profile.reason, None);

        let mut adopted_entry = profile_entry("legacy", 999.0, current);
        let adopted_fields = adopted_entry.as_object_mut().unwrap();
        adopted_fields.insert(
            "localbox_adopted_from_tuner_version".to_string(),
            (current - 1).into(),
        );
        adopted_fields.insert(
            "localbox_adopted_at".to_string(),
            "2026-08-28T12:34:56Z".into(),
        );
        write_profile(home.path(), serde_json::json!([adopted_entry]));
        let adopted = models_catalog(&catalog(), home.path());
        let adopted_profile = &adopted
            .models
            .iter()
            .find(|model| model.name == "q36apex")
            .unwrap()
            .run_profile;
        assert_eq!(adopted_profile.source, "tuned");
        assert_eq!(adopted_profile.origin.as_deref(), Some("adopted"));
        assert_eq!(
            adopted_profile.adopted_from_tuner_version,
            Some(current - 1)
        );
        assert_eq!(
            adopted_profile.adopted_at.as_deref(),
            Some("2026-08-28T12:34:56Z")
        );
        assert!(render_models_catalog(&adopted)
            .contains(&format!("adopted from tuner v{}", current - 1)));
    }

    #[test]
    fn purge_targets_stay_under_the_root_and_deduplicate() {
        let raw: Map<String, Value> = serde_json::from_str(
            r#"{
                "Models": {
                    "a": { "Repo": "x/a", "Root": "shared", "Contexts": { "": 1 } },
                    "b": { "Repo": "x/b", "Root": "shared", "Contexts": { "": 1 } },
                    "c": { "Repo": "x/c", "Contexts": { "": 1 } },
                    "evil": { "Repo": "x/e", "Root": "../outside", "Contexts": { "": 1 } },
                    "abs": { "Repo": "x/f", "Root": "/etc", "Contexts": { "": 1 } }
                }
            }"#,
        )
        .unwrap();
        let catalog = Catalog::from_layers(&Map::new(), &raw, &Map::new()).unwrap();
        let root = Path::new("/gguf");
        let targets = purge_targets(&catalog, root);
        // "shared" once, "c" by key; traversal and absolute spellings skipped.
        assert_eq!(targets, vec![root.join("c"), root.join("shared")]);
    }

    #[test]
    fn tail_returns_the_last_lines_only() {
        assert_eq!(tail_lines("a\nb\nc\nd", 2), "c\nd");
        assert_eq!(tail_lines("a\nb", 10), "a\nb");
        assert_eq!(tail_lines("", 5), "");
    }

    #[test]
    fn the_newest_log_wins_by_modified_time() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.log"), "old").unwrap();
        let newer = dir.path().join("new.log");
        std::fs::write(&newer, "new").unwrap();
        let old_time = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(dir.path().join("old.log"))
            .unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(old_time))
            .unwrap();
        drop(file);
        assert_eq!(newest_log(dir.path()), Some(newer));
        assert_eq!(newest_log(&dir.path().join("missing")), None);
    }
}
