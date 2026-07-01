//! LocalBox's implementation of the shared launcher contract.
//!
//! - [`catalog`] — the three-layer config load (`defaults.json` <
//!   `llm-models.json` catalog < per-machine `settings.json`) over the shared
//!   precedence engine, exposing the model map and the launcher's scalar
//!   settings.
//! - [`launcher`] — [`launcher::LlamaLauncher`], the `Launcher`-trait
//!   implementation a benchmark/tuner drives: model/quant/context resolution
//!   delegating to the shared domain crate, on-disk GGUF and vision-projector
//!   resolution, per-mode install roots and binary resolution, and the server
//!   lifecycle primitives.

#![forbid(unsafe_code)]

//! - [`proxy`] — no-think proxy lifecycle: the reap-before-probe /
//!   repoint-on-mismatch / kill-stale-listener orchestration over the shared
//!   tri-state target check, socket→PID resolution, and owned-vs-any teardown.

//! - [`env`] — the agent env envelope: one plan is both the DryRun snapshot
//!   and the live setter; save → mutate → finally-restore over a testable
//!   store seam.

pub mod catalog;
pub mod env;
pub mod launcher;
pub mod localpilot_config;
pub mod permissions;
pub mod proxy;
