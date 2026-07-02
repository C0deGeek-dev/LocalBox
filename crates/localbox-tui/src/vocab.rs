//! The plain-language vocabulary: friendly words over the technical launch
//! fields — Run with / Quality / Memory / Speed / Images — so a non-developer
//! never has to read `quant`, `AutoBest`, or `turboquant` to launch a model.
//!
//! The plain-language contract is a test, not a hope: the plan summary must
//! carry the friendly labels and must NOT leak the jargon terms.

use localx_llama_core::{Mode, ModelDef};

use crate::plan::GuidedPlan;

/// What the GPU probe saw: the card's marketing name and its memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuInfo {
    /// The card name as the vendor tool reports it.
    pub name: String,
    /// Total graphics memory in whole GB; `0` = the tool named no number.
    pub vram_gb: u32,
}

/// The one-line hardware banner above the guided menus: what card was
/// found (NVIDIA via `nvidia-smi`, AMD via its tools) and how much
/// graphics memory the fit hints are judged against.
#[must_use]
pub fn gpu_banner(gpu: Option<&GpuInfo>) -> String {
    match gpu {
        Some(info) if info.vram_gb > 0 => format!(
            "Computer:  {} · {} GB graphics memory",
            info.name, info.vram_gb
        ),
        Some(info) => format!("Computer:  {}", info.name),
        None => {
            "Computer:  no NVIDIA or AMD GPU found — running on the processor (slower)".to_string()
        }
    }
}

/// Friendly name for a run target (action).
#[must_use]
pub fn target_label(value: &str) -> String {
    match value {
        "localpilot" => "LocalPilot (recommended)".to_string(),
        "claude" => "Claude Code".to_string(),
        "codex" => "Codex".to_string(),
        "serve" => "Share to other apps".to_string(),
        other => other.to_string(),
    }
}

/// Friendly name for the llama.cpp engine.
#[must_use]
pub fn engine_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Native => "Standard",
        Mode::Turboquant => "Turbo (auto-tuned for your GPU)",
        Mode::Mtpturbo => "Turbo+ (draft speed-ups)",
    }
}

/// Group digits in threes (`65536` → `65,536`) for the words estimate.
fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Friendly name for a context window: memory-as-words (`tokens × 0.75`).
#[must_use]
pub fn memory_label(def: &ModelDef, context_key: &str) -> String {
    let tokens = def
        .contexts
        .get(context_key)
        .copied()
        .filter(|t| *t > 0)
        .map(|t| {
            let words = (t as f64 * 0.75) as u64;
            format!(" (~{} words)", group_thousands(words))
        })
        .unwrap_or_default();
    if context_key.is_empty() {
        format!("Standard{tokens}")
    } else {
        format!("Large — {context_key}{tokens}")
    }
}

/// Friendly name for a quant: a plain quality hint plus the on-disk size.
#[must_use]
pub fn quality_label(def: &ModelDef, quant: &str) -> String {
    let size = def
        .quants
        .get(quant)
        .and_then(|q| q.size_gb)
        .map(|gb| format!(" · {gb:.1} GB"))
        .unwrap_or_default();
    let lower = quant.to_lowercase();
    let hint = if lower.contains("compact") || lower.contains("mini") || lower.contains("q4") {
        "smaller & faster"
    } else if lower.contains("quality") || lower.contains("q6") || lower.contains("q8") {
        "best quality"
    } else {
        "balanced"
    };
    format!("{hint}{size}")
}

/// The recommended plan in plain words — the guided launcher's centrepiece.
#[must_use]
pub fn plan_summary(plan: &GuidedPlan, def: &ModelDef) -> String {
    let name = def
        .display_name
        .clone()
        .unwrap_or_else(|| plan.model_key.clone());
    let speed = if plan.use_auto_best {
        format!("{} · auto-tuned", engine_label(plan.mode))
    } else {
        engine_label(plan.mode).to_string()
    };
    let kv = match (&plan.kv_cache_k, &plan.kv_cache_v) {
        (Some(k), Some(v)) if k == v => k.clone(),
        (Some(k), Some(v)) => format!("{k}/{v}"),
        (Some(k), None) => k.clone(),
        (None, _) if plan.use_auto_best => "chosen by auto-tune".to_string(),
        (None, _) => "auto (default)".to_string(),
    };
    let on_off = |b: bool| if b { "on" } else { "off" };
    [
        format!("Model:     {name}"),
        format!("Run with:  {}", target_label(&plan.target)),
        format!(
            "Quality:   {} · {}",
            quality_label(def, &plan.quant),
            plan.quant
        ),
        format!("Memory:    {}", memory_label(def, &plan.context_key)),
        format!("Speed:     {speed}"),
        format!("KV cache:  {kv}"),
        format!(
            "Images:    {}   ·   Strict: {}",
            on_off(plan.vision),
            on_off(plan.strict)
        ),
    ]
    .join("\n")
}

/// Plain-language help: what each choice means and when to change it.
#[must_use]
pub fn glossary() -> &'static str {
    "What these mean:\n\
     \n\
     \x20 Run with  – which coding assistant drives the model. LocalPilot is the\n\
     \x20             built-in one and the safe default.\n\
     \x20 Quality   – bigger files understand more but use more graphics memory (GB);\n\
     \x20             smaller ones are faster and lighter. \"Balanced\" suits most people.\n\
     \x20 Memory    – how much of the conversation the model can keep in mind. Standard\n\
     \x20             is fine; Large remembers more but uses more graphics memory.\n\
     \x20 Speed     – the engine. \"Turbo (auto-tuned)\" is tuned to your GPU and is the\n\
     \x20             recommended default; \"Standard\" is the plain engine.\n\
     \x20 Images    – turn on if you want the model to look at pictures you paste.\n\
     \n\
     Auto-tune  – \"Auto-tune this model (run a benchmark)\" measures your GPU once and\n\
     \x20            saves the fastest safe settings (engine + KV cache). After that,\n\
     \x20            leaving Auto-tune \"on\" uses those saved results automatically.\n\
     \n\
     Tip: pick a model and choose \"Launch now\" — the recommended settings already\n\
     fit your machine. Use \"Customize\" only if you want to change something."
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::plan::{resolve_launch_plan, DefaultLaunch, PlanOverrides};

    fn def() -> ModelDef {
        serde_json::from_str(
            r#"{
            "DisplayName": "Qwen 3.6 APEX",
            "Root": "q36apex",
            "Repo": "mudler/apex",
            "Quants": {
                "apex-balanced": { "File": "b.gguf", "SizeGB": 18.6 },
                "apex-i-mini": { "File": "m.gguf", "SizeGB": 12.1 }
            },
            "Quant": "apex-balanced",
            "Contexts": { "": 32768, "64k": 65536 }
        }"#,
        )
        .unwrap()
    }

    fn plan() -> GuidedPlan {
        resolve_launch_plan(
            "q36apex",
            &def(),
            &DefaultLaunch::default(),
            &PlanOverrides::default(),
        )
    }

    #[test]
    fn the_plain_language_contract_holds_on_the_summary() {
        let mut plan = plan();
        plan.mode = Mode::Turboquant;
        plan.use_auto_best = true;
        let summary = plan_summary(&plan, &def());
        // The friendly labels MUST appear...
        for required in ["Run with:", "Quality:", "Memory:", "Speed:", "Turbo"] {
            assert!(summary.contains(required), "summary must carry {required}");
        }
        // ...and the jargon MUST NOT.
        for banned in ["quant", "AutoBest", "turboquant", "mtpturbo"] {
            assert!(
                !summary.contains(banned),
                "summary leaked jargon '{banned}':\n{summary}"
            );
        }
    }

    #[test]
    fn memory_reads_as_words_and_quality_as_hint_plus_gb() {
        let def = def();
        assert_eq!(memory_label(&def, ""), "Standard (~24,576 words)");
        assert_eq!(memory_label(&def, "64k"), "Large — 64k (~49,152 words)");
        assert_eq!(quality_label(&def, "apex-balanced"), "balanced · 18.6 GB");
        assert_eq!(
            quality_label(&def, "apex-i-mini"),
            "smaller & faster · 12.1 GB"
        );
    }

    #[test]
    fn the_gpu_banner_names_the_card_and_degrades_plainly() {
        let card = GpuInfo {
            name: "NVIDIA GeForce RTX 4090".to_string(),
            vram_gb: 24,
        };
        assert_eq!(
            gpu_banner(Some(&card)),
            "Computer:  NVIDIA GeForce RTX 4090 · 24 GB graphics memory"
        );
        let unsized_card = GpuInfo {
            name: "AMD Radeon RX 7900 XTX".to_string(),
            vram_gb: 0,
        };
        assert_eq!(
            gpu_banner(Some(&unsized_card)),
            "Computer:  AMD Radeon RX 7900 XTX"
        );
        let none = gpu_banner(None);
        assert!(none.contains("no NVIDIA or AMD GPU"));
        assert!(none.contains("processor"), "names the CPU fallback plainly");
    }

    #[test]
    fn friendly_names_cover_targets_and_engines() {
        assert_eq!(target_label("localpilot"), "LocalPilot (recommended)");
        assert_eq!(target_label("serve"), "Share to other apps");
        assert_eq!(engine_label(Mode::Native), "Standard");
        assert_eq!(
            engine_label(Mode::Turboquant),
            "Turbo (auto-tuned for your GPU)"
        );
        assert_eq!(engine_label(Mode::Mtpturbo), "Turbo+ (draft speed-ups)");
    }

    #[test]
    fn the_glossary_speaks_to_non_developers() {
        let text = glossary();
        for required in ["Run with", "graphics memory", "Launch now"] {
            assert!(text.contains(required), "glossary must mention {required}");
        }
        assert!(!text.contains("turboquant"));
    }

    #[test]
    fn kv_line_reflects_auto_tune_ownership() {
        let mut p = plan();
        p.use_auto_best = true;
        p.kv_cache_k = None;
        p.kv_cache_v = None;
        assert!(plan_summary(&p, &def()).contains("KV cache:  chosen by auto-tune"));
        p.use_auto_best = false;
        assert!(plan_summary(&p, &def()).contains("KV cache:  auto (default)"));
        p.kv_cache_k = Some("q8_0".to_string());
        p.kv_cache_v = Some("q8_0".to_string());
        assert!(plan_summary(&p, &def()).contains("KV cache:  q8_0"));
        p.kv_cache_v = Some("turbo3".to_string());
        assert!(plan_summary(&p, &def()).contains("KV cache:  q8_0/turbo3"));
    }
}
