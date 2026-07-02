//! Pins the launcher's version envelope to the committed wire fixture.
//!
//! The fixture (`tests/fixtures/launcher-envelope.json`) is the envelope a
//! benchmark consumer gates on before trusting this launcher. Consumers run
//! their own gate against this exact file in their CI, so any envelope change
//! must land here first — and a release bump regenerates the `version` field
//! in the same commit (the fixture is part of the release parity surface).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;

use localbox_launcher::catalog::Catalog;
use localbox_launcher::launcher::LlamaLauncher;
use localx_llama_core::Launcher;

const FIXTURE: &str = include_str!("fixtures/launcher-envelope.json");

fn manifest_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn the_live_envelope_matches_the_committed_wire_fixture() {
    let repo_root = manifest_dir().join("../..");
    let version = fs::read_to_string(repo_root.join("VERSION"))
        .expect("VERSION file")
        .trim()
        .to_string();
    let catalog = Catalog::load(&repo_root.join("local-llm")).expect("catalog loads");
    let launcher = LlamaLauncher::new(catalog, version.clone(), repo_root.join("home"), 24);

    let live = serde_json::to_value(launcher.version()).expect("envelope serializes");
    let pinned: serde_json::Value = serde_json::from_str(FIXTURE).expect("fixture parses");

    assert_eq!(
        pinned["version"], version,
        "the fixture version must track the VERSION file (regenerate the fixture with the release bump)"
    );
    assert_eq!(
        live, pinned,
        "the live envelope drifted from the committed wire fixture consumers gate on"
    );
}
