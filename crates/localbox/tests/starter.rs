//! End-to-end: `localbox starter` sizes, reports and installs the starter model
//! against a loopback Hub (`LOCALBOX_HF_ENDPOINT`) in a throwaway home, with
//! the GPU size pinned through `VRAMGB` so the host's hardware does not matter.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;

const FILE: &str = "Qwen3.5-9B-Q4_K_M.gguf";
const BODY: &[u8] = b"starter-gguf-bytes";

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A loopback Hub listing one quant with the given published digest.
fn spawn_hub(published_sha256: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let listing = format!(
        r#"{{"siblings":[{{"rfilename":"{FILE}","size":{},"lfs":{{"sha256":"{published_sha256}","size":{}}}}},{{"rfilename":"mmproj-F16.gguf","size":4}},{{"rfilename":"mmproj-BF16.gguf","size":4}}]}}"#,
        BODY.len(),
        BODY.len()
    );
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
                .unwrap_or("")
                .to_string();
            let (status, body): (&str, Vec<u8>) = if path.contains("/api/models/") {
                ("200 OK", listing.as_bytes().to_vec())
            } else if path.ends_with(FILE) {
                ("200 OK", BODY.to_vec())
            } else {
                ("404 Not Found", b"not found".to_vec())
            };
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        }
    });
    format!("http://{addr}")
}

fn home_with_gpu(home: &Path, vram_gb: u64) -> PathBuf {
    let local_llm = home.join(".local-llm");
    std::fs::create_dir_all(&local_llm).unwrap();
    let settings = serde_json::json!({ "LlamaCppGgufRoot": home.join("gguf"), "VRAMGB": vram_gb });
    std::fs::write(local_llm.join("settings.json"), settings.to_string()).unwrap();
    std::fs::write(local_llm.join("llm-models.json"), "{\"Models\":{}}\n").unwrap();
    home.join("gguf")
}

fn starter(home: &Path, endpoint: &str, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_localbox"))
        .arg("starter")
        .args(args)
        .env("USERPROFILE", home)
        .env("HOME", home)
        .env("LOCALBOX_HF_ENDPOINT", endpoint)
        .current_dir(home)
        .output()
        .expect("run localbox starter")
}

fn find(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find(&path, name) {
                return Some(found);
            }
        } else if path.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(path);
        }
    }
    None
}

#[test]
fn the_recommendation_shows_its_size_and_downloads_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let gguf = home_with_gpu(temp.path(), 24);
    let hub = spawn_hub(sha256_hex(BODY));

    let out = starter(temp.path(), &hub, &["--json"]);

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["status"], "recommended");
    assert_eq!(value["model"], "Qwen3.5 9B");
    assert_eq!(value["mode"], "gpu");
    assert_eq!(value["context"], "64k");
    assert_eq!(value["size_source"], "hub");
    assert!(
        find(&gguf, FILE).is_none(),
        "nothing downloaded without --download"
    );
}

#[test]
fn download_puts_a_matching_file_in_place_and_reports_it_verified() {
    let temp = tempfile::tempdir().unwrap();
    let gguf = home_with_gpu(temp.path(), 24);
    let hub = spawn_hub(sha256_hex(BODY));

    let out = starter(temp.path(), &hub, &["--download", "--json"]);

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // stdout is the JSON object alone; status lines went to stderr.
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let path = find(&gguf, FILE).expect("downloaded");
    assert_eq!(std::fs::read(&path).unwrap(), BODY);
    assert_eq!(value["verified"].as_array().unwrap().len(), 1);
    assert_eq!(value["context_in_catalog"], true);

    // A re-run checks the file already on disk instead of fetching it again.
    let again = starter(temp.path(), &hub, &["--download"]);
    assert!(
        again.status.success(),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
    let text = String::from_utf8_lossy(&again.stdout);
    assert!(
        text.contains("checked against the published SHA-256"),
        "{text}"
    );
}

#[test]
fn download_discards_a_file_that_does_not_match_the_published_digest() {
    let temp = tempfile::tempdir().unwrap();
    let gguf = home_with_gpu(temp.path(), 24);
    let hub = spawn_hub(sha256_hex(b"what the hub says it should be"));

    let out = starter(temp.path(), &hub, &["--download"]);

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("SHA-256"), "{stderr}");
    assert!(
        find(&gguf, FILE).is_none(),
        "a mismatching file is not put in place"
    );
    assert!(
        find(&gguf, &format!("{FILE}.partial")).is_none(),
        "and cannot be resumed"
    );
}

#[test]
fn a_cpu_starter_gets_a_start_command_that_keeps_it_off_the_gpu() {
    let temp = tempfile::tempdir().unwrap();
    home_with_gpu(temp.path(), 0);
    let hub = spawn_hub(sha256_hex(BODY));

    let out = starter(
        temp.path(),
        &hub,
        &[
            "--download",
            "--json",
            "--vram-gb",
            "2",
            "--ram-gb",
            "16",
            "--ram-available-gb",
            "12",
        ],
    );

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["mode"], "cpu");
    let argv: Vec<&str> = value["serve_args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg.as_str().unwrap())
        .collect();
    assert_eq!(
        &argv[..2],
        &["serve", value["catalog_key"].as_str().unwrap()]
    );
    assert!(
        argv.windows(2).any(|pair| pair == ["--context", "64k"]),
        "{argv:?}"
    );
    assert!(argv.ends_with(&["--server-arg", "--n-gpu-layers", "--server-arg", "0"]));
}

#[test]
fn a_text_only_rerun_keeps_a_projector_the_user_already_chose() {
    let temp = tempfile::tempdir().unwrap();
    let gguf = home_with_gpu(temp.path(), 24);
    let hub = spawn_hub(sha256_hex(BODY));
    let chosen = Command::new(env!("CARGO_BIN_EXE_localbox"))
        .args([
            "download",
            "unsloth/Qwen3.5-9B-GGUF",
            "--quant",
            "Q4_K_M",
            "--mmproj",
            "mmproj-BF16.gguf",
        ])
        .env("USERPROFILE", temp.path())
        .env("HOME", temp.path())
        .env("LOCALBOX_HF_ENDPOINT", &hub)
        .current_dir(temp.path())
        .output()
        .unwrap();
    assert!(
        chosen.status.success(),
        "{}",
        String::from_utf8_lossy(&chosen.stderr)
    );

    let out = starter(temp.path(), &hub, &["--download"]);

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let catalog =
        std::fs::read_to_string(temp.path().join(".local-llm").join("llm-models.json")).unwrap();
    assert!(catalog.contains("mmproj-BF16.gguf"), "{catalog}");
    assert!(!catalog.contains("mmproj-F16.gguf"), "{catalog}");
    assert!(find(&gguf, FILE).is_some());
}

#[test]
fn a_failed_json_download_still_prints_one_machine_readable_object() {
    let temp = tempfile::tempdir().unwrap();
    home_with_gpu(temp.path(), 24);
    let hub = spawn_hub(sha256_hex(b"what the hub says it should be"));

    let out = starter(temp.path(), &hub, &["--download", "--json"]);

    assert!(!out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["status"], "error");
    assert_eq!(value["stage"], "download");
    assert!(
        value["error"].as_str().unwrap().contains("SHA-256"),
        "{value}"
    );
}

#[test]
fn a_json_setup_error_is_still_one_object() {
    let temp = tempfile::tempdir().unwrap();
    home_with_gpu(temp.path(), 24);
    let hub = spawn_hub(sha256_hex(BODY));

    let out = starter(temp.path(), &hub, &["--json", "--vram-gb", "lots"]);

    assert!(!out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["status"], "error");
    assert_eq!(value["stage"], "setup");
    assert!(
        value["error"].as_str().unwrap().contains("--vram-gb"),
        "{value}"
    );
}

#[test]
fn too_little_memory_is_a_no_fit_with_numbers_not_a_forced_model() {
    let temp = tempfile::tempdir().unwrap();
    home_with_gpu(temp.path(), 0);
    let hub = spawn_hub(sha256_hex(BODY));

    let out = starter(
        temp.path(),
        &hub,
        &[
            "--json",
            "--vram-gb",
            "0",
            "--ram-gb",
            "4",
            "--ram-available-gb",
            "2",
        ],
    );

    assert!(!out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["status"], "no_fit");
    assert!(value["reason"]
        .as_str()
        .unwrap()
        .contains("2.0 GB of 4.0 GB"));
}
