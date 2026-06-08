[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
$profilePath = Join-Path $repoRoot 'local-llm\LocalLLMProfile.ps1'
$fixturePath = Join-Path $PSScriptRoot 'fixtures\autobest\best-q3635ba3b.json'
$tunerDir = Join-Path $HOME '.local-llm\tuner'
$targetPath = Join-Path $tunerDir 'best-q3635ba3b.json'
$backupPath = $null

if (-not (Test-Path $profilePath)) { throw "Profile entry point not found: $profilePath" }
if (-not (Test-Path $fixturePath)) { throw "Fixture not found: $fixturePath" }

try {
    if (-not (Test-Path $tunerDir)) {
        New-Item -ItemType Directory -Path $tunerDir -Force | Out-Null
    }

    if (Test-Path $targetPath) {
        $backupPath = "$targetPath.smoke-backup-$(Get-Date -Format 'yyyyMMddHHmmssfff')"
        Move-Item -LiteralPath $targetPath -Destination $backupPath -Force
    }

    Copy-Item -LiteralPath $fixturePath -Destination $targetPath -Force

    . $profilePath

    $entry = Get-BestLlamaCppConfig -Key 'q3635ba3b' -ContextKey '32k' -Mode 'native' -PromptLength 'short' -Quant 'iq2m' -VramGB 24
    if (-not $entry) { throw 'Get-BestLlamaCppConfig did not load the LocalBench fixture entry.' }
    if ($entry.source -ne 'localbench') { throw "Expected source localbench; got '$($entry.source)'." }

    # Vision fallback: the text-only fixture must NOT match a vision launch by default,
    # but should be returned as a fallback when -AllowVisionFallback is set.
    $visionStrict = Get-BestLlamaCppConfig -Key 'q3635ba3b' -ContextKey '32k' -Mode 'native' -PromptLength 'short' -Quant 'iq2m' -VramGB 24 -Vision $true
    if ($visionStrict) { throw 'Vision launch matched a text-only entry without -AllowVisionFallback.' }
    $visionFallback = Get-BestLlamaCppConfig -Key 'q3635ba3b' -ContextKey '32k' -Mode 'native' -PromptLength 'short' -Quant 'iq2m' -VramGB 24 -Vision $true -AllowVisionFallback
    if (-not $visionFallback) { throw '-AllowVisionFallback did not return the text-only entry for a vision launch.' }
    $preferred = Get-PreferredLlamaCppBestConfig -Key 'q3635ba3b' -ContextKey '32k' -Mode 'native' -Profile 'pure' -Quant 'iq2m' -VramGB 24 -Vision $true -AllowVisionFallback
    if (-not $preferred -or -not $preferred.VisionFallback) {
        throw 'Get-PreferredLlamaCppBestConfig did not flag the text-only entry as VisionFallback under a vision launch.'
    }

    $def = Get-ModelDef -Key 'q3635ba3b'
    $argsParams = @{
        Def = $def
        ContextKey = '32k'
        Mode = 'native'
        ModelArgPath = (Join-Path $env:TEMP 'fixture\model.gguf')
        Port = 18080
        Parallel = 1
        CacheReuse = 256
    }
    foreach ($k in $entry.overrides.Keys) {
        $argsParams[$k] = $entry.overrides[$k]
    }

    $args = Build-LlamaServerArgs @argsParams
    foreach ($expected in @('--cache-type-k', 'q8_0', '--cache-type-v', '--ubatch-size', '512', '--batch-size', '1024')) {
        if ($args -notcontains $expected) {
            throw "Build-LlamaServerArgs output did not contain expected token '$expected'."
        }
    }
    foreach ($expected in @('--reasoning', 'off', '--reasoning-budget', '0', '--reasoning-format', 'none')) {
        if ($args -notcontains $expected) {
            throw "Build-LlamaServerArgs output did not contain strip-mode reasoning token '$expected'."
        }
    }
    foreach ($expected in @('--parallel', '1', '--cache-reuse', '256')) {
        if ($args -notcontains $expected) {
            throw "Build-LlamaServerArgs output did not contain agent-cache token '$expected'."
        }
    }

    [pscustomobject]@{
        LoadedSource = $entry.source
        ArgCount = @($args).Count
        Fixture = $fixturePath
    }
}
finally {
    if (Test-Path $targetPath) {
        Remove-Item -LiteralPath $targetPath -Force
    }
    if ($backupPath -and (Test-Path $backupPath)) {
        Move-Item -LiteralPath $backupPath -Destination $targetPath -Force
    }
}
