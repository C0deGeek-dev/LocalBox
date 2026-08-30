//! End-to-end: `localbox download <owner/repo>` installs a model straight from a
//! Hugging Face repo and is idempotent. The real binary runs against a loopback
//! Hub (via `LOCALBOX_HF_ENDPOINT`) in a throwaway home, so the whole flow —
//! listing, quant pick, catalog write, download — is exercised with no network.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

/// A loopback stand-in for the Hub: answers every `/api/models/...` with the
/// given siblings JSON and resolves each named GGUF to its own bytes.
fn spawn_hub(siblings_json: &'static str, files: Vec<(String, Vec<u8>)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]);
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("");
            let (status, ctype, body): (&str, &str, Vec<u8>) = if path.contains("/api/models/") {
                (
                    "200 OK",
                    "application/json",
                    siblings_json.as_bytes().to_vec(),
                )
            } else if path.contains("/resolve/main/") {
                if let Some((_, body)) = files.iter().find(|(name, _)| path.ends_with(name)) {
                    ("200 OK", "application/octet-stream", body.clone())
                } else {
                    (
                        "404 Not Found",
                        "text/plain",
                        b"unknown model file".to_vec(),
                    )
                }
            } else {
                ("404 Not Found", "text/plain", b"not found".to_vec())
            };
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        }
    });
    format!("http://{addr}")
}

fn run_localbox(home: &Path, endpoint: &str, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_localbox"))
        .args(args)
        .env("USERPROFILE", home)
        .env("HOME", home)
        .env("LOCALBOX_HF_ENDPOINT", endpoint)
        .current_dir(home)
        .output()
        .expect("run localbox")
}

fn run_guided(home: &Path, endpoint: &str, input: &str) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_localbox"))
        .arg("--plain")
        .env("USERPROFILE", home)
        .env("HOME", home)
        .env("LOCALBOX_HF_ENDPOINT", endpoint)
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start guided localbox");
    child
        .stdin
        .take()
        .expect("guided stdin")
        .write_all(input.as_bytes())
        .expect("write guided choices");
    child.wait_with_output().expect("finish guided localbox")
}

fn configure_home(home: &Path) -> PathBuf {
    let local_llm = home.join(".local-llm");
    std::fs::create_dir_all(&local_llm).unwrap();
    let gguf_root = home.join("gguf");
    // Pin recommendation input so this behavior test is independent of the
    // developer or CI host's actual GPU.
    let settings = serde_json::json!({
        "LlamaCppGgufRoot": gguf_root,
        "VRAMGB": 24
    });
    std::fs::write(
        local_llm.join("settings.json"),
        serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();
    local_llm
}

fn seed_empty_catalog(local_llm: &Path) -> String {
    let original = "{\"Models\":{}}\n".to_string();
    std::fs::write(local_llm.join("llm-models.json"), &original).unwrap();
    original
}

/// Recursively search `dir` for a file named `name`.
fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_file(&path, name) {
                return Some(found);
            }
        } else if path.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(path);
        }
    }
    None
}

#[test]
fn download_catalogues_every_quant_but_fetches_only_the_selected_one() {
    let temp = tempfile::tempdir().expect("temp home");
    let home = temp.path();
    let local_llm = configure_home(home);

    let q4_bytes = b"q4-gguf!".to_vec();
    let iq4_bytes = b"iq4-gguf".to_vec();
    let q6_part_1 = b"q6-one".to_vec();
    let q6_part_2 = b"q6-two".to_vec();
    let siblings = r#"{"description":"<div>Useful &amp; friendly <b>tiny model</b></div><script>bad()</script>","siblings":[
        {"rfilename":"README.md","size":3},
        {"rfilename":"tiny.i1-IQ4_XS.gguf","size":8},
        {"rfilename":"tiny.i1-Q4_K_M.gguf","size":8},
        {"rfilename":"tiny.i1-Q6_K-00001-of-00002.gguf","size":6},
        {"rfilename":"tiny.i1-Q6_K-00002-of-00002.gguf","size":6}
    ]}"#;
    let base = spawn_hub(
        siblings,
        vec![
            ("tiny.i1-IQ4_XS.gguf".to_string(), iq4_bytes.clone()),
            ("tiny.i1-Q4_K_M.gguf".to_string(), q4_bytes.clone()),
            (
                "tiny.i1-Q6_K-00001-of-00002.gguf".to_string(),
                q6_part_1.clone(),
            ),
            (
                "tiny.i1-Q6_K-00002-of-00002.gguf".to_string(),
                q6_part_2.clone(),
            ),
        ],
    );

    // First install: every quant is selectable, but only Q4_K_M is fetched.
    let first = run_localbox(home, &base, &["download", "owner/tiny", "--quant", "q4km"]);
    let out = String::from_utf8_lossy(&first.stdout);
    assert!(
        first.status.success(),
        "first run failed: {}\n{out}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(out.contains("Added"), "expected an install message: {out}");

    assert!(
        out.contains("3 quant(s)"),
        "expected full catalog count: {out}"
    );

    let catalog: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(local_llm.join("llm-models.json")).unwrap())
            .unwrap();
    let model = &catalog["Models"]["tiny"];
    assert_eq!(model["Repo"], "owner/tiny");
    assert_eq!(model["Quant"], "i1-q4km");
    assert_eq!(model["Description"], "Useful & friendly tiny model");
    assert!(!model["Description"].as_str().unwrap().contains('<'));
    assert!(
        model.get("SourceType").is_none(),
        "an HF install must not write retired catalog metadata"
    );
    assert_eq!(model["Quants"].as_object().unwrap().len(), 3);
    assert!(model["Quants"].get("i1-iq4xs").is_some());
    assert!(model["Quants"].get("i1-q6k").is_some());

    let q4 = find_file(&home.join("gguf"), "tiny.i1-Q4_K_M.gguf")
        .expect("the selected GGUF was downloaded");
    assert_eq!(std::fs::read(&q4).unwrap(), q4_bytes);
    assert!(find_file(&home.join("gguf"), "tiny.i1-IQ4_XS.gguf").is_none());
    assert!(find_file(&home.join("gguf"), "tiny.i1-Q6_K-00001-of-00002.gguf").is_none());

    let info = run_localbox(home, &base, &["info", "tiny"]);
    let info_out = String::from_utf8_lossy(&info.stdout);
    assert!(info.status.success(), "info failed: {info_out}");
    assert!(info_out.contains("Useful & friendly tiny model"));
    assert!(!info_out.contains("<div>") && !info_out.contains("<script>"));
    for quant in ["i1-iq4xs", "i1-q4km", "i1-q6k"] {
        assert!(info_out.contains(quant), "info omitted {quant}: {info_out}");
    }

    // The ordinary catalog-key path downloads one alternate quant through the
    // shared launcher without re-listing or fetching its neighbors.
    let iq4 = run_localbox(home, &base, &["download", "tiny", "--quant", "i1-iq4xs"]);
    assert!(
        iq4.status.success(),
        "alternate download failed: {}",
        String::from_utf8_lossy(&iq4.stderr)
    );
    let iq4_path = find_file(&home.join("gguf"), "tiny.i1-IQ4_XS.gguf").unwrap();
    assert_eq!(std::fs::read(iq4_path).unwrap(), iq4_bytes);
    assert!(find_file(&home.join("gguf"), "tiny.i1-Q6_K-00001-of-00002.gguf").is_none());

    // Selecting the multipart quant through the same catalog key fetches every
    // shard and no other variant.
    let q6 = run_localbox(home, &base, &["download", "tiny", "--quant", "i1-q6k"]);
    assert!(
        q6.status.success(),
        "multipart download failed: {}",
        String::from_utf8_lossy(&q6.stderr)
    );
    let part_1 = find_file(&home.join("gguf"), "tiny.i1-Q6_K-00001-of-00002.gguf").unwrap();
    let part_2 = find_file(&home.join("gguf"), "tiny.i1-Q6_K-00002-of-00002.gguf").unwrap();
    assert_eq!(std::fs::read(part_1).unwrap(), q6_part_1);
    assert_eq!(std::fs::read(part_2).unwrap(), q6_part_2);

    // A same-repo rerun is now a no-op catalog merge and a present-file skip.
    let second = run_localbox(home, &base, &["download", "owner/tiny", "--quant", "q4km"]);
    let out2 = String::from_utf8_lossy(&second.stdout);
    assert!(second.status.success(), "second run failed: {out2}");
    assert!(
        out2.contains("already complete in your catalog"),
        "expected an idempotent message: {out2}"
    );
    assert_eq!(std::fs::read(&q4).unwrap(), q4_bytes);
}

#[test]
fn a_same_repo_run_backfills_a_legacy_one_quant_entry_without_clobbering_it() {
    let temp = tempfile::tempdir().expect("temp home");
    let home = temp.path();
    let local_llm = configure_home(home);
    let original = serde_json::json!({
        "Models": {
            "legacy": {
                "Repo": "owner/tiny",
                "Root": "custom-root",
                "Quants": {
                    "i1-q4km": {
                        "File": "tiny.i1-Q4_K_M.gguf",
                        "Note": "keep this user note"
                    }
                },
                "Quant": "i1-q4km",
                "Contexts": {"": 65536},
                "CustomField": true
            }
        }
    });
    std::fs::write(
        local_llm.join("llm-models.json"),
        serde_json::to_string_pretty(&original).unwrap(),
    )
    .unwrap();

    let siblings = r#"{"siblings":[
        {"rfilename":"tiny.i1-IQ4_XS.gguf","size":8},
        {"rfilename":"tiny.i1-Q4_K_M.gguf","size":8},
        {"rfilename":"tiny.i1-Q6_K.gguf","size":7}
    ]}"#;
    let base = spawn_hub(
        siblings,
        vec![("tiny.i1-Q4_K_M.gguf".to_string(), b"q4-gguf!".to_vec())],
    );

    let run = run_localbox(home, &base, &["download", "owner/tiny", "--quant", "q4km"]);
    let out = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success(),
        "repair failed: {}\n{out}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(out.contains("with 2 missing quant(s): i1-iq4xs, i1-q6k"));

    let catalog: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(local_llm.join("llm-models.json")).unwrap())
            .unwrap();
    let model = &catalog["Models"]["legacy"];
    assert_eq!(model["Root"], "custom-root");
    assert_eq!(model["Quant"], "i1-q4km");
    assert_eq!(model["Contexts"][""], 65536);
    assert_eq!(model["CustomField"], true);
    assert_eq!(model["Quants"]["i1-q4km"]["Note"], "keep this user note");
    assert_eq!(model["Quants"].as_object().unwrap().len(), 3);
    assert!(find_file(&home.join("gguf"), "tiny.i1-Q4_K_M.gguf").is_some());
    assert!(find_file(&home.join("gguf"), "tiny.i1-IQ4_XS.gguf").is_none());
}

#[test]
fn guided_add_can_register_only_cancel_cleanly_or_download_one_explicit_quant() {
    const SIBLINGS: &str = r#"{"siblings":[
        {"rfilename":"tiny.i1-IQ4_XS.gguf","size":7},
        {"rfilename":"tiny.i1-Q4_K_M.gguf","size":8},
        {"rfilename":"tiny.i1-Q6_K.gguf","size":12}
    ]}"#;
    let files = || {
        vec![
            ("tiny.i1-IQ4_XS.gguf".to_string(), b"iq4tiny".to_vec()),
            ("tiny.i1-Q4_K_M.gguf".to_string(), b"q4-tiny!".to_vec()),
            ("tiny.i1-Q6_K.gguf".to_string(), b"q6-is-bigger".to_vec()),
        ]
    };

    // Empty catalog root menu: 1 = Add. Quant menu: 4 = Register only.
    let register_temp = tempfile::tempdir().expect("register home");
    let register_home = register_temp.path();
    let register_catalog = configure_home(register_home);
    seed_empty_catalog(&register_catalog);
    let register_hub = spawn_hub(SIBLINGS, files());
    let registered = run_guided(register_home, &register_hub, "1\nowner/tiny\n4\n\n");
    assert!(
        registered.status.success(),
        "register-only failed: {}",
        String::from_utf8_lossy(&registered.stderr)
    );
    let registered_out = String::from_utf8_lossy(&registered.stdout);
    assert!(registered_out.contains("[recommended]"), "{registered_out}");
    assert!(
        registered_out.contains("No model files were downloaded"),
        "{registered_out}"
    );
    let catalog: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(register_catalog.join("llm-models.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(catalog["Models"]["tiny"]["Quant"], "i1-q4km");
    assert_eq!(
        catalog["Models"]["tiny"]["Quants"]
            .as_object()
            .unwrap()
            .len(),
        3
    );
    assert!(find_file(&register_home.join("gguf"), "tiny.i1-IQ4_XS.gguf").is_none());
    assert!(find_file(&register_home.join("gguf"), "tiny.i1-Q4_K_M.gguf").is_none());
    assert!(find_file(&register_home.join("gguf"), "tiny.i1-Q6_K.gguf").is_none());

    // Blank/back at the quant menu cancels. Neither catalog bytes nor model
    // files change (the rich path's Esc reaches the same None outcome).
    let cancel_temp = tempfile::tempdir().expect("cancel home");
    let cancel_home = cancel_temp.path();
    let cancel_catalog = configure_home(cancel_home);
    let original = seed_empty_catalog(&cancel_catalog);
    let cancel_hub = spawn_hub(SIBLINGS, files());
    let cancelled = run_guided(cancel_home, &cancel_hub, "1\nowner/tiny\n\n");
    assert!(cancelled.status.success());
    assert_eq!(
        std::fs::read_to_string(cancel_catalog.join("llm-models.json")).unwrap(),
        original
    );
    assert!(!cancel_home.join("gguf").exists());

    // 2 = explicit i1-q4km selection. All variants register, only Q4 downloads.
    let download_temp = tempfile::tempdir().expect("download home");
    let download_home = download_temp.path();
    let download_catalog = configure_home(download_home);
    seed_empty_catalog(&download_catalog);
    let download_hub = spawn_hub(SIBLINGS, files());
    let downloaded = run_guided(download_home, &download_hub, "1\nowner/tiny\n2\n\n");
    assert!(
        downloaded.status.success(),
        "guided download failed: {}",
        String::from_utf8_lossy(&downloaded.stderr)
    );
    let catalog: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(download_catalog.join("llm-models.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(catalog["Models"]["tiny"]["Quant"], "i1-q4km");
    assert_eq!(
        catalog["Models"]["tiny"]["Quants"]
            .as_object()
            .unwrap()
            .len(),
        3
    );
    assert!(find_file(&download_home.join("gguf"), "tiny.i1-IQ4_XS.gguf").is_none());
    assert_eq!(
        std::fs::read(find_file(&download_home.join("gguf"), "tiny.i1-Q4_K_M.gguf").unwrap())
            .unwrap(),
        b"q4-tiny!"
    );
    assert!(find_file(&download_home.join("gguf"), "tiny.i1-Q6_K.gguf").is_none());
}

#[test]
fn an_unknown_name_that_is_not_a_repo_still_errors_clearly() {
    let temp = tempfile::tempdir().expect("temp home");
    let out = Command::new(env!("CARGO_BIN_EXE_localbox"))
        .args(["download", "definitely-not-a-model"])
        .env("USERPROFILE", temp.path())
        .env("HOME", temp.path())
        .current_dir(temp.path())
        .output()
        .expect("run localbox");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("unknown model") && err.contains("Hugging Face repo id"),
        "expected the improved error: {err}"
    );
}
