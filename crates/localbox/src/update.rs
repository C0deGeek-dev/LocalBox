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
    asset_arch_matches, build_stamp_is_stale, check_download_pin, cuda_major_order,
    select_cpu_asset, AssetArch, PinOutcome,
};

/// How many releases back to look for one this host can install. Deep enough
/// to step over marker releases and platform gaps, shallow enough to stay one
/// API page.
const RELEASE_SCAN_DEPTH: u8 = 30;

/// A named release asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    /// Upstream-reported download size in bytes. GitHub supplies this for
    /// release assets; keeping it in the pure plan lets preview and live
    /// execution describe exactly the same download set.
    pub size: Option<u64>,
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

/// The architecture family this host's release assets must belong to.
#[must_use]
pub fn host_asset_arch() -> AssetArch {
    if cfg!(target_arch = "aarch64") {
        AssetArch::Arm64
    } else {
        AssetArch::X64
    }
}

/// Whether an asset name fits this host's architecture.
#[must_use]
pub fn arch_matches(name: &str) -> bool {
    asset_arch_matches(name, host_asset_arch())
}

/// Whether the asset is an archive this updater can unpack.
#[must_use]
pub fn is_archive(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".zip") || lower.ends_with(".tar.gz") || lower.ends_with(".tar.xz")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReleaseOs {
    Windows,
    Linux,
    MacOs,
}

fn release_os(name: &str) -> Option<ReleaseOs> {
    let lower = name.to_ascii_lowercase();
    if lower.contains("windows") || lower.contains("-win-") {
        Some(ReleaseOs::Windows)
    } else if lower.contains("linux") || lower.contains("ubuntu") {
        Some(ReleaseOs::Linux)
    } else if lower.contains("macos") || lower.contains("darwin") {
        Some(ReleaseOs::MacOs)
    } else {
        None
    }
}

fn release_arch(name: &str) -> Option<AssetArch> {
    let lower = name.to_ascii_lowercase();
    if ["arm64", "aarch64"]
        .iter()
        .any(|token| lower.contains(token))
    {
        Some(AssetArch::Arm64)
    } else if ["x64", "x86_64", "amd64"]
        .iter()
        .any(|token| lower.contains(token))
    {
        Some(AssetArch::X64)
    } else {
        None
    }
}

/// CUDA major encoded in evolving release spellings such as `cuda-12.4`,
/// `cuda12.4`, or `cuda_13`. A `cudart` prefix is deliberately accepted so
/// server and runtime archives can be compared with the same parser.
fn asset_cuda_major(name: &str) -> Option<u32> {
    let lower = name.to_ascii_lowercase();
    lower.match_indices("cuda").find_map(|(start, _)| {
        let digits = lower[start + "cuda".len()..]
            .trim_start_matches(['-', '_'])
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>();
        (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
    })
}

fn runtime_companion<'a>(names: &[&'a str], server: &str) -> Result<Option<&'a str>, String> {
    let Some(cuda_major) = asset_cuda_major(server) else {
        return Ok(None);
    };
    let Some(os) = release_os(server) else {
        return Ok(None);
    };
    if os == ReleaseOs::MacOs {
        return Ok(None);
    }
    let arch = release_arch(server);
    let same_platform = |name: &&str| {
        let lower = name.to_ascii_lowercase();
        lower.contains("cudart")
            && is_archive(name)
            && release_os(name) == Some(os)
            && release_arch(name) == arch
    };
    let matching = names
        .iter()
        .copied()
        .find(|name| same_platform(name) && asset_cuda_major(name) == Some(cuda_major));
    if matching.is_some() {
        return Ok(matching);
    }

    // Official Windows llama.cpp/Prism releases split the runtime from the
    // server. Linux CUDA archives are currently self-contained, but if a
    // release starts publishing Linux cudart packages, their presence declares
    // the same split contract and a mismatched-major runtime must not be used.
    let required = os == ReleaseOs::Windows || names.iter().any(same_platform);
    if required {
        let platform = match os {
            ReleaseOs::Windows => "Windows",
            ReleaseOs::Linux => "Linux",
            ReleaseOs::MacOs => "macOS",
        };
        let arch = match arch {
            Some(AssetArch::Arm64) => "arm64",
            Some(AssetArch::X64) => "x64",
            None => "matching architecture",
        };
        return Err(format!(
            "release asset {server} requires a {platform} {arch} CUDA {cuda_major} runtime \
             companion, but the same release has no compatible cudart archive; choose a \
             different pinned release or refresh the release pins"
        ));
    }
    Ok(None)
}

fn with_runtime_companion<'a>(names: &[&'a str], server: &'a str) -> Result<Vec<&'a str>, String> {
    let mut assets = vec![server];
    if let Some(runtime) = runtime_companion(names, server)? {
        assets.push(runtime);
    }
    Ok(assets)
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
        // One selector for every host: the shared rule takes the architecture
        // rather than assuming x64, so ARM no longer needs a local copy.
        Variant::Cpu => select_cpu_asset(&eligible, host_asset_arch()),
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

/// Select the PrismML assets required by this host, plus a plain-language
/// warning when the only available CUDA build's major does not match the
/// driver's (that pairing can emit garbage output rather than an error — the
/// launch smoke test is the backstop that catches it).
///
/// Windows CUDA needs both the fork binaries and the separately packaged CUDA
/// runtime DLLs; Apple Silicon uses the standard Metal archive (not the
/// CPU-focused KleidiAI one). Linux picks CUDA by driver major, Vulkan on an
/// AMD GPU, else the plain CPU archive.
pub fn select_prism_assets<'a>(
    names: &[&'a str],
    driver_major: Option<u32>,
    amd_gpu: bool,
) -> Result<(Vec<&'a str>, Option<String>), String> {
    if cfg!(windows) {
        if !cfg!(target_arch = "x86_64") {
            return Err("the Prism engine currently supports Windows x64 only".to_string());
        }
        let Some(driver) = driver_major else {
            return Err("the Prism engine requires an NVIDIA CUDA driver on Windows".to_string());
        };
        if driver < 12 {
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
        let assets = with_runtime_companion(names, binary)?;
        let warning = (driver != 12).then(|| {
            format!(
                "the Prism Windows build targets CUDA 12.4 but the driver reports CUDA \
                 {driver}; a mismatched build can emit garbage output — the launch smoke \
                 test will catch that before an agent sees it"
            )
        });
        return Ok((assets, warning));
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
        return Ok((vec![metal], None));
    }
    if cfg!(target_os = "linux") {
        return select_prism_linux_asset(names, driver_major, amd_gpu)
            .map(|(asset, warning)| (vec![asset], warning));
    }
    Err(
        "the Prism engine currently supports Windows CUDA, Apple Silicon Metal, and Linux"
            .to_string(),
    )
}

/// The Linux arm of the Prism selection: CUDA archives carry a `-linux-cuda-`
/// token, while the CPU/Vulkan builds use `-ubuntu-`; the rocm and KleidiAI
/// archives are deliberately not selected (AMD routes to Vulkan, matching the
/// native-mode preference).
fn select_prism_linux_asset<'a>(
    names: &[&'a str],
    driver_major: Option<u32>,
    amd_gpu: bool,
) -> Result<(&'a str, Option<String>), String> {
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    };
    if let Some(driver) = driver_major {
        let cuda: Vec<&str> = names
            .iter()
            .copied()
            .filter(|n| {
                let lower = n.to_ascii_lowercase();
                lower.contains("-bin-linux-cuda-")
                    && lower.ends_with(&format!("-{arch}.tar.gz"))
                    && arch_matches(n)
            })
            .collect();
        let mut majors: Vec<u32> = cuda
            .iter()
            .filter_map(|n| {
                let lower = n.to_ascii_lowercase();
                let rest = &lower[lower.find("-cuda-")? + "-cuda-".len()..];
                rest.split(['.', '-']).next()?.parse().ok()
            })
            .collect();
        majors.sort_unstable();
        majors.dedup();
        for major in cuda_major_order(driver, &majors) {
            let token = format!("-cuda-{major}.");
            // Within a major, the newest toolkit build wins (12.8 over 12.4).
            let hit = cuda
                .iter()
                .filter(|n| n.to_ascii_lowercase().contains(&token))
                .max_by_key(|n| {
                    let lower = n.to_ascii_lowercase();
                    lower[lower.find(&token).unwrap_or(0) + token.len()..]
                        .split('-')
                        .next()
                        .and_then(|minor| minor.parse::<u32>().ok())
                        .unwrap_or(0)
                })
                .copied();
            if let Some(hit) = hit {
                let warning = (major != driver).then(|| {
                    format!(
                        "the chosen Prism build targets CUDA {major} but the driver reports \
                         CUDA {driver}; a mismatched build can emit garbage output — the \
                         launch smoke test will catch that before an agent sees it"
                    )
                });
                return Ok((hit, warning));
            }
        }
        return Err(format!(
            "the Prism release has no Linux {arch} CUDA archive for this driver"
        ));
    }
    if amd_gpu {
        return names
            .iter()
            .copied()
            .find(|n| {
                n.to_ascii_lowercase()
                    .ends_with(&format!("-bin-ubuntu-vulkan-{arch}.tar.gz"))
            })
            .map(|hit| (hit, None))
            .ok_or_else(|| format!("the Prism release has no Linux {arch} Vulkan archive"));
    }
    names
        .iter()
        .copied()
        .find(|n| {
            n.to_ascii_lowercase()
                .ends_with(&format!("-bin-ubuntu-{arch}.tar.gz"))
        })
        .map(|hit| (hit, None))
        .ok_or_else(|| format!("the Prism release has no Linux {arch} CPU archive"))
}

/// The `.build-stamp` variant line for an installed asset set. Prism derives
/// it from the selected asset so the stamp says what was actually installed
/// (cuda-12.4 vs cuda-12.8 vs vulkan vs cpu vs metal) instead of a per-OS
/// hardcode; other modes keep their host-probe variant.
#[must_use]
pub fn stamp_variant(
    mode: localx_llama_core::Mode,
    asset_names: &[&str],
    driver_major: Option<u32>,
    amd_gpu: bool,
) -> String {
    if mode != localx_llama_core::Mode::PrismMl {
        return native_variant(driver_major, amd_gpu).as_str().to_string();
    }
    let binary = asset_names
        .iter()
        .find(|n| !n.to_ascii_lowercase().starts_with("cudart-"))
        .copied()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if binary.contains("-macos-") {
        return "metal".to_string();
    }
    if let Some(idx) = binary.find("-cuda-") {
        let version: String = binary[idx + "-cuda-".len()..]
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        if !version.is_empty() {
            return format!("cuda-{}", version.trim_end_matches('.'));
        }
    }
    if binary.contains("-vulkan-") {
        return "vulkan".to_string();
    }
    "cpu".to_string()
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

/// Fetch a repository's releases, newest first.
///
/// # Errors
/// A plain message when the API cannot be reached or answers unexpectedly.
pub async fn fetch_releases(repo: &str, limit: u8) -> Result<Vec<Release>, String> {
    let url = format!("https://api.github.com/repos/{repo}/releases?per_page={limit}");
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
    Ok(releases_from_json(&value))
}

/// The downloadable assets carried by one release payload.
fn assets_from_json(value: &serde_json::Value) -> Vec<Asset> {
    value["assets"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|a| {
                    Some(Asset {
                        name: a["name"].as_str()?.to_string(),
                        url: a["browser_download_url"].as_str()?.to_string(),
                        size: a["size"].as_u64(),
                        digest: a["digest"].as_str().and_then(parse_github_digest),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
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
    let assets = assets_from_json(&value);
    Ok(Release { tag, assets })
}

/// Parse a `/releases` list payload into releases, newest first.
///
/// Split from the HTTP call so the selection rule below is unit-testable.
#[must_use]
pub fn releases_from_json(value: &serde_json::Value) -> Vec<Release> {
    value
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|entry| {
                    Some(Release {
                        tag: entry["tag_name"].as_str()?.to_string(),
                        assets: assets_from_json(entry),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The newest release this host can actually install, given a per-release
/// asset selector.
///
/// GitHub's "latest release" is a publisher's flag, not a statement that the
/// release carries binaries: llama.cpp marks a version-numbered marker release
/// (`v0.3.0`, carrying only `nightly-tag.txt`) as latest while every usable
/// build lives on an unflagged `b####` tag. Walking newest-first and taking
/// the first release whose assets fit skips those, and skips a real release
/// published without this platform's archive.
#[must_use]
pub fn newest_installable<F>(releases: &[Release], fits: F) -> Option<&Release>
where
    F: Fn(&Release) -> bool,
{
    releases.iter().find(|release| fits(release))
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
    // Each archive gets an isolated extraction root. This matters for split
    // packages: once the main archive has placed llama-server at the staging
    // root, a nested cudart payload must still be normalized and merged rather
    // than being stranded under its archive's top-level folder.
    let extraction = tempfile::Builder::new()
        .prefix(".localbox-asset-")
        .tempdir_in(root)
        .map_err(|e| format!("could not create asset extraction directory: {e}"))?;
    let archive = extraction.path().join(&asset.name);
    std::fs::write(&archive, &bytes).map_err(|e| e.to_string())?;
    // bsdtar ships with Windows 10+ and unpacks zip as well as tar archives,
    // so one extraction path serves every OS with no archive dependency.
    let status = Command::new("tar")
        .arg("-xf")
        .arg(&archive)
        .arg("-C")
        .arg(extraction.path())
        .status()
        .map_err(|e| format!("could not run tar: {e}"))?;
    let _ = std::fs::remove_file(&archive);
    if !status.success() {
        return Err(format!("extracting {} failed ({status})", asset.name));
    }
    flatten_extracted(extraction.path());
    let payload = payload_root(extraction.path());
    merge_extracted_tree(&payload, root)?;
    #[cfg(unix)]
    set_unix_exec_bits(root);
    Ok(computed)
}

fn payload_root(extracted: &Path) -> PathBuf {
    if extracted
        .join(localx_llama_runtime::server::server_exe_name())
        .is_file()
    {
        return extracted.to_path_buf();
    }
    let mut current = extracted.to_path_buf();
    loop {
        let Ok(entries) = std::fs::read_dir(&current) else {
            return current;
        };
        let entries = entries.flatten().collect::<Vec<_>>();
        if entries.len() != 1 || !entries[0].path().is_dir() {
            return current;
        }
        current = entries[0].path();
    }
}

fn merge_extracted_tree(source: &Path, target: &Path) -> Result<(), String> {
    std::fs::create_dir_all(target).map_err(|e| e.to_string())?;
    let entries = std::fs::read_dir(source)
        .map_err(|e| format!("could not read extracted payload {}: {e}", source.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let from = entry.path();
        let to = target.join(entry.file_name());
        if from.is_dir() && to.is_dir() {
            merge_extracted_tree(&from, &to)?;
            continue;
        }
        if std::fs::symlink_metadata(&to).is_ok() {
            if to.is_dir() {
                std::fs::remove_dir_all(&to).map_err(|e| e.to_string())?;
            } else {
                std::fs::remove_file(&to).map_err(|e| e.to_string())?;
            }
        }
        std::fs::rename(&from, &to).map_err(|e| {
            format!(
                "could not merge extracted payload {} into {}: {e}",
                from.display(),
                to.display()
            )
        })?;
    }
    Ok(())
}

/// Human-readable identity and expected byte count for the exact asset set.
/// Unknown upstream sizes remain explicit instead of being treated as zero.
#[must_use]
pub fn asset_set_summary(assets: &[Asset]) -> String {
    let items = assets
        .iter()
        .map(|asset| match asset.size {
            Some(size) => format!(
                "{} ({})",
                asset.name,
                localbox_launcher::fetch::human_bytes(size)
            ),
            None => format!("{} (size unknown)", asset.name),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let total = assets
        .iter()
        .try_fold(0_u64, |sum, asset| sum.checked_add(asset.size?));
    match total {
        Some(total) => format!(
            "{items}; expected download {}",
            localbox_launcher::fetch::human_bytes(total)
        ),
        None => format!("{items}; total download size unknown"),
    }
}

/// Download and verify every selected asset in a sibling staging directory,
/// write the install stamp there, then swap the complete tree into place. A
/// failed asset or failed activation leaves the existing install untouched.
///
/// `pins` is parallel to `assets`: `None` deliberately represents an unpinned
/// pin-refresh download, which still verifies an upstream release digest when
/// one is available.
///
/// # Errors
/// A plain message for an invalid plan, download/integrity/extraction failure,
/// a staged tree without `llama-server`, or an activation/rollback failure.
/// How long a staged binary gets to answer `--version` before the probe gives
/// up and rejects it.
///
/// A warm engine answers in about two seconds. The generous ceiling is for the
/// cold case: the first execution of a freshly extracted multi-hundred-megabyte
/// backend library can stall while antivirus reads all of it. A probe that
/// hangs must still fail rather than block the install forever.
const LOAD_PROBE_TIMEOUT_SECS: u64 = 120;

/// Windows `STATUS_DLL_NOT_FOUND`: the loader could not find a DLL the binary
/// imports. This is what an archive that links a dependency dynamically and
/// then does not package it looks like from the outside.
const STATUS_DLL_NOT_FOUND: i32 = 0xC000_0135_u32 as i32;

/// Windows `STATUS_ENTRYPOINT_NOT_FOUND`: a DLL was found but does not export
/// what the binary imports — the right name, the wrong version.
const STATUS_ENTRYPOINT_NOT_FOUND: i32 = 0xC000_0139_u32 as i32;

/// The first non-empty line of the probe's output, bounded.
fn first_line(primary: &str, fallback: &str) -> String {
    primary
        .lines()
        .chain(fallback.lines())
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("no version reported")
        .chars()
        .take(200)
        .collect()
}

/// A bounded, single-line excerpt of whatever the binary managed to say.
fn probe_excerpt(stderr: &str) -> String {
    let collapsed = stderr.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(200).collect()
}

/// Turn a load probe's exit into a verdict.
///
/// Pure so the exit-code mapping is testable without spawning anything. The
/// mapping carries the diagnostic on Windows, where a binary that cannot
/// resolve its imports is killed by the loader before `main` and writes
/// nothing at all to stderr — the readable "… .dll was not found" text exists
/// only in a GUI dialog. Without the translation the rejection would be a bare
/// negative number.
///
/// # Errors
/// A plain, operator-facing message for any outcome that is not a clean exit.
pub fn classify_load_probe(
    code: Option<i32>,
    stdout: &str,
    stderr: &str,
) -> Result<String, String> {
    let excerpt = probe_excerpt(stderr);
    let with_excerpt = |message: String| {
        if excerpt.is_empty() {
            message
        } else {
            format!("{message} The binary reported: {excerpt}")
        }
    };
    match code {
        Some(0) => Ok(first_line(stdout, stderr)),
        Some(STATUS_DLL_NOT_FOUND) => Err(with_excerpt(
            "the staged llama-server could not start because a runtime library it links against is missing from this host (Windows STATUS_DLL_NOT_FOUND). The release archive links that dependency dynamically and does not package it."
                .to_string(),
        )),
        Some(STATUS_ENTRYPOINT_NOT_FOUND) => Err(with_excerpt(
            "the staged llama-server found a runtime library but not the entry points it imports (Windows STATUS_ENTRYPOINT_NOT_FOUND), so the version on this host is the wrong one."
                .to_string(),
        )),
        Some(127) => Err(with_excerpt(
            "the staged llama-server could not start because a shared library it links against is missing from this host."
                .to_string(),
        )),
        Some(status) => Err(with_excerpt(format!(
            "the staged llama-server exited with status {status} instead of reporting its version."
        ))),
        None => Err(format!(
            "the staged llama-server did not report its version within {LOAD_PROBE_TIMEOUT_SECS}s and was stopped."
        )),
    }
}

/// Run the staged binary's `--version` and require a clean exit.
///
/// Deliberately a load probe, not a smoke test: no model, no port. `--version`
/// exercises exactly what fails when an archive is missing a dependency — the
/// loader resolving every import — and costs one short process spawn against a
/// download of hundreds of megabytes.
///
/// # Errors
/// A plain message when the binary cannot be spawned, does not exit cleanly,
/// or does not answer within [`LOAD_PROBE_TIMEOUT_SECS`].
pub fn probe_staged_server(server: &Path) -> Result<String, String> {
    let mut child = Command::new(server)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("the staged llama-server could not be started: {e}"))?;

    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(LOAD_PROBE_TIMEOUT_SECS);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                return Err(format!(
                    "the staged llama-server could not be waited on: {e}"
                ));
            }
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };

    let (stdout, stderr) = match child.wait_with_output() {
        Ok(output) => (
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        ),
        Err(_) => (String::new(), String::new()),
    };
    classify_load_probe(status.and_then(|status| status.code()), &stdout, &stderr)
}

pub async fn install_asset_set(
    assets: &[Asset],
    pins: &[Option<String>],
    root: &Path,
    require_pins: bool,
    tag: &str,
    variant: &str,
    load_probe: bool,
) -> Result<Vec<(String, String)>, String> {
    if assets.is_empty() {
        return Err("the update plan contains no assets".to_string());
    }
    if assets.len() != pins.len() {
        return Err("the update plan's asset and pin counts do not match".to_string());
    }
    let parent = root
        .parent()
        .ok_or_else(|| format!("install root {} has no parent directory", root.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("could not create install parent {}: {e}", parent.display()))?;
    let stage = tempfile::Builder::new()
        .prefix(".localbox-update-")
        .tempdir_in(parent)
        .map_err(|e| format!("could not create update staging directory: {e}"))?;
    let mut hashes = Vec::with_capacity(assets.len());
    for (asset, pin) in assets.iter().zip(pins) {
        let hash = install_asset(asset, stage.path(), pin.as_deref(), require_pins).await?;
        hashes.push((asset.name.clone(), hash));
    }
    let server = stage
        .path()
        .join(localx_llama_runtime::server::server_exe_name());
    if !server.is_file() {
        return Err(format!(
            "verified assets extracted without {}; the working install was not replaced",
            server.display()
        ));
    }
    // Existing here is not the same as runnable. A pinned, checksum-verified
    // archive is byte-for-byte what upstream published, which says nothing
    // about whether it carries every library it links against — and the server
    // binary can be a thin shim whose imports live in a sibling it cannot
    // load. Ask the staged binary to report its version while `stage` is still
    // a temporary directory: rejecting here drops the staged tree and leaves
    // the working engine and its build stamp exactly where they were.
    if load_probe {
        match probe_staged_server(&server) {
            Ok(version) => println!("Staged build reports: {version}."),
            Err(message) => {
                return Err(format!(
                    concat!(
                        "{} The staged install was discarded and the working engine ",
                        "was left in place. Pass --skip-load-probe to install it anyway."
                    ),
                    message
                ))
            }
        }
    }
    write_stamp(stage.path(), tag, variant)
        .map_err(|e| format!("could not stage the install stamp: {e}"))?;
    let staged_path = stage.into_path();
    activate_staged(&staged_path, root)?;
    Ok(hashes)
}

fn activate_staged(staged: &Path, root: &Path) -> Result<(), String> {
    activate_staged_with(staged, root, |from, to| std::fs::rename(from, to))
}

fn activate_staged_with<F>(staged: &Path, root: &Path, rename: F) -> Result<(), String>
where
    F: Fn(&Path, &Path) -> std::io::Result<()>,
{
    let server = staged.join(localx_llama_runtime::server::server_exe_name());
    if !server.is_file() {
        return Err(format!("staged install has no {}", server.display()));
    }
    let backup = staged.with_extension("backup");
    if std::fs::symlink_metadata(&backup).is_ok() {
        return Err(format!(
            "refusing to overwrite unexpected backup path {}",
            backup.display()
        ));
    }
    let had_existing = std::fs::symlink_metadata(root).is_ok();
    if had_existing {
        rename(root, &backup).map_err(|e| {
            format!(
                "could not move the existing install {} aside: {e}",
                root.display()
            )
        })?;
    }
    if let Err(activation_error) = rename(staged, root) {
        let rollback = if had_existing {
            rename(&backup, root).map_err(|e| e.to_string())
        } else {
            Ok(())
        };
        let _ = std::fs::remove_dir_all(staged);
        return match rollback {
            Ok(()) => Err(format!(
                "could not activate the staged install ({activation_error}); the previous \
                 install was restored"
            )),
            Err(rollback_error) => Err(format!(
                "could not activate the staged install ({activation_error}) and could not \
                 restore the previous install ({rollback_error}); recovery copy: {}",
                backup.display()
            )),
        };
    }
    if had_existing {
        if let Err(error) = std::fs::remove_dir_all(&backup) {
            eprintln!(
                "Warning: installed successfully but could not remove backup {}: {error}",
                backup.display()
            );
        }
    }
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
    /// Install these assets (fresh install or stale stamp).
    Install {
        release: Release,
        assets: Vec<Asset>,
    },
    /// mtpturbo staleness verdict (source-built; no prebuilt asset exists).
    MtpStatus { message: String },
    /// The resolved release is *older* than the installed build. Never applied
    /// implicitly: replacing a working engine with an earlier one means
    /// upstream withdrew or retagged a release, not that an upgrade is wanted.
    WouldDowngrade {
        /// The tag currently installed, per the build stamp.
        installed: String,
        /// The older tag the release lookup resolved to.
        resolved: String,
    },
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

/// The comparable part of a release tag: a family prefix plus its numbers.
///
/// Tags across the engines follow two shapes — a dotted `v` version
/// (`tqp-v0.3.1`) and a llama.cpp build number (`b9596`, `prism-b9596-9fcaed7`).
/// Anything else, or a mismatch of family, yields `None` so callers treat the
/// pair as unordered rather than guessing.
#[must_use]
pub fn tag_order_key(tag: &str) -> Option<(String, Vec<u64>)> {
    let tag = tag.trim();
    if tag.is_empty() {
        return None;
    }
    let lower = tag.to_ascii_lowercase();

    // `...v1.2.3` — the dotted version wins when both shapes could match.
    if let Some(pos) = lower.find('v') {
        let rest = &lower[pos + 1..];
        let digits: String = rest
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        let parts: Vec<&str> = digits.split('.').filter(|p| !p.is_empty()).collect();
        if parts.len() >= 2 {
            if let Some(nums) = parts.iter().map(|p| p.parse::<u64>().ok()).collect() {
                return Some((format!("{}v", &lower[..pos]), nums));
            }
        }
    }

    // `b9596`, possibly embedded (`prism-b9596-9fcaed7`).
    let bytes = lower.as_bytes();
    for (i, c) in lower.char_indices() {
        if c != 'b' {
            continue;
        }
        let starts_token = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        if !starts_token {
            continue;
        }
        let digits: String = lower[i + 1..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if !digits.is_empty() {
            if let Ok(n) = digits.parse::<u64>() {
                return Some((format!("{}b", &lower[..i]), vec![n]));
            }
        }
    }
    None
}

/// Whether installing `resolved` over `installed` moves *backwards*.
///
/// Conservative on purpose: only tags of the same family whose numbers both
/// parse are ordered at all. Unknown or mismatched shapes are never reported
/// as a downgrade, so this can block a real mistake but never a legitimate
/// re-pin it simply cannot read.
#[must_use]
pub fn is_downgrade(installed: &str, resolved: &str) -> bool {
    match (tag_order_key(installed), tag_order_key(resolved)) {
        (Some((lhs_family, lhs)), Some((rhs_family, rhs))) if lhs_family == rhs_family => rhs < lhs,
        _ => false,
    }
}

pub async fn plan_binary_update(
    catalog: &Catalog,
    mode: localx_llama_core::Mode,
    root: &Path,
    driver_major: Option<u32>,
    amd_gpu: bool,
    allow_downgrade: bool,
) -> Result<UpdatePlan, String> {
    let Some((repo, _configured)) = mode_release_source(catalog, mode) else {
        return Ok(UpdatePlan::MtpStatus {
            message: mtp_status(catalog, root),
        });
    };
    // Always the newest usable release, never the recorded tag: the tag in
    // `settings.json` records what was last installed and verified, not a
    // ceiling. "Newest usable" rather than GitHub's `latest` flag, because
    // that flag says nothing about a release carrying binaries for this host.
    let releases = fetch_releases(&repo, RELEASE_SCAN_DEPTH).await?;
    let fits = |release: &Release| {
        select_release_assets_reporting(release, mode, driver_major, amd_gpu)
            .is_ok_and(|(assets, _)| !assets.is_empty())
    };
    let release = newest_installable(&releases, fits)
        .ok_or_else(|| {
            format!(
                "none of the {} newest releases in {repo} carry an asset for this platform;                  provide your own llama-server (bring-your-own)",
                releases.len()
            )
        })?
        .clone();

    if let Some(installed) = read_stamp_tag(root) {
        if !build_stamp_is_stale(&installed, &release.tag) {
            return Ok(UpdatePlan::UpToDate { tag: release.tag });
        }
        // The stamp differing means "install", in both directions. Going
        // backwards is the direction that costs something: it replaces a
        // working engine with an earlier one, and the pin that asks for it is
        // usually stale or reverted rather than deliberate.
        if !allow_downgrade && is_downgrade(&installed, &release.tag) {
            return Ok(UpdatePlan::WouldDowngrade {
                installed,
                resolved: release.tag,
            });
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
    let (assets, warning) = select_release_assets_reporting(release, mode, driver_major, amd_gpu)?;
    if let Some(warning) = warning {
        eprintln!("Warning: {warning}");
    }
    Ok(assets)
}

/// The selection above, with its host-compatibility warning returned rather
/// than printed. Scanning releases for the newest installable one calls this
/// repeatedly, and a warning about the release finally chosen should be said
/// once, by the caller that acts on it.
///
/// # Errors
/// A plain message when no asset fits this host.
pub fn select_release_assets_reporting(
    release: &Release,
    mode: localx_llama_core::Mode,
    driver_major: Option<u32>,
    amd_gpu: bool,
) -> Result<(Vec<Asset>, Option<String>), String> {
    use localx_llama_core::Mode;
    let mut host_warning = None;
    let names: Vec<&str> = release.assets.iter().map(|a| a.name.as_str()).collect();
    let picked: Vec<&str> = match mode {
        Mode::Native => {
            let variant = native_variant(driver_major, amd_gpu);
            let server = select_native_asset(&names, variant, driver_major)
                .or_else(|| select_native_asset(&names, Variant::Cpu, None))
                .ok_or_else(|| {
                    format!(
                        "release {} has no prebuilt asset for this platform; provide your own \
                         llama-server (bring-your-own) or pin a different tag",
                        release.tag
                    )
                })?;
            with_runtime_companion(&names, server)?
        }
        Mode::Turboquant => {
            let (choice, warning) = select_turbo_asset(&names, driver_major);
            host_warning = warning;
            choice.into_iter().collect()
        }
        Mode::Mtpturbo => Vec::new(),
        Mode::PrismMl => {
            let (choice, warning) = select_prism_assets(&names, driver_major, amd_gpu)?;
            host_warning = warning;
            choice
        }
    };
    if picked.is_empty() {
        return Err(format!(
            "release {} has no prebuilt asset for this platform; provide your own \
             llama-server (bring-your-own) or pin a different tag",
            release.tag
        ));
    }
    let assets = picked
        .iter()
        .map(|name| {
            release
                .assets
                .iter()
                .find(|asset| asset.name == *name)
                .cloned()
                .ok_or_else(|| "selected asset vanished from the release listing".to_string())
        })
        .collect::<Result<Vec<Asset>, String>>()?;
    Ok((assets, host_warning))
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

    fn legacy_cpu_asset<'a>(names: &[&'a str]) -> Option<&'a str> {
        for token in ["-avx2-", "-avx512-", "-avx-", "-noavx-", "-cpu-"] {
            if let Some(hit) = names
                .iter()
                .find(|name| name.to_ascii_lowercase().contains(token))
            {
                return Some(hit);
            }
        }
        None
    }

    #[test]
    fn cuda_driver_major_parses_from_nvidia_smi_banner() {
        let legacy = "| NVIDIA-SMI 591.74  Driver Version: 591.74  CUDA Version: 13.1 |";
        let current = "| NVIDIA-SMI 610.62  KMD Version: 610.62  CUDA UMD Version: 13.3 |";
        assert_eq!(parse_cuda_driver_major(legacy), Some(13));
        assert_eq!(parse_cuda_driver_major(current), Some(13));
        assert_eq!(parse_cuda_driver_major("no gpu here"), None);
    }

    #[test]
    fn runtime_companion_matching_handles_evolving_release_names() {
        struct Case<'a> {
            server: &'a str,
            names: &'a [&'a str],
            expected: Result<Option<&'a str>, &'a str>,
        }
        let cases = [
            Case {
                server: "llama-b1-bin-win-cuda-12.4-x64.zip",
                names: &[
                    "cudart-llama-bin-win-cuda-13.1-x64.zip",
                    "cudart-llama-bin-win-cuda-12.8-arm64.zip",
                    "cudart-llama-bin-win-cuda-12.8-x64.zip",
                ],
                expected: Ok(Some("cudart-llama-bin-win-cuda-12.8-x64.zip")),
            },
            Case {
                server: "engine-windows-amd64-cuda13.3.zip",
                names: &["runtime-cudart-windows-amd64-cuda13.1.zip"],
                expected: Ok(Some("runtime-cudart-windows-amd64-cuda13.1.zip")),
            },
            Case {
                server: "llama-bin-linux-cuda-12.8-x86_64.tar.gz",
                names: &["llama-bin-linux-cuda-12.8-x86_64.tar.gz"],
                expected: Ok(None),
            },
            Case {
                server: "llama-bin-linux-cuda_13-x86_64.tar.xz",
                names: &["cudart-llama-linux-cuda_13-x86_64.tar.xz"],
                expected: Ok(Some("cudart-llama-linux-cuda_13-x86_64.tar.xz")),
            },
            Case {
                server: "llama-bin-win-cuda-12.4-x64.zip",
                names: &["cudart-llama-bin-win-cuda-13.3-x64.zip"],
                expected: Err("CUDA 12 runtime companion"),
            },
        ];

        for case in cases {
            match (runtime_companion(case.names, case.server), case.expected) {
                (Ok(actual), Ok(expected)) => assert_eq!(actual, expected),
                (Err(actual), Err(expected)) => assert!(actual.contains(expected), "{actual}"),
                (actual, expected) => {
                    panic!("unexpected result: {actual:?}, expected {expected:?}")
                }
            }
        }
    }

    fn fixture_asset(name: &str, url: &str, size: Option<u64>) -> Asset {
        Asset {
            name: name.to_string(),
            url: url.to_string(),
            size,
            digest: None,
        }
    }

    #[test]
    fn asset_set_summary_keeps_names_and_sizes_together() {
        let known = [
            fixture_asset("server.zip", "unused", Some(1024)),
            fixture_asset("cudart.zip", "unused", Some(2048)),
        ];
        assert_eq!(
            asset_set_summary(&known),
            "server.zip (1024 B), cudart.zip (2048 B); expected download 3072 B"
        );
        let partly_unknown = [
            fixture_asset("server.zip", "unused", Some(1024)),
            fixture_asset("cudart.zip", "unused", None),
        ];
        assert_eq!(
            asset_set_summary(&partly_unknown),
            "server.zip (1024 B), cudart.zip (size unknown); total download size unknown"
        );
    }

    #[test]
    fn fixture_release_plans_server_and_runtime_as_one_sized_set() {
        let release = Release {
            tag: "b-fixture".to_string(),
            assets: vec![
                fixture_asset(
                    "llama-b-fixture-bin-win-cuda-12.8-x64.zip",
                    "server",
                    Some(30),
                ),
                fixture_asset(
                    "cudart-llama-bin-win-cuda-12.4-x64.zip",
                    "runtime",
                    Some(10),
                ),
            ],
        };
        let names = release
            .assets
            .iter()
            .map(|asset| asset.name.as_str())
            .collect::<Vec<_>>();
        let selected = with_runtime_companion(&names, names[0]).unwrap();
        let assets = selected
            .iter()
            .map(|name| {
                release
                    .assets
                    .iter()
                    .find(|asset| asset.name == *name)
                    .unwrap()
                    .clone()
            })
            .collect::<Vec<_>>();

        assert_eq!(assets.len(), 2);
        assert_eq!(assets[0].name, names[0]);
        assert!(assets[1].name.contains("cudart"));
        assert_eq!(
            asset_set_summary(&assets),
            "llama-b-fixture-bin-win-cuda-12.8-x64.zip (30 B), cudart-llama-bin-win-cuda-12.4-x64.zip (10 B); expected download 40 B"
        );
    }

    #[test]
    fn activation_replaces_the_tree_and_cleans_its_backup() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("live");
        let staged = parent.path().join("staged");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::write(root.join("old.txt"), b"old").unwrap();
        std::fs::write(
            staged.join(localx_llama_runtime::server::server_exe_name()),
            b"new",
        )
        .unwrap();
        write_stamp(&staged, "b2", "cuda-12").unwrap();

        activate_staged(&staged, &root).unwrap();

        assert!(!root.join("old.txt").exists());
        assert!(root
            .join(localx_llama_runtime::server::server_exe_name())
            .is_file());
        assert_eq!(read_stamp_tag(&root).as_deref(), Some("b2"));
        assert!(!staged.with_extension("backup").exists());
    }

    #[test]
    fn activation_failure_restores_the_previous_tree_and_cleans_staging() {
        use std::cell::Cell;

        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("live");
        let staged = parent.path().join("staged");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::write(root.join("old.txt"), b"old").unwrap();
        write_stamp(&root, "b1", "cpu").unwrap();
        std::fs::write(
            staged.join(localx_llama_runtime::server::server_exe_name()),
            b"new",
        )
        .unwrap();
        write_stamp(&staged, "b2", "cuda-12").unwrap();
        let calls = Cell::new(0_u8);

        let result = activate_staged_with(&staged, &root, |from, to| {
            let call = calls.get() + 1;
            calls.set(call);
            if call == 2 {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected activation failure",
                ))
            } else {
                std::fs::rename(from, to)
            }
        });

        assert!(result
            .unwrap_err()
            .contains("previous install was restored"));
        assert_eq!(calls.get(), 3);
        assert_eq!(std::fs::read(root.join("old.txt")).unwrap(), b"old");
        assert_eq!(read_stamp_tag(&root).as_deref(), Some("b1"));
        assert!(!staged.exists());
        assert!(!staged.with_extension("backup").exists());
    }

    #[tokio::test]
    async fn checksum_failure_never_replaces_or_stamps_a_working_install() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request);
            let body = b"not the pinned archive";
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
        });
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("live");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("old.txt"), b"old").unwrap();
        write_stamp(&root, "b1", "cpu").unwrap();
        let assets = [fixture_asset(
            "server.zip",
            &format!("http://{address}/server.zip"),
            Some(22),
        )];
        let pins = [Some("00".repeat(32))];

        let result = install_asset_set(&assets, &pins, &root, true, "b2", "cuda-12", false).await;
        server.join().unwrap();

        assert!(result.is_err());
        assert_eq!(std::fs::read(root.join("old.txt")).unwrap(), b"old");
        assert_eq!(read_stamp_tag(&root).as_deref(), Some("b1"));
        assert!(std::fs::read_dir(parent.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".localbox-update-")
        }));
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

    #[test]
    fn native_cpu_selection_uses_the_shared_current_legacy_and_fork_rules() {
        let os = os_asset_token();
        let arch = match host_asset_arch() {
            AssetArch::Arm64 => "arm64",
            AssetArch::X64 => "x64",
        };
        let current = format!("llama-b9596-bin-{os}-cpu-{arch}.zip");
        let legacy = format!("llama-b9596-bin-{os}-avx2-{arch}.zip");
        let fork_renamed = format!("llama-fork-{os}-{arch}-cpu.zip");

        let defaults: serde_json::Value =
            serde_json::from_str(include_str!("../../../local-llm/defaults.json")).unwrap();
        let pinned = defaults["LlamaCppDownloadPins"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .filter(|name| {
                let lower = name.to_ascii_lowercase();
                is_archive(name)
                    && lower.contains(os)
                    && arch_matches(name)
                    && !lower.contains("cudart")
            })
            .collect::<Vec<_>>();
        assert_eq!(
            select_cpu_asset(&pinned, host_asset_arch()),
            legacy_cpu_asset(&pinned)
        );

        assert_eq!(
            select_native_asset(&[current.as_str()], Variant::Cpu, None),
            Some(current.as_str())
        );
        assert_eq!(
            select_native_asset(&[legacy.as_str()], Variant::Cpu, None),
            Some(legacy.as_str())
        );
        assert_eq!(
            select_native_asset(&[fork_renamed.as_str()], Variant::Cpu, None),
            Some(fork_renamed.as_str())
        );
        assert_eq!(legacy_cpu_asset(&[fork_renamed.as_str()]), None);
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
        let (picked, warning) = select_prism_assets(&names, Some(13), false).unwrap();
        assert_eq!(
            picked,
            vec![
                "llama-prism-b1-62061f9-bin-win-cuda-12.4-x64.zip",
                "cudart-llama-bin-win-cuda-12.4-x64.zip"
            ]
        );
        // The 12.4-only build on a CUDA 13 driver warns about garbage output.
        assert!(warning.unwrap().contains("garbage output"));
        // A matching CUDA 12 driver selects silently.
        let (_, warning) = select_prism_assets(&names, Some(12), false).unwrap();
        assert!(warning.is_none());
        assert!(select_prism_assets(&names, None, false).is_err());
        assert!(select_prism_assets(&names, Some(11), false).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn prism_macos_selection_prefers_standard_metal_over_kleidiai() {
        let names = [
            "llama-prism-b9591-62061f9-bin-macos-arm64-kleidiai.tar.gz",
            "llama-prism-b9591-62061f9-bin-macos-arm64.tar.gz",
        ];
        let (picked, warning) = select_prism_assets(&names, None, false).unwrap();
        assert_eq!(
            picked,
            vec!["llama-prism-b9591-62061f9-bin-macos-arm64.tar.gz"]
        );
        assert!(warning.is_none());
    }

    #[test]
    fn prism_linux_selection_covers_cuda_vulkan_and_cpu() {
        // The real prism-b9596-9fcaed7 Linux/ubuntu asset names. The helper
        // follows the *host* architecture (arch_matches), so expectations
        // branch on it — an arm64 runner legitimately selects the arm64
        // twins, and the release ships no arm64 Linux CUDA archive at all.
        let names = [
            "llama-prism-b9596-9fcaed7-bin-linux-cuda-12.4-x64.tar.gz",
            "llama-prism-b9596-9fcaed7-bin-linux-cuda-12.8-x64.tar.gz",
            "llama-prism-b9596-9fcaed7-bin-ubuntu-arm64.tar.gz",
            "llama-prism-b9596-9fcaed7-bin-ubuntu-rocm-7.2-x64.tar.gz",
            "llama-prism-b9596-9fcaed7-bin-ubuntu-vulkan-arm64.tar.gz",
            "llama-prism-b9596-9fcaed7-bin-ubuntu-vulkan-x64.tar.gz",
            "llama-prism-b9596-9fcaed7-bin-ubuntu-x64.tar.gz",
        ];
        if cfg!(target_arch = "aarch64") {
            // No arm64 Linux CUDA archive exists → a clear error, not a
            // silent x64 pick.
            assert!(select_prism_linux_asset(&names, Some(12), false).is_err());
            let (asset, _) = select_prism_linux_asset(&names, None, true).unwrap();
            assert_eq!(
                asset,
                "llama-prism-b9596-9fcaed7-bin-ubuntu-vulkan-arm64.tar.gz"
            );
            let (asset, _) = select_prism_linux_asset(&names, None, false).unwrap();
            assert_eq!(asset, "llama-prism-b9596-9fcaed7-bin-ubuntu-arm64.tar.gz");
            return;
        }
        // A CUDA 12 driver takes the highest matching 12.x archive, silently.
        let (asset, warning) = select_prism_linux_asset(&names, Some(12), false).unwrap();
        assert_eq!(
            asset,
            "llama-prism-b9596-9fcaed7-bin-linux-cuda-12.8-x64.tar.gz"
        );
        assert!(warning.is_none());
        // A CUDA 13 driver still gets a 12.x build, but with the garbage warning.
        let (asset, warning) = select_prism_linux_asset(&names, Some(13), false).unwrap();
        assert!(asset.contains("-linux-cuda-12."));
        assert!(warning.unwrap().contains("garbage output"));
        // An AMD GPU routes to Vulkan (never rocm), CPU otherwise — and the
        // plain CPU pick must not accidentally match the vulkan/rocm twins.
        let (asset, _) = select_prism_linux_asset(&names, None, true).unwrap();
        assert_eq!(
            asset,
            "llama-prism-b9596-9fcaed7-bin-ubuntu-vulkan-x64.tar.gz"
        );
        let (asset, _) = select_prism_linux_asset(&names, None, false).unwrap();
        assert_eq!(asset, "llama-prism-b9596-9fcaed7-bin-ubuntu-x64.tar.gz");
    }

    #[test]
    fn stamp_variant_follows_the_selected_prism_asset() {
        use localx_llama_core::Mode;
        assert_eq!(
            stamp_variant(
                Mode::PrismMl,
                &[
                    "llama-prism-b1-9fcaed7-bin-win-cuda-12.4-x64.zip",
                    "cudart-llama-bin-win-cuda-12.4-x64.zip"
                ],
                Some(13),
                false
            ),
            "cuda-12.4"
        );
        assert_eq!(
            stamp_variant(
                Mode::PrismMl,
                &["llama-prism-b9596-9fcaed7-bin-linux-cuda-12.8-x64.tar.gz"],
                Some(12),
                false
            ),
            "cuda-12.8"
        );
        assert_eq!(
            stamp_variant(
                Mode::PrismMl,
                &["llama-prism-b9596-9fcaed7-bin-macos-arm64.tar.gz"],
                None,
                false
            ),
            "metal"
        );
        assert_eq!(
            stamp_variant(
                Mode::PrismMl,
                &["llama-prism-b9596-9fcaed7-bin-ubuntu-vulkan-x64.tar.gz"],
                None,
                true
            ),
            "vulkan"
        );
        assert_eq!(
            stamp_variant(
                Mode::PrismMl,
                &["llama-prism-b9596-9fcaed7-bin-ubuntu-x64.tar.gz"],
                None,
                false
            ),
            "cpu"
        );
        // Non-prism modes keep the host-probe variant.
        assert_eq!(
            stamp_variant(
                Mode::Native,
                &["llama-b9596-bin-win-cuda-13.3-x64.zip"],
                Some(13),
                false
            ),
            "cuda"
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
    fn newest_installable_walks_past_a_marker_release() {
        // llama.cpp flags a version-numbered marker release as "latest" while
        // every usable Windows build lives on an unflagged b#### tag, so
        // GitHub's latest-release endpoint resolves something with no
        // binaries at all. Walk newest-first and take the first that fits.
        let rel = |tag: &str, assets: &[&str]| Release {
            tag: tag.to_string(),
            assets: assets
                .iter()
                .map(|name| Asset {
                    name: (*name).to_string(),
                    url: String::new(),
                    size: None,
                    digest: None,
                })
                .collect(),
        };
        let releases = vec![
            rel("v0.3.0", &["nightly-tag.txt"]),
            rel("b10797", &["llama-b10797-bin-win-cuda-13.3-x64.zip"]),
            rel("b10796", &["llama-b10796-bin-win-cuda-13.3-x64.zip"]),
        ];
        let fits = |release: &Release| release.assets.iter().any(|a| a.name.ends_with(".zip"));
        assert_eq!(
            newest_installable(&releases, fits).map(|r| r.tag.as_str()),
            Some("b10797")
        );
        // Nothing usable anywhere is reported as such, not as a silent pick.
        assert!(newest_installable(&releases[..1], fits).is_none());
        assert!(newest_installable(&[], fits).is_none());
    }

    #[test]
    fn releases_are_parsed_newest_first_with_digests() {
        let payload = serde_json::json!([
            {
                "tag_name": "b10797",
                "assets": [{
                    "name": "llama-b10797-bin-win-cuda-13.3-x64.zip",
                    "browser_download_url": "https://example.invalid/a.zip",
                    "size": 42,
                    "digest": "sha256:DDD2DFBC5E726D1A08F6D40D2129807693231B732655E5C17A058DDFD0708651"
                }]
            },
            { "tag_name": "v0.3.0", "assets": [] }
        ]);
        let parsed = releases_from_json(&payload);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].tag, "b10797");
        assert_eq!(parsed[0].assets[0].size, Some(42));
        assert_eq!(
            parsed[0].assets[0].digest.as_deref(),
            Some("ddd2dfbc5e726d1a08f6d40d2129807693231b732655e5c17a058ddfd0708651"),
            "the published digest is normalised to lowercase hex"
        );
        assert!(parsed[1].assets.is_empty());
    }

    #[test]
    fn a_clean_probe_reports_the_build_it_measured() {
        assert_eq!(
            classify_load_probe(
                Some(0),
                "version: 9596 (18ef86ece)
built with Clang
",
                ""
            ),
            Ok("version: 9596 (18ef86ece)".to_string())
        );
        // Some builds announce themselves on stderr; take whichever spoke.
        assert_eq!(
            classify_load_probe(
                Some(0),
                "",
                "version: 1 (7f1a8b1)
"
            ),
            Ok("version: 1 (7f1a8b1)".to_string())
        );
        // A clean exit that said nothing is still a pass — the loader resolved
        // every import, which is what the probe is actually asking.
        assert_eq!(
            classify_load_probe(
                Some(0),
                "   
",
                ""
            ),
            Ok("no version reported".to_string())
        );
    }

    /// The failure that motivated the probe. On Windows the loader kills the
    /// process before `main`, so stderr is empty and the exit code carries the
    /// entire diagnostic — an untranslated rejection would read as a bare
    /// negative number.
    #[test]
    fn a_missing_runtime_library_is_named_not_left_as_a_status_number() {
        let verdict = classify_load_probe(Some(0xC000_0135_u32 as i32), "", "");
        let message = verdict.expect_err("a binary that cannot load must be rejected");
        assert!(
            message.contains("runtime library"),
            "the rejection did not name the cause: {message}"
        );
        assert!(
            !message.contains("-1073741515"),
            "the rejection leaked the raw status instead of explaining it: {message}"
        );

        let wrong_version = classify_load_probe(Some(0xC000_0139_u32 as i32), "", "");
        assert!(wrong_version
            .expect_err("a wrong-version library must be rejected")
            .contains("entry points"));
    }

    #[test]
    fn a_unix_loader_failure_keeps_what_the_binary_said() {
        let message = classify_load_probe(
            Some(127),
            "",
            "llama-server: error while loading shared libraries: libcrypto.so.3: cannot open shared object file",
        )
        .expect_err("exit 127 is a rejection");
        assert!(message.contains("shared library"), "{message}");
        assert!(
            message.contains("libcrypto.so.3"),
            "the excerpt naming the missing library was dropped: {message}"
        );
    }

    #[test]
    fn other_failures_and_timeouts_are_reported_as_themselves() {
        let other = classify_load_probe(Some(3), "", "boom").expect_err("non-zero is a rejection");
        assert!(other.contains("status 3"), "{other}");
        assert!(other.contains("boom"), "{other}");

        let timed_out = classify_load_probe(None, "", "").expect_err("a timeout is a rejection");
        assert!(
            timed_out.contains("did not report its version"),
            "{timed_out}"
        );
    }

    /// A probe that cannot even start the binary must say so rather than
    /// panic — the install is rejected either way.
    #[test]
    fn a_binary_that_cannot_be_spawned_is_a_rejection_not_a_panic() {
        let missing = std::path::Path::new("definitely-not-a-real-llama-server-binary");
        let message = probe_staged_server(missing).expect_err("a missing binary cannot pass");
        assert!(message.contains("could not be started"), "{message}");
    }

    #[test]
    fn tag_order_reads_both_release_shapes() {
        assert_eq!(
            tag_order_key("tqp-v0.3.1"),
            Some(("tqp-v".to_string(), vec![0, 3, 1]))
        );
        assert_eq!(tag_order_key("b9596"), Some(("b".to_string(), vec![9596])));
        assert_eq!(
            tag_order_key("prism-b9596-9fcaed7"),
            Some(("prism-b".to_string(), vec![9596]))
        );
        // Nothing orderable: never guess.
        assert_eq!(tag_order_key("main"), None);
        assert_eq!(tag_order_key(""), None);
    }

    #[test]
    fn downgrade_is_detected_only_within_one_tag_family() {
        // The case that shipped a broken engine: a reverted pin resolving to
        // an older build than the one already installed.
        assert!(is_downgrade("tqp-v0.3.1", "tqp-v0.2.0"));
        assert!(is_downgrade("b10103", "b9596"));
        assert!(is_downgrade("prism-b9596-9fcaed7", "prism-b9591-62061f9"));

        // Forward and equal are not downgrades.
        assert!(!is_downgrade("tqp-v0.2.0", "tqp-v0.3.1"));
        assert!(!is_downgrade("tqp-v0.3.1", "tqp-v0.3.1"));
        assert!(!is_downgrade("b9596", "b10103"));

        // Different families, or shapes we cannot read, stay unordered so a
        // legitimate re-pin is never blocked by a guess.
        assert!(!is_downgrade("tqp-v0.3.1", "b9596"));
        assert!(!is_downgrade("tqp-v0.3.1", "some-branch-build"));
        assert!(!is_downgrade("whatever", "tqp-v0.2.0"));
    }

    #[test]
    fn patch_and_minor_order_within_a_family() {
        assert!(is_downgrade("tqp-v0.3.10", "tqp-v0.3.9"));
        assert!(!is_downgrade("tqp-v0.3.9", "tqp-v0.3.10"));
        assert!(is_downgrade("tqp-v1.0.0", "tqp-v0.9.9"));
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

    #[test]
    fn nested_runtime_payload_merges_beside_an_existing_server() {
        let parent = tempfile::tempdir().unwrap();
        let extracted = parent.path().join("extract");
        let nested = extracted.join("cudart-package").join("bin");
        let target = parent.path().join("stage");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(
            target.join(localx_llama_runtime::server::server_exe_name()),
            b"server",
        )
        .unwrap();
        std::fs::write(nested.join("cudart64_12.dll"), b"runtime").unwrap();

        let payload = payload_root(&extracted);
        assert_eq!(payload, nested);
        merge_extracted_tree(&payload, &target).unwrap();

        assert_eq!(
            std::fs::read(target.join("cudart64_12.dll")).unwrap(),
            b"runtime"
        );
        assert!(target
            .join(localx_llama_runtime::server::server_exe_name())
            .is_file());
    }
}
