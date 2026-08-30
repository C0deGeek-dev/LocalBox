//! The three-layer config load over the shared precedence engine:
//! `defaults.json` (lowest) < `llm-models.json` (the catalog; sole source of
//! `Models`/`CommandAliases`) < per-machine `settings.json` (highest, never
//! able to override the catalog-only keys).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use localx_llama_core::config::assemble_config;
use localx_llama_core::model::validate_model_def;
use localx_llama_core::{Mode, ModelDef};

const MODEL_FIELDS: &[&str] = &[
    "Repo",
    "Root",
    "File",
    "DisplayName",
    "Quants",
    "Quant",
    "Contexts",
    "Parser",
    "Tier",
    "Strict",
    "KvCacheK",
    "KvCacheV",
    "NGpuLayers",
    "NCpuMoe",
    "Mlock",
    "NoMmap",
    "FlashAttn",
    "ChatTemplate",
    "ThinkingPolicy",
    "SpecType",
    "ExtraArgs",
    "VisionModule",
    "DraftModule",
    "Description",
    // LocalBox catalog policy outside the shared model-domain data.
    "RequiredMode",
];

fn metadata_map(value: Option<Value>, field: &str) -> Result<Map<String, Value>, String> {
    match value {
        None | Some(Value::Null) => Ok(Map::new()),
        Some(Value::Object(entries)) => Ok(entries),
        Some(_) => Err(format!("{field} must be an object")),
    }
}

fn quant_table<'a>(
    quants: &'a mut Map<String, Value>,
    quant: &str,
    field: &str,
) -> Result<&'a mut Map<String, Value>, String> {
    let value = quants
        .get_mut(quant)
        .ok_or_else(|| format!("{field} names unknown quant '{quant}'"))?;
    if let Value::String(file) = value {
        *value = serde_json::json!({ "File": std::mem::take(file) });
    }
    value
        .as_object_mut()
        .ok_or_else(|| format!("Quants.{quant} must be a filename or object"))
}

fn prepare_model(value: &Value) -> Result<(Value, Vec<String>), String> {
    let mut model = value
        .as_object()
        .cloned()
        .ok_or_else(|| "entry must be an object".to_string())?;
    let sizes = metadata_map(model.remove("QuantSizesGB"), "QuantSizesGB")?;
    let notes = metadata_map(model.remove("QuantNotes"), "QuantNotes")?;

    let mut warnings: Vec<String> = model
        .keys()
        .filter(|field| !MODEL_FIELDS.contains(&field.as_str()))
        .map(|field| format!("unknown field '{field}' is ignored"))
        .collect();

    let has_legacy_metadata = !sizes.is_empty() || !notes.is_empty();
    let quants = model.get_mut("Quants").and_then(Value::as_object_mut);
    if has_legacy_metadata && quants.is_none() {
        return Err("legacy quant metadata requires a Quants object".to_string());
    }
    if let Some(quants) = quants {
        for (quant, size) in sizes {
            let size = size
                .as_f64()
                .filter(|size| size.is_finite() && *size > 0.0)
                .ok_or_else(|| format!("QuantSizesGB.{quant} must be a positive number"))?;
            let table = quant_table(quants, &quant, "QuantSizesGB")?;
            if let Some(current) = table.get("SizeGB") {
                if current.as_f64() != Some(size) {
                    return Err(format!(
                        "Quants.{quant}.SizeGB disagrees with QuantSizesGB.{quant}"
                    ));
                }
            } else {
                table.insert("SizeGB".to_string(), Value::from(size));
            }
        }
        for (quant, note) in notes {
            let note = note
                .as_str()
                .filter(|note| !note.trim().is_empty())
                .ok_or_else(|| format!("QuantNotes.{quant} must be a non-blank string"))?
                .to_string();
            let table = quant_table(quants, &quant, "QuantNotes")?;
            if let Some(current) = table.get("Note") {
                if current.as_str() != Some(note.as_str()) {
                    return Err(format!(
                        "Quants.{quant}.Note disagrees with QuantNotes.{quant}"
                    ));
                }
            } else {
                table.insert("Note".to_string(), Value::from(note));
            }
        }
        for (quant, value) in quants {
            if let Some(table) = value.as_object() {
                warnings.extend(
                    table
                        .keys()
                        .filter(|field| !["File", "SizeGB", "Note"].contains(&field.as_str()))
                        .map(|field| format!("unknown field 'Quants.{quant}.{field}' is ignored")),
                );
            }
        }
    }
    Ok((Value::Object(model), warnings))
}

/// A catalog/config failure.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    /// A config layer exists but does not parse.
    #[error("could not parse {path}: {reason}")]
    BadLayer { path: String, reason: String },
    /// The catalog file is missing entirely.
    #[error(
        "catalog not found at {0}. Copy llm-models.example.json to llm-models.json \
         (or run `localbox` once to seed it) before launching."
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
    required_modes: BTreeMap<String, Mode>,
    warnings: Vec<String>,
}

fn parse_required_mode(value: &Value, def: &ModelDef) -> Result<Option<Mode>, String> {
    let configured = value.get("RequiredMode");
    let parsed = match configured {
        None | Some(Value::Null) => None,
        Some(Value::String(raw)) if raw.trim().is_empty() => None,
        Some(Value::String(raw)) => Some(match raw.trim().to_ascii_lowercase().as_str() {
            "native" => Mode::Native,
            "turboquant" => Mode::Turboquant,
            "mtpturbo" => Mode::Mtpturbo,
            "prism" | "prismml" => Mode::PrismMl,
            other => return Err(format!("unknown RequiredMode '{other}'")),
        }),
        Some(_) => return Err("RequiredMode must be a string".to_string()),
    };
    Ok(parsed.or_else(|| {
        def.repo
            .eq_ignore_ascii_case("prism-ml/Ternary-Bonsai-27B-gguf")
            .then_some(Mode::PrismMl)
    }))
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
    /// The catalog is per-user: `llm-models.json` when seeded, falling back to
    /// the shipped `llm-models.example.json` template on an unseeded checkout.
    ///
    /// # Errors
    /// [`CatalogError::CatalogMissing`] when neither the seeded catalog nor
    /// the template exists, a parse error for a corrupt layer, or a model
    /// entry that no longer deserializes.
    pub fn load(local_llm_dir: &Path) -> Result<Self, CatalogError> {
        let seeded = local_llm_dir.join("llm-models.json");
        let template = local_llm_dir.join("llm-models.example.json");
        let catalog_path = if seeded.is_file() {
            seeded
        } else if template.is_file() {
            template
        } else {
            return Err(CatalogError::CatalogMissing(seeded.display().to_string()));
        };
        let defaults = read_layer(&local_llm_dir.join("defaults.json"))?;
        let catalog = read_layer(&catalog_path)?;
        let settings = read_layer(&local_llm_dir.join("settings.json"))?;
        let assembled = Self::from_layers(&defaults, &catalog, &settings)?;
        for warning in &assembled.warnings {
            eprintln!("Warning: {warning}");
        }
        Ok(assembled)
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
        let mut required_modes = BTreeMap::new();
        let mut warnings = Vec::new();
        if let Some(entries) = cfg.get("Models").and_then(Value::as_object) {
            for (key, value) in entries {
                let (prepared, model_warnings) =
                    prepare_model(value).map_err(|reason| CatalogError::BadModel {
                        key: key.clone(),
                        reason,
                    })?;
                warnings.extend(
                    model_warnings
                        .into_iter()
                        .map(|warning| format!("model '{key}': {warning}")),
                );
                let def: ModelDef =
                    serde_json::from_value(prepared).map_err(|e| CatalogError::BadModel {
                        key: key.clone(),
                        reason: e.to_string(),
                    })?;
                if let Err(error) = validate_model_def(key, &def) {
                    warnings.push(format!("model '{key}' is invalid and was skipped: {error}"));
                    continue;
                }
                // Existing user catalogs are never overwritten during upgrades.
                // RequiredMode is LocalBox catalog policy rather than shared
                // model-domain data; infer Bonsai for pre-RequiredMode files.
                if let Some(mode) =
                    parse_required_mode(value, &def).map_err(|reason| CatalogError::BadModel {
                        key: key.clone(),
                        reason,
                    })?
                {
                    required_modes.insert(key.clone(), mode);
                }
                models.insert(key.clone(), def);
            }
        }
        Ok(Self {
            cfg,
            models,
            required_modes,
            warnings,
        })
    }

    /// The model definition for a key, when the catalog knows it.
    #[must_use]
    pub fn model(&self, key: &str) -> Option<&ModelDef> {
        self.models.get(key)
    }

    /// Engine policy declared by this catalog entry, including compatibility
    /// inference for Bonsai entries created before `RequiredMode` existed.
    #[must_use]
    pub fn required_mode(&self, key: &str) -> Option<Mode> {
        self.required_modes.get(key).copied()
    }

    /// Every catalog model key, sorted.
    #[must_use]
    pub fn model_keys(&self) -> Vec<&str> {
        self.models.keys().map(String::as_str).collect()
    }

    /// Catalog fields that were preserved on disk but are unknown to the
    /// typed runtime model. [`Catalog::load`] also prints these warnings.
    #[must_use]
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Resolve a copy-pasteable model name to its canonical catalog key.
    ///
    /// Exact keys win, followed by `CommandAliases`, then the model's on-disk
    /// folder name. Launching and catalog inspection deliberately share this
    /// resolver so an alias advertised by `localbox models` always works with
    /// `localbox serve`.
    #[must_use]
    pub fn resolve_model_key(&self, name: &str) -> Option<String> {
        if self.model(name).is_some() {
            return Some(name.to_string());
        }
        if let Some(key) = self
            .setting("CommandAliases")
            .and_then(Value::as_object)
            .and_then(|aliases| aliases.get(name))
            .and_then(Value::as_str)
            .filter(|key| self.model(key).is_some())
        {
            return Some(key.to_string());
        }
        self.model_keys()
            .into_iter()
            .find(|key| {
                self.model(key)
                    .is_some_and(|def| def.root.as_deref() == Some(name))
            })
            .map(str::to_string)
    }

    /// Every configured command alias for a canonical key, sorted.
    #[must_use]
    pub fn aliases_for(&self, key: &str) -> Vec<&str> {
        let mut aliases = self
            .setting("CommandAliases")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|entries| entries.iter())
            .filter_map(|(alias, target)| (target.as_str() == Some(key)).then_some(alias.as_str()))
            .collect::<Vec<_>>();
        aliases.sort_unstable();
        aliases
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
    fn legacy_quant_metadata_migrates_into_structured_entries() {
        let catalog = obj(r#"{
                "Models": {
                    "legacy": {
                        "Repo": "owner/model",
                        "Quants": { "q4km": "model-Q4_K_M.gguf" },
                        "Quant": "q4km",
                        "QuantSizesGB": { "q4km": 5.25 },
                        "QuantNotes": { "q4km": "recommended for 8GB cards" }
                    }
                }
            }"#);
        let catalog = Catalog::from_layers(&Map::new(), &catalog, &Map::new()).unwrap();
        let quant = &catalog.model("legacy").unwrap().quants["q4km"];
        assert_eq!(quant.file, "model-Q4_K_M.gguf");
        assert_eq!(quant.size_gb, Some(5.25));
        assert_eq!(quant.note.as_deref(), Some("recommended for 8GB cards"));
    }

    #[test]
    fn unknown_model_and_quant_fields_are_surfaced() {
        for catalog in [
            obj(r#"{ "Models": { "m": { "Repo": "o/m", "TypoField": true } } }"#),
            obj(
                r#"{ "Models": { "m": { "Repo": "o/m", "Quants": { "q4": { "File": "m.gguf", "TypoField": true } } } } }"#,
            ),
        ] {
            let catalog = Catalog::from_layers(&Map::new(), &catalog, &Map::new()).unwrap();
            assert!(catalog
                .warnings()
                .iter()
                .any(|warning| warning.contains("TypoField")));
        }
    }

    #[test]
    fn shipped_example_uses_structured_quant_metadata() {
        let raw = include_str!("../../../local-llm/llm-models.example.json");
        assert!(!raw.contains("QuantSizesGB"));
        assert!(!raw.contains("QuantNotes"));
        assert!(!raw.contains("LimitTools"));
        assert!(!raw.contains("SourceType"));
        let catalog = Catalog::from_layers(&Map::new(), &obj(raw), &Map::new()).unwrap();
        assert!(catalog.warnings().is_empty(), "{:#?}", catalog.warnings());
        for key in catalog.model_keys() {
            for quant in catalog.model(key).unwrap().quants.values() {
                assert!(quant.size_gb.is_some(), "{key} has a quant without SizeGB");
                assert!(quant.note.is_some(), "{key} has a quant without Note");
            }
        }
    }

    #[test]
    fn an_invalid_model_is_named_and_skipped_without_hiding_valid_entries() {
        let catalog = obj(r#"{
            "Models": {
                "broken": {
                    "Repo": "owner/broken",
                    "Quants": { "q4": "broken-Q4_K_M.gguf" },
                    "Quant": "missing"
                },
                "valid": {
                    "Repo": "owner/valid",
                    "Quants": { "q4": "valid-Q4_K_M.gguf" },
                    "Quant": "q4"
                }
            }
        }"#);

        let catalog = Catalog::from_layers(&Map::new(), &catalog, &Map::new()).unwrap();

        assert!(catalog.model("broken").is_none());
        assert!(catalog.model("valid").is_some());
        assert!(catalog.warnings().iter().any(|warning| {
            warning.contains("model 'broken' is invalid and was skipped")
                && warning.contains("broken.Quant 'missing'")
        }));
    }

    #[test]
    fn conflicting_legacy_and_structured_metadata_is_rejected() {
        let catalog = obj(r#"{
                "Models": {
                    "m": {
                        "Repo": "o/m",
                        "Quants": { "q4": { "File": "m.gguf", "SizeGB": 5.0 } },
                        "QuantSizesGB": { "q4": 6.0 }
                    }
                }
            }"#);
        let error = Catalog::from_layers(&Map::new(), &catalog, &Map::new()).unwrap_err();
        assert!(error.to_string().contains("disagrees"));
    }

    #[test]
    fn advertised_aliases_and_folders_use_the_same_resolver_as_launches() {
        let catalog = Catalog::from_layers(&Map::new(), &obj(CATALOG), &Map::new()).unwrap();
        assert_eq!(
            catalog.resolve_model_key("apex").as_deref(),
            Some("q36apex")
        );
        assert_eq!(
            catalog.resolve_model_key("q36apex").as_deref(),
            Some("q36apex")
        );
        assert_eq!(catalog.aliases_for("q36apex"), vec!["apex"]);
        assert!(catalog.resolve_model_key("missing").is_none());
    }

    #[test]
    fn an_existing_bonsai_catalog_infers_its_required_prism_engine() {
        let catalog = Catalog::from_layers(
            &Map::new(),
            &obj(r#"{"Models":{"bonsai":{"Repo":"prism-ml/Ternary-Bonsai-27B-gguf"}}}"#),
            &Map::new(),
        )
        .unwrap();

        assert_eq!(catalog.required_mode("bonsai"), Some(Mode::PrismMl));
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
    fn an_unseeded_checkout_falls_back_to_the_template() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("llm-models.example.json"), CATALOG).unwrap();
        let catalog = Catalog::load(dir.path()).unwrap();
        assert!(catalog.model("q36apex").is_some());
        // A seeded per-user catalog wins over the template.
        std::fs::write(
            dir.path().join("llm-models.json"),
            r#"{ "Models": { "mine": { "Repo": "me/mine", "Contexts": { "": 4096 } } } }"#,
        )
        .unwrap();
        let catalog = Catalog::load(dir.path()).unwrap();
        assert!(catalog.model("mine").is_some());
        assert!(catalog.model("q36apex").is_none());
    }

    #[test]
    fn a_bad_model_entry_names_the_key() {
        let catalog = obj(r#"{ "Models": { "broken": { "Repo": 42 } } }"#);
        let err = Catalog::from_layers(&Map::new(), &catalog, &Map::new()).unwrap_err();
        assert!(matches!(err, CatalogError::BadModel { ref key, .. } if key == "broken"));
    }
}
