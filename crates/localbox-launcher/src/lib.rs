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

pub mod catalog;
pub mod launcher;
