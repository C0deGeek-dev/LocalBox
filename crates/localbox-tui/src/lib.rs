//! The friendly LocalBox launcher UI core.
//!
//! - [`vocab`] — the plain-language vocabulary: friendly words over the
//!   technical launch fields, the glossary, and the recommended-plan summary
//!   (the no-jargon contract is a test).
//! - [`plan`] — the guided launch plan and its resolution precedence
//!   (explicit > saved DefaultLaunch cross-model preferences > per-model
//!   definition > hard defaults; quant/context stay per-model), plus the
//!   `.llm-default` workspace override walk-up.

#![forbid(unsafe_code)]

//! - [`ui`] — the backend-agnostic widgets (model picker, plan summary,
//!   confirm menu) with `TestBackend` snapshot tests and the fit-aware
//!   traffic-light colors.

pub mod plan;
pub mod ui;
pub mod vocab;
