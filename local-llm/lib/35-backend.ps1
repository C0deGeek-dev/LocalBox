# Backend dispatcher. Single entry point for the wizard, per-model shortcuts,
# and entry-point commands so they all reach llama-server through one path.

function Resolve-LlamaCppMode {
    # Falls back to LlamaCppDefaultMode from settings when -Mode is unspecified.
    param([string]$Mode)

    if (-not [string]::IsNullOrWhiteSpace($Mode)) {
        return $Mode.ToLowerInvariant()
    }

    $cfgMode = if ($script:Cfg.Contains('LlamaCppDefaultMode') -and -not [string]::IsNullOrWhiteSpace($script:Cfg.LlamaCppDefaultMode)) {
        [string]$script:Cfg.LlamaCppDefaultMode
    } else {
        'native'
    }

    return $cfgMode.ToLowerInvariant()
}

function Invoke-Backend {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][ValidateSet('launch-claude', 'stop', 'status')][string]$Action,
        [string]$Key,
        [AllowEmptyString()][string]$ContextKey = '',
        [ValidateSet('native', 'turboquant', 'mtpturbo')][string]$LlamaCppMode,
        [string]$KvCacheK,
        [string]$KvCacheV,
        [switch]$LimitTools,
        [switch]$LocalPilot,
        [switch]$Codex,
        [switch]$Strict,
        [switch]$UseVision,
        [switch]$AutoBest,
        [ValidateSet('auto','pure','balanced','short','long')][string]$AutoBestProfile = 'auto',
        [string[]]$ExtraArgs,
        [string[]]$ExtraLocalPilotArgs,
        [AllowEmptyString()][string]$SpecType,
        [int]$SpecDraftNMax,
        [switch]$DryRun
    )

    if ($Action -eq 'stop') {
        Stop-LlamaServer
        return
    }

    if ($Action -eq 'status') {
        Get-LlamaServerStatus
        return
    }

    if ([string]::IsNullOrWhiteSpace($Key)) {
        throw "Invoke-Backend $Action requires -Key."
    }

    $mode = Resolve-LlamaCppMode -Mode $LlamaCppMode
    Start-ClaudeWithLlamaCppModel `
        -Key $Key `
        -ContextKey $ContextKey `
        -Mode $mode `
        -KvCacheK $KvCacheK `
        -KvCacheV $KvCacheV `
        -LimitTools:$LimitTools `
        -LocalPilot:$LocalPilot `
        -Codex:$Codex `
        -Strict:$Strict `
        -UseVision:$UseVision `
        -AutoBest:$AutoBest `
        -AutoBestProfile $AutoBestProfile `
        -ExtraArgs $ExtraArgs `
        -ExtraLocalPilotArgs $ExtraLocalPilotArgs `
        -SpecType $SpecType `
        -SpecDraftNMax $SpecDraftNMax `
        -DryRun:$DryRun
}
