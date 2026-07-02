//! The CPU-only embedding server: start (idempotent), health probe, stop.
//!
//! The embed model always runs with `-ngl 0` so it never takes VRAM from a
//! chat model running alongside it. State lives in a pidfile so a different
//! shell (or a later run) can stop what an earlier one started.

use std::path::{Path, PathBuf};

use localbox_launcher::catalog::Catalog;
use localbox_launcher::launcher::expand_path_with_home;
use serde::{Deserialize, Serialize};

use crate::exec::{kill_pid, os_listener_pids};
use crate::fetch::{download_with_resume, hf_download_url};

/// The resolved embed-serve settings (each with its shipped default).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedConfig {
    pub repo: String,
    pub file: String,
    pub root: String,
    pub pooling: String,
    pub port: u16,
}

impl EmbedConfig {
    /// Resolve from optional settings values, applying the shipped defaults.
    #[must_use]
    pub fn resolve(
        repo: Option<&str>,
        file: Option<&str>,
        root: Option<&str>,
        pooling: Option<&str>,
        port: Option<i64>,
    ) -> Self {
        let pick = |v: Option<&str>, d: &str| {
            v.map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(d)
                .to_string()
        };
        Self {
            repo: pick(repo, "Qwen/Qwen3-Embedding-0.6B-GGUF"),
            file: pick(file, "Qwen3-Embedding-0.6B-Q8_0.gguf"),
            root: pick(root, "qwen3-embedding-0.6b"),
            pooling: pick(pooling, "last"),
            port: port
                .and_then(|p| u16::try_from(p).ok())
                .filter(|p| *p > 0)
                .unwrap_or(8090),
        }
    }

    /// Resolve from the merged catalog settings.
    #[must_use]
    pub fn from_catalog(catalog: &Catalog) -> Self {
        Self::resolve(
            catalog.setting_str("EmbedModelRepo"),
            catalog.setting_str("EmbedModelFile"),
            catalog.setting_str("EmbedModelRoot"),
            catalog.setting_str("EmbedPooling"),
            catalog
                .setting("EmbedPort")
                .and_then(serde_json::Value::as_i64),
        )
    }

    /// The embed model's on-disk path under the GGUF root.
    #[must_use]
    pub fn model_path(&self, gguf_root: &Path) -> PathBuf {
        gguf_root.join(&self.root).join(&self.file)
    }
}

/// The cross-shell embed-server state (pidfile shape, PascalCase on disk).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct EmbedState {
    pub pid: Option<u32>,
    pub port: u16,
    pub base_url: String,
    pub model: String,
    pub pooling: String,
}

/// Where the embed pidfile lives.
#[must_use]
pub fn embed_state_path(home: &Path) -> PathBuf {
    home.join(".local-llm").join("embed-server.json")
}

/// Persist the embed state (best effort — serving matters more than the file).
pub fn write_embed_state(home: &Path, state: &EmbedState) {
    let path = embed_state_path(home);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(path, json);
    }
}

/// Read the embed state, when a pidfile exists and parses.
#[must_use]
pub fn read_embed_state(home: &Path) -> Option<EmbedState> {
    let raw = std::fs::read_to_string(embed_state_path(home)).ok()?;
    serde_json::from_str(raw.trim_start_matches('\u{feff}')).ok()
}

/// Probe `/v1/embeddings` and return the embedding dimension when healthy.
pub async fn probe_embeddings(port: u16) -> Option<usize> {
    let body = serde_json::json!({
        "model": "embed",
        "input": ["embedding server health probe"],
    });
    let client = reqwest::Client::new();
    let response = client
        .post(format!("http://127.0.0.1:{port}/v1/embeddings"))
        .json(&body)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .ok()?;
    let value: serde_json::Value = response.json().await.ok()?;
    let dims = value["data"][0]["embedding"].as_array()?.len();
    (dims > 0).then_some(dims)
}

/// Ensure the embed model file exists, downloading it on a miss.
///
/// # Errors
/// A plain message when the GGUF root is unknown or the download fails.
pub async fn ensure_embed_model(
    catalog: &Catalog,
    config: &EmbedConfig,
    home: &Path,
) -> Result<PathBuf, String> {
    let root = catalog
        .gguf_root()
        .ok_or("LlamaCppGgufRoot is not configured; set it in settings.json")?;
    let root = expand_path_with_home(&root.to_string_lossy(), home);
    let path = config.model_path(&root);
    if !path.is_file() {
        let url = hf_download_url(&config.repo, &config.file);
        eprintln!("Downloading embedding model ({url}) ...");
        let client = reqwest::Client::new();
        download_with_resume(&client, &url, &path)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(path)
}

/// Stop the embed server: the pidfile's PID first, then whatever listens on
/// its port; the pidfile is cleared. Returns whether anything was stopped.
#[must_use]
pub fn stop_embed(home: &Path) -> bool {
    let mut stopped = false;
    let state = read_embed_state(home);
    if let Some(state) = &state {
        if let Some(pid) = state.pid {
            kill_pid(pid);
            stopped = true;
        }
        for pid in os_listener_pids(state.port) {
            kill_pid(pid);
            stopped = true;
        }
    }
    let _ = std::fs::remove_file(embed_state_path(home));
    stopped
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn embed_settings_apply_the_shipped_defaults() {
        let config = EmbedConfig::resolve(None, Some("  "), None, None, None);
        assert_eq!(config.repo, "Qwen/Qwen3-Embedding-0.6B-GGUF");
        assert_eq!(config.file, "Qwen3-Embedding-0.6B-Q8_0.gguf");
        assert_eq!(config.root, "qwen3-embedding-0.6b");
        assert_eq!(config.pooling, "last");
        assert_eq!(config.port, 8090);

        let custom = EmbedConfig::resolve(
            Some("o/r"),
            Some("f.gguf"),
            Some("r"),
            Some("mean"),
            Some(9001),
        );
        assert_eq!(custom.port, 9001);
        assert_eq!(custom.pooling, "mean");
        // An unusable port value falls back rather than binding port 0.
        assert_eq!(
            EmbedConfig::resolve(None, None, None, None, Some(0)).port,
            8090
        );
        assert_eq!(
            EmbedConfig::resolve(None, None, None, None, Some(700_000)).port,
            8090
        );
    }

    #[test]
    fn embed_state_round_trips_the_pascal_case_pidfile_shape() {
        let state = EmbedState {
            pid: Some(4242),
            port: 8090,
            base_url: "http://127.0.0.1:8090".to_string(),
            model: "m.gguf".to_string(),
            pooling: "last".to_string(),
        };
        let value = serde_json::to_value(&state).unwrap();
        // The on-disk keys stay PascalCase — the shape older shells wrote.
        assert!(value.get("Pid").is_some());
        assert!(value.get("BaseUrl").is_some());
        let back: EmbedState = serde_json::from_value(value).unwrap();
        assert_eq!(back.pid, Some(4242));
    }
}
