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
$plan = New-LocalBoxTuiLaunchPlan -Key $model.key -ContextKey $contextKey -Action claude -Mode native
if (-not $plan.launchCommand -or $plan.launchCommand -notmatch 'Invoke-LLMSelection') {
    throw 'New-LocalBoxTuiLaunchPlan did not return a launcher command.'
}

$settings = Get-LocalBoxTuiSettings
if (-not $settings.path) {
    throw 'Get-LocalBoxTuiSettings did not return a settings path.'
}

$benchPilot = Get-LocalBoxTuiBenchPilotStatus
if ($null -eq $benchPilot.available) {
    throw 'Get-LocalBoxTuiBenchPilotStatus did not return structured availability.'
}

@($status, $models[0], $detail, $plan, $settings, $benchPilot) | ConvertTo-Json -Depth 16 -Compress | Out-Null
Write-Host "LocalBox TUI API smoke test passed."
