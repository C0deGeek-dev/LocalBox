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
use localx_llama_core::ModelDef;

use crate::guided::model_tier;

/// Resolve a user-typed model name to its catalog key: the key itself, a
/// `CommandAliases` entry, or the model's on-disk folder name.
#[must_use]
pub fn resolve_model_key(catalog: &Catalog, name: &str) -> Option<String> {
    if catalog.model(name).is_some() {
        return Some(name.to_string());
    }
    if let Some(aliases) = catalog
        .setting("CommandAliases")
        .and_then(serde_json::Value::as_object)
    {
        if let Some(key) = aliases.get(name).and_then(serde_json::Value::as_str) {
            if catalog.model(key).is_some() {
                return Some(key.to_string());
            }
        }
    }
    catalog
        .model_keys()
        .iter()
        .find(|key| {
            catalog
                .model(key)
                .is_some_and(|def| def.root.as_deref() == Some(name))
        })
        .map(|key| (*key).to_string())
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
