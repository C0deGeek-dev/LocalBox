//! The saved launch `localbox serve --again` replays.
//!
//! A successful `serve` records how it was asked to start, per proxy port, so
//! a caller that only knows "this model was set up here" (a coding agent after
//! a reboot) can start the same thing again unattended. The record keeps the
//! request, not the resolved llama-server arguments: a later tune or engine
//! update still applies to the replay. A serve that bypasses the proxy
//! (`--keep-thinking`) or repeats an option is not replayed. Only options that are safe to repeat
//! without a person present are kept; a serve that used anything else leaves
//! no record, so a replay never runs something different from what was asked.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The record format version.
pub const SCHEMA: u32 = 1;

/// How one successful `serve` was requested.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServeRecord {
    pub schema: u32,
    /// The catalog key the serve resolved.
    pub model: String,
    /// The proxy port the serve listened on.
    pub proxy_port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer_profile: Option<String>,
    #[serde(default)]
    pub auto_best: bool,
    #[serde(default)]
    pub no_auto_best: bool,
    #[serde(default)]
    pub allow_untuned: bool,
    #[serde(default)]
    pub vision: bool,
    #[serde(default)]
    pub draft: bool,
    /// The model runs on the CPU only (no layers on the GPU), as a starter
    /// sized for system memory asks.
    #[serde(default)]
    pub cpu_only: bool,
}

/// The raw llama-server arguments that mean "no layers on the GPU".
fn is_cpu_placement(values: &[&str]) -> bool {
    matches!(values, [flag, "0"] if matches!(*flag, "-ngl" | "--n-gpu-layers" | "--gpu-layers"))
}

impl ServeRecord {
    /// The record for a successful `serve` given `args` (the model argument
    /// first), or why the serve cannot be replayed unattended.
    ///
    /// # Errors
    /// The first option that makes the launch unsafe or impossible to repeat
    /// exactly.
    pub fn from_serve_args(args: &[String], model: &str, proxy_port: u16) -> Result<Self, String> {
        let mut record = Self {
            schema: SCHEMA,
            model: model.to_string(),
            proxy_port,
            context: None,
            mode: None,
            quant: None,
            prefer_profile: None,
            auto_best: false,
            no_auto_best: false,
            allow_untuned: false,
            vision: false,
            draft: false,
            cpu_only: false,
        };
        let mut server_args: Vec<&str> = Vec::new();
        let mut seen: Vec<&str> = Vec::new();
        let mut rest = args.iter().skip(1);
        while let Some(arg) = rest.next() {
            // `serve` reads the first of a repeated option; a replay must not
            // guess which one ran.
            if arg != "--server-arg" && arg.starts_with("--") {
                if seen.contains(&arg.as_str()) {
                    return Err(format!("it gave {arg} more than once"));
                }
                seen.push(arg.as_str());
            }
            let mut value = || {
                rest.next()
                    .map(String::as_str)
                    .ok_or_else(|| format!("{arg} has no value"))
            };
            match arg.as_str() {
                "--context" => record.context = Some(value()?.to_string()),
                "--mode" => record.mode = Some(value()?.to_string()),
                "--quant" => record.quant = Some(value()?.to_string()),
                "--prefer-profile" => record.prefer_profile = Some(value()?.to_string()),
                "--auto-best" => record.auto_best = true,
                "--no-auto-best" => record.no_auto_best = true,
                "--allow-untuned" => record.allow_untuned = true,
                "--vision" => record.vision = true,
                "--draft" => record.draft = true,
                "--server-arg" => server_args.push(value()?),
                // How this one call reports or settles a running model, not
                // what runs: nothing to repeat.
                "--json" | "--adopt-tune" => {}
                "--if-running" | "--agent" => {
                    value()?;
                }
                other => return Err(format!("it used {other}, which a replay does not repeat")),
            }
        }
        match server_args.as_slice() {
            [] => {}
            values if is_cpu_placement(values) => record.cpu_only = true,
            _ => {
                return Err(
                    "it passed raw llama-server arguments, which a replay does not repeat"
                        .to_string(),
                )
            }
        }
        Ok(record)
    }

    /// The `serve` arguments that request this launch again.
    #[must_use]
    pub fn serve_args(&self) -> Vec<String> {
        let mut args = vec![self.model.clone()];
        let mut value = |flag: &str, value: &Option<String>| {
            if let Some(value) = value {
                args.push(flag.to_string());
                args.push(value.clone());
            }
        };
        value("--context", &self.context);
        value("--mode", &self.mode);
        value("--quant", &self.quant);
        value("--prefer-profile", &self.prefer_profile);
        for (on, flag) in [
            (self.auto_best, "--auto-best"),
            (self.no_auto_best, "--no-auto-best"),
            (self.allow_untuned, "--allow-untuned"),
            (self.vision, "--vision"),
            (self.draft, "--draft"),
        ] {
            if on {
                args.push(flag.to_string());
            }
        }
        if self.cpu_only {
            args.extend(["--server-arg", "--n-gpu-layers", "--server-arg", "0"].map(String::from));
        }
        args
    }
}

/// Where the record for `proxy_port` lives in LocalBox's state directory.
#[must_use]
pub fn record_path(state_dir: &Path, proxy_port: u16) -> PathBuf {
    state_dir.join(format!("serve-again-{proxy_port}.json"))
}

/// The saved record for `proxy_port`, if there is one.
///
/// # Errors
/// A plain message when the file exists but cannot be read or is not a record
/// this build understands.
pub fn read(state_dir: &Path, proxy_port: u16) -> Result<Option<ServeRecord>, String> {
    let path = record_path(state_dir, proxy_port);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("could not read {}: {error}", path.display())),
    };
    let record: ServeRecord = serde_json::from_str(&raw).map_err(|error| {
        format!(
            "{} is not a saved launch this LocalBox reads: {error}",
            path.display()
        )
    })?;
    if record.schema != SCHEMA || record.proxy_port != proxy_port {
        return Err(format!(
            "{} is not a saved launch for port {proxy_port}",
            path.display()
        ));
    }
    Ok(Some(record))
}

/// Save `record` as the launch for its port, replacing an older one.
///
/// # Errors
/// The I/O error, as a plain message.
pub fn write(state_dir: &Path, record: &ServeRecord) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(record).map_err(|error| error.to_string())?;
    crate::fallback::write_atomically(&record_path(state_dir, record.proxy_port), &bytes)
}

/// Forget the launch saved for `proxy_port` (a later serve there could not be
/// replayed, so the older record no longer describes what runs).
///
/// # Errors
/// The I/O error, as a plain message; a missing record is not an error.
pub fn remove(state_dir: &Path, proxy_port: u16) -> Result<(), String> {
    match std::fs::remove_file(record_path(state_dir, proxy_port)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("could not remove the saved launch: {error}")),
    }
}

/// Run `work` while holding the replay lock for `proxy_port`. A second
/// replay for the same port waits here, then finds the model already serving.
/// The operating system releases the lock when the process ends.
///
/// # Errors
/// A plain message when the lock cannot be taken, or whatever `work` returns.
pub fn with_port_lock<T>(
    state_dir: &Path,
    proxy_port: u16,
    work: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    std::fs::create_dir_all(state_dir)
        .map_err(|e| format!("could not create {}: {e}", state_dir.display()))?;
    let path = state_dir.join(format!(".serve-again-{proxy_port}.lock"));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|e| format!("could not open the serve lock {}: {e}", path.display()))?;
    let mut lock = fd_lock::RwLock::new(file);
    let busy = matches!(lock.try_write(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock);
    if busy {
        eprintln!("Waiting for another LocalBox start on port {proxy_port} to finish …");
    }
    let _held = lock
        .write()
        .map_err(|e| format!("could not take the serve lock {}: {e}", path.display()))?;
    work()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn a_setup_serve_is_saved_and_replayed_with_the_same_request() {
        let given = args(&[
            "qwen3-5-9b-gguf",
            "--quant",
            "Q4_K_M",
            "--context",
            "64k",
            "--prefer-profile",
            "balanced",
            "--allow-untuned",
            "--json",
        ]);
        let record = ServeRecord::from_serve_args(&given, "qwen3-5-9b-gguf", 11_435).unwrap();
        assert_eq!(
            record.serve_args(),
            args(&[
                "qwen3-5-9b-gguf",
                "--context",
                "64k",
                "--quant",
                "Q4_K_M",
                "--prefer-profile",
                "balanced",
                "--allow-untuned",
            ])
        );
        assert!(!record.cpu_only);
    }

    #[test]
    fn a_cpu_starter_keeps_zero_gpu_layers_on_replay() {
        for flag in ["--n-gpu-layers", "-ngl", "--gpu-layers"] {
            let given = args(&[
                "qwen3-5-2b",
                "--server-arg",
                flag,
                "--server-arg",
                "0",
                "--prefer-profile",
                "balanced",
            ]);
            let record = ServeRecord::from_serve_args(&given, "qwen3-5-2b", 11_435).unwrap();
            assert!(record.cpu_only, "{flag}");
            let replay = record.serve_args();
            let tail: Vec<&str> = replay
                .iter()
                .rev()
                .take(4)
                .rev()
                .map(String::as_str)
                .collect();
            assert_eq!(
                tail,
                ["--server-arg", "--n-gpu-layers", "--server-arg", "0"]
            );
        }
    }

    #[test]
    fn a_serve_with_other_raw_or_network_options_is_not_replayable() {
        for extra in [
            &["--server-arg", "--spec-type", "--server-arg", "ngram-mod"][..],
            &["--server-arg", "-ngl", "--server-arg", "20"][..],
            &["--server-arg", "--host"][..],
            &["--lan"][..],
            &["--password", "x"][..],
            &["--allow-public-no-auth"][..],
            &["--dry-run"][..],
            &["--keep-thinking"][..],
            &["--quant", "q4km", "--quant", "q8"][..],
            &["--context", "32k", "--context", "64k"][..],
            &["--allow-untuned", "--allow-untuned"][..],
            &["--something-new"][..],
        ] {
            let mut given = args(&["m"]);
            given.extend(args(extra));
            assert!(
                ServeRecord::from_serve_args(&given, "m", 11_435).is_err(),
                "{extra:?} must not be saved"
            );
        }
    }

    #[test]
    fn records_are_per_port_and_survive_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(dir.path(), 11_435).unwrap(), None);
        let record =
            ServeRecord::from_serve_args(&args(&["m", "--context", "32k"]), "m", 11_435).unwrap();
        write(dir.path(), &record).unwrap();
        assert_eq!(read(dir.path(), 11_435).unwrap(), Some(record));
        assert_eq!(read(dir.path(), 11_436).unwrap(), None);
        // A file copied to another port's name is refused, not replayed there.
        std::fs::copy(
            record_path(dir.path(), 11_435),
            record_path(dir.path(), 11_436),
        )
        .unwrap();
        assert!(read(dir.path(), 11_436).is_err());
        remove(dir.path(), 11_435).unwrap();
        remove(dir.path(), 11_435).unwrap();
        assert_eq!(read(dir.path(), 11_435).unwrap(), None);
    }

    #[test]
    fn an_unknown_field_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            record_path(dir.path(), 11_435),
            r#"{"schema":1,"model":"m","proxyPort":11435,"serverArgs":["--host","0.0.0.0"]}"#,
        )
        .unwrap();
        assert!(read(dir.path(), 11_435).is_err());
    }

    #[test]
    fn the_port_lock_serialises_and_returns_the_work_result() {
        let dir = tempfile::tempdir().unwrap();
        let value = with_port_lock(dir.path(), 11_435, || Ok(7)).unwrap();
        assert_eq!(value, 7);
        let error = with_port_lock::<()>(dir.path(), 11_435, || Err("no".to_string()));
        assert_eq!(error, Err("no".to_string()));
    }
}
