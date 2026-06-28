# CPU-only embedding server: a small, self-contained sibling of `llmdefaultserve`.
#
# Serves a GGUF embedding model through llama-server's OpenAI-compatible
# `POST /v1/embeddings` on a dedicated loopback port, forced onto the CPU
# (`-ngl 0`) so it costs ZERO GPU VRAM. That CPU rule is load-bearing: a
# GPU-resident embed model would steal VRAM from a chat model running alongside
# it, so any benchmark that pairs the two would see a degraded chat model on the
# embeddings side only. Keeping embeddings on the CPU leaves the chat model
# byte-identical whether or not embeddings are running.
#
# Distinct from `llmdefaultserve` (the chat model + no-think proxy on 8080/11435):
# this has its own port, its own process, and its own lifecycle state, so
# stopping one never touches the other. Pairs with `llmdefaultserve`: run both
# and a consumer (e.g. LocalMind's semantic dedup / rerank) points its chat
# endpoint at 8080 and its embedding endpoint at this server.

# In-process handle to the running embed server (one at a time). A pidfile
# (Get-LocalLLMEmbedStatePath) mirrors it so a *different* shell — the one that
# pre-flights or stops the server — can find it too.
$script:EmbedServerSession = $null

# Defaults for the embed model. Hardcoded to the researched, license-cleared
# choice (Qwen3-Embedding-0.6B, Apache-2.0, 1024-d, `--pooling last`); each is
# overridable by a matching `$script:Cfg` key so an operator can swap in the
# fallback (nomic-embed-text-v1.5) from settings.json without editing code.
function Resolve-LocalLLMEmbedDefaults {
    [CmdletBinding()]
    param()

    $cfg = if ($script:Cfg) { $script:Cfg } else { @{} }
    $get = {
        param($Key, $Fallback)
        if ($cfg -and $cfg.Contains($Key) -and -not [string]::IsNullOrWhiteSpace([string]$cfg[$Key])) {
            return [string]$cfg[$Key]
        }
        return $Fallback
    }

    return [ordered]@{
        Repo    = (& $get 'EmbedModelRepo' 'Qwen/Qwen3-Embedding-0.6B-GGUF')
        File    = (& $get 'EmbedModelFile' 'Qwen3-Embedding-0.6B-Q8_0.gguf')
        Root    = (& $get 'EmbedModelRoot' 'qwen3-embedding-0.6b')
        Pooling = (& $get 'EmbedPooling' 'last')
        Port    = [int](& $get 'EmbedPort' '8090')
    }
}

# The loopback base URL a consumer points `embedding_base_url` at.
function Get-LocalLLMEmbedBaseUrl {
    [CmdletBinding()]
    param([int]$Port)
    if ($Port -le 0) { $Port = [int](Resolve-LocalLLMEmbedDefaults).Port }
    return "http://127.0.0.1:$Port"
}

# Where the cross-shell lifecycle handle lives.
function Get-LocalLLMEmbedStatePath {
    [CmdletBinding()]
    param()
    return (Join-Path $HOME '.local-llm\embed-server.json')
}

# The exact llama-server argv for a CPU embedding server. PURE (no I/O, no
# launch), so the recipe is the thing that gets tested — the served command is a
# recorded contract, not a comment. `-ngl 0` (CPU-only) and `--embeddings` are
# the load-bearing flags; `--pooling last` is required by Qwen3-Embedding.
function Get-LocalLLMEmbedServerArgs {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$ModelPath,
        [Parameter(Mandatory = $true)][int]$Port,
        [string]$Pooling = 'last',
        [string]$HostAddress = '127.0.0.1'
    )

    $serverArgs = @(
        '-m', $ModelPath,
        '--embeddings',
        '-ngl', '0',
        '--host', $HostAddress,
        '--port', [string]$Port
    )
    if (-not [string]::IsNullOrWhiteSpace($Pooling)) {
        $serverArgs += @('--pooling', $Pooling)
    }
    return $serverArgs
}

# Acquire the embed GGUF into the models dir (acquire-don't-vendor: downloaded on
# demand, never committed). Idempotent — Download-HuggingFaceFile reuses an
# existing file. Returns the local path.
function Resolve-LocalLLMEmbedModelPath {
    [CmdletBinding()]
    param(
        [string]$Repo,
        [string]$File,
        [string]$Root
    )

    $defaults = Resolve-LocalLLMEmbedDefaults
    if ([string]::IsNullOrWhiteSpace($Repo)) { $Repo = $defaults.Repo }
    if ([string]::IsNullOrWhiteSpace($File)) { $File = $defaults.File }
    if ([string]::IsNullOrWhiteSpace($Root)) { $Root = $defaults.Root }

    $folder = Join-Path $script:Cfg.LlamaCppGgufRoot $Root
    Ensure-Directory $folder
    return (Download-HuggingFaceFile -Repo $Repo -FileName $File -DestinationFolder $folder)
}

# POST a probe input to a running embed server and return the embedding length
# (the vector dimension), or 0 on any failure. Used by the operator smoke test
# and as a health gate.
function Test-LocalLLMEmbedEndpoint {
    [CmdletBinding()]
    param(
        [int]$Port = 0,
        [string]$Model = 'embed',
        [int]$TimeoutSec = 10
    )
    $base = Get-LocalLLMEmbedBaseUrl -Port $Port
    $body = @{ model = $Model; input = @('embedding server health probe') } | ConvertTo-Json -Depth 4
    try {
        $r = Invoke-RestMethod -Uri "$base/v1/embeddings" -Method Post -Body $body `
            -ContentType 'application/json' -TimeoutSec $TimeoutSec
        $vec = $r.data[0].embedding
        if ($vec) { return @($vec).Count }
        return 0
    }
    catch {
        return 0
    }
}

# Start (or reuse) the CPU embedding server. `-DryRun` renders the plan and the
# exact argv without acquiring the model or launching anything.
function Start-LocalLLMEmbedServe {
    [CmdletBinding()]
    param(
        [int]$Port = 0,
        [string]$Repo,
        [string]$File,
        [string]$Root,
        [string]$ModelPath,
        [string]$Pooling,
        [Alias('WhatIf')][switch]$DryRun
    )

    $defaults = Resolve-LocalLLMEmbedDefaults
    if ($Port -le 0) { $Port = [int]$defaults.Port }
    if ([string]::IsNullOrWhiteSpace($Pooling)) { $Pooling = [string]$defaults.Pooling }
    $base = Get-LocalLLMEmbedBaseUrl -Port $Port

    if ($DryRun) {
        # Best-effort exe resolution: a dry run is useful even before llama.cpp is
        # installed, so a missing server binary degrades to a placeholder name.
        $serverPath = try { Find-LlamaServerExe } catch { 'llama-server' }
        $previewModel = if (-not [string]::IsNullOrWhiteSpace($ModelPath)) { $ModelPath } else { "<gguf-root>\$($defaults.Root)\$($defaults.File)" }
        $argv = Get-LocalLLMEmbedServerArgs -ModelPath $previewModel -Port $Port -Pooling $Pooling
        Write-Host 'CPU embedding serve (dry run) — no model acquired, no server started.' -ForegroundColor Cyan
        Write-Host "  endpoint: $base/v1/embeddings  (CPU-only, -ngl 0, zero GPU VRAM)" -ForegroundColor DarkGray
        Write-Host "  model:    $($defaults.Repo) / $($defaults.File)" -ForegroundColor DarkGray
        Write-Host "  command:  $serverPath $(Format-LocalLLMArgvLine -Argv $argv)" -ForegroundColor DarkGray
        return [pscustomobject]@{ DryRun = $true; BaseUrl = $base; Port = $Port; Args = $argv }
    }

    $serverPath = Find-LlamaServerExe

    # Idempotent: if something already serves embeddings on this port, reuse it
    # rather than fighting for the port (a re-run of the pre-flight is harmless).
    if (-not (Test-LlamaCppPortFree -Port $Port)) {
        if ((Test-LocalLLMEmbedEndpoint -Port $Port) -gt 0) {
            Write-Host "Embedding server already running on $base — reusing." -ForegroundColor Green
            return [pscustomobject]@{ DryRun = $false; BaseUrl = $base; Port = $Port; Reused = $true }
        }
        throw "Port $Port is in use by something that is not an embedding server. Stop it or pass -Port."
    }

    if ([string]::IsNullOrWhiteSpace($ModelPath)) {
        $ModelPath = Resolve-LocalLLMEmbedModelPath -Repo $Repo -File $File -Root $Root
    }
    if (-not (Test-Path -LiteralPath $ModelPath)) {
        throw "Embedding model GGUF not found: $ModelPath"
    }

    $argv = Get-LocalLLMEmbedServerArgs -ModelPath $ModelPath -Port $Port -Pooling $Pooling
    $logs = New-LlamaServerLogPaths
    Write-Host "Starting CPU embedding server on $base (-ngl 0, no GPU VRAM)..." -ForegroundColor Cyan
    $proc = Start-LlamaServerNative -ServerPath $serverPath -ServerArgs $argv -OutLogPath $logs.Out -ErrLogPath $logs.Err
    Wait-LlamaServer -Port $Port -Process $proc -OutLogPath $logs.Out -ErrLogPath $logs.Err

    $session = @{
        Pid     = $proc.Id
        Port    = $Port
        BaseUrl = $base
        Model   = $ModelPath
        Pooling = $Pooling
    }
    $script:EmbedServerSession = $session
    try {
        $session | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Get-LocalLLMEmbedStatePath) -Encoding utf8
    }
    catch {
        Write-Verbose "Could not write embed-server state file: $($_.Exception.Message)"
    }

    $dim = Test-LocalLLMEmbedEndpoint -Port $Port
    Write-Host "Embedding server up: $base/v1/embeddings (pid $($proc.Id), dim $dim)." -ForegroundColor Green
    return [pscustomobject]@{ DryRun = $false; BaseUrl = $base; Port = $Port; Pid = $proc.Id; Dim = $dim }
}

# Stop the tracked embed server (in-process handle first, then the pidfile so a
# different shell can stop it). Leaves the chat server / proxy untouched.
function Stop-LocalLLMEmbedServe {
    [CmdletBinding()]
    param([switch]$Quiet)

    $targetPid = $null
    if ($script:EmbedServerSession -and $script:EmbedServerSession.Pid) {
        $targetPid = [int]$script:EmbedServerSession.Pid
    }
    else {
        $statePath = Get-LocalLLMEmbedStatePath
        if (Test-Path -LiteralPath $statePath) {
            try { $targetPid = [int]((Get-Content -LiteralPath $statePath -Raw | ConvertFrom-Json).Pid) }
            catch { $targetPid = $null }
        }
    }

    if ($targetPid) {
        $p = Get-Process -Id $targetPid -ErrorAction SilentlyContinue
        if ($p -and -not $p.HasExited) {
            if (-not $Quiet) { Write-Host "Stopping embedding server (pid $targetPid)..." -ForegroundColor DarkGray }
            Stop-Process -Id $targetPid -Force -ErrorAction SilentlyContinue
        }
    }
    elseif (-not $Quiet) {
        Write-Host 'No embedding server tracked.' -ForegroundColor DarkGray
    }

    $script:EmbedServerSession = $null
    Remove-Item -LiteralPath (Get-LocalLLMEmbedStatePath) -ErrorAction SilentlyContinue
}

# Thin entrypoints (siblings of llmdefaultserve / llmstop).
function llmembedserve {
    [CmdletBinding()]
    param([int]$Port = 0, [Alias('WhatIf')][switch]$DryRun)
    Start-LocalLLMEmbedServe -Port $Port -DryRun:$DryRun
}

function llmembedstop {
    [CmdletBinding()]
    param([switch]$Quiet)
    Stop-LocalLLMEmbedServe -Quiet:$Quiet
}
