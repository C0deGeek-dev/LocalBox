//! The Customize path: progressive disclosure over the resolved plan.
//!
//! Every row shows its current value; choosing one opens the detailed picker
//! and the delta merges back into the overrides, re-rendering the summary.
//! Two rules are load-bearing:
//! - **Auto-tune owns Engine + KV.** With auto-tune on, those rows lock with a
//!   plain explanation (never a silent block), and any manual KV override is
//!   dropped so it cannot strand against the tuner's choice; turning auto-tune
//!   off restores manual control.
//! - **Save is target-gated.** A launch target that isn't a resumable agent
//!   (`serve`) cannot be saved as the default launch.

use localx_llama_core::{Mode, ModelDef};

use crate::plan::{GuidedPlan, PlanOverrides};
use crate::vocab;

/// One row of the customize menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomizeRow {
    pub label: String,
    pub action: CustomizeAction,
}

/// What choosing a row does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomizeAction {
    PickTarget,
    PickQuant,
    PickContext,
    PickMode,
    /// Engine is auto-tuned; explains instead of editing.
    ModeLocked,
    /// The model format requires a specific engine.
    ModeRequired,
    PickAutoTune,
    PickKv,
    /// KV is auto-tuned; explains instead of editing.
    KvLocked,
    ToggleVision,
    ToggleStrict,
    SaveDefault,
    Done,
}

/// The plain explanation shown when a locked row is chosen.
#[must_use]
pub fn locked_explanation(action: &CustomizeAction) -> Option<&'static str> {
    match action {
        CustomizeAction::ModeLocked => Some(
            "Auto-tune selects and tunes the engine for you. Turn Auto-tune off to choose it yourself.",
        ),
        CustomizeAction::ModeRequired => Some(
            "This model's weight format requires this engine, so it cannot be changed.",
        ),
        CustomizeAction::KvLocked => Some(
            "Auto-tune chooses the KV cache for you. Turn Auto-tune off to set it yourself.",
        ),
        _ => None,
    }
}

/// Build the customize menu for the current plan: every row carries its
/// current value; Engine and KV lock (with the auto-tuned marker) when
/// auto-tune is on.
#[must_use]
pub fn customize_menu(plan: &GuidedPlan, def: &ModelDef) -> Vec<CustomizeRow> {
    customize_menu_with_required_mode(plan, def, None)
}

/// Build the customize menu with LocalBox-specific catalog engine policy.
#[must_use]
pub fn customize_menu_with_required_mode(
    plan: &GuidedPlan,
    def: &ModelDef,
    required_mode: Option<Mode>,
) -> Vec<CustomizeRow> {
    let mut rows = Vec::new();
    let row = |label: String, action: CustomizeAction| CustomizeRow { label, action };

    rows.push(row(
        format!("Run with:         {}", vocab::target_label(&plan.target)),
        CustomizeAction::PickTarget,
    ));
    if !def.quants.is_empty() {
        rows.push(row(
            format!("Quality (quant):  {}", plan.quant),
            CustomizeAction::PickQuant,
        ));
    }
    let ctx = if plan.context_key.is_empty() {
        "default"
    } else {
        &plan.context_key
    };
    rows.push(row(
        format!("Memory (context): {ctx}"),
        CustomizeAction::PickContext,
    ));
    if required_mode.is_some() {
        rows.push(row(
            format!("Engine (mode):    {}  (required)", plan.mode.as_str()),
            CustomizeAction::ModeRequired,
        ));
    } else if plan.use_auto_best {
        rows.push(row(
            format!("Engine (mode):    {}  (auto-tuned)", plan.mode.as_str()),
            CustomizeAction::ModeLocked,
        ));
    } else {
        rows.push(row(
            format!("Engine (mode):    {}", plan.mode.as_str()),
            CustomizeAction::PickMode,
        ));
    }
    let auto = if plan.use_auto_best {
        format!("{} (on)", plan.auto_best_profile)
    } else {
        "off (manual)".to_string()
    };
    rows.push(row(
        format!("Auto-tune:        {auto}"),
        CustomizeAction::PickAutoTune,
    ));
    if plan.use_auto_best {
        rows.push(row(
            "KV cache:         (chosen by auto-tune)".to_string(),
            CustomizeAction::KvLocked,
        ));
    } else {
        let kv = match (&plan.kv_cache_k, &plan.kv_cache_v) {
            (Some(k), Some(v)) if k == v => k.clone(),
            (Some(k), Some(v)) => format!("{k}/{v}"),
            (Some(k), None) => k.clone(),
            (None, _) => "auto (default)".to_string(),
        };
        rows.push(row(
            format!("KV cache:         {kv}"),
            CustomizeAction::PickKv,
        ));
    }
    let on_off = |b: bool| if b { "on" } else { "off" };
    rows.push(row(
        format!("Images (vision):  {}", on_off(plan.vision)),
        CustomizeAction::ToggleVision,
    ));
    rows.push(row(
        format!("Strict output:    {}", on_off(plan.strict)),
        CustomizeAction::ToggleStrict,
    ));
    rows.push(row(
        "— Save these as my default —".to_string(),
        CustomizeAction::SaveDefault,
    ));
    rows.push(row(
        "✓  Done — back to launch".to_string(),
        CustomizeAction::Done,
    ));
    rows
}

/// Turn auto-tune on with a profile — and drop any manual KV override so it
/// cannot strand against the tuner's choice.
pub fn set_auto_tune_on(overrides: &mut PlanOverrides, profile: &str) {
    overrides.use_auto_best = Some(true);
    overrides.auto_best_profile = Some(profile.to_string());
    // Auto-tune owns the KV cache.
    overrides.kv_cache_k = None;
    overrides.kv_cache_v = None;
}

/// Turn auto-tune off (manual control of engine + KV returns).
pub fn set_auto_tune_off(overrides: &mut PlanOverrides) {
    overrides.use_auto_best = Some(false);
}

/// Whether the plan may be saved as the default launch. Only resumable agent
/// targets qualify; `serve` (and anything unknown) is refused with the plain
/// reason.
pub fn save_gate(target: &str) -> Result<(), String> {
    match target {
        "localpilot" | "claude" | "codex" => Ok(()),
        other => Err(format!(
            "'{}' can't be saved as a default launch target.",
            vocab::target_label(other)
        )),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::plan::{resolve_launch_plan, DefaultLaunch};
    use localx_llama_core::Mode;

    fn def() -> ModelDef {
        serde_json::from_str(
            r#"{
            "Repo": "mudler/apex",
            "Quants": { "apex-balanced": "b.gguf" },
            "Quant": "apex-balanced",
            "Contexts": { "": 32768 }
        }"#,
        )
        .unwrap()
    }

    fn plan_with(overrides: &PlanOverrides) -> GuidedPlan {
        resolve_launch_plan("q36apex", &def(), &DefaultLaunch::default(), overrides)
    }

    #[test]
    fn auto_tune_on_locks_engine_and_kv_and_drops_the_manual_override() {
        let mut overrides = PlanOverrides {
            kv_cache_k: Some("q8_0".to_string()),
            kv_cache_v: Some("q8_0".to_string()),
            mode: Some(Mode::Turboquant),
            ..PlanOverrides::default()
        };
        set_auto_tune_on(&mut overrides, "balanced");
        // The stranded manual KV is gone; the profile is recorded.
        assert_eq!(overrides.kv_cache_k, None);
        assert_eq!(overrides.kv_cache_v, None);
        assert_eq!(overrides.use_auto_best, Some(true));
        assert_eq!(overrides.auto_best_profile.as_deref(), Some("balanced"));

        // The menu shows Engine + KV locked, with plain explanations.
        let menu = customize_menu(&plan_with(&overrides), &def());
        let engine = menu.iter().find(|r| r.label.starts_with("Engine")).unwrap();
        assert_eq!(engine.action, CustomizeAction::ModeLocked);
        assert!(engine.label.contains("(auto-tuned)"));
        let kv = menu
            .iter()
            .find(|r| r.label.starts_with("KV cache"))
            .unwrap();
        assert_eq!(kv.action, CustomizeAction::KvLocked);
        assert!(kv.label.contains("(chosen by auto-tune)"));
        assert!(locked_explanation(&engine.action)
            .unwrap()
            .contains("Turn Auto-tune off"));
        assert!(locked_explanation(&kv.action)
            .unwrap()
            .contains("Turn Auto-tune off"));
    }

    #[test]
    fn auto_tune_off_restores_manual_engine_and_kv() {
        let mut overrides = PlanOverrides::default();
        set_auto_tune_on(&mut overrides, "auto");
        set_auto_tune_off(&mut overrides);
        let menu = customize_menu(&plan_with(&overrides), &def());
        let engine = menu.iter().find(|r| r.label.starts_with("Engine")).unwrap();
        assert_eq!(engine.action, CustomizeAction::PickMode);
        let kv = menu
            .iter()
            .find(|r| r.label.starts_with("KV cache"))
            .unwrap();
        assert_eq!(kv.action, CustomizeAction::PickKv);
        assert!(kv.label.contains("auto (default)"));
        let auto = menu
            .iter()
            .find(|r| r.label.starts_with("Auto-tune"))
            .unwrap();
        assert!(auto.label.contains("off (manual)"));
    }

    #[test]
    fn serve_cannot_be_saved_as_the_default_launch() {
        assert!(save_gate("localpilot").is_ok());
        assert!(save_gate("claude").is_ok());
        assert!(save_gate("codex").is_ok());
        let err = save_gate("serve").unwrap_err();
        assert!(err.contains("Share to other apps"));
        assert!(err.contains("can't be saved"));
    }

    #[test]
    fn every_row_shows_its_current_value_and_the_menu_loops_home() {
        let overrides = PlanOverrides {
            vision: Some(true),
            ..PlanOverrides::default()
        };
        let menu = customize_menu(&plan_with(&overrides), &def());
        let labels: Vec<&str> = menu.iter().map(|r| r.label.as_str()).collect();
        assert!(labels
            .iter()
            .any(|l| l.contains("Run with:") && l.contains("LocalPilot")));
        assert!(labels
            .iter()
            .any(|l| l.contains("Quality (quant):  apex-balanced")));
        assert!(labels
            .iter()
            .any(|l| l.contains("Memory (context): default")));
        assert!(labels.iter().any(|l| l.contains("Images (vision):  on")));
        assert_eq!(menu.last().unwrap().action, CustomizeAction::Done);
        assert!(menu
            .iter()
            .any(|r| r.action == CustomizeAction::SaveDefault));
    }
}
