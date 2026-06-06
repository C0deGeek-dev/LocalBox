$ErrorActionPreference = 'Stop'
$env:LOCALBOX_SKIP_PROXY_CHECK = '1'

$repoRoot = Split-Path -Parent $PSScriptRoot
. (Join-Path $repoRoot 'local-llm\LocalLLMProfile.ps1')

$status = Get-LocalBoxTuiStatus
if (-not $status -or $status.name -ne 'LocalBox') {
    throw 'Get-LocalBoxTuiStatus did not return a LocalBox status object.'
}

$models = @(Get-LocalBoxTuiModels)
if ($models.Count -eq 0) {
    $models = @(Get-LocalBoxTuiModels -All)
}
if ($models.Count -eq 0) {
    throw 'Get-LocalBoxTuiModels returned no models.'
}

$model = $models[0]
$detail = Get-LocalBoxTuiModelDetail -Key $model.key
if (-not $detail -or -not $detail.summary) {
    throw 'Get-LocalBoxTuiModelDetail did not return summary detail.'
}

$contextKey = if (@($model.contexts).Count -gt 0) { [string]$model.contexts[0].key } else { '' }
$quantKey = if (@($model.quants).Count -gt 0) { [string]$model.quants[0].key } else { '' }
$plan = New-LocalBoxTuiLaunchPlan -Key $model.key -ContextKey $contextKey -Action claude -Mode native
if (-not $plan.launchCommand -or $plan.launchCommand -notmatch 'Invoke-LLMSelection') {
    throw 'New-LocalBoxTuiLaunchPlan did not return a launcher command.'
}

$defaultPlan = New-LocalBoxTuiLaunchPlan -Key $model.key -ContextKey 'default' -Action claude -Mode native
if ($defaultPlan.contextKey -ne '' -or $defaultPlan.launchCommand -notmatch "-ContextKey ''") {
    throw 'New-LocalBoxTuiLaunchPlan did not normalize the default context alias.'
}

$findBestPlan = New-LocalBoxTuiLaunchPlan -Key $model.key -ContextKey $contextKey -Action findbest -Mode native
if ($findBestPlan.launchCommand -notmatch 'Invoke-LocalBoxTuiFindBest' -or $findBestPlan.launchCommand -match 'Invoke-LLMSelection') {
    throw 'New-LocalBoxTuiLaunchPlan routed findbest through the interactive wizard path.'
}

$resetBestPlan = New-LocalBoxTuiLaunchPlan -Key $model.key -ContextKey $contextKey -Action resetbest -Mode native
if ($resetBestPlan.launchCommand -notmatch 'Invoke-LocalBoxTuiResetBest' -or $resetBestPlan.launchCommand -match 'Invoke-LLMSelection') {
    throw 'New-LocalBoxTuiLaunchPlan routed resetbest through the interactive wizard path.'
}

$launchOptions = Get-LocalBoxTuiLaunchOptions -Key $model.key
if (-not (@($launchOptions.actions) | Where-Object { $_.key -eq 'serve' -and $_.label -eq 'Serve' })) {
    throw 'Get-LocalBoxTuiLaunchOptions did not expose the serve action contract.'
}
$legacyLocalPilotAction = 'localpilot' + '-rust'
if (@($launchOptions.actions) | Where-Object { $_.key -eq $legacyLocalPilotAction }) {
    throw 'Get-LocalBoxTuiLaunchOptions still exposes the old split LocalPilot action.'
}

$servePlan = New-LocalBoxTuiLaunchPlan -Key $model.key -ContextKey $contextKey -Action serve -Mode native
if ($servePlan.action -ne 'serve' -or $servePlan.launchCommand -notmatch "-Action 'serve'") {
    throw 'New-LocalBoxTuiLaunchPlan did not preserve the serve action.'
}

$localpilotPlan = New-LocalBoxTuiLaunchPlan -Key $model.key -ContextKey $contextKey -Action localpilot -Mode native
if ($localpilotPlan.action -ne 'localpilot' -or $localpilotPlan.launchCommand -notmatch "-Action 'localpilot'") {
    throw 'New-LocalBoxTuiLaunchPlan did not preserve the LocalPilot action.'
}

if (-not [string]::IsNullOrWhiteSpace($quantKey)) {
    $quantFindBestPlan = New-LocalBoxTuiLaunchPlan -Key $model.key -ContextKey $contextKey -Action findbest -Mode native -Quant $quantKey
    if ($quantFindBestPlan.launchCommand -notmatch "-Quant '$([regex]::Escape($quantKey))'") {
        throw 'New-LocalBoxTuiLaunchPlan did not preserve quant for findbest.'
    }

    $quantResetBestPlan = New-LocalBoxTuiLaunchPlan -Key $model.key -ContextKey $contextKey -Action resetbest -Mode native -Quant $quantKey
    if ($quantResetBestPlan.launchCommand -notmatch "-Quant '$([regex]::Escape($quantKey))'") {
        throw 'New-LocalBoxTuiLaunchPlan did not preserve quant for resetbest.'
    }
}

$settings = Get-LocalBoxTuiSettings
if (-not $settings.path) {
    throw 'Get-LocalBoxTuiSettings did not return a settings path.'
}

$localBench = Get-LocalBoxTuiLocalBenchStatus
if ($null -eq $localBench.available) {
    throw 'Get-LocalBoxTuiLocalBenchStatus did not return structured availability.'
}

@($status, $models[0], $detail, $plan, $defaultPlan, $findBestPlan, $resetBestPlan, $launchOptions, $servePlan, $localpilotPlan, $settings, $localBench) | ConvertTo-Json -Depth 16 -Compress | Out-Null
Write-Host "LocalBox TUI API smoke test passed."
