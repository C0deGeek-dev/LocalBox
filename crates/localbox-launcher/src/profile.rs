//! One run-profile resolver for every LocalBox launch surface.
//!
//! A saved LocalBench result is preferred automatically. Callers receive a
//! typed outcome when it is absent or unusable, so they can require an
//! explicit fallback decision instead of losing a warning in child-process
//! output.

use std::path::{Path, PathBuf};

use localx_llama_core::tuner::Profile;
use localx_llama_core::{Mode, TunerBestConfig, TunerEntry};

use crate::orchestrate::LaunchRequest;

/// Optional constraints and ranking hints for a profile lookup.
#[derive(Debug, Clone, Copy, Default)]
pub struct RunProfileQuery<'a> {
    /// Only entries for this quant are eligible.
    pub quant: Option<&'a str>,
    /// Only entries for this context key are eligible.
    pub context_key: Option<&'a str>,
    /// Only entries for this backend are eligible.
    pub mode: Option<Mode>,
    /// Prefer this tuning profile before score.
    pub preferred_profile: Option<Profile>,
    /// Prefer measurements closest to this VRAM size before score.
    pub vram_gb: Option<i64>,
}

/// Why the saved profile could not be applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnavailableReason {
    /// No profile file has been written.
    Missing,
    /// The file is not valid JSON for the shared tuner schema.
    Invalid,
    /// The store schema is newer or older than this LocalBox build supports.
    UnsupportedSchema,
    /// The store is valid but has no entry satisfying the requested fields.
    NoMatch,
}

impl UnavailableReason {
    /// Stable machine-readable spelling used by the catalog JSON contract.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Invalid => "invalid",
            Self::UnsupportedSchema => "unsupported_schema",
            Self::NoMatch => "no_match",
        }
    }
}

/// The resolved saved-profile state for one model and query.
#[derive(Debug, Clone)]
pub struct RunProfileResolution {
    /// Exact source path inspected.
    pub path: PathBuf,
    /// Selected tuned entry, when one is usable.
    pub entry: Option<TunerEntry>,
    /// Typed failure when no entry is usable.
    pub unavailable: Option<UnavailableReason>,
}

impl RunProfileResolution {
    /// Whether this resolution will use tuned settings.
    #[must_use]
    pub const fn is_tuned(&self) -> bool {
        self.entry.is_some()
    }

    /// A visible, actionable warning suitable for CLI and parent processes.
    #[must_use]
    pub fn warning(&self, key: &str) -> Option<String> {
        let reason = match self.unavailable? {
            UnavailableReason::Missing => "no saved tuned profile exists",
            UnavailableReason::Invalid => "the saved tuned profile is invalid",
            UnavailableReason::UnsupportedSchema => {
                "the saved tuned profile uses an unsupported schema"
            }
            UnavailableReason::NoMatch => {
                "no saved tuned entry matches the requested quant/context/mode"
            }
        };
        Some(format!(
            "Warning: {reason} for '{key}' at {}; continuing uses LocalBox defaults. Run `localbench findbest {key}` to configure tuned settings.",
            self.path.display()
        ))
    }

    /// Apply the selected entry while preserving explicitly pinned fields.
    pub fn apply_to_request(
        &self,
        request: &mut LaunchRequest,
        explicit_mode: bool,
        explicit_quant: bool,
        explicit_context: bool,
    ) {
        let Some(entry) = &self.entry else {
            return;
        };
        if !explicit_mode {
            request.mode = entry.mode;
        }
        if !explicit_quant {
            request.quant = Some(entry.quant.clone());
        }
        if !explicit_context {
            request.context_key = entry.context_key.clone();
        }
        request.params = entry.overrides.to_launch_params();
    }
}

fn profile_path(home: &Path, key: &str) -> PathBuf {
    home.join(".local-llm")
        .join("tuner")
        .join(format!("best-{key}.json"))
}

fn profile_rank(entry: &TunerEntry, preferred: Option<Profile>) -> u8 {
    u8::from(preferred.is_some_and(|profile| entry.profile != profile))
}

/// Select an entry from an already-loaded store. This is the pure seam used
/// by the file-backed resolver and by guided-plan previews.
#[must_use]
pub fn select_run_profile<'a>(
    store: &'a TunerBestConfig,
    query: RunProfileQuery<'_>,
) -> Option<&'a TunerEntry> {
    if !store.schema_supported() {
        return None;
    }
    let mut entries = store
        .entries
        .iter()
        .filter(|entry| query.quant.is_none_or(|quant| entry.quant == quant))
        .filter(|entry| {
            query
                .context_key
                .is_none_or(|context| entry.context_key == context)
        })
        .filter(|entry| query.mode.is_none_or(|mode| entry.mode == mode))
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| {
        profile_rank(a, query.preferred_profile)
            .cmp(&profile_rank(b, query.preferred_profile))
            .then_with(|| {
                query.vram_gb.map_or(std::cmp::Ordering::Equal, |vram| {
                    (a.vram_gb - vram).abs().cmp(&(b.vram_gb - vram).abs())
                })
            })
            .then_with(|| b.score.total_cmp(&a.score))
    });
    entries.into_iter().next()
}

/// Resolve the best saved entry using the same selection path for direct,
/// guided, and parent-process launches.
#[must_use]
pub fn resolve_run_profile(
    home: &Path,
    key: &str,
    query: RunProfileQuery<'_>,
) -> RunProfileResolution {
    let path = profile_path(home, key);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(_) => {
            return RunProfileResolution {
                path,
                entry: None,
                unavailable: Some(UnavailableReason::Missing),
            };
        }
    };
    let store: TunerBestConfig = match serde_json::from_str(&raw) {
        Ok(store) => store,
        Err(_) => {
            return RunProfileResolution {
                path,
                entry: None,
                unavailable: Some(UnavailableReason::Invalid),
            };
        }
    };
    if !store.schema_supported() {
        return RunProfileResolution {
            path,
            entry: None,
            unavailable: Some(UnavailableReason::UnsupportedSchema),
        };
    }
    let entry = select_run_profile(&store, query).cloned();
    RunProfileResolution {
        path,
        unavailable: entry.is_none().then_some(UnavailableReason::NoMatch),
        entry,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use localx_llama_core::tuner::{Overrides, PromptLength, SearchStrategy};

    fn entry(quant: &str, score: f64, profile: Profile, vram_gb: i64) -> TunerEntry {
        TunerEntry {
            quant: quant.to_string(),
            context_key: "64k".to_string(),
            context_tokens: Some(65_536),
            mode: Mode::Native,
            vram_gb,
            prompt_length: PromptLength::Short,
            profile,
            search_strategy: Some(SearchStrategy::Greedy),
            beam_width: None,
            score,
            score_unit: "tok/s".to_string(),
            pure_score: None,
            args: Vec::new(),
            overrides: Overrides::default(),
            measured_at: "2026-08-10T00:00:00Z".to_string(),
            tuner_version: 4,
            trial_count: None,
            gpu_names: None,
            llamacpp_build: None,
        }
    }

    fn write_store(home: &Path, entries: Vec<TunerEntry>) {
        let dir = home.join(".local-llm").join("tuner");
        std::fs::create_dir_all(&dir).unwrap();
        let store = TunerBestConfig {
            schema: 1,
            key: "model".to_string(),
            vram_gb: Some(24),
            entries,
        };
        std::fs::write(
            dir.join("best-model.json"),
            serde_json::to_string(&store).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn missing_invalid_and_unsupported_are_typed() {
        let home = tempfile::tempdir().unwrap();
        let missing = resolve_run_profile(home.path(), "model", RunProfileQuery::default());
        assert_eq!(missing.unavailable, Some(UnavailableReason::Missing));
        std::fs::create_dir_all(home.path().join(".local-llm/tuner")).unwrap();
        std::fs::write(home.path().join(".local-llm/tuner/best-model.json"), "{").unwrap();
        let invalid = resolve_run_profile(home.path(), "model", RunProfileQuery::default());
        assert_eq!(invalid.unavailable, Some(UnavailableReason::Invalid));
        std::fs::write(
            home.path().join(".local-llm/tuner/best-model.json"),
            r#"{"schema":99,"key":"model","entries":[]}"#,
        )
        .unwrap();
        let unsupported = resolve_run_profile(home.path(), "model", RunProfileQuery::default());
        assert_eq!(
            unsupported.unavailable,
            Some(UnavailableReason::UnsupportedSchema)
        );
    }

    #[test]
    fn one_selector_honors_constraints_profile_vram_then_score() {
        let home = tempfile::tempdir().unwrap();
        write_store(
            home.path(),
            vec![
                entry("q4", 100.0, Profile::Pure, 48),
                entry("q4", 80.0, Profile::Balanced, 24),
                entry("q8", 200.0, Profile::Balanced, 24),
            ],
        );
        let found = resolve_run_profile(
            home.path(),
            "model",
            RunProfileQuery {
                quant: Some("q4"),
                preferred_profile: Some(Profile::Balanced),
                vram_gb: Some(24),
                ..RunProfileQuery::default()
            },
        );
        assert_eq!(found.entry.unwrap().score, 80.0);
    }
}
