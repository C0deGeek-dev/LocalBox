//! Synthesise a catalog entry from a Hugging Face repo reference and a chosen
//! quant.
//!
//! This is the last read/plan step before the effectful write: it turns the
//! [`HfRef`](crate::hf_meta::HfRef) and the selected
//! [`QuantCandidate`](crate::quant::QuantCandidate) into the JSON a catalog
//! `Models` entry expects — the same `ModelDef` shape a hand-written entry uses,
//! so once persisted the model launches, resolves, and lists exactly like any
//! other. It is pure: it builds a value, it does not touch the catalog file.

use serde_json::{json, Value};

use crate::hf_meta::HfRef;
use crate::quant::QuantCandidate;

/// A stable catalog key / on-disk folder slug derived from a repo name:
/// lowercase, alphanumeric runs joined by single hyphens, length-capped. The
/// output can never contain a path separator or a traversal sequence, so it is
/// safe as both a catalog key and a directory name on every platform.
#[must_use]
pub fn catalog_key_from_ref(hf: &HfRef) -> String {
    const MAX: usize = 64;
    let mut slug = String::new();
    let mut pending_sep = false;
    for ch in hf.repo().chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_sep && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(ch.to_ascii_lowercase());
            pending_sep = false;
        } else {
            pending_sep = true;
        }
    }
    let capped: String = slug.chars().take(MAX).collect();
    let trimmed = capped.trim_matches('-');
    if trimmed.is_empty() {
        "model".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Bytes → gigabytes, rounded to one decimal (the catalog's `SizeGB` precision).
fn size_gb(bytes: u64) -> f64 {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    ((bytes as f64 / GB) * 10.0).round() / 10.0
}

/// Build the `(catalog key, Models entry)` for a repo + quant. The entry names
/// the quant's primary file (llama.cpp loads the remaining shards of a split
/// GGUF from it), carries the total size when known, and defaults the context
/// window; `Root` matches the key so downloads land in their own folder.
#[must_use]
pub fn synthesize_entry(hf: &HfRef, quant: &QuantCandidate) -> (String, Value) {
    let key = catalog_key_from_ref(hf);
    let primary = quant.files.first().cloned().unwrap_or_default();
    let mut quant_value = serde_json::Map::new();
    quant_value.insert("File".to_string(), json!(primary));
    if let Some(total) = quant.total_size {
        quant_value.insert("SizeGB".to_string(), json!(size_gb(total)));
    }
    let entry = json!({
        "DisplayName": hf.repo(),
        "Root": key,
        "SourceType": "gguf",
        "Repo": hf.repo_id(),
        "Quants": { quant.key.clone(): Value::Object(quant_value) },
        "Quant": quant.key.clone(),
        "Contexts": { "": 32768 },
    });
    (key, entry)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::hf_meta::parse_hf_ref;
    use localx_llama_core::ModelDef;

    fn candidate(key: &str, files: &[&str], total: Option<u64>) -> QuantCandidate {
        QuantCandidate {
            key: key.to_string(),
            files: files.iter().map(|s| s.to_string()).collect(),
            total_size: total,
        }
    }

    #[test]
    fn the_key_is_a_safe_slug_of_the_repo_name() {
        let hf = parse_hf_ref("mradermacher/Qwen3.8-27B-Uncensored-Heretic-Abliterated-i1-GGUF")
            .unwrap();
        let key = catalog_key_from_ref(&hf);
        assert_eq!(key, "qwen3-8-27b-uncensored-heretic-abliterated-i1-gguf");
        // No path separators or traversal sequences can appear in a slug.
        assert!(!key.contains('/') && !key.contains('\\') && !key.contains(".."));
    }

    #[test]
    fn a_long_repo_name_is_capped_and_trimmed() {
        let hf = parse_hf_ref(&format!("owner/{}", "a".repeat(200))).unwrap();
        let key = catalog_key_from_ref(&hf);
        assert!(key.len() <= 64);
        assert!(!key.starts_with('-') && !key.ends_with('-'));
    }

    #[test]
    fn the_entry_deserializes_as_a_model_def_and_carries_the_quant() {
        let hf = parse_hf_ref("bartowski/Llama-3.2-1B-Instruct-GGUF").unwrap();
        let quant = candidate(
            "q4km",
            &["Llama-3.2-1B-Instruct-Q4_K_M.gguf"],
            Some(805_306_368),
        );
        let (key, entry) = synthesize_entry(&hf, &quant);
        assert_eq!(key, "llama-3-2-1b-instruct-gguf");

        let def: ModelDef = serde_json::from_value(entry).expect("valid ModelDef");
        assert_eq!(def.repo, "bartowski/Llama-3.2-1B-Instruct-GGUF");
        assert_eq!(def.root.as_deref(), Some(key.as_str()));
        assert_eq!(def.quant.as_deref(), Some("q4km"));
        let q = def.quants.get("q4km").expect("quant present");
        assert_eq!(q.file, "Llama-3.2-1B-Instruct-Q4_K_M.gguf");
        assert_eq!(q.size_gb, Some(0.8));
        assert_eq!(def.contexts.get(""), Some(&32768));
    }

    #[test]
    fn a_multipart_entry_names_the_first_shard() {
        let hf = parse_hf_ref("owner/Big-GGUF").unwrap();
        let quant = candidate(
            "q6k",
            &[
                "Big-Q6_K-00001-of-00003.gguf",
                "Big-Q6_K-00002-of-00003.gguf",
                "Big-Q6_K-00003-of-00003.gguf",
            ],
            None,
        );
        let (_key, entry) = synthesize_entry(&hf, &quant);
        let def: ModelDef = serde_json::from_value(entry).unwrap();
        assert_eq!(
            def.quants.get("q6k").unwrap().file,
            "Big-Q6_K-00001-of-00003.gguf"
        );
        // Size unknown → no SizeGB emitted.
        assert_eq!(def.quants.get("q6k").unwrap().size_gb, None);
    }
}
