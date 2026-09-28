//! Shared Hugging Face model discovery and catalog registration.
//!
//! Discovery reads a repo's GGUF listing and analyzes every quant candidate.
//! Registration writes the complete candidate set additively, but deliberately
//! performs no model download. CLI and guided callers decide separately whether
//! an explicit user action should start acquisition.

use std::path::Path;

use localbox_launcher::hf_meta::{self, HfGgufFile, HfRef};
use localx_llama_core::quant::{self, QuantCandidate, QuantFile};
use localx_llama_core::vram::{quant_fit_class, FitClass};

use crate::guided::{install_catalog_model, CatalogInsert};

const GIB: f64 = 1_073_741_824.0;

/// A repository reference plus every selectable GGUF quant discovered in its
/// current Hub listing.
#[derive(Debug, Clone)]
pub struct HfDiscovery {
    pub hf: HfRef,
    pub candidates: Vec<QuantCandidate>,
    /// Root-level multimodal projectors, kept separate from model quants.
    pub projectors: Vec<HfGgufFile>,
    /// Bounded plain text from remote metadata, when the Hub supplied one.
    pub description: Option<String>,
}

impl HfDiscovery {
    /// Select the only projector automatically; ambiguous repositories require
    /// an explicit filename rather than a guessed precision or model pairing.
    ///
    /// # Errors
    /// An unknown filename or an ambiguous projector listing.
    pub fn select_projector(&self, requested: Option<&str>) -> Result<Option<&str>, String> {
        if let Some(requested) = requested {
            return self
                .projectors
                .iter()
                .find(|file| file.rfilename == requested)
                .map(|file| Some(file.rfilename.as_str()))
                .ok_or_else(|| {
                    format!(
                        "unknown vision projector '{requested}'; available: {}",
                        self.projectors
                            .iter()
                            .map(|file| file.rfilename.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                });
        }
        match self.projectors.as_slice() {
            [] => Ok(None),
            [file] => Ok(Some(file.rfilename.as_str())),
            files => Err(format!(
                "multiple vision projectors available: {}. Choose one with --mmproj <filename>, or use Add a Hugging Face model",
                files.iter().map(|file| file.rfilename.as_str()).collect::<Vec<_>>().join(", ")
            )),
        }
    }

    /// Reuse a user's configured projector when refreshing the same repository.
    ///
    /// # Errors
    /// An unreadable or malformed catalog.
    pub fn configured_projector(&self, catalog_dir: &Path) -> Result<Option<String>, String> {
        let path = catalog_dir.join("llm-models.json");
        let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let catalog: serde_json::Value =
            serde_json::from_str(raw.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())?;
        let repo_id = self.hf.repo_id();
        Ok(catalog
            .get("Models")
            .and_then(serde_json::Value::as_object)
            .and_then(|models| {
                models.values().find(|model| {
                    model.get("Repo").and_then(serde_json::Value::as_str) == Some(repo_id.as_str())
                })
            })
            .and_then(|model| model.get("VisionModule"))
            .and_then(serde_json::Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .map(str::to_string))
    }

    /// Resolve an explicit quant spelling, or the established repository
    /// default when `requested` is absent.
    ///
    /// # Errors
    /// A clear selection error when no candidates exist or the spelling does
    /// not match one of them.
    pub fn select(&self, requested: Option<&str>) -> Result<QuantCandidate, String> {
        quant::select_quant(self.candidates.clone(), requested).map_err(|e| e.to_string())
    }

    /// Add or enrich this repository in `catalog_dir`, using `selected` as the
    /// default only for a newly created entry. Same-repository merges preserve
    /// the user's existing default and every other existing value.
    ///
    /// This function never downloads model files.
    ///
    /// # Errors
    /// A catalog synthesis/persistence error, or an unknown selected key.
    pub fn register(
        &self,
        catalog_dir: &Path,
        selected_key: &str,
    ) -> Result<CatalogInsert, String> {
        let configured = self.configured_projector(catalog_dir)?;
        let projector = match configured.as_deref() {
            Some(projector) => Some(projector),
            None => self.select_projector(None)?,
        };
        self.register_with_projector(catalog_dir, selected_key, projector)
    }

    /// Register the chosen projector together with the model, without downloads.
    /// Existing catalog values always win.
    ///
    /// # Errors
    /// A catalog error or an unknown quant key.
    pub fn register_with_projector(
        &self,
        catalog_dir: &Path,
        selected_key: &str,
        projector: Option<&str>,
    ) -> Result<CatalogInsert, String> {
        let selected = self.select(Some(selected_key))?;
        let (key_hint, mut entry) =
            localbox_launcher::catalog_entry::synthesize_entry_with_description(
                &self.hf,
                &self.candidates,
                &selected,
                self.description.as_deref(),
            );
        if let Some(projector) = projector {
            entry["VisionModule"] = serde_json::json!(projector);
        }
        install_catalog_model(catalog_dir, &key_hint, &entry)
    }
}

fn is_root_projector(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    !name.contains(['/', '\\', ':'])
        && !name.chars().any(char::is_control)
        && lower.starts_with("mmproj")
        && lower.ends_with(".gguf")
}

/// Read and analyze every GGUF quant in one already-parsed Hugging Face repo.
/// No catalog or model file is written.
///
/// # Errors
/// A typed Hub failure rendered as a user-facing message.
pub fn discover_hf_repo(hf: HfRef) -> Result<HfDiscovery, String> {
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let client = reqwest::Client::new();
    let info = runtime
        .block_on(hf_meta::inspect_hf_repo(&client, &hf))
        .map_err(|e| e.to_string())?;
    let mut projectors: Vec<_> = info
        .files
        .iter()
        .filter(|file| is_root_projector(&file.rfilename))
        .cloned()
        .collect();
    projectors.sort_by(|a, b| a.rfilename.cmp(&b.rfilename));
    projectors.dedup_by(|a, b| a.rfilename == b.rfilename);
    let files: Vec<QuantFile> = info
        .files
        .into_iter()
        .map(|file| QuantFile {
            name: file.rfilename,
            size: file.size,
        })
        .collect();
    let candidates = quant::quant_candidates(&files);
    // Keep the empty-list error identical to the normal selector contract.
    quant::select_quant(candidates.clone(), None).map_err(|e| e.to_string())?;
    Ok(HfDiscovery {
        hf,
        candidates,
        projectors,
        description: info.description,
    })
}

/// A candidate's combined file size in GiB, rounded to the one decimal stored
/// in the catalog and shown by LocalBox. Unknown multipart totals stay unknown.
#[must_use]
pub fn candidate_size_gb(candidate: &QuantCandidate) -> Option<f64> {
    #[allow(clippy::cast_precision_loss)]
    candidate
        .total_size
        .map(|bytes| ((bytes as f64 / GIB) * 10.0).round() / 10.0)
}

/// Recommend a quant from weight size and the existing LocalBox VRAM headroom
/// policy. This is deliberately conservative, not a benchmark claim:
///
/// 1. Q4_K_M when it safely fits;
/// 2. otherwise the largest known variant that fits;
/// 3. otherwise the smallest tight variant;
/// 4. otherwise the smallest over-budget variant;
/// 5. the established repository default when sizes are unavailable.
#[must_use]
pub fn recommend_quant(candidates: &[QuantCandidate], vram_gb: i64) -> Option<&QuantCandidate> {
    if candidates.is_empty() {
        return None;
    }
    let fit = |candidate: &QuantCandidate| quant_fit_class(candidate_size_gb(candidate), vram_gb);
    let q4km = candidates.iter().find(|candidate| {
        let compact = candidate.key.to_ascii_lowercase().replace(['_', '-'], "");
        compact.strip_prefix("i1").unwrap_or(&compact) == "q4km"
    });
    if let Some(candidate) = q4km.filter(|candidate| fit(candidate) == FitClass::Fits) {
        return Some(candidate);
    }

    let choose_by_size = |class: FitClass, largest: bool| {
        candidates
            .iter()
            .filter(|candidate| fit(candidate) == class)
            .filter_map(|candidate| candidate_size_gb(candidate).map(|size| (candidate, size)))
            .max_by(|(_, left), (_, right)| {
                let order = left.total_cmp(right);
                if largest {
                    order
                } else {
                    order.reverse()
                }
            })
            .map(|(candidate, _)| candidate)
    };
    choose_by_size(FitClass::Fits, true)
        .or_else(|| choose_by_size(FitClass::Tight, false))
        .or_else(|| choose_by_size(FitClass::Over, false))
        .or_else(|| {
            let selected = quant::select_quant(candidates.to_vec(), None).ok()?;
            candidates
                .iter()
                .find(|candidate| candidate.key == selected.key)
        })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn candidate(key: &str, gib: Option<f64>) -> QuantCandidate {
        QuantCandidate {
            key: key.to_string(),
            files: vec![format!("{key}.gguf")],
            total_size: gib.map(|size| (size * GIB) as u64),
        }
    }

    #[test]
    fn recommendation_prefers_a_safe_q4km_then_the_largest_safe_variant() {
        let candidates = vec![
            candidate("i1-iq3m", Some(11.0)),
            candidate("i1-q4km", Some(15.4)),
            candidate("i1-q5km", Some(16.5)),
        ];
        assert_eq!(recommend_quant(&candidates, 24).unwrap().key, "i1-q4km");

        let q4_too_large = vec![
            candidate("iq2s", Some(6.0)),
            candidate("iq3m", Some(8.0)),
            candidate("q4km", Some(12.0)),
        ];
        assert_eq!(recommend_quant(&q4_too_large, 16).unwrap().key, "iq3m");
    }

    #[test]
    fn recommendation_degrades_to_smallest_tight_then_smallest_over() {
        let tight = vec![candidate("q4", Some(9.0)), candidate("q5", Some(11.0))];
        assert_eq!(recommend_quant(&tight, 13).unwrap().key, "q4");

        let over = vec![candidate("q4", Some(13.0)), candidate("q5", Some(15.0))];
        assert_eq!(recommend_quant(&over, 13).unwrap().key, "q4");
    }

    #[test]
    fn unknown_sizes_use_the_established_default() {
        let candidates = vec![candidate("q3km", None), candidate("q4km", None)];
        assert_eq!(recommend_quant(&candidates, 24).unwrap().key, "q4km");
        assert!(recommend_quant(&[], 24).is_none());
    }
    #[test]
    fn root_projectors_are_safe_filenames_and_never_guessed_when_ambiguous() {
        for name in ["mmproj.gguf", "mmproj-F16.gguf", "MMProj-Q8_0.GGUF"] {
            assert!(is_root_projector(name));
        }
        for name in [
            "sub/mmproj.gguf",
            "../mmproj.gguf",
            "mmproj.txt",
            "model-Q4_K_M.gguf",
            "mmproj:stream.gguf",
            "mmproj\n.gguf",
        ] {
            assert!(!is_root_projector(name));
        }
        let mut discovery = HfDiscovery {
            hf: hf_meta::parse_hf_ref("owner/tiny").unwrap(),
            candidates: vec![candidate("q4km", Some(1.0))],
            projectors: Vec::new(),
            description: None,
        };
        assert_eq!(discovery.select_projector(None).unwrap(), None);
        discovery.projectors.push(HfGgufFile {
            rfilename: "mmproj-F16.gguf".to_string(),
            size: Some(8),
        });
        assert_eq!(
            discovery.select_projector(None).unwrap(),
            Some("mmproj-F16.gguf")
        );
        discovery.projectors.push(HfGgufFile {
            rfilename: "mmproj-Q8_0.gguf".to_string(),
            size: Some(4),
        });
        assert!(discovery
            .select_projector(None)
            .unwrap_err()
            .contains("--mmproj"));
        assert_eq!(
            discovery
                .select_projector(Some("mmproj-Q8_0.gguf"))
                .unwrap(),
            Some("mmproj-Q8_0.gguf")
        );
        assert!(discovery.select_projector(Some("missing.gguf")).is_err());
    }
}
