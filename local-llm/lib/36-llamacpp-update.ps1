# llama.cpp binary freshness: check installed build vs upstream and update.
#
#   native      -> github.com/ggerganov/llama.cpp        latest release (download)
#   turboquant  -> github.com/<TurboquantRepo>           latest release (download)
#   mtpturbo    -> EsmaeelNabil/llama.cpp @ <branch>      source build (~15-30 min)
#
# Per the agreed policy: the cheap release downloads (native/turboquant) update
# automatically; the expensive mtpturbo source build is only CHECKED and WARNED
# about unless -Force or $env:LOCALBOX_AUTO_BUILD is set. All checks are network-
# dependent and fully non-fatal: a failed check never blocks tuning.

function Get-LlamaCppInstalledBuildStamp {
    param([Parameter(Mandatory = $true)][ValidateSet('native','turboquant','mtpturbo')][string]$Mode)

    $root = switch ($Mode) {
        'turboquant' { Get-LlamaCppTurboquantInstallRoot }
        'mtpturbo'   { Get-LlamaCppMtpTurboInstallRoot }
        default      { Get-LlamaCppInstallRoot }
    }
    $path = Join-Path $root '.build-stamp'
    if (-not (Test-Path $path)) { return '' }
    try { return (Get-Content -Raw -LiteralPath $path -ErrorAction Stop).Trim() }
    catch { return '' }
}

function Get-LlamaCppMtpTurboRemoteShortSha {
    # Resolves the current HEAD of the configured mtpturbo fork+branch without a
    # full clone. Returns a 7-char short SHA, or '' if git/network is unavailable.
    if (-not (Get-Command git -ErrorAction SilentlyContinue)) { return '' }
    $repo = Get-LlamaCppMtpTurboRepo
    $branch = Get-LlamaCppMtpTurboBranch
    try {
        $line = (& git ls-remote "https://github.com/$repo.git" $branch 2>$null | Select-Object -First 1)
        if ($line) {
            $sha = ($line -split "\s+")[0]
            if ($sha.Length -ge 7) { return $sha.Substring(0, 7) }
        }
    }
    catch {}
    return ''
}

function Update-LlamaCppBinaries {
    # Checks (and for native/turboquant, installs) the latest llama.cpp build for
    # one or all modes. -PreFlight runs quietly and only reports when action is
    # taken/needed (used automatically before a tune). -Force also rebuilds
    # mtpturbo from source when its branch has moved.
    [CmdletBinding()]
    param(
        [ValidateSet('all', 'native', 'turboquant', 'mtpturbo')][string]$Mode = 'all',
        [switch]$Force,
        [switch]$PreFlight,
        [switch]$CheckOnly
    )

    $modes = if ($Mode -eq 'all') { @('native', 'turboquant', 'mtpturbo') } else { @($Mode) }

    foreach ($m in $modes) {
        try {
            if ($m -eq 'mtpturbo') {
                $stamp = Get-LlamaCppInstalledBuildStamp -Mode 'mtpturbo'
                $installedSha = if ($stamp -match '^mtpturbo-([0-9a-fA-F]+)-') { $Matches[1] } else { '' }
                $repo = Get-LlamaCppMtpTurboRepo
                $branch = Get-LlamaCppMtpTurboBranch
                $remoteSha = Get-LlamaCppMtpTurboRemoteShortSha
                if ([string]::IsNullOrWhiteSpace($remoteSha)) {
                    if (-not $PreFlight) { Write-Warning "mtpturbo: could not resolve $repo#$branch HEAD (git/network?); skipping." }
                    continue
                }
                if ([string]::IsNullOrWhiteSpace($installedSha)) {
                    Write-Warning "mtpturbo: not installed. Build $repo#$branch (~15-30 min): update-llama -Mode mtpturbo -Force  (or set LOCALBOX_AUTO_BUILD=1)."
                }
                elseif ($installedSha -ne $remoteSha) {
                    if (($Force -or (Test-LocalBoxAutoBuildEnabled)) -and -not $CheckOnly) {
                        Write-Host "mtpturbo: $installedSha -> $remoteSha on $repo#$branch (rebuilding from source, 15-30 min)..." -ForegroundColor Cyan
                        Build-LlamaServerMtpTurbo | Out-Null
                    }
                    else {
                        Write-Warning "mtpturbo: newer commit available ($installedSha -> $remoteSha on $repo#$branch). Rebuild (~15-30 min) with: update-llama -Mode mtpturbo -Force  (or set LOCALBOX_AUTO_BUILD=1)."
                    }
                }
                elseif (-not $PreFlight) {
                    Write-Host "mtpturbo llama.cpp: up to date ($installedSha)." -ForegroundColor DarkGray
                }
                continue
            }

            # native / turboquant: cheap release download -> auto-update when stale.
            $installed = ((Get-LlamaCppInstalledBuildStamp -Mode $m) -split "\r?\n" | Select-Object -First 1)
            $latest = ''
            try {
                $latest = if ($m -eq 'turboquant') { [string](Get-LlamaCppTurboquantLatestRelease).tag_name } else { [string](Get-LlamaCppLatestRelease).tag_name }
            }
            catch {
                if (-not $PreFlight) { Write-Warning "${m}: could not query latest release ($($_.Exception.Message))." }
                continue
            }
            if ([string]::IsNullOrWhiteSpace($latest)) { continue }

            $current = if ([string]::IsNullOrWhiteSpace($installed)) { '(not installed)' } else { $installed }
            if (-not [string]::IsNullOrWhiteSpace($installed) -and $installed -eq $latest) {
                if (-not $PreFlight) { Write-Host "${m} llama.cpp: up to date ($installed)." -ForegroundColor DarkGray }
            }
            elseif ($CheckOnly) {
                Write-Host "${m} llama.cpp: update available ($current -> $latest). Run: update-llama -Mode $m" -ForegroundColor Yellow
            }
            else {
                Write-Host "${m} llama.cpp: $current -> $latest (updating)..." -ForegroundColor Cyan
                if ($m -eq 'turboquant') { Install-LlamaServerTurboquant -Force | Out-Null } else { Install-LlamaServerNative -Force | Out-Null }
            }
        }
        catch {
            Write-Warning "llama.cpp update check for '$m' failed: $($_.Exception.Message)"
        }
    }
}

function Invoke-LlamaCppUpdatePreflight {
    # Called before a tune. Honors an opt-out config flag and never throws.
    param([Parameter(Mandatory = $true)][ValidateSet('native', 'turboquant', 'mtpturbo')][string]$Mode)

    if ($script:Cfg -and $script:Cfg.SkipLlamaUpdateCheck) { return }
    try { Update-LlamaCppBinaries -Mode $Mode -PreFlight }
    catch { Write-Warning "llama.cpp pre-flight update check failed (continuing): $($_.Exception.Message)" }
}

function update-llama {
    # User command: check + update llama.cpp builds. Pass -Force to also rebuild
    # mtpturbo from source when its branch has moved.
    [CmdletBinding()]
    param(
        [ValidateSet('all', 'native', 'turboquant', 'mtpturbo')][string]$Mode = 'all',
        [switch]$Force,
        [switch]$CheckOnly
    )
    Update-LlamaCppBinaries -Mode $Mode -Force:$Force -CheckOnly:$CheckOnly
}
