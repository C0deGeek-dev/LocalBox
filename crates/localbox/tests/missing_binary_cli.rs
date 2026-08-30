#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;

#[test]
fn real_cli_flow_reports_the_exact_missing_binary_repair() {
    let home = tempfile::tempdir().expect("temporary home");
    let catalog_dir = home.path().join(".local-llm");
    let gguf_dir = catalog_dir.join("gguf").join("fixture");
    std::fs::create_dir_all(&gguf_dir).expect("fixture directories");
    std::fs::write(gguf_dir.join("fixture.gguf"), b"fixture").expect("fixture GGUF");
    let settings = serde_json::json!({
        "LlamaCppGgufRoot": catalog_dir.join("gguf"),
        "VRAMGB": 8
    });
    let models = serde_json::json!({
        "Models": {
            "fixture": {
                "Root": "fixture",
                "Repo": "owner/fixture",
                "File": "fixture.gguf",
                "Contexts": { "": 4096 }
            }
        }
    });
    std::fs::write(
        catalog_dir.join("settings.json"),
        serde_json::to_vec_pretty(&settings).expect("settings JSON"),
    )
    .expect("settings file");
    std::fs::write(
        catalog_dir.join("llm-models.json"),
        serde_json::to_vec_pretty(&models).expect("models JSON"),
    )
    .expect("models file");

    let mut command = Command::new(env!("CARGO_BIN_EXE_localbox"));
    command.args(["serve", "fixture", "--mode", "turboquant", "--no-auto-best"]);
    if cfg!(windows) {
        command.env("USERPROFILE", home.path());
    } else {
        command.env("HOME", home.path());
    }
    let output = command.output().expect("run localbox CLI");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "CLI unexpectedly succeeded");
    assert!(
        stderr.contains("localbox update --mode turboquant"),
        "stderr did not contain the repair command: {stderr}"
    );
}
