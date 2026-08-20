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
//!
//! - [`fetch`] — the resumable model-file downloader and the download targets
//!   a catalog model implies (GGUF, vision projector, draft model), so a
//!   consumer that only holds the launcher contract can still put a model on
//!   disk instead of sending the user to another tool.
//!
//! - [`hf_meta`] — Hugging Face repo references and GGUF file listing over the
//!   Hub API: the read step that lets the download command install a model
//!   straight from an `owner/repo` reference not yet in the catalog.

pub mod catalog;

/// The product version shipped with this build (the repo `VERSION` file) —
/// the version the launcher contract's envelope reports.
#[must_use]
pub fn product_version() -> &'static str {
    include_str!("../../../VERSION").trim()
}

pub mod env;
pub mod fetch;
pub mod hf_meta;
pub mod launcher;
pub mod localpilot_config;
pub mod orchestrate;
pub mod permissions;
pub mod posture;
pub mod profile;
pub mod proxy;
pub mod smoke;
