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

/// Parse the driver's CUDA major from `nvidia-smi` output
/// (`CUDA Version: 13.1` → 13).
#[must_use]
pub fn parse_cuda_driver_major(output: &str) -> Option<u32> {
    let start = output.find("CUDA Version:")?;
    output[start + "CUDA Version:".len()..]
        .trim_start()
        .split(['.', ' ', '\n', '\r'])
        .next()?
        .parse()
        .ok()
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
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Release { tag, assets })
}

/// Download an asset, apply the pin posture, and unpack it into `root`.
///
/// # Errors
/// A plain message on download, verification, or extraction failure.
pub async fn install_asset(
    asset: &Asset,
    root: &Path,
    pin: Option<&str>,
    require_pins: bool,
) -> Result<(), String> {
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

    match check_download_pin(&bytes, pin, require_pins).map_err(|e| e.to_string())? {
        PinOutcome::Verified => {}
        PinOutcome::Unpinned { computed } => {
            eprintln!("  Downloaded {} sha256={computed} (unpinned).", asset.name);
            eprintln!(
                "  To pin it, add \"{}\": \"{computed}\" under LlamaCppDownloadPins in settings.json.",
                asset.name
            );
        }
    }

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
    Ok(())
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
    /// Install this asset (fresh install or stale stamp).
    Install { release: Release, asset: Asset },
    /// mtpturbo staleness verdict (source-built; no prebuilt asset exists).
    MtpStatus { message: String },
}

/// Decide the update plan for the native or turboquant mode.
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

pub async fn plan_binary_update(
    catalog: &Catalog,
    mode: localx_llama_core::Mode,
    root: &Path,
    driver_major: Option<u32>,
    amd_gpu: bool,
) -> Result<UpdatePlan, String> {
    use localx_llama_core::Mode;
    let (repo, pinned_tag) = match mode {
        Mode::Native => (
            "ggerganov/llama.cpp".to_string(),
            catalog.setting_str("LlamaCppPinnedTag").map(str::to_string),
        ),
        Mode::Turboquant => (
            catalog
                .setting_str("LlamaCppTurboquantRepo")
                .unwrap_or("C0deGeek-dev/llama-cpp-turboquant")
                .to_string(),
            catalog
                .setting_str("LlamaCppTurboquantPinnedTag")
                .map(str::to_string),
        ),
        Mode::Mtpturbo => {
            return Ok(UpdatePlan::MtpStatus {
                message: mtp_status(catalog, root),
            })
        }
    };
    let release = fetch_release(&repo, pinned_tag.as_deref()).await?;

    if let Some(installed) = read_stamp_tag(root) {
        if !build_stamp_is_stale(&installed, &release.tag) {
            return Ok(UpdatePlan::UpToDate { tag: release.tag });
        }
    }

    let names: Vec<&str> = release.assets.iter().map(|a| a.name.as_str()).collect();
    let picked = match mode {
        Mode::Native => {
            let variant = native_variant(driver_major, amd_gpu);
            select_native_asset(&names, variant, driver_major)
                .or_else(|| select_native_asset(&names, Variant::Cpu, None))
        }
        Mode::Turboquant => {
            let (choice, warning) = select_turbo_asset(&names, driver_major);
            if let Some(warning) = warning {
                eprintln!("Warning: {warning}");
            }
            choice
        }
        Mode::Mtpturbo => None,
    };
    let name = picked.ok_or_else(|| {
        format!(
            "release {} has no prebuilt asset for this platform; provide your own \
             llama-server (bring-your-own) or pin a different tag",
            release.tag
        )
    })?;
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == name)
        .cloned()
        .ok_or("selected asset vanished from the release listing")?;
    Ok(UpdatePlan::Install { release, asset })
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
        let banner = "| NVIDIA-SMI 591.74  Driver Version: 591.74  CUDA Version: 13.1 |";
        assert_eq!(parse_cuda_driver_major(banner), Some(13));
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
