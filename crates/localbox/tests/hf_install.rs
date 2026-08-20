//! End-to-end: `localbox download <owner/repo>` installs a model straight from a
//! Hugging Face repo and is idempotent. The real binary runs against a loopback
//! Hub (via `LOCALBOX_HF_ENDPOINT`) in a throwaway home, so the whole flow —
//! listing, quant pick, catalog write, download — is exercised with no network.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;

/// A loopback stand-in for the Hub: answers every `/api/models/...` with the
/// given siblings JSON and every `/resolve/main/...` with the GGUF bytes.
fn spawn_hub(siblings_json: &'static str, gguf: Vec<u8>) -> String {
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
            let (ctype, body): (&str, Vec<u8>) = if path.contains("/api/models/") {
                ("application/json", siblings_json.as_bytes().to_vec())
            } else if path.contains("/resolve/main/") {
                ("application/octet-stream", gguf.clone())
            } else {
                ("text/plain", b"not found".to_vec())
            };
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\
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
fn download_installs_from_a_hugging_face_repo_and_is_idempotent() {
    let temp = tempfile::tempdir().expect("temp home");
    let home = temp.path();
    // Pin the GGUF root inside the throwaway home (absolute, cross-platform).
    let local_llm = home.join(".local-llm");
    std::fs::create_dir_all(&local_llm).unwrap();
    let settings = serde_json::json!({ "LlamaCppGgufRoot": home.join("gguf") });
    std::fs::write(
        local_llm.join("settings.json"),
        serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();

    let gguf_bytes = b"hello-gguf\n".to_vec(); // 11 bytes
    let siblings = r#"{"siblings":[
        {"rfilename":"README.md","size":3},
        {"rfilename":"tiny.Q4_K_M.gguf","size":11}
    ]}"#;
    let base = spawn_hub(siblings, gguf_bytes.clone());

    let run = || {
        Command::new(env!("CARGO_BIN_EXE_localbox"))
            .args(["download", "owner/tiny"])
            .env("USERPROFILE", home)
            .env("HOME", home)
            .env("LOCALBOX_HF_ENDPOINT", &base)
            .current_dir(home)
            .output()
            .expect("run localbox")
    };

    // First install: writes the catalog entry and fetches the file.
    let first = run();
    let out = String::from_utf8_lossy(&first.stdout);
    assert!(
        first.status.success(),
        "first run failed: {}\n{out}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(out.contains("Added"), "expected an install message: {out}");

    let catalog = std::fs::read_to_string(local_llm.join("llm-models.json")).unwrap();
    assert!(
        catalog.contains("owner/tiny"),
        "catalog missing the repo: {catalog}"
    );

    let downloaded = find_file(&home.join("gguf"), "tiny.Q4_K_M.gguf")
        .expect("the GGUF was downloaded somewhere under the gguf root");
    assert_eq!(std::fs::read(&downloaded).unwrap(), gguf_bytes);

    // Second install: the repo is already catalogued and the file is on disk.
    let second = run();
    let out2 = String::from_utf8_lossy(&second.stdout);
    assert!(second.status.success(), "second run failed: {out2}");
    assert!(
        out2.contains("already in your catalog"),
        "expected an idempotent message: {out2}"
    );
    assert_eq!(std::fs::read(&downloaded).unwrap(), gguf_bytes);
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
