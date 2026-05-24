# Top-level command surface (one-liners that wrap into the wizard / reload).

function llm     { Start-LLMWizard @args }
function llmmenu { Start-LLMWizard @args }
function llmc    { Start-LLMWizardClassic }
function llms    { Start-LLMWizardSpectreExplicit }
function llmtui  {
    $profilePath = Join-Path $script:LLMProfileRoot 'LocalLLMProfile.ps1'
    $candidateRoots = New-Object System.Collections.Generic.List[string]
    if ($script:Cfg -and $script:Cfg.ContainsKey('LocalBoxRoot') -and -not [string]::IsNullOrWhiteSpace([string]$script:Cfg.LocalBoxRoot)) {
        $candidateRoots.Add([string]$script:Cfg.LocalBoxRoot) | Out-Null
    }
    if (-not [string]::IsNullOrWhiteSpace($env:LOCALBOX_ROOT)) {
        $candidateRoots.Add($env:LOCALBOX_ROOT) | Out-Null
    }
    $candidateRoots.Add((Split-Path -Parent $script:LLMProfileRoot)) | Out-Null

    $walk = Get-Location
    while ($walk) {
        $candidateRoots.Add($walk.Path) | Out-Null
        $walk = $walk.Parent
    }

    $project = $null
    foreach ($root in @($candidateRoots | Where-Object { $_ } | Select-Object -Unique)) {
        $candidateProject = Join-Path $root 'tui\LocalBox.Tui\LocalBox.Tui.csproj'
        if (Test-Path -LiteralPath $candidateProject) {
            $project = $candidateProject
            break
        }
    }

    $exe = Join-Path $script:LLMProfileRoot 'bin\LocalBox.Tui.exe'
    $tuiArgs = @('--profile', $profilePath) + $args

    if ($project -and (Test-Path -LiteralPath $project) -and (Get-Command dotnet -ErrorAction SilentlyContinue)) {
        & dotnet run --project $project -- @tuiArgs
        return
    }

    if (Test-Path -LiteralPath $exe) {
        & $exe @tuiArgs
        return
    }

    throw "LocalBox.Tui was not found. Run from a repo checkout with dotnet available, or publish/install it with: pwsh .\tui\publish-tui.ps1 -Install"
}
function bptui {
    $candidateRoots = New-Object System.Collections.Generic.List[string]
    if ($env:BENCHPILOT_ROOT) {
        $candidateRoots.Add($env:BENCHPILOT_ROOT) | Out-Null
    }
    if ($script:Cfg -and $script:Cfg.ContainsKey('BenchPilotRoot') -and -not [string]::IsNullOrWhiteSpace([string]$script:Cfg.BenchPilotRoot)) {
        $candidateRoots.Add([string]$script:Cfg.BenchPilotRoot) | Out-Null
    }
    if (Get-Command Resolve-BenchPilotRoot -ErrorAction SilentlyContinue) {
        $resolved = try { Resolve-BenchPilotRoot } catch { $null }
        if ($resolved -and $resolved.Root) {
            $candidateRoots.Add([string]$resolved.Root) | Out-Null
        }
    }

    $project = $null
    foreach ($root in @($candidateRoots | Where-Object { $_ } | Select-Object -Unique)) {
        $candidateProject = Join-Path $root 'tui\BenchPilot.Tui\BenchPilot.Tui.csproj'
        if (Test-Path -LiteralPath $candidateProject) {
            $project = $candidateProject
            break
        }
    }

    $exe = Join-Path $HOME '.local-llm\tools\benchpilot\bin\BenchPilot.Tui.exe'

    if ($project -and (Get-Command dotnet -ErrorAction SilentlyContinue)) {
        & dotnet run --project $project -- @args
        return
    }

    if (Test-Path -LiteralPath $exe) {
        & $exe @args
        return
    }

    throw "BenchPilot.Tui was not found. Run from a BenchPilot repo checkout with dotnet available, or publish/install it with: pwsh .\tui\publish-tui.ps1 -Install"
}
function llmremote { Start-LocalLLMRemoteGateway @args }
function reloadllm { Reload-LocalLLMConfig }

# llama.cpp: status + stop. The wizard handles launch interactively;
# these are escape hatches for an already-running session.
#   lps   = show status of the running llama-server
#   lstop = stop every llama-server.exe; no restart
function lps    { Get-LlamaServerStatus }
function lstop  { Stop-AllLlamaServers }
function bp     { bpstatus }

# Frees all VRAM by stopping every LocalBox-managed llama-server.
function unloadall { Unload-LocalLLM }
function llmstop   { Unload-LocalLLM }
function llm-stop  { Unload-LocalLLM }

# Status: rich per-process llama-server inspector with launch-arg parsing,
# /props + /slots queries, nvidia-smi per-PID memory, and Windows GPU
# performance counters. Forwards the same switches as Invoke-LlamaCppStatus
# (-Detailed, -Json, -Watch, etc.).
function llm-status { Invoke-LlamaCppStatus @args }
function llmstatus  { Invoke-LlamaCppStatus @args }
