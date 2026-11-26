//! The starter model: one small, known model sized to this machine's memory,
//! for a first run that has no model yet.
//!
//! The rule is pure — memory figures in, a recommendation out — so every tier
//! is unit-tested without hardware. Each rung's memory need is measured, not
//! estimated from the file size: the whole GPU memory a running llama-server
//! took for that model at that context (weights, KV cache, compute buffers and
//! the CUDA context), read from `nvidia-smi` on an RTX 4090 with the CUDA
//! build, rounded up to 0.1 GB. The same figure is used for a CPU run as an
//! estimate: it includes the CUDA context a CPU run does not need, but a CPU
//! run has not been measured.

use serde_json::{json, Value};

/// A starter candidate: where it comes from and what it needs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rung {
    /// Short human name.
    pub label: &'static str,
    /// Hugging Face repository id.
    pub repo: &'static str,
    /// Quant spelling as the repository lists it.
    pub quant: &'static str,
    /// Vision projector to record (the repositories list several precisions).
    pub projector: &'static str,
    /// Download size of the weights in GB (GiB), as listed on the Hub.
    pub size_gb: f64,
    /// Memory a running server needs at the 64k catalog default context.
    pub need_64k_gb: f64,
    /// Memory a running server needs at 32k context.
    pub need_32k_gb: f64,
    /// Small enough to answer at a usable speed without a GPU.
    pub cpu_ok: bool,
}

/// The ladder, smallest first. Needs measured 2026-11-25 (see the module doc):
/// 2B 2300/2000 MiB, 4B 4561/3857 MiB, 9B 6823/6119 MiB at 64k/32k.
pub const LADDER: [Rung; 3] = [
    Rung {
        label: "Qwen3.5 2B",
        repo: "unsloth/Qwen3.5-2B-GGUF",
        quant: "Q4_K_M",
        projector: "mmproj-F16.gguf",
        size_gb: 1.2,
        need_64k_gb: 2.3,
        need_32k_gb: 2.0,
        cpu_ok: true,
    },
    Rung {
        label: "Qwen3.5 4B",
        repo: "unsloth/Qwen3.5-4B-GGUF",
        quant: "Q4_K_M",
        projector: "mmproj-F16.gguf",
        size_gb: 2.6,
        need_64k_gb: 4.5,
        need_32k_gb: 3.8,
        cpu_ok: true,
    },
    Rung {
        label: "Qwen3.5 9B",
        repo: "unsloth/Qwen3.5-9B-GGUF",
        quant: "Q4_K_M",
        projector: "mmproj-F16.gguf",
        size_gb: 5.3,
        need_64k_gb: 6.7,
        need_32k_gb: 6.0,
        cpu_ok: false,
    },
];

/// GPU memory kept back when only the total is known: the display and
/// driver use some, and nothing measured how much.
pub const GPU_RESERVE_GB: f64 = 1.0;
/// GPU memory kept back from what is measured free right now, for the
/// small allocations other programs make after the measurement.
pub const GPU_FREE_HEADROOM_GB: f64 = 0.5;
/// Available RAM kept free for the operating system and other programs.
pub const RAM_RESERVE_GB: f64 = 2.0;

/// The memory a recommendation is made from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Memory {
    /// Total GPU memory (0 when no GPU answered).
    pub vram_total_gb: f64,
    /// GPU memory free right now on the first GPU, as measured, when the GPU
    /// reports it. Nothing is added to the reading.
    pub vram_free_gb: Option<f64>,
    /// Model servers running on the GPU when the reading was taken (see
    /// [`note_running`]).
    pub running_on_gpu: u32,
    /// Total system RAM.
    pub ram_total_gb: f64,
    /// RAM available right now.
    pub ram_available_gb: f64,
}

/// Where the model is meant to run. A launch must honour it: a starter chosen
/// for the CPU because the GPU is too small must not start on GPU defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Gpu,
    Cpu,
}

impl Mode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Gpu => "gpu",
            Mode::Cpu => "cpu",
        }
    }
}

/// The outcome of the starter rule.
#[derive(Debug, Clone, PartialEq)]
pub enum Recommendation {
    /// A rung that fits, how it should run, and why.
    Starter {
        rung: Rung,
        mode: Mode,
        /// Catalog context key the launch should use (`64k` or `32k`).
        context: &'static str,
        need_gb: f64,
        reason: String,
    },
    /// Nothing on the ladder fits; the reason gives the numbers.
    NoFit { reason: String },
}

/// The largest fitting rung in `rungs` (searched largest first) within
/// `budget` GB, preferring the 64k context and stepping down to 32k.
fn largest_fit<'a>(
    rungs: impl DoubleEndedIterator<Item = &'a Rung>,
    budget: f64,
) -> Option<(Rung, &'static str, f64)> {
    rungs.rev().find_map(|rung| {
        if rung.need_64k_gb <= budget {
            Some((*rung, "64k", rung.need_64k_gb))
        } else if rung.need_32k_gb <= budget {
            Some((*rung, "32k", rung.need_32k_gb))
        } else {
            None
        }
    })
}

/// Pick the starter for `memory`.
#[must_use]
pub fn recommend(memory: Memory) -> Recommendation {
    let smallest = LADDER[0];
    let mut gpu_note = String::new();
    if memory.vram_total_gb > 0.0 {
        // What is free right now decides when the GPU reports it: other
        // programs may already hold a lot of it. Only without a measurement
        // does the rule fall back to the total less a fixed reserve, and it
        // says so.
        let (budget, measured) = match memory.vram_free_gb {
            Some(free) => (free.min(memory.vram_total_gb) - GPU_FREE_HEADROOM_GB, true),
            None => (memory.vram_total_gb - GPU_RESERVE_GB, false),
        };
        let have = match memory.vram_free_gb {
            Some(free) => format!(
                "{free:.1} GB of the GPU's {:.0} GB is free now{}",
                memory.vram_total_gb,
                running_note(memory)
            ),
            None => format!(
                "the GPU has {:.0} GB in total; how much is free could not be measured, so \
                 all but {GPU_RESERVE_GB:.0} GB is assumed free",
                memory.vram_total_gb
            ),
        };
        if let Some((rung, context, need)) = largest_fit(LADDER.iter(), budget) {
            let left = if measured {
                format!("leaving at least {GPU_FREE_HEADROOM_GB:.1} GB of it free")
            } else {
                format!("leaving at least {GPU_RESERVE_GB:.0} GB for the display")
            };
            return Recommendation::Starter {
                rung,
                mode: Mode::Gpu,
                context,
                need_gb: need,
                reason: format!(
                    "{}: {} needs about {need:.1} GB at {context} context, {left}.",
                    capitalize(&have),
                    rung.label
                ),
            };
        }
        gpu_note = format!(
            "{} — too little for {} ({:.1} GB at 32k), so it runs on the CPU. ",
            capitalize(&have),
            smallest.label,
            smallest.need_32k_gb
        );
    }
    let budget = memory.ram_available_gb - RAM_RESERVE_GB;
    match largest_fit(LADDER.iter().filter(|rung| rung.cpu_ok), budget) {
        Some((rung, context, need)) => Recommendation::Starter {
            rung,
            mode: Mode::Cpu,
            context,
            need_gb: need,
            reason: format!(
                "{gpu_note}{}{:.1} GB of {:.1} GB RAM available: {} needs about {need:.1} GB at {context} context, \
                 leaving {RAM_RESERVE_GB:.0} GB for the system.",
                if gpu_note.is_empty() { "No GPU found. " } else { "" },
                memory.ram_available_gb,
                memory.ram_total_gb,
                rung.label
            ),
        },
        None => Recommendation::NoFit {
            reason: format!(
                "{gpu_note}Only {:.1} GB of {:.1} GB RAM is available; the smallest starter, {}, needs about \
                 {:.1} GB at 32k context plus {RAM_RESERVE_GB:.0} GB for the system. Close other programs \
                 and try again, or add a smaller model yourself.",
                memory.ram_available_gb,
                memory.ram_total_gb,
                smallest.label,
                smallest.need_32k_gb
            ),
        },
    }
}

/// What the free reading leaves out, in words.
fn running_note(memory: Memory) -> String {
    if memory.running_on_gpu == 0 {
        return String::new();
    }
    format!(
        " ({} model server(s) running on the GPU hold part of the rest; \
         `localbox stop` stops LocalBox's for a reading without them)",
        memory.running_on_gpu
    )
}

/// Note the model servers that may hold GPU memory now. Their share is not
/// credited back: the GPU does not report per-process use everywhere, and a
/// server may sit partly on the CPU, on another device or on a CPU build, so
/// any figure would be a guess. A server started with every layer on the CPU
/// (`-ngl 0`) holds no GPU memory and is left out.
#[must_use]
pub fn note_running(memory: Memory, running: &[crate::live::RunningModel]) -> Memory {
    let on_gpu = running
        .iter()
        .filter(|model| !runs_on_cpu(&model.argv))
        .count();
    Memory {
        running_on_gpu: u32::try_from(on_gpu).unwrap_or(u32::MAX),
        ..memory
    }
}

/// Whether a llama-server argv keeps every layer off the GPU.
fn runs_on_cpu(argv: &[String]) -> bool {
    argv.windows(2).any(|pair| {
        matches!(pair[0].as_str(), "-ngl" | "--n-gpu-layers" | "--gpu-layers") && pair[1] == "0"
    })
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// This machine's memory: total GPU memory from the catalog's `VRAMGB` setting
/// when the user set one, else from the GPU probe (0 when no GPU answers —
/// never the launcher's fallback figure, which would invent a GPU); total and
/// available RAM from the operating system.
#[must_use]
pub fn probe_memory(configured_vram_gb: Option<u64>) -> Memory {
    const GIB: f64 = 1_073_741_824.0;
    let mut system = sysinfo::System::new();
    system.refresh_memory();
    #[allow(clippy::cast_precision_loss)]
    Memory {
        vram_total_gb: gpu_memory_gb(configured_vram_gb, crate::exec::probe_vram_gb()),
        vram_free_gb: crate::exec::probe_vram_free_gb(),
        running_on_gpu: 0,
        ram_total_gb: system.total_memory() as f64 / GIB,
        ram_available_gb: system.available_memory() as f64 / GIB,
    }
}

/// A configured `VRAMGB` above zero wins; otherwise the probe, where 0 means
/// no GPU.
#[must_use]
pub fn gpu_memory_gb(configured: Option<u64>, probed: u32) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    configured
        .filter(|gb| *gb > 0)
        .map_or_else(|| f64::from(probed), |gb| gb as f64)
}

/// The catalog's `VRAMGB` setting, when `catalog_dir` has a readable catalog.
#[must_use]
pub fn configured_vram_gb(catalog_dir: &std::path::Path) -> Option<u64> {
    localbox_launcher::catalog::Catalog::load(catalog_dir)
        .ok()?
        .setting("VRAMGB")
        .and_then(serde_json::Value::as_u64)
}

/// The token count catalog model `key` configures for context key `context`.
#[must_use]
pub fn catalog_context_tokens(
    catalog_dir: &std::path::Path,
    key: &str,
    context: &str,
) -> Option<u64> {
    let raw = std::fs::read_to_string(catalog_dir.join("llm-models.json")).ok()?;
    let catalog: Value = serde_json::from_str(raw.trim_start_matches('\u{feff}')).ok()?;
    catalog
        .get("Models")?
        .get(key)?
        .get("Contexts")?
        .get(context)?
        .as_u64()
}

/// The `localbox` arguments that start an installed starter the way it was
/// sized: the quant that was downloaded, its context, and catalog defaults —
/// the settings the memory need was measured with, so a saved tune is skipped
/// on purpose — and for a CPU recommendation no layers on the GPU. An entry
/// whose context is missing or larger than the measured window gets the
/// limitation and the fix instead of a command that would run differently.
///
/// # Errors
/// The user-facing limitation when catalog entry `key` cannot honour the
/// measured launch.
pub fn start_plan(
    catalog_dir: &std::path::Path,
    key: &str,
    quant: &str,
    mode: Mode,
    context: &str,
) -> Result<Vec<String>, String> {
    let measured = measured_tokens(context);
    match catalog_context_tokens(catalog_dir, key, context) {
        None => {
            return Err(format!(
                "Your catalog entry '{key}' has no '{context}' context, so it cannot start the \
                 way it was sized. Add \"{context}\": {measured} under its Contexts in \
                 llm-models.json, then run `localbox starter` again for the start command."
            ))
        }
        Some(tokens) if tokens > measured => {
            return Err(format!(
                "Your catalog entry '{key}' sets '{context}' to {tokens} tokens, more than the \
                 {measured} its memory need was measured at, so it would not run the way it was \
                 sized. Set it to {measured} in llm-models.json, then run `localbox starter` \
                 again for the start command."
            ))
        }
        Some(_) => {}
    }
    let mut argv: Vec<String> = [
        "serve",
        key,
        "--quant",
        quant,
        "--context",
        context,
        "--no-auto-best",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    if mode == Mode::Cpu {
        argv.extend(
            ["--server-arg", "--n-gpu-layers", "--server-arg", "0"]
                .into_iter()
                .map(str::to_string),
        );
    }
    Ok(argv)
}

fn measured_tokens(context: &str) -> u64 {
    if context == "32k" {
        32_768
    } else {
        65_536
    }
}

/// One recommendation as text a caller can hand back with `--expect`:
/// `<repo>:<quant>:<mode>:<context>`.
#[must_use]
pub fn offer_id(rung: &Rung, mode: Mode, context: &str) -> String {
    format!("{}:{}:{}:{context}", rung.repo, rung.quant, mode.as_str())
}

/// The scriptable form of a recommendation (`localbox starter --json`).
#[must_use]
pub fn to_json(
    recommendation: &Recommendation,
    memory: Memory,
    memory_source: &str,
    size: Option<(f64, &str)>,
) -> Value {
    let memory = json!({
        "vram_total_gb": round1(memory.vram_total_gb),
        "vram_free_gb": memory.vram_free_gb.map(round1),
        "running_on_gpu": memory.running_on_gpu,
        "ram_total_gb": round1(memory.ram_total_gb),
        "ram_available_gb": round1(memory.ram_available_gb),
        "source": memory_source,
    });
    match recommendation {
        Recommendation::Starter {
            rung,
            mode,
            context,
            need_gb,
            reason,
        } => {
            let (size_gb, size_source) = size.unwrap_or((rung.size_gb, "nominal"));
            json!({
                "schema": "localbox-starter-v1",
                "status": "recommended",
                "model": rung.label,
                "repo": rung.repo,
                "quant": rung.quant,
                "offer": offer_id(rung, *mode, context),
                "projector": rung.projector,
                "mode": mode.as_str(),
                "context": context,
                "size_gb": round1(size_gb),
                "size_source": size_source,
                "need_gb": need_gb,
                "memory": memory,
                "reason": reason,
            })
        }
        Recommendation::NoFit { reason } => json!({
            "schema": "localbox-starter-v1",
            "status": "no_fit",
            "memory": memory,
            "reason": reason,
        }),
    }
}

fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn server(args: &[&str]) -> crate::live::RunningModel {
        crate::live::RunningModel {
            pid: 1,
            port: Some(8080),
            model: Some("Qwen3.5-9B-Q4_K_M.gguf".to_string()),
            path: None,
            argv: args.iter().map(ToString::to_string).collect(),
        }
    }

    #[test]
    fn running_servers_are_named_not_credited() {
        // The 9B runs; only 4 GB reads as free, and that reading decides.
        let busy = Memory {
            vram_free_gb: Some(4.0),
            ..memory(24.0, 64.0, 40.0)
        };
        let noted = note_running(
            busy,
            &[
                server(&["llama-server", "-c", "65536"]),
                server(&["llama-server", "-ngl", "0"]),
            ],
        );
        assert_eq!(noted.vram_free_gb, Some(4.0));
        assert_eq!(
            noted.running_on_gpu, 1,
            "a CPU-only server holds no GPU memory"
        );
        let Recommendation::Starter { rung, reason, .. } = recommend(noted) else {
            panic!("expected a starter");
        };
        assert!(rung.need_32k_gb <= 3.5, "{}", rung.label);
        assert!(
            reason.contains("1 model server(s) running on the GPU hold part of the rest"),
            "{reason}"
        );
    }

    #[test]
    fn free_gpu_memory_decides_when_it_is_measured() {
        // 24 GB in total, but other programs hold all but 4 GB of it.
        let busy = Memory {
            vram_free_gb: Some(4.0),
            ..memory(24.0, 64.0, 40.0)
        };
        let Recommendation::Starter {
            rung, mode, reason, ..
        } = recommend(busy)
        else {
            panic!("expected a starter");
        };
        assert_eq!(mode, Mode::Gpu);
        assert!(
            rung.need_32k_gb <= 4.0 - GPU_FREE_HEADROOM_GB,
            "{}",
            rung.label
        );
        assert!(
            reason.starts_with("4.0 GB of the GPU's 24 GB is free now"),
            "{reason}"
        );

        // Unmeasured: the fallback says it assumes.
        let Recommendation::Starter { reason, .. } = recommend(memory(24.0, 64.0, 40.0)) else {
            panic!("expected a starter");
        };
        assert!(
            reason.contains("could not be measured, so all but 1 GB is assumed free"),
            "{reason}"
        );

        // Too little free for any GPU rung: the CPU, with the measurement named.
        let full = Memory {
            vram_free_gb: Some(1.0),
            ..memory(24.0, 64.0, 40.0)
        };
        let Recommendation::Starter { mode, reason, .. } = recommend(full) else {
            panic!("expected a starter");
        };
        assert_eq!(mode, Mode::Cpu);
        assert!(
            reason.starts_with("1.0 GB of the GPU's 24 GB is free now"),
            "{reason}"
        );
    }

    fn memory(vram: f64, total: f64, available: f64) -> Memory {
        Memory {
            vram_total_gb: vram,
            vram_free_gb: None,
            running_on_gpu: 0,
            ram_total_gb: total,
            ram_available_gb: available,
        }
    }

    fn pick(m: Memory) -> (&'static str, Mode, &'static str) {
        match recommend(m) {
            Recommendation::Starter {
                rung,
                mode,
                context,
                ..
            } => (rung.label, mode, context),
            Recommendation::NoFit { reason } => panic!("no fit: {reason}"),
        }
    }

    #[test]
    fn the_ladder_is_ordered_and_each_context_step_shrinks_the_need() {
        for pair in LADDER.windows(2) {
            assert!(pair[0].need_32k_gb < pair[1].need_32k_gb);
        }
        for rung in LADDER {
            assert!(rung.need_32k_gb < rung.need_64k_gb, "{}", rung.label);
            assert!(rung.size_gb < rung.need_32k_gb, "{}", rung.label);
        }
    }

    #[test]
    fn a_gpu_gets_the_largest_rung_its_total_memory_holds() {
        assert_eq!(
            pick(memory(24.0, 64.0, 40.0)),
            ("Qwen3.5 9B", Mode::Gpu, "64k")
        );
        assert_eq!(
            pick(memory(12.0, 32.0, 20.0)),
            ("Qwen3.5 9B", Mode::Gpu, "64k")
        );
        assert_eq!(
            pick(memory(8.0, 16.0, 8.0)),
            ("Qwen3.5 9B", Mode::Gpu, "64k")
        );
        assert_eq!(
            pick(memory(6.0, 16.0, 8.0)),
            ("Qwen3.5 4B", Mode::Gpu, "64k")
        );
        assert_eq!(
            pick(memory(4.0, 8.0, 4.0)),
            ("Qwen3.5 2B", Mode::Gpu, "64k")
        );
    }

    #[test]
    fn a_tight_gpu_steps_down_to_32k_before_a_smaller_model() {
        // 7 GB: 9B needs 6.7 at 64k, budget is 6.0 -> 9B at 32k.
        assert_eq!(
            pick(memory(7.0, 16.0, 8.0)),
            ("Qwen3.5 9B", Mode::Gpu, "32k")
        );
        // 3 GB: budget 2.0 holds the 2B only at 32k.
        assert_eq!(
            pick(memory(3.0, 16.0, 8.0)),
            ("Qwen3.5 2B", Mode::Gpu, "32k")
        );
    }

    #[test]
    fn a_gpu_too_small_for_any_rung_falls_back_to_the_cpu_and_says_why() {
        match recommend(memory(2.0, 16.0, 12.0)) {
            Recommendation::Starter {
                rung, mode, reason, ..
            } => {
                assert_eq!((rung.label, mode), ("Qwen3.5 4B", Mode::Cpu));
                assert!(reason.contains("too little"), "{reason}");
                assert!(reason.contains("2 GB in total"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn without_a_gpu_available_ram_decides_and_the_cpu_tops_out_at_4b() {
        assert_eq!(
            pick(memory(0.0, 32.0, 24.0)),
            ("Qwen3.5 4B", Mode::Cpu, "64k")
        );
        assert_eq!(
            pick(memory(0.0, 16.0, 6.0)),
            ("Qwen3.5 4B", Mode::Cpu, "32k")
        );
        assert_eq!(
            pick(memory(0.0, 16.0, 4.5)),
            ("Qwen3.5 2B", Mode::Cpu, "64k")
        );
        assert_eq!(
            pick(memory(0.0, 8.0, 4.0)),
            ("Qwen3.5 2B", Mode::Cpu, "32k")
        );
    }

    #[test]
    fn total_ram_alone_never_makes_room() {
        // Plenty installed, little free: available memory decides.
        match recommend(memory(0.0, 32.0, 3.0)) {
            Recommendation::NoFit { reason } => {
                assert!(reason.contains("3.0 GB of 32.0 GB"), "{reason}");
                assert!(reason.contains("Qwen3.5 2B"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn below_the_smallest_need_there_is_no_recommendation_not_a_forced_2b() {
        assert!(matches!(
            recommend(memory(0.0, 4.0, 2.5)),
            Recommendation::NoFit { .. }
        ));
        assert!(matches!(
            recommend(memory(2.0, 4.0, 2.5)),
            Recommendation::NoFit { .. }
        ));
    }

    #[test]
    fn the_reason_names_the_number_that_decided() {
        let Recommendation::Starter { reason, .. } = recommend(memory(24.0, 64.0, 40.0)) else {
            panic!()
        };
        assert!(reason.contains("24 GB in total"), "{reason}");
        let Recommendation::Starter { reason, .. } = recommend(memory(0.0, 16.0, 9.5)) else {
            panic!()
        };
        assert!(
            reason.contains("9.5 GB of 16.0 GB RAM available"),
            "{reason}"
        );
        assert!(reason.starts_with("No GPU found."), "{reason}");
    }

    fn catalog_with(contexts: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("llm-models.json"),
            format!(r#"{{"Models":{{"starter":{{"Contexts":{contexts}}}}}}}"#),
        )
        .unwrap();
        dir
    }

    #[test]
    fn the_start_command_carries_the_context_and_keeps_a_cpu_starter_off_the_gpu() {
        let dir = catalog_with(r#"{"64k":65536,"32k":32768}"#);
        let gpu = start_plan(dir.path(), "starter", "q4km", Mode::Gpu, "64k").unwrap();
        assert_eq!(
            gpu,
            [
                "serve",
                "starter",
                "--quant",
                "q4km",
                "--context",
                "64k",
                "--no-auto-best"
            ]
        );
        let cpu = start_plan(dir.path(), "starter", "q4km", Mode::Cpu, "32k").unwrap();
        assert!(cpu.ends_with(&[
            "--server-arg".to_string(),
            "--n-gpu-layers".to_string(),
            "--server-arg".to_string(),
            "0".to_string()
        ]));
    }

    #[test]
    fn an_entry_without_the_sized_context_gets_the_fix_not_a_command() {
        let dir = catalog_with(r#"{"8k":8192}"#);
        let limitation = start_plan(dir.path(), "starter", "q4km", Mode::Gpu, "64k").unwrap_err();
        assert!(limitation.contains("no '64k' context"), "{limitation}");
        assert!(limitation.contains("\"64k\": 65536"), "{limitation}");
    }

    #[test]
    fn a_context_key_pointing_at_a_larger_window_is_refused() {
        let dir = catalog_with(r#"{"64k":131072}"#);
        let limitation = start_plan(dir.path(), "starter", "q4km", Mode::Gpu, "64k").unwrap_err();
        assert!(limitation.contains("131072 tokens"), "{limitation}");
        let smaller = catalog_with(r#"{"64k":49152}"#);
        assert!(start_plan(smaller.path(), "starter", "q4km", Mode::Gpu, "64k").is_ok());
    }

    #[test]
    fn a_configured_gpu_size_wins_and_no_probe_answer_means_no_gpu() {
        assert!((gpu_memory_gb(Some(8), 24) - 8.0).abs() < f64::EPSILON);
        assert!((gpu_memory_gb(Some(0), 24) - 24.0).abs() < f64::EPSILON);
        assert!(gpu_memory_gb(None, 0).abs() < f64::EPSILON);
    }

    #[test]
    fn the_json_carries_mode_and_context_for_the_launch() {
        let m = memory(2.0, 16.0, 12.0);
        let value = to_json(&recommend(m), m, "override", Some((2.6, "hub")));
        assert_eq!(value["schema"], "localbox-starter-v1");
        assert_eq!(value["status"], "recommended");
        assert_eq!(value["mode"], "cpu");
        assert_eq!(value["context"], "64k");
        assert_eq!(value["size_source"], "hub");
        assert_eq!(value["memory"]["source"], "override");
        let none = memory(0.0, 4.0, 1.0);
        let value = to_json(&recommend(none), none, "probe", None);
        assert_eq!(value["status"], "no_fit");
        assert!(value.get("repo").is_none());
    }
}
