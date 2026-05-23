# llama.cpp install / detection. Locates llama-server.exe, downloads a release
# from github.com/ggerganov/llama.cpp when missing, or pulls the turboquant
# Docker image. All work is lazy — nothing happens at module load.

function Get-LlamaCppInstallRoot {
    return (Join-Path $HOME ".local-llm\llama-cpp")
}

function Find-LlamaServerExe {
    # 1) explicit path from catalog/settings
    $configured = $script:Cfg.LlamaCppServerPath
    if (-not [string]::IsNullOrWhiteSpace($configured) -and (Test-Path $configured)) {
        return $configured
    }

    # 2) install root
    $defaultPath = Join-Path (Get-LlamaCppInstallRoot) "llama-server.exe"
    if (Test-Path $defaultPath) {
        return $defaultPath
    }

    # 3) PATH
    $cmd = Get-Command llama-server.exe -ErrorAction SilentlyContinue
    if ($cmd) {
        return $cmd.Source
    }

    return $null
}

function Find-LlamaBenchExe {
    $defaultPath = Join-Path (Get-LlamaCppInstallRoot) "llama-bench.exe"
    if (Test-Path $defaultPath) {
        return $defaultPath
    }

    $cmd = Get-Command llama-bench.exe -ErrorAction SilentlyContinue
    if ($cmd) {
        return $cmd.Source
    }

    return $null
}

function Find-LlamaPerplexityExe {
    $defaultPath = Join-Path (Get-LlamaCppInstallRoot) "llama-perplexity.exe"
    if (Test-Path $defaultPath) {
        return $defaultPath
    }

    $cmd = Get-Command llama-perplexity.exe -ErrorAction SilentlyContinue
    if ($cmd) {
        return $cmd.Source
    }

    return $null
}

function Find-LlamaPerplexityTurboquantExe {
    # Searches for llama-perplexity.exe under the turboquant install root.
    # The turboquant ZIP layout isn't fixed (releases sometimes nest under a
    # version folder), so we glob recursively.
    $root = Get-LlamaCppTurboquantInstallRoot
    if (-not (Test-Path $root)) { return $null }

    $hit = Get-ChildItem -Path $root -Filter 'llama-perplexity.exe' -Recurse -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($hit) { return $hit.FullName }

    return $null
}

function Get-LlamaCppGpuVariant {
    # Returns 'cuda' | 'vulkan' | 'cpu' based on configured override or
    # auto-detection. CUDA is preferred when nvidia-smi works; vulkan covers
    # AMD/Intel where Vulkan is broadly available; cpu is the safe fallback.
    if ($script:Cfg.Contains("LlamaCppVariant") -and -not [string]::IsNullOrWhiteSpace($script:Cfg.LlamaCppVariant)) {
        $v = $script:Cfg.LlamaCppVariant.ToLowerInvariant()
        if ($v -in @('cuda', 'vulkan', 'cpu')) {
            return $v
        }
    }

    $info = Get-LocalLLMVRAMInfo
    if ($info.Source -eq 'auto') {
        return 'cuda'
    }

    if (Get-Command vulkaninfo -ErrorAction SilentlyContinue) {
        return 'vulkan'
    }

    return 'cpu'
}

function Get-LlamaCppLatestRelease {
    # Hits the public GitHub API. Returns the parsed JSON object or throws.
    $headers = @{ "User-Agent" = "LocalLLMProfile/1.0"; "Accept" = "application/vnd.github+json" }
    $url = "https://api.github.com/repos/ggerganov/llama.cpp/releases/latest"

    $oldProgress = $global:ProgressPreference
    $global:ProgressPreference = 'SilentlyContinue'
    try {
        return Invoke-RestMethod -Uri $url -Headers $headers -TimeoutSec 30
    }
    finally {
        $global:ProgressPreference = $oldProgress
    }
}

function Select-LlamaCppReleaseAsset {
    param(
        [Parameter(Mandatory = $true)]$Release,
        [Parameter(Mandatory = $true)][string]$Variant
    )

    # Asset names follow `llama-bXXXX-bin-win-<variant>-x64.zip`. Variants we
    # care about (in order of preference per requested kind):
    #   cuda   -> cuda-*, cu*, then any cuda
    #   vulkan -> vulkan
    #   cpu    -> avx2, then avx512, then avx, then noavx
    $assets = @($Release.assets | Where-Object {
        $_.name -match '\.zip$' -and $_.name -match 'win' -and $_.name -notmatch 'cudart'
    })

    if ($assets.Count -eq 0) {
        throw "No Windows ZIP assets found in latest llama.cpp release."
    }

    $patterns = switch ($Variant) {
        'cuda'   { @('-cuda-12','-cuda-11','-cuda') }
        'vulkan' { @('-vulkan') }
        'cpu'    { @('-avx2-','-avx512-','-avx-','-noavx-') }
        default  { @('-cpu') }
    }

    foreach ($pat in $patterns) {
        $hit = $assets | Where-Object { $_.name -match [regex]::Escape($pat) } | Select-Object -First 1
        if ($hit) { return $hit }
    }

    throw "No matching $Variant asset found in release $($Release.tag_name). Available: $((@($assets | ForEach-Object { $_.name })) -join ', ')"
}

function Select-LlamaCppCudartAsset {
    param([Parameter(Mandatory = $true)]$Release)

    return @($Release.assets | Where-Object { $_.name -match '^cudart-' -and $_.name -match 'win' }) | Select-Object -First 1
}

function Install-LlamaServerNative {
    [CmdletBinding()]
    param([switch]$Force)

    $installRoot = Get-LlamaCppInstallRoot
    Ensure-Directory $installRoot

    $serverPath = Join-Path $installRoot "llama-server.exe"
    if (-not $Force -and (Test-Path $serverPath)) {
        Write-Host "llama-server already installed: $serverPath" -ForegroundColor DarkGray
        return $serverPath
    }

    $variant = Get-LlamaCppGpuVariant
    Write-Host "Resolving latest llama.cpp release ($variant)..." -ForegroundColor Cyan

    $release = Get-LlamaCppLatestRelease
    $asset = Select-LlamaCppReleaseAsset -Release $release -Variant $variant

    Write-Host "Downloading $($asset.name) ($([math]::Round($asset.size / 1MB, 1)) MB)..." -ForegroundColor Cyan

    $tmpZip = Join-Path $env:TEMP $asset.name
    if (Test-Path $tmpZip) { Remove-Item $tmpZip -Force }

    $oldProgress = $global:ProgressPreference
    $global:ProgressPreference = 'SilentlyContinue'
    try {
        Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $tmpZip -UseBasicParsing -TimeoutSec 600
    }
    finally {
        $global:ProgressPreference = $oldProgress
    }

    Write-Host "Extracting to $installRoot..." -ForegroundColor Cyan
    Expand-Archive -LiteralPath $tmpZip -DestinationPath $installRoot -Force
    Remove-Item $tmpZip -Force -ErrorAction SilentlyContinue

    # Some archives nest binaries under a folder; flatten executable tools if needed.
    if (-not (Test-Path $serverPath)) {
        $found = Get-ChildItem -Path $installRoot -Filter "llama-server.exe" -Recurse -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($found) {
            $sourceDir = Split-Path -Parent $found.FullName
            Get-ChildItem -Path $sourceDir -Filter "*.exe" -File | ForEach-Object {
                Move-Item -LiteralPath $_.FullName -Destination $installRoot -Force
            }
        }
    }

    if (-not (Test-Path $serverPath)) {
        throw "Extraction completed but llama-server.exe was not found under $installRoot."
    }
    if (-not (Test-Path (Join-Path $installRoot "llama-bench.exe"))) {
        Write-Warning "llama-bench.exe was not found in the installed llama.cpp archive; tuner will fall back to server probes."
    }
    if (-not (Test-Path (Join-Path $installRoot "llama-perplexity.exe"))) {
        Write-Warning "llama-perplexity.exe was not found in the installed llama.cpp archive; KV quality checks will be unavailable."
    }

    "$($release.tag_name)`n$variant" | Set-Content -LiteralPath (Join-Path $installRoot ".build-stamp") -Encoding utf8

    if ($variant -eq 'cuda') {
        $cudartAsset = Select-LlamaCppCudartAsset -Release $release
        if ($cudartAsset) {
            $cudartZip = Join-Path $env:TEMP $cudartAsset.name
            if (Test-Path $cudartZip) { Remove-Item $cudartZip -Force }
            Write-Host "Downloading CUDA runtime ($($cudartAsset.name))..." -ForegroundColor Cyan
            $oldProgress = $global:ProgressPreference
            $global:ProgressPreference = 'SilentlyContinue'
            try {
                Invoke-WebRequest -Uri $cudartAsset.browser_download_url -OutFile $cudartZip -UseBasicParsing -TimeoutSec 600
            }
            finally {
                $global:ProgressPreference = $oldProgress
            }
            Expand-Archive -LiteralPath $cudartZip -DestinationPath $installRoot -Force
            Remove-Item $cudartZip -Force -ErrorAction SilentlyContinue
        }
    }

    Write-Host "Installed llama-server: $serverPath" -ForegroundColor Green
    return $serverPath
}

function Ensure-LlamaServerNative {
    # Returns the resolved path to llama-server.exe, installing it if absent
    # (after asking once). Throws if the user declines.
    param([switch]$NonInteractive)

    $existing = Find-LlamaServerExe
    if ($existing) { return $existing }

    if ($NonInteractive) {
        return Install-LlamaServerNative
    }

    Write-Host ""
    Write-Host "llama-server is not installed." -ForegroundColor Yellow
    Write-Host "  Source: github.com/ggerganov/llama.cpp releases" -ForegroundColor DarkGray
    Write-Host "  Target: $(Get-LlamaCppInstallRoot)" -ForegroundColor DarkGray
    $answer = (Read-Host "Download and install now? [Y/n]").Trim().ToLowerInvariant()

    if ($answer -in @("n", "no")) {
        throw "llama-server is required for the llama.cpp backend. Aborted."
    }

    return Install-LlamaServerNative
}

function Ensure-LlamaBenchExe {
    param([switch]$NonInteractive)

    $existing = Find-LlamaBenchExe
    if ($existing) { return $existing }

    if ($NonInteractive) {
        Install-LlamaServerNative -Force | Out-Null
        $installed = Find-LlamaBenchExe
        if ($installed) { return $installed }
        return $null
    }

    Write-Host ""
    Write-Host "llama-bench is not installed." -ForegroundColor Yellow
    Write-Host "  It ships in the same upstream llama.cpp archive as llama-server." -ForegroundColor DarkGray
    $answer = (Read-Host "Download/reinstall llama.cpp tools now? [Y/n]").Trim().ToLowerInvariant()

    if ($answer -in @("n", "no")) {
        return $null
    }

    Install-LlamaServerNative -Force | Out-Null
    return (Find-LlamaBenchExe)
}

function Ensure-LlamaPerplexityExe {
    param(
        [switch]$NonInteractive,
        [ValidateSet('native','turboquant','mtpturbo')][string]$Mode = 'native'
    )

    if ($Mode -eq 'turboquant') {
        $existing = Find-LlamaPerplexityTurboquantExe
    } elseif ($Mode -eq 'mtpturbo') {
        $existing = Find-MtpTurboPerplexityExe
    } else {
        $existing = Find-LlamaPerplexityExe
    }
    if ($existing) { return $existing }

    # mtpturbo is BYO-binary; we never download and never prompt. If the tools
    # aren't present, perplexity-based KV quality checks are simply unavailable.
    if ($Mode -eq 'mtpturbo') {
        if (-not $NonInteractive) {
            Write-Host ""
            Write-Host "llama-perplexity (mtpturbo) is not installed." -ForegroundColor Yellow
            Write-Host "  Build EsmaeelNabil/llama.cpp branch feat/mtp-turboquant-kv-cache" -ForegroundColor DarkGray
            Write-Host "  and place llama-perplexity.exe under: $(Get-LlamaCppMtpTurboInstallRoot)" -ForegroundColor DarkGray
        }
        return $null
    }

    if ($Mode -eq 'turboquant' -and $NonInteractive) {
        Install-LlamaServerTurboquant -Force | Out-Null
        $installed = Find-LlamaPerplexityTurboquantExe
        if ($installed) { return $installed }
        return $null
    }

    if ($Mode -eq 'turboquant' -and -not $NonInteractive) {
        Write-Host ""
        Write-Host "llama-perplexity (turboquant) is not installed." -ForegroundColor Yellow
        Write-Host "  Source: github.com/$(Get-LlamaCppTurboquantRepo)/releases/latest" -ForegroundColor DarkGray
        Write-Host "  Target: $(Get-LlamaCppTurboquantInstallRoot)" -ForegroundColor DarkGray
        $answer = (Read-Host "Download/reinstall turboquant llama.cpp tools now? [Y/n]").Trim().ToLowerInvariant()

        if ($answer -in @("n", "no")) {
            return $null
        }
    }

    if ($Mode -eq 'turboquant') {
        Install-LlamaServerTurboquant -Force | Out-Null
        return (Find-LlamaPerplexityTurboquantExe)
    }

    if ($NonInteractive) {
        Install-LlamaServerNative -Force | Out-Null
        $installed = Find-LlamaPerplexityExe
        if ($installed) { return $installed }
        return $null
    }

    Write-Host ""
    Write-Host "llama-perplexity is not installed." -ForegroundColor Yellow
    Write-Host "  It ships in the same upstream llama.cpp archive as llama-server." -ForegroundColor DarkGray
    $answer = (Read-Host "Download/reinstall llama.cpp tools now? [Y/n]").Trim().ToLowerInvariant()

    if ($answer -in @("n", "no")) {
        return $null
    }

    Install-LlamaServerNative -Force | Out-Null
    return (Find-LlamaPerplexityExe)
}

function Get-LlamaCppTurboquantInstallRoot {
    $root = $script:Cfg.LlamaCppTurboquantRoot
    if ([string]::IsNullOrWhiteSpace($root)) {
        $root = Join-Path $HOME ".local-llm\llama-cpp-turboquant"
    }
    return $root
}

function Find-TurboquantServerExe {
    # Searches for llama-server.exe under the turboquant install root. The
    # turboquant ZIP layout isn't fixed (releases sometimes nest under a
    # version folder), so we glob recursively.
    $root = Get-LlamaCppTurboquantInstallRoot
    if (-not (Test-Path $root)) { return $null }

    $hit = Get-ChildItem -Path $root -Filter 'llama-server.exe' -Recurse -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($hit) { return $hit.FullName }

    return $null
}

function Get-LlamaCppTurboquantRepo {
    $repo = $script:Cfg.LlamaCppTurboquantRepo
    if ([string]::IsNullOrWhiteSpace($repo)) { $repo = "TheTom/llama-cpp-turboquant" }
    return $repo
}

function Get-LlamaCppTurboquantLatestRelease {
    $headers = @{ "User-Agent" = "LocalLLMProfile/1.0"; "Accept" = "application/vnd.github+json" }
    $url = "https://api.github.com/repos/$(Get-LlamaCppTurboquantRepo)/releases/latest"

    $oldProgress = $global:ProgressPreference
    $global:ProgressPreference = 'SilentlyContinue'
    try {
        return Invoke-RestMethod -Uri $url -Headers $headers -TimeoutSec 30
    }
    finally {
        $global:ProgressPreference = $oldProgress
    }
}

function Select-TurboquantReleaseAsset {
    # Turboquant currently ships Windows-x64-CUDA only on the win side.
    # Match the windows zip with -cuda in the name; reject the macOS asset.
    param([Parameter(Mandatory = $true)]$Release)

    $hit = $Release.assets | Where-Object {
        $_.name -match '\.zip$' -and $_.name -match 'windows' -and $_.name -match 'cuda'
    } | Select-Object -First 1

    if (-not $hit) {
        $names = (@($Release.assets | ForEach-Object { $_.name })) -join ', '
        throw "No Windows CUDA turboquant asset found in release $($Release.tag_name). Available: $names"
    }

    return $hit
}

function Install-LlamaServerTurboquant {
    [CmdletBinding()]
    param([switch]$Force)

    $installRoot = Get-LlamaCppTurboquantInstallRoot
    Ensure-Directory $installRoot

    $existing = Find-TurboquantServerExe
    if (-not $Force -and $existing) {
        Write-Host "Turboquant llama-server already installed: $existing" -ForegroundColor DarkGray
        return $existing
    }

    Write-Host "Resolving latest turboquant release ($(Get-LlamaCppTurboquantRepo))..." -ForegroundColor Cyan

    $release = Get-LlamaCppTurboquantLatestRelease
    $asset = Select-TurboquantReleaseAsset -Release $release

    $sizeMB = [math]::Round($asset.size / 1MB, 1)
    Write-Host "Asset: $($asset.name)  ($sizeMB MB)" -ForegroundColor DarkGray
    Write-Host "Note: turboquant currently ships only a CUDA 12.4 x64 build for Windows." -ForegroundColor DarkYellow

    $tmpZip = Join-Path $env:TEMP $asset.name
    if (Test-Path $tmpZip) { Remove-Item $tmpZip -Force }

    # Free-disk sanity: need ~ asset size unzipped + ~ asset size for the zip.
    $drive = (Split-Path -Qualifier $installRoot)
    if ($drive) {
        $free = (Get-PSDrive -Name $drive.TrimEnd(':') -ErrorAction SilentlyContinue).Free
        if ($free -and $free -lt ($asset.size * 2)) {
            Write-Warning "Low disk: $([math]::Round($free / 1GB, 1)) GB free on $drive (need ~$([math]::Round($asset.size * 2 / 1GB, 1)) GB for ZIP + extracted files)."
        }
    }

    Write-Host "Downloading $sizeMB MB to $tmpZip..." -ForegroundColor Cyan
    $oldProgress = $global:ProgressPreference
    $global:ProgressPreference = 'SilentlyContinue'
    try {
        Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $tmpZip -UseBasicParsing -TimeoutSec 1800
    }
    finally {
        $global:ProgressPreference = $oldProgress
    }

    Write-Host "Extracting to $installRoot..." -ForegroundColor Cyan
    Expand-Archive -LiteralPath $tmpZip -DestinationPath $installRoot -Force
    Remove-Item $tmpZip -Force -ErrorAction SilentlyContinue

    $serverPath = Find-TurboquantServerExe
    if (-not $serverPath) {
        throw "Extracted turboquant archive but llama-server.exe was not found anywhere under $installRoot. The release layout may have changed."
    }

    "$($release.tag_name)" | Set-Content -LiteralPath (Join-Path $installRoot ".build-stamp") -Encoding utf8

    Repair-TurboquantOpenSslDeps -InstallDir (Split-Path -Parent $serverPath)

    Write-Host "Installed turboquant llama-server: $serverPath" -ForegroundColor Green
    return $serverPath
}

function Repair-TurboquantOpenSslDeps {
    # Some turboquant builds link OpenSSL but ship without libcrypto/libssl/zlib.
    # If the install dir is missing them, copy from common system locations
    # (Git for Windows is the reliable bet on most dev machines). Idempotent:
    # files already present are left alone.
    param([Parameter(Mandatory = $true)][string]$InstallDir)

    $needed = @('libcrypto-3-x64.dll', 'libssl-3-x64.dll', 'zlib1.dll')

    $missing = @($needed | Where-Object { -not (Test-Path (Join-Path $InstallDir $_)) })
    if ($missing.Count -eq 0) { return }

    $sourceDirs = @(
        (Join-Path ${env:ProgramFiles}      'Git\mingw64\bin'),
        (Join-Path ${env:ProgramFiles(x86)} 'Git\mingw64\bin'),
        (Join-Path ${env:ProgramFiles}      'OpenSSL-Win64\bin'),
        (Join-Path ${env:ProgramFiles}      'OpenSSL\bin')
    ) | Where-Object { -not [string]::IsNullOrWhiteSpace($_) -and (Test-Path $_) }

    foreach ($dll in $missing) {
        $copied = $false
        foreach ($dir in $sourceDirs) {
            $src = Join-Path $dir $dll
            if (Test-Path $src) {
                Copy-Item -LiteralPath $src -Destination $InstallDir -Force -ErrorAction SilentlyContinue
                if (Test-Path (Join-Path $InstallDir $dll)) {
                    Write-Host "Copied missing dependency $dll from $dir" -ForegroundColor DarkGreen
                    $copied = $true
                    break
                }
            }
        }
        if (-not $copied) {
            Write-Warning "Could not locate $dll. Install Git for Windows or copy the DLL manually into $InstallDir."
        }
    }
}

function Ensure-LlamaServerTurboquant {
    # Returns the resolved path to the turboquant llama-server.exe, installing
    # it if absent (after asking once). Throws if the user declines.
    param([switch]$NonInteractive)

    $existing = Find-TurboquantServerExe
    if ($existing) {
        Repair-TurboquantOpenSslDeps -InstallDir (Split-Path -Parent $existing)
        return $existing
    }

    if ($NonInteractive) {
        return Install-LlamaServerTurboquant
    }

    Write-Host ""
    Write-Host "turboquant llama-server is not installed." -ForegroundColor Yellow
    Write-Host "  Source: github.com/$(Get-LlamaCppTurboquantRepo)/releases/latest" -ForegroundColor DarkGray
    Write-Host "  Target: $(Get-LlamaCppTurboquantInstallRoot)" -ForegroundColor DarkGray
    Write-Host "  Note  : ~700 MB download (Windows x64 CUDA 12.4 only)" -ForegroundColor DarkGray
    $answer = (Read-Host "Download and install now? [Y/n]").Trim().ToLowerInvariant()

    if ($answer -in @("n", "no")) {
        throw "turboquant is required for the llama.cpp turboquant backend. Aborted."
    }

    return Install-LlamaServerTurboquant
}

# ---- mtpturbo (BYO binary): MTP + turboquant in a single build -----------------
# No release binaries exist for forks that combine MTP and turboquant KV cache,
# so this path is build-from-source. If the user's machine has the toolchain
# we offer to clone + compile + install end-to-end; otherwise we name the
# missing prereqs and the exact winget commands to install them.

# Default upstream + branch. Override via settings if you fork it.
$script:LlamaCppMtpTurboRepoDefault   = 'EsmaeelNabil/llama.cpp'
$script:LlamaCppMtpTurboBranchDefault = 'feat/mtp-turboquant-kv-cache'

function Get-LlamaCppMtpTurboInstallRoot {
    $root = $script:Cfg.LlamaCppMtpTurboRoot
    if ([string]::IsNullOrWhiteSpace($root)) {
        $root = Join-Path $HOME ".local-llm\llama-cpp-mtpturbo"
    }
    return $root
}

function Get-LlamaCppMtpTurboSourceRoot {
    return (Join-Path $HOME ".local-llm\src\llama-cpp-mtpturbo")
}

function Get-LlamaCppMtpTurboRepo {
    $repo = $script:Cfg.LlamaCppMtpTurboRepo
    if ([string]::IsNullOrWhiteSpace($repo)) { return $script:LlamaCppMtpTurboRepoDefault }
    return $repo
}

function Get-LlamaCppMtpTurboBranch {
    $branch = $script:Cfg.LlamaCppMtpTurboBranch
    if ([string]::IsNullOrWhiteSpace($branch)) { return $script:LlamaCppMtpTurboBranchDefault }
    return $branch
}

function Find-MtpTurboServerExe {
    $root = Get-LlamaCppMtpTurboInstallRoot
    if (-not (Test-Path $root)) { return $null }

    $hit = Get-ChildItem -Path $root -Filter 'llama-server.exe' -Recurse -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($hit) { return $hit.FullName }

    return $null
}

function Find-MtpTurboPerplexityExe {
    $root = Get-LlamaCppMtpTurboInstallRoot
    if (-not (Test-Path $root)) { return $null }

    $hit = Get-ChildItem -Path $root -Filter 'llama-perplexity.exe' -Recurse -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($hit) { return $hit.FullName }

    return $null
}

function Find-CudaToolkitRoot {
    # Returns the highest-version CUDA Toolkit install with nvcc.exe present.
    # Honors $env:CUDA_PATH first.
    if (-not [string]::IsNullOrWhiteSpace($env:CUDA_PATH) -and (Test-Path (Join-Path $env:CUDA_PATH 'bin\nvcc.exe'))) {
        return $env:CUDA_PATH
    }

    $base = 'C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA'
    if (-not (Test-Path $base)) { return $null }

    $candidate = Get-ChildItem -Path $base -Directory -ErrorAction SilentlyContinue |
        Where-Object { Test-Path (Join-Path $_.FullName 'bin\nvcc.exe') } |
        Sort-Object Name -Descending |
        Select-Object -First 1

    if ($candidate) { return $candidate.FullName }
    return $null
}

function Find-MsvcBuildEnv {
    # Returns @{ VcVars = '...vcvars64.bat'; InstallPath = '...' } or $null.
    #
    # Detection order, stops at first install whose vcvars64.bat exists on disk:
    #   1. $env:LOCALBOX_VCVARS        -- explicit override (full path to vcvars64.bat)
    #   2. vswhere -requires VC.Tools.x86.x64    (narrow: x64 toolset component)
    #   3. vswhere -requires Workload.VCTools    (broader: C++ workload, covers Build Tools)
    #   4. vswhere with no -requires             (any VS/Build Tools install, probe each)
    #   5. Filesystem scan of standard install roots (no vswhere case)

    $probe = {
        param($installPath)
        if ([string]::IsNullOrWhiteSpace($installPath)) { return $null }
        $vc = Join-Path $installPath 'VC\Auxiliary\Build\vcvars64.bat'
        if (Test-Path $vc) { return @{ VcVars = $vc; InstallPath = $installPath } }
        return $null
    }

    # 1. Explicit override.
    if (-not [string]::IsNullOrWhiteSpace($env:LOCALBOX_VCVARS) -and (Test-Path $env:LOCALBOX_VCVARS)) {
        $vc = (Resolve-Path $env:LOCALBOX_VCVARS).Path
        $installPath = Split-Path -Parent (Split-Path -Parent (Split-Path -Parent (Split-Path -Parent $vc)))
        return @{ VcVars = $vc; InstallPath = $installPath }
    }

    # 2-4. vswhere strategies, narrowest first.
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (Test-Path $vswhere) {
        $strategies = @(
            @('-latest', '-products', '*', '-requires', 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64', '-property', 'installationPath'),
            @('-latest', '-products', '*', '-requires', 'Microsoft.VisualStudio.Workload.VCTools',           '-property', 'installationPath'),
            @(           '-products', '*',                                                                   '-property', 'installationPath')
        )
        foreach ($argList in $strategies) {
            $paths = & $vswhere @argList 2>$null
            foreach ($p in $paths) {
                $hit = & $probe $p
                if ($hit) { return $hit }
            }
        }
    }

    # 5. Filesystem fallback. Newer year first, then Enterprise > Pro > Community > BuildTools.
    $editionRank = @{ 'Enterprise' = 0; 'Professional' = 1; 'Community' = 2; 'BuildTools' = 3 }
    $candidates = New-Object System.Collections.Generic.List[object]
    foreach ($base in @($env:ProgramFiles, ${env:ProgramFiles(x86)})) {
        if ([string]::IsNullOrWhiteSpace($base)) { continue }
        $vsroot = Join-Path $base 'Microsoft Visual Studio'
        if (-not (Test-Path $vsroot)) { continue }
        foreach ($yearDir in Get-ChildItem -Path $vsroot -Directory -ErrorAction SilentlyContinue) {
            foreach ($editionDir in Get-ChildItem -Path $yearDir.FullName -Directory -ErrorAction SilentlyContinue) {
                $candidates.Add([pscustomobject]@{
                    Path    = $editionDir.FullName
                    Year    = $yearDir.Name
                    Edition = $editionDir.Name
                }) | Out-Null
            }
        }
    }
    $sorted = $candidates | Sort-Object `
        @{ Expression = 'Year'; Descending = $true }, `
        @{ Expression = { if ($editionRank.ContainsKey($_.Edition)) { $editionRank[$_.Edition] } else { 999 } }; Descending = $false }
    foreach ($c in $sorted) {
        $hit = & $probe $c.Path
        if ($hit) { return $hit }
    }

    return $null
}

function Test-LlamaCppMtpTurboBuildPrereqs {
    # Probe for everything the build script needs. Returns @{ Ok; Missing; Found }.
    $found   = [ordered]@{}
    $missing = New-Object System.Collections.Generic.List[string]

    $git = Get-Command git -ErrorAction SilentlyContinue
    if ($git) { $found['git'] = $git.Source } else { $missing.Add('git') | Out-Null }

    $cmake = Get-Command cmake -ErrorAction SilentlyContinue
    if ($cmake) { $found['cmake'] = $cmake.Source } else { $missing.Add('cmake') | Out-Null }

    $ninja = Get-Command ninja -ErrorAction SilentlyContinue
    if ($ninja) { $found['ninja'] = $ninja.Source } else { $missing.Add('ninja') | Out-Null }

    $cuda = Find-CudaToolkitRoot
    if ($cuda) { $found['cuda'] = $cuda } else { $missing.Add('cuda-toolkit') | Out-Null }

    $msvc = Find-MsvcBuildEnv
    if ($msvc) { $found['msvc'] = $msvc.InstallPath } else { $missing.Add('msvc-buildtools') | Out-Null }

    return @{
        Ok      = ($missing.Count -eq 0)
        Missing = @($missing)
        Found   = $found
    }
}

function Write-MtpTurboPrereqGuidance {
    param([Parameter(Mandatory = $true)][string[]]$Missing)

    Write-Host ""
    Write-Host "Missing build prerequisites for mtpturbo:" -ForegroundColor Yellow
    foreach ($m in $Missing) {
        switch ($m) {
            'git'            { Write-Host "  - git           : winget install --id Git.Git"           -ForegroundColor DarkGray }
            'cmake'          { Write-Host "  - cmake         : winget install --id Kitware.CMake"     -ForegroundColor DarkGray }
            'ninja'          { Write-Host "  - ninja         : winget install --id Ninja-build.Ninja" -ForegroundColor DarkGray }
            'cuda-toolkit'   { Write-Host "  - CUDA Toolkit  : https://developer.nvidia.com/cuda-12-4-0-download-archive (12.4 recommended; components: nvcc, cudart, cublas)" -ForegroundColor DarkGray }
            'msvc-buildtools'{ Write-Host "  - VS BuildTools : winget install --id Microsoft.VisualStudio.2022.BuildTools --override `"--add Microsoft.VisualStudio.Workload.VCTools --includeRecommended`"" -ForegroundColor DarkGray }
        }
    }
    Write-Host ""
    Write-Host "After installing, re-launch this command. The build needs ~5 GB free disk and 15-30 min on a typical 4090 box." -ForegroundColor DarkGray
}

function Get-LocalNvidiaComputeCapability {
    # "8.9" for RTX 4090. Returns $null if nvidia-smi is unavailable.
    if (-not (Get-Command nvidia-smi -ErrorAction SilentlyContinue)) { return $null }

    try {
        $out = & nvidia-smi --query-gpu=compute_cap --format=csv,noheader 2>$null | Select-Object -First 1
        if ($out) { return $out.Trim() }
    } catch {}
    return $null
}

function Build-LlamaServerMtpTurbo {
    # Clones (or pulls) the configured fork+branch, configures + builds with
    # CUDA + Ninja for the local GPU's compute capability, installs the
    # resulting binaries + CUDA runtime DLLs into the install root, and writes
    # a .build-stamp. Throws on any failure.
    [CmdletBinding()]
    param()

    $prereqs = Test-LlamaCppMtpTurboBuildPrereqs
    if (-not $prereqs.Ok) {
        Write-MtpTurboPrereqGuidance -Missing $prereqs.Missing
        throw "Cannot build mtpturbo: missing $($prereqs.Missing -join ', ')"
    }

    $repo   = Get-LlamaCppMtpTurboRepo
    $branch = Get-LlamaCppMtpTurboBranch
    $srcRoot = Get-LlamaCppMtpTurboSourceRoot
    $installRoot = Get-LlamaCppMtpTurboInstallRoot
    Ensure-Directory (Split-Path -Parent $srcRoot)
    Ensure-Directory $installRoot

    Write-Host ""
    Write-Host "Building mtpturbo llama.cpp from source." -ForegroundColor Cyan
    Write-Host "  Repo    : github.com/$repo (branch $branch)" -ForegroundColor DarkGray
    Write-Host "  Source  : $srcRoot" -ForegroundColor DarkGray
    Write-Host "  Install : $installRoot" -ForegroundColor DarkGray

    # Clone or pull.
    if (Test-Path (Join-Path $srcRoot '.git')) {
        Write-Host "Updating existing clone..." -ForegroundColor Cyan
        & git -C $srcRoot fetch --depth 1 origin $branch 2>&1 | Out-Host
        if ($LASTEXITCODE -ne 0) { throw "git fetch failed" }
        & git -C $srcRoot checkout FETCH_HEAD 2>&1 | Out-Host
        if ($LASTEXITCODE -ne 0) { throw "git checkout failed" }
    } else {
        Write-Host "Cloning $repo#$branch (shallow)..." -ForegroundColor Cyan
        & git clone --depth 1 -b $branch "https://github.com/$repo.git" $srcRoot 2>&1 | Out-Host
        if ($LASTEXITCODE -ne 0) { throw "git clone failed" }
    }

    $headSha = (& git -C $srcRoot rev-parse --short HEAD).Trim()

    # Resolve compute capability — fallback to a broad set if nvidia-smi is missing.
    $cc = Get-LocalNvidiaComputeCapability
    $cudaArch = if ($cc) { ($cc -replace '\.', '') + '-real' } else { '75-virtual;80-real;86-real;89-real' }
    Write-Host "  GPU arch: $cudaArch (detected compute_cap=$cc)" -ForegroundColor DarkGray

    $cudaRoot = $prereqs.Found['cuda']
    $msvc = Find-MsvcBuildEnv
    $vcvars = $msvc.VcVars

    # Wrap configure+build in a single .cmd so vcvars64 + cmake share an
    # environment. Avoids inheriting a half-set %PATH% from PowerShell.
    $buildScript = Join-Path $srcRoot '.localbox-build-mtpturbo.cmd'
    $scriptBody = @"
@echo off
setlocal enableextensions
cd /d "%~dp0"

call "$vcvars"
if errorlevel 1 exit /b 12

set "PATH=$cudaRoot\bin;%PATH%"

if exist build rmdir /s /q build

cmake -B build -G Ninja -DCMAKE_BUILD_TYPE=Release -DGGML_CUDA=ON -DLLAMA_CURL=OFF -DBUILD_SHARED_LIBS=ON -DGGML_NATIVE=OFF -DCMAKE_CUDA_ARCHITECTURES=$cudaArch -DCMAKE_CUDA_FLAGS=-allow-unsupported-compiler -DCMAKE_CUDA_COMPILER="$cudaRoot\bin\nvcc.exe"
if errorlevel 1 exit /b 2

cmake --build build --config Release --target llama-server llama-bench llama-perplexity -- -j 0
if errorlevel 1 exit /b 3

exit /b 0
"@
    Set-Content -LiteralPath $buildScript -Value $scriptBody -Encoding ASCII

    Write-Host "Building (15-30 min; output streaming below)..." -ForegroundColor Cyan
    & cmd.exe /c "`"$buildScript`"" | Out-Host
    if ($LASTEXITCODE -ne 0) {
        throw "Build failed with exit code $LASTEXITCODE. Inspect $srcRoot\build\ for details."
    }

    # Install: copy project binaries + CUDA runtime DLLs into the install root.
    $binDir = Join-Path $srcRoot 'build\bin'
    if (-not (Test-Path (Join-Path $binDir 'llama-server.exe'))) {
        throw "Build reported success but llama-server.exe is missing under $binDir."
    }

    Get-ChildItem -Path $binDir -Include '*.exe','*.dll' -File | ForEach-Object {
        Copy-Item -LiteralPath $_.FullName -Destination $installRoot -Force
    }

    foreach ($dll in @('cudart64_12.dll','cublas64_12.dll','cublasLt64_12.dll')) {
        $candidate = Join-Path $cudaRoot "bin\$dll"
        if (Test-Path $candidate) {
            Copy-Item -LiteralPath $candidate -Destination $installRoot -Force
        }
    }

    $stamp = "mtpturbo-$headSha-cuda$(Split-Path -Leaf $cudaRoot)-$cudaArch"
    Set-Content -LiteralPath (Join-Path $installRoot '.build-stamp') -Value $stamp -Encoding utf8

    $serverPath = Join-Path $installRoot 'llama-server.exe'
    Write-Host "Installed mtpturbo llama-server: $serverPath" -ForegroundColor Green
    Write-Host "Build stamp: $stamp" -ForegroundColor DarkGray
    return $serverPath
}

function Ensure-LlamaServerMtpTurbo {
    # Returns the resolved path to the mtpturbo llama-server.exe. If absent
    # and toolchain present: prompt to auto-build. If toolchain missing: print
    # install guidance and throw. -NonInteractive skips both prompts and the
    # auto-build (returns a throw).
    param([switch]$NonInteractive)

    $existing = Find-MtpTurboServerExe
    if ($existing) {
        Repair-TurboquantOpenSslDeps -InstallDir (Split-Path -Parent $existing)
        return $existing
    }

    $root = Get-LlamaCppMtpTurboInstallRoot
    $prereqs = Test-LlamaCppMtpTurboBuildPrereqs

    if (-not $prereqs.Ok) {
        Write-Host ""
        Write-Host "mtpturbo llama-server.exe not found under $root." -ForegroundColor Yellow
        Write-MtpTurboPrereqGuidance -Missing $prereqs.Missing
        throw "mtpturbo is not installed and the toolchain to build it is incomplete (missing: $($prereqs.Missing -join ', '))."
    }

    if ($NonInteractive) {
        throw "mtpturbo llama-server.exe not found under $root. Re-run interactively to auto-build, or run Build-LlamaServerMtpTurbo manually."
    }

    Write-Host ""
    Write-Host "mtpturbo llama-server.exe not found under $root." -ForegroundColor Yellow
    Write-Host "  Build prereqs detected:" -ForegroundColor DarkGray
    foreach ($k in $prereqs.Found.Keys) {
        Write-Host ("    {0,-7} {1}" -f $k, $prereqs.Found[$k]) -ForegroundColor DarkGray
    }
    Write-Host "  Repo: github.com/$(Get-LlamaCppMtpTurboRepo) (branch $(Get-LlamaCppMtpTurboBranch))" -ForegroundColor DarkGray
    Write-Host "  Build takes 15-30 min and needs ~5 GB free disk." -ForegroundColor DarkGray
    $answer = (Read-Host "Build it now? [Y/n]").Trim().ToLowerInvariant()
    if ($answer -in @('n','no')) {
        throw "mtpturbo build declined."
    }

    return (Build-LlamaServerMtpTurbo)
}
