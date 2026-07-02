//! The LocalBox application layer: live wiring over the launcher library.
//!
//! - [`fetch`] — resumable, pin-aware HTTP download (Hugging Face GGUF pulls
//!   with `.partial` append-resume; llama.cpp release assets verify upstream).
//! - [`exec`] — process/socket effects: the live proxy-lifecycle ops, server
//!   spawning, the agent-environment guard, and interactive agent launch.
//! - [`live`] — launch orchestration: execute a resolved launch plan
//!   (download → server → proxy → smoke → agent), stop, and status.

#![forbid(unsafe_code)]

use localx_llama_core::{LauncherVersion, RUNTIME_LLAMACPP, TARGET_LOCALBOX};

pub mod exec;
pub mod fetch;
pub mod live;

/// The product version shipped with this build (the repo `VERSION` file).
#[must_use]
pub fn product_version() -> &'static str {
    include_str!("../../../VERSION").trim()
}

/// The version envelope this product presents on the launcher contract.
#[must_use]
pub fn product_envelope() -> LauncherVersion {
    LauncherVersion {
        version: product_version().to_string(),
        api_version: 1,
        launcher_export_version: 1,
        supported_targets: vec![TARGET_LOCALBOX.to_string()],
        supported_runtimes: vec![RUNTIME_LLAMACPP.to_string()],
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn the_product_envelope_matches_the_committed_contract_fixture() {
        let pinned: serde_json::Value = serde_json::from_str(include_str!(
            "../../localbox-launcher/tests/fixtures/launcher-envelope.json"
        ))
        .unwrap();
        assert_eq!(serde_json::to_value(product_envelope()).unwrap(), pinned);
    }
}
