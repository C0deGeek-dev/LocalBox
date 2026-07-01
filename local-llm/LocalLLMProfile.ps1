# =========================
# LocalBox profile entry point
# llama.cpp + Claude Code + LocalPilot
# Windows / PowerShell only — does not work in WSL/bash.
# =========================
#
# Usage:
#   1. Keep this file beside llm-models.json and the lib/ directory.
#   2. Dot-source from your PowerShell profile:
#        . "$HOME\.local-llm\LocalLLMProfile.ps1"
#   3. Reload:
#        . $PROFILE
#
# Code lives in lib/*.ps1, dot-sourced in numeric order. Add new functionality
# by editing the matching lib file (or adding a new numbered one) — keep this
# entry point minimal.
#
# Do not enable top-level StrictMode in a profile.

$script:LLMProfileRoot = if ($PSScriptRoot) { $PSScriptRoot } else { Split-Path -Parent $PROFILE }
$script:LocalLLMConfigPath = if ($env:LOCAL_LLM_CONFIG) {
    $env:LOCAL_LLM_CONFIG
}
else {
    # The model catalog is per-user (gitignored; install.ps1 seeds it). Fall back to
    # the shipped template when running from a checkout that has no seeded catalog,
    # so a fresh clone is still runnable.
    $userCatalog = Join-Path $script:LLMProfileRoot "llm-models.json"
    if (Test-Path -LiteralPath $userCatalog) { $userCatalog } else { Join-Path $script:LLMProfileRoot "llm-models.example.json" }
}

# Dot-source every lib file in numeric prefix order. Dot-sourcing pulls
# functions and $script: variables into THIS file's scope, which is what the
# rest of the codebase expects ($script:Cfg, etc.).
$libDir = Join-Path $script:LLMProfileRoot "lib"

if (-not (Test-Path $libDir)) {
    throw "LocalLLMProfile: lib/ directory not found at $libDir. Reinstall via install.ps1."
}

foreach ($file in (Get-ChildItem -Path $libDir -Filter '*.ps1' | Sort-Object Name)) {
    $sourcePath = $file.FullName

    if ($file.LinkType -eq 'SymbolicLink') {
        $sourcePath = $file.Target
        if ($sourcePath -is [array]) {
            $sourcePath = $sourcePath[0]
        }
    }

    if ([string]::IsNullOrWhiteSpace($sourcePath) -or -not (Test-Path -LiteralPath $sourcePath -PathType Leaf -ErrorAction SilentlyContinue)) {
        Write-Verbose "Skipping unresolved LocalLLM lib file: $($file.FullName)"
        continue
    }

    . $sourcePath
}

# Bootstrap: load config + dependent runtime state, then register per-model
# shortcut functions. Order matters here (these statements EXECUTE at load
# time and depend on functions from the lib files above).
$script:Cfg = Import-LocalLLMConfig
$script:NoThinkProxyPort = [int]$script:Cfg.NoThinkProxyPort

# Warn once per session if the deployed no-think proxy is older than the
# launcher's required version. The check is non-fatal (proxy may still work);
# silent on success. LOCALBOX_SKIP_PROXY_CHECK=1 disables it entirely.
if ($env:LOCALBOX_SKIP_PROXY_CHECK -ne '1' -and (Get-Command Test-LocalLLMProxyVersion -ErrorAction SilentlyContinue)) {
    try { Test-LocalLLMProxyVersion | Out-Null } catch {
        Write-Verbose "Proxy version check failed: $($_.Exception.Message)"
    }
}

Register-ModelShortcuts
