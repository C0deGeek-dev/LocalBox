//! The guided launch plan and its resolution precedence.
//!
//! One resolver, no I/O: **explicit overrides > the saved DefaultLaunch's
//! cross-model preferences (target/mode/auto-tune) > the per-model definition
//! (quant/context/strict) > hard defaults**. Quant and context seed from the
//! DefaultLaunch only when the selected model IS the saved default model — a
//! recipe tuned for one model must never leak its quant onto another.
//! A returning user's plan therefore IS their last-good plan, and "Launch
//! now" replays it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use localx_llama_core::{Mode, ModelDef};

/// The fully-resolved guided plan for one model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuidedPlan {
    pub model_key: String,
    /// Run target: `localpilot` / `claude` / `codex` / `serve`.
    pub target: String,
    pub quant: String,
    pub context_key: String,
    pub mode: Mode,
    pub auto_best_profile: String,
    pub use_auto_best: bool,
    pub vision: bool,
    pub strict: bool,
    pub kv_cache_k: Option<String>,
    pub kv_cache_v: Option<String>,
}

/// The saved last-good launch recipe (`DefaultLaunch` in settings).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct DefaultLaunch {
    /// The model the recipe was saved for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_key: Option<String>,
    /// The run target (`Action` on disk).
    #[serde(rename = "Action", skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(rename = "LlamaCppMode", skip_serializing_if = "Option::is_none")]
    pub llama_cpp_mode: Option<Mode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_best_profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub use_auto_best: Option<bool>,
    /// Per-model: applies only when the launched model matches `model_key`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    /// Per-model: applies only when the launched model matches `model_key`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quant: Option<String>,
    /// Per-model: applies only when the launched model matches `model_key`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_key: Option<String>,
    #[serde(rename = "KvCacheK", skip_serializing_if = "Option::is_none")]
    pub kv_cache_k: Option<String>,
    #[serde(rename = "KvCacheV", skip_serializing_if = "Option::is_none")]
    pub kv_cache_v: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

/// Explicit user choices for this launch (each `Some` wins outright).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanOverrides {
    pub target: Option<String>,
    pub quant: Option<String>,
    pub context_key: Option<String>,
    pub mode: Option<Mode>,
    pub auto_best_profile: Option<String>,
    pub use_auto_best: Option<bool>,
    pub vision: Option<bool>,
    pub strict: Option<bool>,
    pub kv_cache_k: Option<String>,
    pub kv_cache_v: Option<String>,
}

fn first_non_blank(candidates: &[Option<&str>]) -> Option<String> {
    candidates
        .iter()
        .flatten()
        .find(|s| !s.trim().is_empty())
        .map(|s| (*s).to_string())
}

/// Resolve the guided plan. Pure — the same inputs always produce the same
/// plan (the non-committing preview IS this function).
#[must_use]
pub fn resolve_launch_plan(
    model_key: &str,
    def: &ModelDef,
    defaults: &DefaultLaunch,
    overrides: &PlanOverrides,
) -> GuidedPlan {
    resolve_launch_plan_with_required_mode(model_key, def, defaults, overrides, None)
}

/// Resolve a guided plan with LocalBox-specific catalog engine policy.
#[must_use]
pub fn resolve_launch_plan_with_required_mode(
    model_key: &str,
    def: &ModelDef,
    defaults: &DefaultLaunch,
    overrides: &PlanOverrides,
    required_mode: Option<Mode>,
) -> GuidedPlan {
    let same_model = defaults.model_key.as_deref() == Some(model_key);
    let def_quant = def.quant.clone().unwrap_or_default();
    let first_quant = def.quants.keys().next().cloned().unwrap_or_default();

    let target = first_non_blank(&[
        overrides.target.as_deref(),
        defaults.action.as_deref(),
        Some("localpilot"),
    ])
    .unwrap_or_default();
    let mode = required_mode
        .or(overrides.mode)
        .or(defaults.llama_cpp_mode)
        .unwrap_or(Mode::Native);
    let auto_best_profile = first_non_blank(&[
        overrides.auto_best_profile.as_deref(),
        defaults.auto_best_profile.as_deref(),
        Some("auto"),
    ])
    .unwrap_or_default();
    let quant = first_non_blank(&[
        overrides.quant.as_deref(),
        if same_model {
            defaults.quant.as_deref()
        } else {
            None
        },
        Some(def_quant.as_str()),
        Some(first_quant.as_str()),
    ])
    .unwrap_or_default();
    let context_key = overrides
        .context_key
        .clone()
        .or_else(|| {
            if same_model {
                defaults.context_key.clone()
            } else {
                None
            }
        })
        .unwrap_or_default();

    let use_auto_best = overrides
        .use_auto_best
        .or(defaults.use_auto_best)
        .unwrap_or(false);
    let vision = overrides
        .vision
        .or(if same_model { defaults.vision } else { None })
        .unwrap_or(false);
    // Strict is per-model like quant/context: the saved recipe's toggle
    // replays only for the model it was saved for.
    let strict = overrides
        .strict
        .or(if same_model { defaults.strict } else { None })
        .unwrap_or_else(|| def.strict.unwrap_or(false));
    let kv_cache_k = first_non_blank(&[
        overrides.kv_cache_k.as_deref(),
        defaults.kv_cache_k.as_deref(),
    ]);
    let kv_cache_v = first_non_blank(&[
        overrides.kv_cache_v.as_deref(),
        defaults.kv_cache_v.as_deref(),
    ]);

    GuidedPlan {
        model_key: model_key.to_string(),
        target,
        quant,
        context_key,
        mode,
        auto_best_profile,
        use_auto_best,
        vision,
        strict,
        kv_cache_k,
        kv_cache_v,
    }
}

/// Walk up from `start` looking for a `.llm-default` file (the workspace's
/// model override); the first match wins. Returns the trimmed model key.
#[must_use]
pub fn find_workspace_default(start: &Path) -> Option<(String, PathBuf)> {
    let mut dir = Some(start);
    while let Some(current) = dir {
        let marker = current.join(".llm-default");
        if marker.is_file() {
            if let Ok(raw) = std::fs::read_to_string(&marker) {
                let key = raw.trim().to_string();
                if !key.is_empty() {
                    return Some((key, marker));
                }
            }
        }
        dir = current.parent();
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn def() -> ModelDef {
        serde_json::from_str(
            r#"{
            "Repo": "mudler/apex",
            "Quants": {
                "apex-balanced": "b.gguf",
                "apex-i-mini": "m.gguf"
            },
            "Quant": "apex-balanced",
            "Strict": true,
            "Contexts": { "": 32768, "64k": 65536 }
        }"#,
        )
        .unwrap()
    }

    fn saved() -> DefaultLaunch {
        DefaultLaunch {
            model_key: Some("q36apex".to_string()),
            action: Some("claude".to_string()),
            llama_cpp_mode: Some(Mode::Turboquant),
            auto_best_profile: Some("balanced".to_string()),
            use_auto_best: Some(true),
            vision: None,
            quant: Some("apex-i-mini".to_string()),
            context_key: Some("64k".to_string()),
            kv_cache_k: None,
            kv_cache_v: None,
            strict: None,
        }
    }

    #[test]
    fn a_returning_users_plan_is_their_last_good_plan() {
        let plan = resolve_launch_plan("q36apex", &def(), &saved(), &PlanOverrides::default());
        assert_eq!(plan.target, "claude");
        assert_eq!(plan.mode, Mode::Turboquant);
        assert!(plan.use_auto_best);
        assert_eq!(plan.auto_best_profile, "balanced");
        // Same model: the per-model quant/context replay too.
        assert_eq!(plan.quant, "apex-i-mini");
        assert_eq!(plan.context_key, "64k");
    }

    #[test]
    fn cross_model_preferences_apply_but_quant_and_context_stay_per_model() {
        // A DIFFERENT model: target/mode/auto-tune carry over; quant/context
        // fall back to this model's own definition.
        let plan = resolve_launch_plan("other-model", &def(), &saved(), &PlanOverrides::default());
        assert_eq!(plan.target, "claude", "cross-model preference applies");
        assert_eq!(plan.mode, Mode::Turboquant);
        assert!(plan.use_auto_best);
        assert_eq!(
            plan.quant, "apex-balanced",
            "the def's own quant, not the recipe's"
        );
        assert_eq!(plan.context_key, "", "the def's default context");
    }

    #[test]
    fn a_saved_strict_toggle_replays_for_its_own_model_only() {
        let mut recipe = saved();
        recipe.strict = Some(false); // saved with strict turned OFF
        let plan = resolve_launch_plan("q36apex", &def(), &recipe, &PlanOverrides::default());
        assert!(!plan.strict, "the recipe's toggle replays for its model");
        let other = resolve_launch_plan("other-model", &def(), &recipe, &PlanOverrides::default());
        assert!(other.strict, "another model keeps its own def strict");
    }

    #[test]
    fn explicit_overrides_beat_everything() {
        let overrides = PlanOverrides {
            target: Some("localpilot".to_string()),
            quant: Some("apex-balanced".to_string()),
            mode: Some(Mode::Native),
            use_auto_best: Some(false),
            vision: Some(false),
            ..PlanOverrides::default()
        };
        let defaults = serde_json::from_value(serde_json::json!({
            "ModelKey": "q36apex",
            "Action": "claude",
            "LlamaCppMode": "turboquant",
            "AutoBestProfile": "balanced",
            "UseAutoBest": true,
            "Quant": "apex-i-mini",
            "ContextKey": "64k",
            "Vision": true
        }))
        .unwrap();
        let plan = resolve_launch_plan("q36apex", &def(), &defaults, &overrides);
        assert_eq!(plan.target, "localpilot");
        assert_eq!(plan.quant, "apex-balanced");
        assert_eq!(plan.mode, Mode::Native);
        assert!(!plan.use_auto_best);
        assert!(
            !plan.vision,
            "an explicit override can turn saved vision off"
        );
    }

    #[test]
    fn saved_vision_replays_for_its_own_model_only() {
        let defaults: DefaultLaunch = serde_json::from_value(serde_json::json!({
            "ModelKey": "q36apex",
            "Action": "claude",
            "Vision": true
        }))
        .unwrap();
        let plan = resolve_launch_plan("q36apex", &def(), &defaults, &PlanOverrides::default());
        assert!(plan.vision, "the saved vision toggle replays for its model");

        let other =
            resolve_launch_plan("other-model", &def(), &defaults, &PlanOverrides::default());
        assert!(
            !other.vision,
            "vision is model-specific and must not leak to another model"
        );
    }

    #[test]
    fn old_recipes_without_vision_stay_text_only() {
        let defaults: DefaultLaunch = serde_json::from_value(serde_json::json!({
            "ModelKey": "q36apex",
            "Action": "claude"
        }))
        .unwrap();
        let plan = resolve_launch_plan("q36apex", &def(), &defaults, &PlanOverrides::default());
        assert!(!plan.vision);
    }

    #[test]
    fn hard_defaults_hold_with_nothing_saved() {
        let plan = resolve_launch_plan(
            "q36apex",
            &def(),
            &DefaultLaunch::default(),
            &PlanOverrides::default(),
        );
        assert_eq!(plan.target, "localpilot");
        assert_eq!(plan.mode, Mode::Native);
        assert_eq!(plan.auto_best_profile, "auto");
        assert!(!plan.use_auto_best);
        assert_eq!(plan.quant, "apex-balanced", "the def's default quant");
        assert!(plan.strict, "the def's strict default applies");
        assert!(!plan.vision, "vision is always opt-in");
    }

    #[test]
    fn a_model_required_mode_beats_saved_and_explicit_engine_choices() {
        let model = def();
        let overrides = PlanOverrides {
            mode: Some(Mode::Native),
            ..PlanOverrides::default()
        };
        let plan = resolve_launch_plan_with_required_mode(
            "q36apex",
            &model,
            &saved(),
            &overrides,
            Some(Mode::PrismMl),
        );
        assert_eq!(plan.mode, Mode::PrismMl);
    }

    #[test]
    fn the_preview_is_non_committing_by_construction() {
        // Resolving twice with the same inputs yields the identical plan and
        // mutates nothing — the resolver is the preview.
        let defaults = saved();
        let overrides = PlanOverrides::default();
        let a = resolve_launch_plan("q36apex", &def(), &defaults, &overrides);
        let b = resolve_launch_plan("q36apex", &def(), &defaults, &overrides);
        assert_eq!(a, b);
        assert_eq!(defaults, saved(), "inputs untouched");
    }

    #[test]
    fn the_workspace_marker_wins_by_walk_up() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        assert!(find_workspace_default(&nested).is_none());
        std::fs::write(dir.path().join(".llm-default"), "q36apex\n").unwrap();
        let (key, marker) = find_workspace_default(&nested).expect("found by walk-up");
        assert_eq!(key, "q36apex");
        assert!(marker.ends_with(".llm-default"));
        // A nearer marker shadows the outer one.
        std::fs::write(nested.join(".llm-default"), "nearer").unwrap();
        let (key, _) = find_workspace_default(&nested).unwrap();
        assert_eq!(key, "nearer");
    }

    #[test]
    fn the_recipe_round_trips_its_on_disk_shape() {
        let json = serde_json::to_value(saved()).unwrap();
        // PascalCase + the historical Action/LlamaCppMode spellings.
        assert_eq!(json["ModelKey"], "q36apex");
        assert_eq!(json["Action"], "claude");
        assert_eq!(json["LlamaCppMode"], "turboquant");
        assert_eq!(json["AutoBestProfile"], "balanced");
        let back: DefaultLaunch = serde_json::from_value(json).unwrap();
        assert_eq!(back, saved());
    }
}
