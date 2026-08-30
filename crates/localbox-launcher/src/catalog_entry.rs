//! Synthesise a catalog entry from a Hugging Face repo reference, every
//! discovered quant, and the quant selected for the first download.
//!
//! This is the last read/plan step before the effectful write: it turns the
//! [`HfRef`](crate::hf_meta::HfRef) and the selected
//! [`QuantCandidate`](localx_llama_core::quant::QuantCandidate)s into the JSON a catalog
//! `Models` entry expects — the same `ModelDef` shape a hand-written entry uses.
//! Cataloguing every variant is deliberately separate from downloading one, so
//! the model can switch quants later without pulling every large GGUF up front.
//! It is pure: it builds a value, it does not touch the catalog file.

use localx_llama_core::quant::{
    default_contexts, format_display_name, quant_note_text, suggest_parser, QuantCandidate,
};
use serde_json::{json, Value};

use crate::hf_meta::HfRef;

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

/// Bytes → **binary** gigabytes: the unit every catalog `SizeGB` is written
/// in, matching the HF listing and the launcher's own download progress.
///
/// Every producer and consumer of `SizeGB` goes through this. Measuring a
/// downloaded file in decimal GB against a catalog written in binary GB makes
/// the same file disagree with itself by ~7%, which the size check then
/// reports as drift.
#[must_use]
pub fn gib(bytes: u64) -> f64 {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    bytes as f64 / GIB
}

/// [`gib`] at the catalog's one-decimal `SizeGB` precision.
#[must_use]
pub fn size_gb(bytes: u64) -> f64 {
    let rounded = (gib(bytes) * 10.0).round() / 10.0;
    if bytes == 0 {
        0.0
    } else {
        rounded.max(0.1)
    }
}

/// Build the `(catalog key, Models entry)` for a repo's quant set. Every quant
/// names its primary file (llama.cpp loads the remaining shards of a split GGUF
/// from it) and carries its total size when known. `selected` becomes the
/// catalog default and is the only variant the caller downloads initially;
/// `Root` matches the key so downloads land in their own folder.
#[must_use]
pub fn synthesize_entry(
    hf: &HfRef,
    quants: &[QuantCandidate],
    selected: &QuantCandidate,
) -> (String, Value) {
    synthesize_entry_with_description(hf, quants, selected, None)
}

/// [`synthesize_entry`] with optional untrusted external description metadata.
/// The sanitizer runs again at this persistence boundary as defense in depth;
/// direct callers cannot accidentally write Hub HTML into the catalog.
#[must_use]
pub fn synthesize_entry_with_description(
    hf: &HfRef,
    quants: &[QuantCandidate],
    selected: &QuantCandidate,
    description: Option<&str>,
) -> (String, Value) {
    let key = catalog_key_from_ref(hf);
    let mut quant_values = serde_json::Map::new();
    for quant in quants {
        let primary = quant.files.first().cloned().unwrap_or_default();
        let size = quant.total_size.filter(|bytes| *bytes > 0).map(size_gb);
        let mut quant_value = serde_json::Map::new();
        quant_value.insert("File".to_string(), json!(primary));
        if let Some(size) = size {
            quant_value.insert("SizeGB".to_string(), json!(size));
        }
        // The picker's note comes from the shared taxonomy, so an imported
        // model reads like a hand-written one instead of arriving blank.
        quant_value.insert("Note".to_string(), json!(quant_note_text(&quant.key, size)));
        quant_values.insert(quant.key.clone(), Value::Object(quant_value));
    }
    let contexts: serde_json::Map<String, Value> = default_contexts()
        .into_iter()
        .map(|(name, tokens)| (name.to_string(), json!(tokens)))
        .collect();
    let mut entry = serde_json::Map::new();
    entry.insert(
        "DisplayName".to_string(),
        json!(format_display_name(&hf.repo_id())),
    );
    entry.insert("Root".to_string(), json!(key));
    entry.insert("Repo".to_string(), json!(hf.repo_id()));
    if let Some(description) = description.and_then(crate::hf_meta::plain_text_description) {
        entry.insert("Description".to_string(), json!(description));
    }
    entry.insert("Quants".to_string(), Value::Object(quant_values));
    entry.insert("Quant".to_string(), json!(selected.key.clone()));
    entry.insert("Contexts".to_string(), Value::Object(contexts));
    // A parser the taxonomy cannot infer is left unset rather than written as
    // the literal "none": an absent key is what a hand-written entry uses.
    let parser = suggest_parser(&hf.repo_id());
    if parser != "none" {
        entry.insert("Parser".to_string(), json!(parser));
    }
    (key, Value::Object(entry))
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
        let selected = candidate(
            "q4km",
            &["Llama-3.2-1B-Instruct-Q4_K_M.gguf"],
            Some(805_306_368),
        );
        let other = candidate(
            "q6k",
            &["Llama-3.2-1B-Instruct-Q6_K.gguf"],
            Some(1_073_741_824),
        );
        let quants = vec![selected.clone(), other];
        let (key, entry) = synthesize_entry(&hf, &quants, &selected);
        assert_eq!(key, "llama-3-2-1b-instruct-gguf");
        assert!(
            entry.get("SourceType").is_none(),
            "new catalog entries must not revive retired metadata"
        );

        let def: ModelDef = serde_json::from_value(entry).expect("valid ModelDef");
        assert_eq!(def.repo, "bartowski/Llama-3.2-1B-Instruct-GGUF");
        assert_eq!(def.root.as_deref(), Some(key.as_str()));
        assert_eq!(def.quant.as_deref(), Some("q4km"));
        assert_eq!(def.quants.len(), 2, "every discovered quant is catalogued");
        let q = def.quants.get("q4km").expect("quant present");
        assert_eq!(q.file, "Llama-3.2-1B-Instruct-Q4_K_M.gguf");
        assert_eq!(q.size_gb, Some(0.8));
        assert_eq!(def.quants.get("q6k").unwrap().size_gb, Some(1.0));
        assert_eq!(def.contexts.get(""), Some(&65536));
    }

    /// An imported entry carries the same shared-taxonomy metadata a
    /// hand-written one does: a formatted display name, the context ladder, a
    /// suggested parser, and a per-quant note. Writing these by hand here is
    /// what left `format_display_name`, `default_contexts`, `suggest_parser`
    /// and `quant_note_text` without a caller.
    #[test]
    fn an_imported_entry_carries_the_shared_taxonomy_metadata() {
        let hf = parse_hf_ref("mradermacher/Qwen3.5-27B-Instruct-i1-GGUF").unwrap();
        let selected = candidate(
            "i1-q4km",
            &["Qwen3.5-27B.i1-Q4_K_M.gguf"],
            Some(805_306_368),
        );
        let (_, entry) = synthesize_entry(&hf, &[selected.clone()], &selected);
        let def: ModelDef = serde_json::from_value(entry).expect("valid ModelDef");

        assert_eq!(
            def.display_name.as_deref(),
            Some("Qwen 3.5 27B Instruct i1 GGUF")
        );
        assert_eq!(def.parser.as_deref(), Some("qwen36"));
        assert_eq!(def.contexts.len(), default_contexts().len());
        assert_eq!(def.contexts.get("128k"), Some(&131_072));
        let note = def.quants["i1-q4km"].note.as_deref().expect("a quant note");
        assert!(
            note.starts_with("Q4_K_M") && note.contains("4-bit k-quant medium"),
            "the note comes from the shared taxonomy: {note}"
        );
    }

    #[test]
    fn external_description_is_plain_bounded_catalog_text() {
        let hf = parse_hf_ref("owner/Some-Model-GGUF").unwrap();
        let selected = candidate("q4km", &["Some-Model-Q4_K_M.gguf"], Some(16));
        let (_, entry) = synthesize_entry_with_description(
            &hf,
            std::slice::from_ref(&selected),
            &selected,
            Some("<div>Useful &amp; safe</div><script>bad()</script>"),
        );
        assert_eq!(entry["Description"], "Useful & safe");
        assert!(!entry["Description"].as_str().unwrap().contains('<'));
    }

    /// A repo the taxonomy cannot place leaves `Parser` unset instead of
    /// writing the literal "none" into the catalog.
    #[test]
    fn an_unrecognised_repo_leaves_the_parser_unset() {
        let hf = parse_hf_ref("owner/Some-Model-GGUF").unwrap();
        let selected = candidate("q4km", &["Some-Model-Q4_K_M.gguf"], None);
        let (_, entry) = synthesize_entry(&hf, &[selected.clone()], &selected);
        assert!(entry.get("Parser").is_none());
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
        let (_key, entry) = synthesize_entry(&hf, std::slice::from_ref(&quant), &quant);
        let def: ModelDef = serde_json::from_value(entry).unwrap();
        assert_eq!(
            def.quants.get("q6k").unwrap().file,
            "Big-Q6_K-00001-of-00003.gguf"
        );
        // Size unknown → no SizeGB emitted.
        assert_eq!(def.quants.get("q6k").unwrap().size_gb, None);
    }

    #[test]
    fn all_imatrix_variants_are_catalogued_but_only_one_is_the_default() {
        let hf = parse_hf_ref("mradermacher/Model-i1-GGUF").unwrap();
        let iq4 = candidate("i1-iq4xs", &["Model.i1-IQ4_XS.gguf"], Some(15));
        let q4 = candidate("i1-q4km", &["Model.i1-Q4_K_M.gguf"], Some(16));
        let q6 = candidate("i1-q6k", &["Model.i1-Q6_K.gguf"], Some(22));
        let quants = vec![iq4, q4.clone(), q6];

        let (_key, entry) = synthesize_entry(&hf, &quants, &q4);
        let def: ModelDef = serde_json::from_value(entry).unwrap();

        assert_eq!(
            def.quants.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["i1-iq4xs", "i1-q4km", "i1-q6k"]
        );
        assert_eq!(def.quant.as_deref(), Some("i1-q4km"));
        assert!(def.quants.values().all(|quant| quant.size_gb == Some(0.1)));
        assert!(localx_llama_core::model::validate_model_def("model", &def).is_ok());
    }
}
