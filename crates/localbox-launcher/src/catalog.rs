//! The three-layer config load over the shared precedence engine:
//! `defaults.json` (lowest) < `llm-models.json` (the catalog; sole source of
//! `Models`/`CommandAliases`) < per-machine `settings.json` (highest, never
//! able to override the catalog-only keys).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use localx_llama_core::config::assemble_config;
use localx_llama_core::ModelDef;

/// A catalog/config failure.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    /// A config layer exists but does not parse.
    #[error("could not parse {path}: {reason}")]
    BadLayer { path: String, reason: String },
    /// The catalog file is missing entirely.
    #[error(
        "catalog not found at {0}. Copy llm-models.example.json to llm-models.json \
         (or re-run the installer) before launching."
    )]
    CatalogMissing(String),
    /// A model entry does not deserialize.
    #[error("model '{key}' in the catalog does not parse: {reason}")]
    BadModel { key: String, reason: String },
    /// I/O reading a layer.
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// The assembled effective configuration: the merged scalar map plus the
/// typed model catalog.
#[derive(Debug, Clone)]
pub struct Catalog {
    cfg: Map<String, Value>,
    models: BTreeMap<String, ModelDef>,
}

fn read_layer(path: &Path) -> Result<Map<String, Value>, CatalogError> {
    if !path.is_file() {
        return Ok(Map::new());
    }
    let raw = std::fs::read_to_string(path)?;
    // Tolerate a UTF-8 BOM from editors; the launcher itself never writes one.
    let raw = raw.trim_start_matches('\u{feff}');
    serde_json::from_str(raw).map_err(|e| CatalogError::BadLayer {
        path: path.display().to_string(),
        reason: e.to_string(),
    })
}

impl Catalog {
    /// Load and merge the three layers from a `local-llm`-style directory.
    ///
    /// # Errors
    /// [`CatalogError::CatalogMissing`] when `llm-models.json` is absent (the
    /// example must be copied first), a parse error for a corrupt layer, or a
    /// model entry that no longer deserializes.
    pub fn load(local_llm_dir: &Path) -> Result<Self, CatalogError> {
        let catalog_path = local_llm_dir.join("llm-models.json");
        if !catalog_path.is_file() {
            return Err(CatalogError::CatalogMissing(
                catalog_path.display().to_string(),
            ));
        }
        let defaults = read_layer(&local_llm_dir.join("defaults.json"))?;
        let catalog = read_layer(&catalog_path)?;
        let settings = read_layer(&local_llm_dir.join("settings.json"))?;
        Self::from_layers(&defaults, &catalog, &settings)
    }

    /// Assemble from already-parsed layers (pure; the unit-test seam).
    ///
    /// # Errors
    /// [`CatalogError::BadModel`] when a model entry does not deserialize.
    pub fn from_layers(
        defaults: &Map<String, Value>,
        catalog: &Map<String, Value>,
        settings: &Map<String, Value>,
    ) -> Result<Self, CatalogError> {
        let legacy = Map::new();
        let cfg = assemble_config(defaults, &legacy, catalog, settings);
        let mut models = BTreeMap::new();
        if let Some(entries) = cfg.get("Models").and_then(Value::as_object) {
            for (key, value) in entries {
                let def: ModelDef =
                    serde_json::from_value(value.clone()).map_err(|e| CatalogError::BadModel {
                        key: key.clone(),
                        reason: e.to_string(),
                    })?;
                models.insert(key.clone(), def);
            }
        }
        Ok(Self { cfg, models })
    }

    /// The model definition for a key, when the catalog knows it.
    #[must_use]
    pub fn model(&self, key: &str) -> Option<&ModelDef> {
        self.models.get(key)
    }

    /// Every catalog model key, sorted.
    #[must_use]
    pub fn model_keys(&self) -> Vec<&str> {
        self.models.keys().map(String::as_str).collect()
    }

    /// A merged scalar setting, when present.
    #[must_use]
    pub fn setting(&self, key: &str) -> Option<&Value> {
        self.cfg.get(key)
    }

    /// A string setting, when present and non-blank.
    #[must_use]
    pub fn setting_str(&self, key: &str) -> Option<&str> {
        self.setting(key)
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
    }

    /// The GGUF root directory (`LlamaCppGgufRoot`), when configured.
    #[must_use]
    pub fn gguf_root(&self) -> Option<PathBuf> {
        self.setting_str("LlamaCppGgufRoot").map(PathBuf::from)
    }

    /// The no-think proxy port (`NoThinkProxyPort`), when configured.
    #[must_use]
    pub fn no_think_proxy_port(&self) -> Option<u16> {
        self.setting("NoThinkProxyPort")
            .and_then(Value::as_u64)
            .and_then(|p| u16::try_from(p).ok())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn obj(json: &str) -> Map<String, Value> {
        serde_json::from_str(json).unwrap()
    }

    const CATALOG: &str = r#"{
        "SchemaNote": "catalog",
        "Models": {
            "q36apex": {
                "DisplayName": "Qwen 3.6 APEX",
                "Root": "q36apex",
                "Repo": "mudler/apex",
                "Quants": { "apex-i-quality": "APEX-I-Quality.gguf" },
                "Quant": "apex-i-quality",
                "Contexts": { "": 32768, "64k": 65536 }
            }
        },
        "CommandAliases": { "apex": "q36apex" }
    }"#;

    #[test]
    fn layers_merge_with_settings_highest_and_catalog_keys_locked() {
        let defaults = obj(r#"{ "NoThinkProxyPort": 11434, "LlamaCppGgufRoot": "C:/gguf" }"#);
        let settings = obj(
            r#"{ "NoThinkProxyPort": 11435, "Models": { "evil": {} }, "LlamaCppGgufRoot": "D:/gguf" }"#,
        );
        let catalog = Catalog::from_layers(&defaults, &obj(CATALOG), &settings).unwrap();
        // Settings override scalars...
        assert_eq!(catalog.no_think_proxy_port(), Some(11435));
        assert_eq!(catalog.gguf_root(), Some(PathBuf::from("D:/gguf")));
        // ...but can never inject/replace catalog-only keys.
        assert!(catalog.model("evil").is_none());
        assert!(catalog.model("q36apex").is_some());
        assert_eq!(catalog.model_keys(), vec!["q36apex"]);
    }

    #[test]
    fn the_real_compact_quant_spelling_parses() {
        let catalog = Catalog::from_layers(&Map::new(), &obj(CATALOG), &Map::new()).unwrap();
        let def = catalog.model("q36apex").unwrap();
        assert_eq!(def.root.as_deref(), Some("q36apex"));
        assert_eq!(def.quants["apex-i-quality"].file, "APEX-I-Quality.gguf");
    }

    #[test]
    fn a_missing_catalog_fails_loud_with_the_remedy() {
        let dir = tempfile::tempdir().unwrap();
        let err = Catalog::load(dir.path()).unwrap_err();
        assert!(matches!(err, CatalogError::CatalogMissing(_)));
        assert!(err.to_string().contains("llm-models.example.json"));
    }

    #[test]
    fn load_reads_all_three_files_and_tolerates_a_bom() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("defaults.json"),
            "{ \"NoThinkProxyPort\": 11434 }",
        )
        .unwrap();
        // A BOM-prefixed catalog still parses.
        std::fs::write(
            dir.path().join("llm-models.json"),
            format!("\u{feff}{CATALOG}"),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("settings.json"),
            "{ \"LlamaCppGgufRoot\": \"E:/models\" }",
        )
        .unwrap();
        let catalog = Catalog::load(dir.path()).unwrap();
        assert_eq!(catalog.no_think_proxy_port(), Some(11434));
        assert_eq!(catalog.gguf_root(), Some(PathBuf::from("E:/models")));
        assert!(catalog.model("q36apex").is_some());
    }

    #[test]
    fn a_bad_model_entry_names_the_key() {
        let catalog = obj(r#"{ "Models": { "broken": { "Repo": 42 } } }"#);
        let err = Catalog::from_layers(&Map::new(), &catalog, &Map::new()).unwrap_err();
        assert!(matches!(err, CatalogError::BadModel { ref key, .. } if key == "broken"));
    }
}
