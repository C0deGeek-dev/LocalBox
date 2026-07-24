//! llama.cpp binary install/update: pin-verified prebuilt release assets for
//! native and turboquant, and a staleness check for the source-built
//! mtpturbo fork.
//!
//! Cross-platform posture: prebuilt assets are selected per OS and verified
//! against SHA-256 pins in settings; there is no package-manager or
//! source-build path — where no asset fits, the answer is a clear
//! bring-your-own `llama-server` message.

use std::path::{Path, PathBuf};
use std::process::Command;

use localbox_launcher::catalog::Catalog;
use localx_llama_runtime::download::{
    build_stamp_is_stale, check_download_pin, cuda_major_order, is_arm64_asset, is_x64_asset,
    PinOutcome,
};

/// A named release asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    /// Upstream-reported SHA-256 (lowercase hex), when the release API
    /// carries one. A cross-check for freshly recorded pins — the local pin
    /// table stays the install-time authority.
    pub digest: Option<String>,
}

/// A resolved release: the tag and its assets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub assets: Vec<Asset>,
}

/// The GPU/CPU flavor of a native llama.cpp build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Cuda,
    Vulkan,
    Cpu,
}

impl Variant {
    /// The stamp spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cuda => "cuda",
            Self::Vulkan => "vulkan",
            Self::Cpu => "cpu",
        }
    }
}

/// The OS token release asset names carry for this platform.
#[must_use]
pub fn os_asset_token() -> &'static str {
    if cfg!(windows) {
        "win"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "ubuntu"
    }
}

/// Whether an asset name fits this host's architecture.
#[must_use]
pub fn arch_matches(name: &str) -> bool {
    if cfg!(target_arch = "aarch64") {
        is_arm64_asset(name)
    } else {
        is_x64_asset(name) || !is_arm64_asset(name)
    }
}

/// Whether the asset is an archive this updater can unpack.
#[must_use]
pub fn is_archive(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".zip") || lower.ends_with(".tar.gz") || lower.ends_with(".tar.xz")
}

/// Parse the driver's CUDA major from old and new `nvidia-smi` banners
/// (`CUDA Version: 13.1` or `CUDA UMD Version: 13.3` → 13).
#[must_use]
pub fn parse_cuda_driver_major(output: &str) -> Option<u32> {
    ["CUDA UMD Version:", "CUDA Version:"]
        .into_iter()
        .find_map(|label| {
            let start = output.find(label)?;
            output[start + label.len()..]
                .trim_start()
                .split(['.', ' ', '\n', '\r'])
                .next()?
                .parse()
                .ok()
        })
}

/// Select the native llama.cpp asset for this OS and variant. CUDA tries the
/// driver's major first (a mismatched-major build floods garbage instead of
/// erroring), `cudart` runtime bundles are never the pick, and CPU accepts
/// the upstream's evolving `-avx2-`/`-cpu-` spellings.
#[must_use]
pub fn select_native_asset<'a>(
    names: &[&'a str],
    variant: Variant,
    driver_major: Option<u32>,
) -> Option<&'a str> {
    let os = os_asset_token();
    let eligible: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| {
            let lower = n.to_ascii_lowercase();
            is_archive(n) && lower.contains(os) && arch_matches(n) && !lower.contains("cudart")
        })
        .collect();
    // macOS ships one Metal build per architecture — no CUDA/CPU split and no
    // `-avx2-`/`-cpu-` token — so the arch-filtered candidate is the pick.
    // Without this, the CPU token scan below finds nothing and the updater
    // falls through to a bring-your-own message on every Mac.
    if os == "macos" {
        return eligible.first().copied();
    }
    match variant {
        Variant::Cuda => {
            let majors: Vec<u32> = [13, 12, 11].into();
            for major in cuda_major_order(driver_major.unwrap_or(0), &majors) {
                let token = format!("-cuda-{major}");
                if let Some(hit) = eligible
                    .iter()
                    .find(|n| n.to_ascii_lowercase().contains(&token))
                {
                    return Some(hit);
                }
            }
            eligible
                .iter()
                .find(|n| n.to_ascii_lowercase().contains("-cuda"))
                .copied()
        }
        Variant::Vulkan => eligible
            .iter()
            .find(|n| n.to_ascii_lowercase().contains("-vulkan"))
            .copied(),
        Variant::Cpu => {
            for token in ["-avx2-", "-avx512-", "-avx-", "-noavx-", "-cpu-"] {
                if let Some(hit) = eligible
                    .iter()
                    .find(|n| n.to_ascii_lowercase().contains(token))
                {
                    return Some(hit);
                }
            }
            None
        }
    }
}

/// Select the turboquant fork's Windows CUDA asset; also reports a
/// plain-language warning when the chosen asset's CUDA major does not match
/// the driver (that pairing emits garbage output rather than an error).
#[must_use]
pub fn select_turbo_asset<'a>(
    names: &[&'a str],
    driver_major: Option<u32>,
) -> (Option<&'a str>, Option<String>) {
    let eligible: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| {
            let lower = n.to_ascii_lowercase();
            lower.ends_with(".zip") && lower.contains("windows") && lower.contains("cuda")
        })
        .collect();
    let majors: Vec<u32> = [13, 12, 11].into();
    let driver = driver_major.unwrap_or(0);
    for major in cuda_major_order(driver, &majors) {
        let dashed = format!("cuda-{major}");
        let plain = format!("cuda{major}");
        if let Some(hit) = eligible.iter().find(|n| {
            let lower = n.to_ascii_lowercase();
            lower.contains(&dashed) || lower.contains(&plain)
        }) {
            let warning = (driver != 0 && major != driver).then(|| {
                format!(
                    "the chosen build targets CUDA {major} but the driver reports CUDA \
                     {driver}; a mismatched build can emit garbage output"
                )
            });
            return (Some(hit), warning);
        }
    }
    (eligible.first().copied(), None)
}

/// Select the PrismML assets required by this host. Windows CUDA needs both
/// the fork binaries and the separately packaged CUDA runtime DLLs; Apple
/// Silicon uses the standard Metal archive (not the CPU-focused KleidiAI one).
pub fn select_prism_assets<'a>(
    names: &[&'a str],
    driver_major: Option<u32>,
) -> Result<Vec<&'a str>, String> {
    if cfg!(windows) {
        if !cfg!(target_arch = "x86_64") {
            return Err("the Prism engine currently supports Windows x64 only".to_string());
        }
        if driver_major.is_none() {
            return Err("the Prism engine requires an NVIDIA CUDA driver on Windows".to_string());
        }
        if driver_major.is_some_and(|major| major < 12) {
            return Err("the Prism Windows build requires a CUDA 12-compatible driver".to_string());
        }
        let binary = names
            .iter()
            .copied()
            .find(|name| {
                let lower = name.to_ascii_lowercase();
                lower.ends_with("-bin-win-cuda-12.4-x64.zip") && !lower.starts_with("cudart-")
            })
            .ok_or("the Prism release has no Windows x64 CUDA 12.4 binary")?;
        let runtime = names
            .iter()
            .copied()
            .find(|name| name.eq_ignore_ascii_case("cudart-llama-bin-win-cuda-12.4-x64.zip"))
            .ok_or("the Prism release has no Windows CUDA 12.4 runtime bundle")?;
        return Ok(vec![binary, runtime]);
    }
    if cfg!(target_os = "macos") {
        if !cfg!(target_arch = "aarch64") {
            return Err("the Prism engine currently supports Apple Silicon only".to_string());
        }
        let metal = names
            .iter()
            .copied()
            .find(|name| {
                name.to_ascii_lowercase()
                    .ends_with("-bin-macos-arm64.tar.gz")
            })
            .ok_or("the Prism release has no macOS Apple Silicon Metal archive")?;
        return Ok(vec![metal]);
    }
    Err("the Prism engine currently supports only Windows CUDA and Apple Silicon Metal".to_string())
}

/// Read a build stamp's first line (the installed release tag), when present.
#[must_use]
pub fn read_stamp_tag(root: &Path) -> Option<String> {
    std::fs::read_to_string(root.join(".build-stamp"))
        .ok()?
        .lines()
        .next()
        .map(str::to_string)
}

/// Write the two-line build stamp (release tag, then variant).
pub fn write_stamp(root: &Path, tag: &str, variant: &str) -> std::io::Result<()> {
    std::fs::write(root.join(".build-stamp"), format!("{tag}\n{variant}\n"))
}

/// The short source SHA recorded in an mtpturbo stamp
/// (`mtpturbo-<sha>-...`), when the stamp has that shape.
#[must_use]
pub fn mtp_stamp_sha(stamp_first_line: &str) -> Option<&str> {
    let rest = stamp_first_line.strip_prefix("mtpturbo-")?;
    let sha: &str = rest.split('-').next()?;
    (!sha.is_empty() && sha.chars().all(|c| c.is_ascii_hexdigit())).then_some(sha)
}

/// Resolve a GitHub release (the pinned tag when set, else latest).
///
/// # Errors
/// A plain message when the API cannot be reached or answers unexpectedly.
pub async fn fetch_release(repo: &str, tag: Option<&str>) -> Result<Release, String> {
    let url = match tag.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => format!("https://api.github.com/repos/{repo}/releases/tags/{t}"),
        None => format!("https://api.github.com/repos/{repo}/releases/latest"),
    };
    let client = reqwest::Client::new();
    let value: serde_json::Value = client
        .get(&url)
        .header(reqwest::header::USER_AGENT, "localbox")
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("release lookup failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("release lookup returned no JSON: {e}"))?;
    let tag = value["tag_name"]
        .as_str()
        .ok_or_else(|| format!("no release found at {url}"))?
        .to_string();
    let assets = value["assets"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|a| {
                    Some(Asset {
                        name: a["name"].as_str()?.to_string(),
                        url: a["browser_download_url"].as_str()?.to_string(),
                        digest: a["digest"].as_str().and_then(parse_github_digest),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Release { tag, assets })
}

/// The lowercase hex from a GitHub `digest` field (`sha256:<hex>`); other
/// algorithms are ignored rather than mistrusted as SHA-256.
#[must_use]
pub fn parse_github_digest(digest: &str) -> Option<String> {
    digest
        .strip_prefix("sha256:")
        .map(|hex| hex.trim().to_ascii_lowercase())
        .filter(|hex| hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Download an asset, apply the pin posture, and unpack it into `root`.
/// Returns the SHA-256 of the installed bytes so a pin-refresh can record it.
///
/// Without a local pin, the upstream release digest (when present) is the
/// integrity check: a mismatch refuses the install rather than recording a
/// hash of unknown bytes.
///
/// # Errors
/// A plain message on download, verification, or extraction failure.
pub async fn install_asset(
    asset: &Asset,
    root: &Path,
    pin: Option<&str>,
    require_pins: bool,
) -> Result<String, String> {
    let client = reqwest::Client::new();
    eprintln!("Downloading {} ...", asset.name);
    let bytes = client
        .get(&asset.url)
        .header(reqwest::header::USER_AGENT, "localbox")
        .timeout(std::time::Duration::from_secs(600))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .bytes()
        .await
        .map_err(|e| e.to_string())?;

    let computed = match check_download_pin(&bytes, pin, require_pins).map_err(|e| e.to_string())? {
        PinOutcome::Verified => localx_llama_runtime::download::sha256_hex(&bytes),
        PinOutcome::Unpinned { computed } => {
            if let Some(digest) = asset.digest.as_deref() {
                if !computed.eq_ignore_ascii_case(digest) {
                    return Err(format!(
                        "{}: downloaded bytes (sha256={computed}) do not match the \
                         upstream release digest ({digest}); refusing to install or pin them",
                        asset.name
                    ));
                }
            }
            eprintln!("  Downloaded {} sha256={computed} (unpinned).", asset.name);
            eprintln!(
                "  To pin it, add \"{}\": \"{computed}\" under LlamaCppDownloadPins in settings.json.",
                asset.name
            );
            computed
        }
    };

    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let archive = root.join(&asset.name);
    std::fs::write(&archive, &bytes).map_err(|e| e.to_string())?;
    // bsdtar ships with Windows 10+ and unpacks zip as well as tar archives,
    // so one extraction path serves every OS with no archive dependency.
    let status = Command::new("tar")
        .arg("-xf")
        .arg(&archive)
        .arg("-C")
        .arg(root)
        .status()
        .map_err(|e| format!("could not run tar: {e}"))?;
    let _ = std::fs::remove_file(&archive);
    if !status.success() {
        return Err(format!("extracting {} failed ({status})", asset.name));
    }
    flatten_extracted(root);
    #[cfg(unix)]
    set_unix_exec_bits(root);
    Ok(computed)
}

/// Ensure the extracted `llama-*` binaries are executable. `.zip` assets do not
/// carry the Unix exec bit reliably, so a fresh macOS/Linux download can land
/// unrunnable; `.tar.*` usually preserves it, but re-asserting is harmless.
#[cfg(unix)]
fn set_unix_exec_bits(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_binary = path.is_file()
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("llama-"));
        if !is_binary {
            continue;
        }
        if let Ok(meta) = std::fs::metadata(&path) {
            let mut perms = meta.permissions();
            perms.set_mode(perms.mode() | 0o111);
            let _ = std::fs::set_permissions(&path, perms);
        }
    }
}

/// If the server binary landed in a nested folder (`build/bin`, a versioned
/// top dir), move that folder's contents up to `root`.
pub fn flatten_extracted(root: &Path) {
    let exe = localx_llama_runtime::server::server_exe_name();
    if root.join(exe).is_file() {
        return;
    }
    let Some(found) = find_file(root, exe, 3) else {
        return;
    };
    let Some(source) = found.parent() else {
        return;
    };
    if let Ok(entries) = std::fs::read_dir(source) {
        for entry in entries.flatten() {
            let target = root.join(entry.file_name());
            let _ = std::fs::rename(entry.path(), target);
        }
    }
}

fn find_file(dir: &Path, name: &str, depth: u8) -> Option<PathBuf> {
    if depth == 0 {
        return None;
    }
    let entries = std::fs::read_dir(dir).ok()?;
    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && entry.file_name().to_string_lossy() == name {
            return Some(path);
        }
        if path.is_dir() {
            subdirs.push(path);
        }
    }
    subdirs
        .into_iter()
        .find_map(|sub| find_file(&sub, name, depth - 1))
}

/// The pin for an asset name from the `LlamaCppDownloadPins` settings map.
#[must_use]
pub fn pin_for(catalog: &Catalog, asset_name: &str) -> Option<String> {
    catalog
        .setting("LlamaCppDownloadPins")?
        .as_object()?
        .get(asset_name)?
        .as_str()
        .map(str::to_string)
}

/// What `localbox update` decided for one mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdatePlan {
    /// The installed build already matches the wanted tag.
    UpToDate { tag: String },
    /// Install these assets (fresh install or stale stamp).
    Install {
        release: Release,
        assets: Vec<Asset>,
    },
    /// mtpturbo staleness verdict (source-built; no prebuilt asset exists).
    MtpStatus { message: String },
}

/// Decide the update plan for a downloadable engine mode.
///
/// # Errors
/// A plain message when the release lookup fails or no asset fits this host.
/// The native-mode build variant for this host: CUDA when an NVIDIA driver is
/// present, Vulkan when an AMD GPU is present (and no NVIDIA driver), else CPU.
/// Wiring AMD → Vulkan stops AMD hosts silently falling back to a CPU build
/// while the GPU banner names their card.
#[must_use]
pub fn native_variant(driver_major: Option<u32>, amd_gpu: bool) -> Variant {
    if driver_major.is_some() {
        Variant::Cuda
    } else if amd_gpu {
        Variant::Vulkan
    } else {
        Variant::Cpu
    }
}

/// The settings key holding a mode's pinned release tag (`None` for the
/// source-built mtpturbo, which has no downloadable release).
#[must_use]
pub fn pinned_tag_setting_key(mode: localx_llama_core::Mode) -> Option<&'static str> {
    use localx_llama_core::Mode;
    match mode {
        Mode::Native => Some("LlamaCppPinnedTag"),
        Mode::Turboquant => Some("LlamaCppTurboquantPinnedTag"),
        Mode::PrismMl => Some("LlamaCppPrismPinnedTag"),
        Mode::Mtpturbo => None,
    }
}

/// The GitHub repo and configured pinned tag a mode's releases come from
/// (`None` for mtpturbo — see [`pinned_tag_setting_key`]).
#[must_use]
pub fn mode_release_source(
    catalog: &Catalog,
    mode: localx_llama_core::Mode,
) -> Option<(String, Option<String>)> {
    use localx_llama_core::Mode;
    let repo = match mode {
        Mode::Native => "ggerganov/llama.cpp".to_string(),
        Mode::Turboquant => catalog
            .setting_str("LlamaCppTurboquantRepo")
            .unwrap_or("C0deGeek-dev/llama-cpp-turboquant")
            .to_string(),
        Mode::PrismMl => catalog
            .setting_str("LlamaCppPrismRepo")
            .unwrap_or("PrismML-Eng/llama.cpp")
            .to_string(),
        Mode::Mtpturbo => return None,
    };
    let pinned = pinned_tag_setting_key(mode)
        .and_then(|key| catalog.setting_str(key))
        .map(str::to_string);
    Some((repo, pinned))
}

/// Whether a configured pin lags the latest upstream release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinFreshness {
    /// The pinned tag is the latest release.
    Current,
    /// The latest release differs from the pin.
    Behind {
        /// The configured pinned tag.
        pinned: String,
        /// The upstream latest tag.
        latest: String,
    },
}

/// Compare a pinned tag against the latest release tag.
#[must_use]
pub fn pin_freshness(pinned: &str, latest: &str) -> PinFreshness {
    if pinned.trim() == latest.trim() {
        PinFreshness::Current
    } else {
        PinFreshness::Behind {
            pinned: pinned.trim().to_string(),
            latest: latest.trim().to_string(),
        }
    }
}

/// Merge a refreshed pin set into a settings layer: set the mode's pinned-tag
/// key and upsert each asset hash under `LlamaCppDownloadPins`, leaving every
/// unrelated key untouched. Pure — the caller owns the file write.
#[must_use]
pub fn refreshed_settings(
    existing: &serde_json::Map<String, serde_json::Value>,
    tag_key: &str,
    tag: &str,
    pins: &[(String, String)],
) -> serde_json::Map<String, serde_json::Value> {
    let mut merged = existing.clone();
    merged.insert(
        tag_key.to_string(),
        serde_json::Value::String(tag.to_string()),
    );
    let mut table = merged
        .get("LlamaCppDownloadPins")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    for (asset, sha) in pins {
        table.insert(
            asset.clone(),
            serde_json::Value::String(sha.to_ascii_lowercase()),
        );
    }
    merged.insert(
        "LlamaCppDownloadPins".to_string(),
        serde_json::Value::Object(table),
    );
    merged
}

pub async fn plan_binary_update(
    catalog: &Catalog,
    mode: localx_llama_core::Mode,
    root: &Path,
    driver_major: Option<u32>,
    amd_gpu: bool,
) -> Result<UpdatePlan, String> {
    let Some((repo, pinned_tag)) = mode_release_source(catalog, mode) else {
        return Ok(UpdatePlan::MtpStatus {
            message: mtp_status(catalog, root),
        });
    };
    let release = fetch_release(&repo, pinned_tag.as_deref()).await?;

    if let Some(installed) = read_stamp_tag(root) {
        if !build_stamp_is_stale(&installed, &release.tag) {
            return Ok(UpdatePlan::UpToDate { tag: release.tag });
        }
    }

    let assets = select_release_assets(&release, mode, driver_major, amd_gpu)?;
    Ok(UpdatePlan::Install { release, assets })
}

/// Select this host's install set from a resolved release (shared by the
/// pinned update path and the pin-refresh path).
///
/// # Errors
/// A plain message when no asset fits this host.
pub fn select_release_assets(
    release: &Release,
    mode: localx_llama_core::Mode,
    driver_major: Option<u32>,
    amd_gpu: bool,
) -> Result<Vec<Asset>, String> {
    use localx_llama_core::Mode;
    let names: Vec<&str> = release.assets.iter().map(|a| a.name.as_str()).collect();
    let picked: Vec<&str> = match mode {
        Mode::Native => {
            let variant = native_variant(driver_major, amd_gpu);
            select_native_asset(&names, variant, driver_major)
                .or_else(|| select_native_asset(&names, Variant::Cpu, None))
                .into_iter()
                .collect()
        }
        Mode::Turboquant => {
            let (choice, warning) = select_turbo_asset(&names, driver_major);
            if let Some(warning) = warning {
                eprintln!("Warning: {warning}");
            }
            choice.into_iter().collect()
        }
        Mode::Mtpturbo => Vec::new(),
        Mode::PrismMl => select_prism_assets(&names, driver_major)?,
    };
    if picked.is_empty() {
        return Err(format!(
            "release {} has no prebuilt asset for this platform; provide your own \
             llama-server (bring-your-own) or pin a different tag",
            release.tag
        ));
    }
    picked
        .iter()
        .map(|name| {
            release
                .assets
                .iter()
                .find(|asset| asset.name == *name)
                .cloned()
                .ok_or_else(|| "selected asset vanished from the release listing".to_string())
        })
        .collect()
}

/// Resolve the **latest** release for a mode and select this host's assets —
/// the read-only half of a pin refresh.
///
/// # Errors
/// A plain message for the mtpturbo mode (source-built, nothing to refresh),
/// an unreachable release API, or a release with no asset for this host.
pub async fn plan_refresh(
    catalog: &Catalog,
    mode: localx_llama_core::Mode,
    driver_major: Option<u32>,
    amd_gpu: bool,
) -> Result<(Release, Vec<Asset>), String> {
    let Some((repo, _pinned)) = mode_release_source(catalog, mode) else {
        return Err(
            "mtpturbo is source-built and has no release pins to refresh; see \
             `localbox update --mode mtpturbo --check`"
                .to_string(),
        );
    };
    let release = fetch_release(&repo, None).await?;
    let assets = select_release_assets(&release, mode, driver_major, amd_gpu)?;
    Ok((release, assets))
}

fn mtp_status(catalog: &Catalog, root: &Path) -> String {
    let repo = catalog
        .setting_str("LlamaCppMtpTurboRepo")
        .unwrap_or("EsmaeelNabil/llama.cpp");
    let branch = catalog
        .setting_str("LlamaCppMtpTurboBranch")
        .unwrap_or("feat/mtp-turboquant-kv-cache");
    let installed = read_stamp_tag(root);
    let installed_sha = installed.as_deref().and_then(mtp_stamp_sha);

    let remote = Command::new("git")
        .args([
            "ls-remote",
            &format!("https://github.com/{repo}.git"),
            branch,
        ])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| {
            String::from_utf8_lossy(&out.stdout)
                .split_whitespace()
                .next()
                .map(|sha| sha.chars().take(7).collect::<String>())
        });

    match (installed_sha, remote) {
        (Some(have), Some(want))
            if want.starts_with(have) || have.starts_with(&want[..have.len().min(want.len())]) =>
        {
            format!("mtpturbo is current (source {have} matches {repo}@{branch}).")
        }
        (Some(have), Some(want)) => format!(
            "mtpturbo is stale: installed source {have}, {repo}@{branch} is at {want}. \
             The mtpturbo fork ships no prebuilt binaries — rebuild it from source, or \
             keep using the installed build."
        ),
        (None, _) => format!(
            "mtpturbo is not installed. It is a source-built fork ({repo}@{branch}) with \
             no prebuilt binaries — build it from source, or use the native/turboquant modes."
        ),
        (_, None) => "could not reach the mtpturbo repository to compare versions.".to_string(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn cuda_driver_major_parses_from_nvidia_smi_banner() {
        let legacy = "| NVIDIA-SMI 591.74  Driver Version: 591.74  CUDA Version: 13.1 |";
        let current = "| NVIDIA-SMI 610.62  KMD Version: 610.62  CUDA UMD Version: 13.3 |";
        assert_eq!(parse_cuda_driver_major(legacy), Some(13));
        assert_eq!(parse_cuda_driver_major(current), Some(13));
        assert_eq!(parse_cuda_driver_major("no gpu here"), None);
    }

    #[test]
    fn native_variant_prefers_cuda_then_vulkan_then_cpu() {
        // NVIDIA driver present → CUDA, regardless of an AMD card.
        assert_eq!(native_variant(Some(13), false), Variant::Cuda);
        assert_eq!(native_variant(Some(13), true), Variant::Cuda);
        // No NVIDIA driver but an AMD GPU → Vulkan (was silently CPU before).
        assert_eq!(native_variant(None, true), Variant::Vulkan);
        // Neither → CPU.
        assert_eq!(native_variant(None, false), Variant::Cpu);
    }

    #[cfg(windows)]
    #[test]
    fn native_asset_selection_prefers_driver_major_and_skips_cudart() {
        let names = [
            "llama-b100-bin-win-cudart-12.4-x64.zip",
            "llama-b100-bin-win-cuda-12.4-x64.zip",
            "llama-b100-bin-win-cuda-13.1-x64.zip",
            "llama-b100-bin-win-avx2-x64.zip",
            "llama-b100-bin-win-cuda-12.4-arm64.zip",
            "llama-b100-bin-ubuntu-cuda-13.1-x64.zip",
        ];
        // Driver major 12 → the 12.x build wins even though 13 exists.
        assert_eq!(
            select_native_asset(&names, Variant::Cuda, Some(12)),
            Some("llama-b100-bin-win-cuda-12.4-x64.zip")
        );
        // Driver major 13 → the 13.x build; cudart and arm64 never match.
        assert_eq!(
            select_native_asset(&names, Variant::Cuda, Some(13)),
            Some("llama-b100-bin-win-cuda-13.1-x64.zip")
        );
        assert_eq!(
            select_native_asset(&names, Variant::Cpu, None),
            Some("llama-b100-bin-win-avx2-x64.zip")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_asset_selection_picks_the_metal_build_on_macos() {
        // The real b9596 asset list: macOS carries no `-cpu-`/`-avx2-` token,
        // so selection must still resolve the arch-matched Metal build rather
        // than falling through to a bring-your-own message.
        let names = [
            "llama-b9596-bin-macos-arm64.tar.gz",
            "llama-b9596-bin-macos-x64.tar.gz",
            "llama-b9596-bin-ubuntu-x64.tar.gz",
            "llama-b9596-bin-win-cpu-x64.zip",
        ];
        let picked = select_native_asset(&names, Variant::Cpu, None).unwrap();
        assert!(picked.starts_with("llama-b9596-bin-macos-"));
        let arch = if cfg!(target_arch = "aarch64") {
            "arm64"
        } else {
            "x64"
        };
        assert!(picked.contains(arch));
    }

    #[test]
    fn turbo_asset_selection_warns_on_cuda_major_mismatch() {
        let names = ["tqp-v0.2.0-windows-cuda12.4.zip"];
        let (asset, warning) = select_turbo_asset(&names, Some(13));
        assert_eq!(asset, Some("tqp-v0.2.0-windows-cuda12.4.zip"));
        let warning = warning.unwrap();
        assert!(warning.contains("garbage output"));

        // A matching major carries no warning.
        let (asset, warning) = select_turbo_asset(&names, Some(12));
        assert!(asset.is_some());
        assert!(warning.is_none());
    }

    #[cfg(windows)]
    #[test]
    fn prism_windows_selection_includes_binary_and_cuda_runtime() {
        let names = [
            "llama-prism-b9591-62061f9-bin-macos-arm64-kleidiai.tar.gz",
            "llama-prism-b9591-62061f9-bin-macos-arm64.tar.gz",
            "llama-prism-b1-62061f9-bin-win-cuda-12.4-x64.zip",
            "cudart-llama-bin-win-cuda-12.4-x64.zip",
        ];
        let picked = select_prism_assets(&names, Some(13)).unwrap();
        assert_eq!(
            picked,
            vec![
                "llama-prism-b1-62061f9-bin-win-cuda-12.4-x64.zip",
                "cudart-llama-bin-win-cuda-12.4-x64.zip"
            ]
        );
        assert!(select_prism_assets(&names, None).is_err());
        assert!(select_prism_assets(&names, Some(11)).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn prism_macos_selection_prefers_standard_metal_over_kleidiai() {
        let names = [
            "llama-prism-b9591-62061f9-bin-macos-arm64-kleidiai.tar.gz",
            "llama-prism-b9591-62061f9-bin-macos-arm64.tar.gz",
        ];
        assert_eq!(
            select_prism_assets(&names, None).unwrap(),
            vec!["llama-prism-b9591-62061f9-bin-macos-arm64.tar.gz"]
        );
    }

    #[test]
    fn github_digests_parse_only_wellformed_sha256() {
        let hex = "6d109e2930c0eaf2f729c3a6fc58dd7809ce2ba7047bfb294547cc389af6de5d";
        assert_eq!(
            parse_github_digest(&format!("sha256:{}", hex.to_uppercase())).as_deref(),
            Some(hex)
        );
        // Other algorithms and malformed hex are ignored, never mistaken for SHA-256.
        assert_eq!(parse_github_digest("sha512:abcdef"), None);
        assert_eq!(parse_github_digest("sha256:tooshort"), None);
        assert_eq!(parse_github_digest(hex), None);
    }

    #[test]
    fn pin_freshness_compares_trimmed_tags() {
        assert_eq!(
            pin_freshness("prism-b9596-9fcaed7", "prism-b9596-9fcaed7\n"),
            PinFreshness::Current
        );
        assert_eq!(
            pin_freshness("prism-b9591-62061f9", "prism-b9596-9fcaed7"),
            PinFreshness::Behind {
                pinned: "prism-b9591-62061f9".into(),
                latest: "prism-b9596-9fcaed7".into(),
            }
        );
    }

    #[test]
    fn refreshed_settings_upserts_pins_and_preserves_unrelated_keys() {
        let existing: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
            r#"{
                "NoThinkProxyPort": 11435,
                "LlamaCppDownloadPins": { "old-asset.zip": "aa", "shared.zip": "bb" }
            }"#,
        )
        .unwrap();
        let merged = refreshed_settings(
            &existing,
            "LlamaCppPrismPinnedTag",
            "prism-b9596-9fcaed7",
            &[
                ("new-asset.zip".to_string(), "CC11".to_string()),
                ("shared.zip".to_string(), "dd22".to_string()),
            ],
        );
        // Unrelated settings survive untouched.
        assert_eq!(merged["NoThinkProxyPort"], 11435);
        // The tag key is set and hashes land lowercase; same-name pins update.
        assert_eq!(merged["LlamaCppPrismPinnedTag"], "prism-b9596-9fcaed7");
        let pins = merged["LlamaCppDownloadPins"].as_object().unwrap();
        assert_eq!(pins["old-asset.zip"], "aa");
        assert_eq!(pins["new-asset.zip"], "cc11");
        assert_eq!(pins["shared.zip"], "dd22");

        // A settings layer with no pin table gains one.
        let merged = refreshed_settings(
            &serde_json::Map::new(),
            "LlamaCppPinnedTag",
            "b9700",
            &[("a.zip".to_string(), "ee".to_string())],
        );
        assert_eq!(merged["LlamaCppDownloadPins"]["a.zip"], "ee");
    }

    #[test]
    fn build_stamps_round_trip_and_mtp_shas_parse() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_stamp_tag(dir.path()), None);
        write_stamp(dir.path(), "b4567", "cuda").unwrap();
        assert_eq!(read_stamp_tag(dir.path()).as_deref(), Some("b4567"));

        assert_eq!(mtp_stamp_sha("mtpturbo-a1b2c3d-cuda"), Some("a1b2c3d"));
        assert_eq!(mtp_stamp_sha("b4567"), None);
        assert_eq!(mtp_stamp_sha("mtpturbo-xyz-cuda"), None);
    }

    #[test]
    fn flatten_moves_a_nested_server_binary_up() {
        let dir = tempfile::tempdir().unwrap();
        let exe = localx_llama_runtime::server::server_exe_name();
        let nested = dir.path().join("build").join("bin");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join(exe), b"bin").unwrap();
        std::fs::write(nested.join("ggml.dll"), b"lib").unwrap();

        flatten_extracted(dir.path());
        assert!(dir.path().join(exe).is_file());
        assert!(dir.path().join("ggml.dll").is_file());
    }
}
