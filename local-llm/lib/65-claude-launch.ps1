# Claude Code / LocalPilot launcher path. Backs up Claude env vars, points
# them at the no-think strip proxy in front of llama-server, launches the
# agent, restores the env on exit.

$script:ClaudeEnvBackup = @{}
$script:NoThinkProxyProcess = $null
$script:ServeGatewaySession = $null

function Get-NoThinkProxyHealth {
    param(
        [Parameter(Mandatory = $true)][int]$Port,
        [string]$AuthToken
    )

    try {
        $headers = @{}
        if (-not [string]::IsNullOrWhiteSpace($AuthToken)) {
            $headers['Authorization'] = "Bearer $AuthToken"
        }

        $response = Invoke-WebRequest -UseBasicParsing -Uri "http://127.0.0.1:$Port/health" -Headers $headers -TimeoutSec 1 -ErrorAction Stop
        $content = [string]$response.Content
        try {
            return ($content | ConvertFrom-Json -AsHashtable)
        }
        catch {
            return @{ status = $content }
        }
    }
    catch {
        return $null
    }
}

function Test-NoThinkProxyTarget {
    param(
        [Parameter(Mandatory = $true)][int]$ListenPort,
        [Parameter(Mandatory = $true)][string]$TargetHost,
        [Parameter(Mandatory = $true)][int]$TargetPort,
        [string]$AuthToken
    )

    $health = Get-NoThinkProxyHealth -Port $ListenPort -AuthToken $AuthToken
    if (-not $health) { return $null }

    $healthHost = if ($health.Contains('target_host')) { [string]$health.target_host } else { '' }
    $healthPort = if ($health.Contains('target_port')) { try { [int]$health.target_port } catch { 0 } } else { 0 }

    if ($healthHost -eq $TargetHost -and $healthPort -eq $TargetPort) {
        return $true
    }

    return $false
}

function Get-LocalLLMServeHealthState {
    # Bounded, non-blocking serve-health probe that distinguishes a *stale* no-think
    # proxy — listening on its port while the upstream model server is down, so
    # requests return a bare `502 Bad Gateway` — from a fully-down or healthy stack,
    # and returns an actionable recommendation. Diagnostic only: it never starts,
    # stops, or blocks anything. Reuses the proxy `/health` probe (1s timeout) and
    # the loopback port probe.
    param(
        [Parameter(Mandatory = $true)][int]$ProxyPort,
        [Parameter(Mandatory = $true)][int]$UpstreamPort,
        [string]$AuthToken
    )

    $proxyUp = [bool](Get-NoThinkProxyHealth -Port $ProxyPort -AuthToken $AuthToken)
    # Test-LlamaCppPortFree returns $true when the port can be bound (nothing is
    # listening); a bound/in-use upstream port therefore means the server is up.
    $upstreamUp = -not (Test-LlamaCppPortFree -Port $UpstreamPort)

    $state = if ($proxyUp -and -not $upstreamUp) { 'stale-proxy' }
    elseif (-not $proxyUp -and -not $upstreamUp) { 'down' }
    elseif (-not $proxyUp -and $upstreamUp) { 'proxy-down' }
    else { 'ok' }

    $recommendation = switch ($state) {
        'stale-proxy' { "The no-think proxy is up on $ProxyPort but the upstream model server on $UpstreamPort is down, so requests return a bare 502. Restart the stack: llmstop; llmdefaultserve" }
        'down' { 'Neither the no-think proxy nor the model server is running. Start them with: llmdefaultserve' }
        'proxy-down' { "The model server on $UpstreamPort is up but the no-think proxy on $ProxyPort is not running. Restart the stack: llmstop; llmdefaultserve" }
        default { '' }
    }

    return [pscustomobject]@{
        State          = $state
        ProxyUp        = $proxyUp
        UpstreamUp     = $upstreamUp
        ProxyPort      = $ProxyPort
        UpstreamPort   = $UpstreamPort
        Recommendation = $recommendation
    }
}

function Clear-StaleNoThinkProxy {
    # Reap an orphaned no-think proxy whose upstream model server is dead. Such a
    # proxy (typically left over from an earlier session that exited without
    # tearing it down) still answers /health, so Test-NoThinkProxyTarget would
    # either treat it as a live match (and hand the launch a 502 upstream) or as a
    # live mismatch. Left in place it strands every request behind a bare 502.
    # This kills it so the caller can start a fresh proxy pointed at the current
    # server. No-op when the port is free, unverifiable, or the upstream is alive.
    # Returns $true only when it actually reaped a stale proxy.
    [CmdletBinding()]
    param(
        [int]$ListenPort = $script:NoThinkProxyPort,
        [string]$AuthToken
    )

    $health = Get-NoThinkProxyHealth -Port $ListenPort -AuthToken $AuthToken
    if (-not $health) { return $false }

    $upstreamPort = if ($health.Contains('target_port')) { try { [int]$health.target_port } catch { 0 } } else { 0 }
    if ($upstreamPort -le 0) { return $false }

    # Test-LlamaCppPortFree returns $true when the port can be bound (nothing is
    # listening); a free upstream port therefore means the model server is down.
    if (-not (Test-LlamaCppPortFree -Port $upstreamPort)) { return $false }

    $conn = Get-NetTCPConnection -LocalPort $ListenPort -State Listen -ErrorAction SilentlyContinue
    if (-not $conn) { return $false }

    $stalePid = @($conn)[0].OwningProcess
    Write-LaunchLog "Reaping stale no-think proxy on $ListenPort (PID=$stalePid): upstream 127.0.0.1:$upstreamPort is down." 'PROXY'
    Stop-Process -Id $stalePid -Force -ErrorAction SilentlyContinue
    if ($script:NoThinkProxyProcess -and $script:NoThinkProxyProcess.Id -eq $stalePid) {
        $script:NoThinkProxyProcess = $null
    }
    Start-Sleep -Milliseconds 300
    return $true
}

function Save-ClaudeEnvBackup {
    $script:ClaudeEnvBackup = @{}

    foreach ($name in $script:ClaudeEnvNames) {
        $script:ClaudeEnvBackup[$name] = (Get-Item "Env:$name" -ErrorAction SilentlyContinue).Value
    }
}

function Restore-ClaudeEnvBackup {
    [CmdletBinding()]
    param()

    foreach ($name in $script:ClaudeEnvNames) {
        if ($script:ClaudeEnvBackup.ContainsKey($name) -and $null -ne $script:ClaudeEnvBackup[$name] -and $script:ClaudeEnvBackup[$name] -ne "") {
            Set-Item "Env:$name" $script:ClaudeEnvBackup[$name]
        }
        else {
            Remove-Item "Env:$name" -ErrorAction SilentlyContinue
        }
    }

    $script:ClaudeEnvBackup = @{}
    Write-LaunchLog "Claude env vars restored." 'INFO'
}

function ConvertTo-LocalLLMBashDoubleQuoted {
    param([AllowEmptyString()][string]$Value)
    return '"' + (([string]$Value) -replace '\\', '\\' -replace '"', '\"' -replace '\$', '\$' -replace '`', '\`') + '"'
}

function ConvertTo-LocalLLMPowerShellDoubleQuoted {
    param([AllowEmptyString()][string]$Value)
    return '"' + (([string]$Value) -replace '`', '``' -replace '"', '`"') + '"'
}

function Get-LocalLLMServeClientEnvCommands {
    # The gateway token is handed to client harnesses through environment
    # variables, so it must exist as a plain string at this boundary; a
    # SecureString could not be exported.
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute('PSAvoidUsingPlainTextForPassword', 'Password')]
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$BaseUrl,
        [string]$Password = ''
    )

    $vars = [ordered]@{ ANTHROPIC_BASE_URL = $BaseUrl }
    if (-not [string]::IsNullOrWhiteSpace($Password)) {
        $vars['ANTHROPIC_AUTH_TOKEN'] = $Password
        $vars['ANTHROPIC_API_KEY']    = $Password
    }

    $bash = @()
    $powershell = @()
    foreach ($name in $vars.Keys) {
        $bash += ("export {0}={1}" -f $name, (ConvertTo-LocalLLMBashDoubleQuoted $vars[$name]))
        $powershell += ('$env:{0} = {1}' -f $name, (ConvertTo-LocalLLMPowerShellDoubleQuoted $vars[$name]))
    }

    return [pscustomobject]@{
        Bash       = $bash
        PowerShell = $powershell
    }
}

function Test-LocalLLMServePublicHttp {
    [CmdletBinding()]
    param([Parameter(Mandatory = $true)][string]$BaseUrl)

    try {
        $uri = [System.Uri]$BaseUrl
    }
    catch {
        return $false
    }

    if ($uri.Scheme -ne 'http') { return $false }

    $hostName = $uri.Host
    if ([string]::IsNullOrWhiteSpace($hostName)) { return $false }
    if ($hostName -ieq 'localhost') { return $false }

    $ip = $null
    if ([System.Net.IPAddress]::TryParse($hostName, [ref]$ip)) {
        $bytes = $ip.GetAddressBytes()
        if ($ip.AddressFamily -eq [System.Net.Sockets.AddressFamily]::InterNetwork) {
            if ($bytes[0] -eq 10) { return $false }
            if ($bytes[0] -eq 127) { return $false }
            if ($bytes[0] -eq 192 -and $bytes[1] -eq 168) { return $false }
            if ($bytes[0] -eq 172 -and $bytes[1] -ge 16 -and $bytes[1] -le 31) { return $false }
            if ($bytes[0] -eq 169 -and $bytes[1] -eq 254) { return $false }
            return $true
        }
        if ([System.Net.IPAddress]::IsLoopback($ip)) { return $false }
        return $true
    }

    return $true
}

function Get-LocalLLMServeGuardDecision {
    # Pure decision logic for the serve-gateway exposure guard, separated so
    # the refuse/allow/opt-in matrix is unit-testable without starting
    # anything. Open (no-auth) HTTP on a public-looking address is refused
    # unless the caller opts in explicitly; password-only public HTTP stays
    # allowed-with-warning.
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute('PSAvoidUsingPlainTextForPassword', 'Password')]
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string[]]$BaseUrls,
        [AllowEmptyString()][string]$Password = '',
        [switch]$AllowPublicNoAuth
    )

    $publicUrls = @($BaseUrls | Where-Object { Test-LocalLLMServePublicHttp -BaseUrl $_ })
    $noAuth = [string]::IsNullOrWhiteSpace($Password)
    $refuse = ($publicUrls.Count -gt 0) -and $noAuth -and (-not $AllowPublicNoAuth)

    $reason = ''
    if ($refuse) {
        $reason = "open (no auth) HTTP on a public-looking address: $($publicUrls -join ', '). " +
            "Set a password (-Password or LOCAL_LLM_SERVE_PASS), bind a private address " +
            "(-ListenHost/-AdvertiseHost), or opt in explicitly with -AllowPublicNoAuth."
    }

    return [pscustomobject]@{
        PublicUrls = $publicUrls
        Refuse     = $refuse
        OptedIn    = [bool]($AllowPublicNoAuth -and $noAuth -and $publicUrls.Count -gt 0)
        Reason     = $reason
    }
}

function Set-ClaudeLocalEnv {
    # Common env-var setup for the llama-server backend. Caller
    # is responsible for Save-ClaudeEnvBackup before and Restore-ClaudeEnvBackup
    # after. -KeepThinking leaves thinking-tokens enabled (skip the no-think
    # toggles); the caller must arrange routing accordingly.
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$BaseUrl,
        [Parameter(Mandatory = $true)][string]$Model,
        [bool]$KeepThinking = $false,
        [int]$ContextTokens = 0,
        [int]$MaxImagesPerRequest = 0
    )

    $env:ANTHROPIC_BASE_URL = $BaseUrl
    $env:ANTHROPIC_AUTH_TOKEN = "local"
    $env:ANTHROPIC_API_KEY = ""

    $env:ANTHROPIC_MODEL = $Model
    $env:ANTHROPIC_DEFAULT_OPUS_MODEL = $Model
    $env:ANTHROPIC_DEFAULT_SONNET_MODEL = $Model
    $env:ANTHROPIC_DEFAULT_HAIKU_MODEL = $Model

    if (-not $KeepThinking) {
        $env:CLAUDE_CODE_DISABLE_THINKING = "1"
        $env:MAX_THINKING_TOKENS = "0"
        $env:CLAUDE_CODE_DISABLE_ADAPTIVE_THINKING = "1"
    }
    $maxOutputTokens = if ($script:Cfg.Contains("LocalModelMaxOutputTokens")) {
        try { [int]$script:Cfg.LocalModelMaxOutputTokens } catch { 4096 }
    } else {
        4096
    }
    if ($maxOutputTokens -gt 0) {
        $env:CLAUDE_CODE_MAX_OUTPUT_TOKENS = [string]$maxOutputTokens
    }
    if ($ContextTokens -gt 0) {
        $env:CLAUDE_CODE_MAX_CONTEXT_TOKENS = [string]$ContextTokens
        $env:CLAUDE_CODE_AUTO_COMPACT_WINDOW = [string]$ContextTokens
    }

    $env:CLAUDE_CODE_ATTRIBUTION_HEADER = "0"
    $env:DISABLE_PROMPT_CACHING = "1"

    # Local models prefill slowly on big prompts; raise SDK timeout so the
    # client doesn't abort + retry mid-prefill (which restarts the work).
    $env:API_TIMEOUT_MS = "1800000"

    # Drop the auto-memory system-prompt block (and the turn-end extract
    # agent). Saves several KB of input tokens per turn — significant when
    # prefill is the bottleneck.
    $env:CLAUDE_CODE_DISABLE_AUTO_MEMORY = "1"

    # llama.cpp's Anthropic-compatible endpoint does not implement Anthropic
    # beta tool shapes like defer_loading/tool_reference. Without this,
    # LocalPilot may withhold real tools behind ToolSearch or send schema
    # fields that local proxies tolerate inconsistently.
    $env:CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS = "1"
    $env:ENABLE_TOOL_SEARCH = "false"

    # Per-model image-per-request ceiling for the local vision backend.
    # LocalPilot's capImagesForLocalBackend defaults to 1 (llama.cpp + mmproj
    # typically collapses into degenerate "/////" output beyond one image). A
    # model known to handle more sets MaxImagesPerRequest in its catalog def to
    # raise the cap; unset leaves LocalPilot's default of 1. For the esoteric
    # 0 (drop all) / -1 (disable cap) values, set CLAUDE_LOCAL_MAX_IMAGES by hand.
    if ($MaxImagesPerRequest -gt 0) {
        $env:CLAUDE_LOCAL_MAX_IMAGES = [string]$MaxImagesPerRequest
    }
}

function Set-LocalBackendTelemetryEnv {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][int]$Port,
        [Parameter(Mandatory = $true)][string]$OutLogPath,
        [Parameter(Mandatory = $true)][string]$ErrLogPath,
        [Parameter(Mandatory = $true)][string]$GgufPath,
        [Parameter(Mandatory = $true)][string]$Model,
        [Parameter(Mandatory = $true)][string]$ContextKey,
        [int]$ContextTokens = 0
    )

    $env:LOCALBOX_BACKEND = "llama.cpp"
    $env:LOCALBOX_LLAMA_SERVER_PID = [string]$ProcessId
    $env:LOCALBOX_LLAMA_SERVER_PORT = [string]$Port
    $env:LOCALBOX_LLAMA_SERVER_OUT_LOG = $OutLogPath
    $env:LOCALBOX_LLAMA_SERVER_ERR_LOG = $ErrLogPath
    $env:LOCALBOX_LLAMA_SERVER_GGUF = $GgufPath
    $env:LOCALBOX_LLAMA_SERVER_MODEL = $Model
    $env:LOCALBOX_CONTEXT_KEY = $ContextKey
    if ($ContextTokens -gt 0) {
        $env:LOCALBOX_CONTEXT_TOKENS = [string]$ContextTokens
    }
    $env:LOCALBOX_LOW_TPS_WARNING = "2"
}

function Start-NoThinkProxy {
    # Puts the no-think proxy in front of llama-server at -TargetPort. The
    # proxy strips Anthropic thinking-config from requests and <think>...
    # </think> blocks from /v1/messages responses (SSE + non-streaming),
    # which keeps reasoning models from leaking <think> tags into the
    # conversation or breaking consumers that JSON.parse the response body.
    param(
        [int]$ListenPort = $script:NoThinkProxyPort,
        [string]$ListenHost = "127.0.0.1",
        [int]$TargetPort = 8080,
        [string]$TargetHost = "127.0.0.1",
        [string]$AuthToken,
        [string]$OutLogPath,
        [string]$ErrLogPath
    )

    $target = "${TargetHost}:${TargetPort}"
    $authLabel = if ([string]::IsNullOrWhiteSpace($AuthToken)) { 'off' } else { 'on' }
    $logsRequested = (-not [string]::IsNullOrWhiteSpace($OutLogPath)) -or (-not [string]::IsNullOrWhiteSpace($ErrLogPath))
    Write-LaunchLog "No-think proxy: listen=$ListenHost`:$ListenPort target=$target auth=$authLabel" 'PROXY'

    # (C) Reap an orphaned proxy whose upstream is dead before probing target
    # match, so a session-leftover can't force a mismatch or a dead-upstream reuse.
    Clear-StaleNoThinkProxy -ListenPort $ListenPort -AuthToken $AuthToken | Out-Null

    $targetMatches = Test-NoThinkProxyTarget -ListenPort $ListenPort -TargetHost $TargetHost -TargetPort $TargetPort -AuthToken $AuthToken
    if ($targetMatches -eq $true) {
        if ($logsRequested) {
            if ($script:NoThinkProxyProcess -and -not $script:NoThinkProxyProcess.HasExited) {
                Write-LaunchLog "Restarting owned no-think proxy so serve gateway logs are captured." 'PROXY'
                Stop-NoThinkProxy
            }
            else {
                throw "No-think proxy port $ListenPort is already running for target $target, but serve gateway logs were requested and this shell does not own that process. Stop the existing proxy with llm-stop or free the port, then start Serve again."
            }
        }
        else {
            Write-LaunchLog "No-think proxy already running for target=$target" 'PROXY'
            return
        }
    }
    if ($targetMatches -eq $false) {
        # (A) A proxy is listening on $ListenPort but pointed at a DIFFERENT target
        # (the model server moved to a new port, or a stale proxy lingered). Repoint
        # it — tear down the mismatched proxy and fall through to start a fresh one
        # aimed at the current target — instead of failing the launch and forcing a
        # silent fallback onto an off-nominal direct route.
        Write-LaunchLog "No-think proxy on $ListenPort points at a different target; repointing to $target." 'PROXY'
        Stop-NoThinkProxy
        $targetMatches = $null
    }

    # $targetMatches is $null: port is free OR something is listening but we cannot
    # verify it (e.g. auth mismatch with the old proxy). Kill any stale listener first
    # so it doesn't compete with the new proxy under Windows SO_REUSEADDR.
    $existingConn = Get-NetTCPConnection -LocalPort $ListenPort -State Listen -ErrorAction SilentlyContinue
    if ($existingConn) {
        $stalePid = @($existingConn)[0].OwningProcess
        Write-LaunchLog "Killing stale listener on port $ListenPort (PID=$stalePid) before starting proxy." 'PROXY'
        Stop-Process -Id $stalePid -Force -ErrorAction SilentlyContinue
        Start-Sleep -Milliseconds 300
    }

    $proxyScript = Join-Path $HOME ".localbox-proxy\no-think-proxy.py"

    if ($script:NoThinkProxyProcess -and -not $script:NoThinkProxyProcess.HasExited) {
        Write-LaunchLog "Reusing existing no-think proxy process (PID=$($script:NoThinkProxyProcess.Id))" 'PROXY'
        return
    }

    Write-LaunchLog "Starting no-think proxy: python $proxyScript $ListenPort $target $ListenHost" 'PROXY'

    if (-not (Test-Path $proxyScript)) {
        throw "No-think proxy not found: $proxyScript. Re-run install.ps1 so Claude/LocalPilot launches do not point at a dead proxy URL."
    }

    $argList = @($proxyScript, [string]$ListenPort, $target, $ListenHost)
    $oldAuthToken = $env:NO_THINK_PROXY_AUTH_TOKEN
    try {
        if ([string]::IsNullOrWhiteSpace($AuthToken)) {
            Remove-Item Env:NO_THINK_PROXY_AUTH_TOKEN -ErrorAction SilentlyContinue
        }
        else {
            $env:NO_THINK_PROXY_AUTH_TOKEN = $AuthToken
        }

        $startArgs = @{
            FilePath      = 'python'
            ArgumentList  = $argList
            PassThru      = $true
            WindowStyle   = 'Hidden'
            ErrorAction   = 'Stop'
        }
        if (-not [string]::IsNullOrWhiteSpace($OutLogPath)) {
            $startArgs.RedirectStandardOutput = $OutLogPath
        }
        if (-not [string]::IsNullOrWhiteSpace($ErrLogPath)) {
            $startArgs.RedirectStandardError = $ErrLogPath
        }

        $script:NoThinkProxyProcess = Start-Process @startArgs
    }
    finally {
        if ([string]::IsNullOrWhiteSpace($oldAuthToken)) {
            Remove-Item Env:NO_THINK_PROXY_AUTH_TOKEN -ErrorAction SilentlyContinue
        }
        else {
            $env:NO_THINK_PROXY_AUTH_TOKEN = $oldAuthToken
        }
    }

    if (-not $script:NoThinkProxyProcess) {
        throw "Failed to start no-think proxy process."
    }

    $deadline = (Get-Date).AddSeconds(10)
    $ready = $false

    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 150

        $targetMatches = Test-NoThinkProxyTarget -ListenPort $ListenPort -TargetHost $TargetHost -TargetPort $TargetPort -AuthToken $AuthToken
        if ($targetMatches -eq $true) {
            $ready = $true
            break
        }
    }

    if (-not $ready) {
        Stop-NoThinkProxy
        throw "No-think proxy did not become ready on 127.0.0.1:$ListenPort for target $target."
    }
}

function Stop-NoThinkProxy {
    if ($script:NoThinkProxyProcess -and -not $script:NoThinkProxyProcess.HasExited) {
        $script:NoThinkProxyProcess.Kill() | Out-Null
    }

    $script:NoThinkProxyProcess = $null
}

function New-LocalLLMServeGatewayLogPaths {
    $dir = Join-Path $HOME ".local-llm\logs"
    if (-not (Test-Path -LiteralPath $dir)) {
        New-Item -ItemType Directory -Force -Path $dir | Out-Null
    }

    $stamp = Get-Date -Format "yyyyMMdd-HHmmss-fff"
    return @{
        Out = Join-Path $dir "serve-gateway-$stamp.out.log"
        Err = Join-Path $dir "serve-gateway-$stamp.err.log"
    }
}

function Format-LocalLLMServeGatewayStatus {
    [CmdletBinding()]
    param([Parameter(Mandatory = $true)][hashtable]$Session)

    $lines = New-Object System.Collections.Generic.List[string]
    $lines.Add("Serve gateway running") | Out-Null
    if ($Session.ContainsKey('Backend'))    { $lines.Add("Backend : $($Session.Backend)") | Out-Null }
    if ($Session.ContainsKey('Model'))      { $lines.Add("Model   : $($Session.Model)") | Out-Null }
    if ($Session.ContainsKey('GatewayPid')) { $lines.Add("Gateway : pid $($Session.GatewayPid)") | Out-Null }
    if ($Session.ContainsKey('ListenHost') -and $Session.ContainsKey('ListenPort')) {
        $lines.Add("Listen  : $($Session.ListenHost):$($Session.ListenPort)") | Out-Null
    }
    if ($Session.ContainsKey('BaseUrls') -and $Session.BaseUrls) {
        foreach ($url in @($Session.BaseUrls)) {
            $lines.Add("URL     : $url") | Out-Null
        }
    }
    if ($Session.ContainsKey('StartedAt') -and $Session.StartedAt) {
        try {
            $uptime = (Get-Date) - ([datetime]$Session.StartedAt)
            $lines.Add("Uptime  : {0:hh\:mm\:ss}" -f $uptime) | Out-Null
        }
        catch {
            Write-Verbose "Could not compute uptime from '$($Session.StartedAt)': $($_.Exception.Message)"
        }
    }
    if ($Session.ContainsKey('GatewayOutLog') -and $Session.GatewayOutLog) {
        $lines.Add("Log out : $($Session.GatewayOutLog)") | Out-Null
    }
    if ($Session.ContainsKey('GatewayErrLog') -and $Session.GatewayErrLog) {
        $lines.Add("Log err : $($Session.GatewayErrLog)") | Out-Null
    }
    if ($Session.ContainsKey('BackendOutLog') -and $Session.BackendOutLog) {
        $lines.Add("Backend : $($Session.BackendOutLog)") | Out-Null
    }
    if ($Session.ContainsKey('BackendErrLog') -and $Session.BackendErrLog) {
        $lines.Add("Backend : $($Session.BackendErrLog)") | Out-Null
    }

    return @($lines)
}

function Show-LocalLLMServeGatewayStatus {
    [CmdletBinding()]
    param([Parameter(Mandatory = $true)][hashtable]$Session)

    Write-Host ""
    foreach ($line in (Format-LocalLLMServeGatewayStatus -Session $Session)) {
        Write-Host $line -ForegroundColor Cyan
    }
}

function Watch-LocalLLMServeGateway {
    [CmdletBinding()]
    param([Parameter(Mandatory = $true)][hashtable]$Session)

    Show-LocalLLMServeGatewayStatus -Session $Session
    Write-Host ""
    Write-Host "Controls: Q = return to menu and keep running, S = stop gateway/backend, R = reprint client env vars" -ForegroundColor Yellow
    Write-Host "Gateway log follows: $($Session.GatewayOutLog)" -ForegroundColor DarkGray
    Write-Host ""

    if ([Console]::IsInputRedirected -or [Console]::IsOutputRedirected) {
        Write-Host "Non-interactive console detected; leaving serve gateway running. Use llm-stop to stop it." -ForegroundColor Yellow
        return
    }

    $lastLineCount = 0
    while ($true) {
        if ($Session.GatewayOutLog -and (Test-Path -LiteralPath $Session.GatewayOutLog)) {
            $lines = @(Get-Content -LiteralPath $Session.GatewayOutLog -ErrorAction SilentlyContinue)
            if ($lines.Count -gt $lastLineCount) {
                foreach ($line in @($lines | Select-Object -Skip $lastLineCount)) {
                    Write-Host $line -ForegroundColor Gray
                }
                $lastLineCount = $lines.Count
            }
        }

        $gatewayRunning = $true
        if ($Session.GatewayPid) {
            $gatewayRunning = $null -ne (Get-Process -Id $Session.GatewayPid -ErrorAction SilentlyContinue)
        }
        if (-not $gatewayRunning) {
            Write-Host "Serve gateway process has exited. Check logs above." -ForegroundColor Red
            return
        }

        $deadline = (Get-Date).AddMilliseconds(1000)
        while ((Get-Date) -lt $deadline) {
            if ([Console]::KeyAvailable) {
                $key = [Console]::ReadKey($true).Key
                switch ($key) {
                    'Q' {
                        Write-Host "Serve gateway left running. Use llm-stop to stop it." -ForegroundColor Green
                        return
                    }
                    'S' {
                        Stop-LocalLLMServeGateway
                        Stop-LlamaServer -Quiet
                        Write-Host "Serve gateway stopped." -ForegroundColor Yellow
                        return
                    }
                    'R' {
                        if ($Session.BaseUrls) {
                            Show-LocalLLMServeClientInstructions -BaseUrls $Session.BaseUrls -Password $Session.Password
                        }
                    }
                }
            }
            Start-Sleep -Milliseconds 100
        }
    }
}

function Get-LocalLLMServeAdvertiseHosts {
    [CmdletBinding()]
    param([string]$ListenHost)

    if (-not [string]::IsNullOrWhiteSpace($ListenHost) -and $ListenHost -notin @('0.0.0.0', '*', '::')) {
        return @($ListenHost)
    }

    $hosts = New-Object System.Collections.Generic.List[string]
    try {
        foreach ($addr in [System.Net.Dns]::GetHostAddresses([System.Net.Dns]::GetHostName())) {
            if ($addr.AddressFamily -ne [System.Net.Sockets.AddressFamily]::InterNetwork) { continue }
            if ([System.Net.IPAddress]::IsLoopback($addr)) { continue }
            $text = [string]$addr
            if ($text -and -not $hosts.Contains($text)) {
                $hosts.Add($text) | Out-Null
            }
        }
    }
    catch {
        Write-Verbose "LAN address enumeration failed: $($_.Exception.Message)"
    }

    if ($hosts.Count -eq 0) {
        $hosts.Add('<server-ip>') | Out-Null
    }

    return @($hosts)
}

function Show-LocalLLMServeClientInstructions {
    # See Get-LocalLLMServeClientEnvCommands: the token crosses an env-var
    # boundary and cannot be a SecureString.
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute('PSAvoidUsingPlainTextForPassword', 'Password')]
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string[]]$BaseUrls,
        [string]$Password = ''
    )

    $baseUrl = $BaseUrls[0]
    $commands = Get-LocalLLMServeClientEnvCommands -BaseUrl $baseUrl -Password $Password

    Write-Host ""
    Write-Host "Serve gateway ready." -ForegroundColor Green
    Write-Host "  Base URL : $baseUrl" -ForegroundColor DarkGray
    if ($BaseUrls.Count -gt 1) {
        Write-Host "  Other LAN URLs:" -ForegroundColor DarkGray
        foreach ($url in @($BaseUrls | Select-Object -Skip 1)) {
            Write-Host "    $url" -ForegroundColor Gray
        }
    }

    if (Test-LocalLLMServePublicHttp -BaseUrl $baseUrl) {
        Write-Host ""
        $authWarn = if ([string]::IsNullOrWhiteSpace($Password)) { "open (no auth)" } else { "password-only" }
        Write-Host "WARNING: this is $authWarn HTTP on a public-looking address. Prompts are not encrypted." -ForegroundColor Yellow
        Write-Host "Keep the gateway on a LAN or VPN and put HTTPS in front of it (reverse proxy or tunnel)." -ForegroundColor Yellow
        Write-Host "Do not expose plain HTTP to the public internet. See the LocalX remote-egress policy." -ForegroundColor Yellow
    }
    else {
        Write-Host "  Reachable on your LAN/VPN only; for off-LAN access, tunnel or front it with HTTPS." -ForegroundColor DarkGray
    }

    Write-Host ""
    Write-Host "On the client, run normal LocalPilot with:" -ForegroundColor Cyan
    foreach ($line in $commands.Bash) {
        Write-Host "  $line" -ForegroundColor Gray
    }
    Write-Host "  localpilot" -ForegroundColor Gray
    Write-Host ""
    Write-Host "PowerShell client equivalent:" -ForegroundColor Cyan
    foreach ($line in $commands.PowerShell) {
        Write-Host "  $line" -ForegroundColor Gray
    }
    Write-Host "  localpilot" -ForegroundColor Gray
    Write-Host ""
}

function Start-LocalLLMLlamaCppServeBackend {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$Key,
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$ContextKey,
        [Parameter(Mandatory = $true)][ValidateSet('native', 'turboquant', 'mtpturbo')][string]$Mode,
        [string]$KvCacheK,
        [string]$KvCacheV,
        [switch]$Strict,
        [switch]$UseVision,
        [switch]$AutoBest,
        [ValidateSet('auto','pure','balanced','short','long')][string]$AutoBestProfile = 'auto',
        [string[]]$ExtraArgs,
        [AllowEmptyString()][string]$SpecType,
        [int]$SpecDraftNMax,
        [switch]$DryRun
    )

    $def = Get-ModelDef -Key $Key

    # Resolve MTP params: per-call > per-model def > caller-provided default
    if ([string]::IsNullOrWhiteSpace($SpecType)) {
        $SpecType = if ($def.ContainsKey('SpecType') -and -not [string]::IsNullOrWhiteSpace($def.SpecType)) { [string]$def.SpecType } else { '' }
    }
    if ($SpecDraftNMax -le 0) {
        $SpecDraftNMax = if ($def.ContainsKey('SpecDraftNMax') -and $null -ne $def.SpecDraftNMax) { [int]$def.SpecDraftNMax } else { 0 }
    }

    if (-not $DryRun) {
        Stop-LlamaServer -Quiet
    }

    if ($DryRun) {
        $folder = Join-Path $script:Cfg.LlamaCppGgufRoot $def.Root
        $fileName = Get-ModelFileName -Def $def
        $ggufPath = Resolve-HuggingFaceLocalPath -DestinationFolder $folder -FileName $fileName
    }
    else {
        $ggufPath = Get-ModelGgufPath -Def $def
    }

    $defaultPort = if ($script:Cfg.Contains('LlamaCppPort')) { [int]$script:Cfg.LlamaCppPort } else { 8080 }
    $port = if ($DryRun) { $defaultPort } else { Find-LlamaCppFreePort -StartPort $defaultPort }
    $thinkingPolicy = if ($def.Contains('ThinkingPolicy') -and -not [string]::IsNullOrWhiteSpace($def.ThinkingPolicy)) { [string]$def.ThinkingPolicy } else { 'strip' }

    $agentParallel = if ($script:Cfg.Contains('LlamaCppAgentParallel')) {
        try { [int]$script:Cfg.LlamaCppAgentParallel } catch { 1 }
    } else {
        1
    }
    $agentCacheReuse = if ($script:Cfg.Contains('LlamaCppAgentCacheReuse')) {
        try { [int]$script:Cfg.LlamaCppAgentCacheReuse } catch { 256 }
    } else {
        256
    }

    $visionModulePath = ''
    if ($UseVision -and -not $DryRun) {
        $visionModulePath = Get-ModelVisionModulePath -Key $Key -Def $def
        if (-not $visionModulePath) {
            Write-Warning "Vision requested but no mmproj found for $Key"
        }
    }

    $buildParams = @{
        Def              = $def
        ContextKey       = $ContextKey
        Mode             = $Mode
        ModelArgPath     = $ggufPath
        Port             = $port
        ThinkingPolicy   = $thinkingPolicy
        VisionModulePath = $(if ($visionModulePath) { $visionModulePath } else { '' })
    }
    if ($agentParallel -gt 0) { $buildParams.Parallel = $agentParallel }
    # CacheReuse is applied as a fallback AFTER the AutoBest merge (below) so a
    # tuned profile's value (including 0 = reuse disabled, needed for vision)
    # takes precedence over the hardcoded config default.
    if (-not [string]::IsNullOrWhiteSpace($KvCacheK)) { $buildParams.KvK = $KvCacheK }
    if (-not [string]::IsNullOrWhiteSpace($KvCacheV)) { $buildParams.KvV = $KvCacheV }
    if ($Strict)    { $buildParams.Strict = $true }
    if ($ExtraArgs) { $buildParams.ExtraArgs = $ExtraArgs }
    if (-not [string]::IsNullOrWhiteSpace($SpecType))       { $buildParams.SpecType = $SpecType }
    if ($SpecDraftNMax -gt 0)                                { $buildParams.SpecDraftNMax = $SpecDraftNMax }

    $autoBestLoadedProfile = $null
    if ($AutoBest) {
        $bestEntry = $null
        $selectionProfile = if ($AutoBestProfile -in @('pure', 'balanced')) { $AutoBestProfile } else { 'auto' }
        $promptProfileOverride = if ($AutoBestProfile -in @('short', 'long')) { $AutoBestProfile } else { $null }
        $loadedProfile = $AutoBestProfile

        if ($promptProfileOverride) {
            $bestEntry = Get-BestLlamaCppConfig -Key $Key -ContextKey $ContextKey -Mode $Mode -PromptLength $promptProfileOverride -Profile pure -Vision $UseVision -AllowVisionFallback:$UseVision
            $loadedProfile = "pure/$promptProfileOverride"
        } else {
            $preferred = Get-PreferredLlamaCppBestConfig -Key $Key -ContextKey $ContextKey -Mode $Mode -Profile $selectionProfile -Vision $UseVision -AllowVisionFallback:$UseVision
            if ($preferred) {
                $bestEntry = $preferred.Entry
                $loadedProfile = "$($preferred.Profile)/$($preferred.PromptLength)"
            }
        }

        if ($bestEntry -and $bestEntry.overrides) {
            $autoBestLoadedProfile = $loadedProfile
            Write-Host "AutoBest: loaded saved tuner config (profile=$loadedProfile, score=$($bestEntry.score) $($bestEntry.scoreUnit), trials=$($bestEntry.trial_count))." -ForegroundColor Cyan
            if ($UseVision -and -not [bool]$bestEntry.vision) {
                Write-Warning "AutoBest: no vision-tuned config exists for this model; loaded a text-only tune as fallback. It was measured without the mmproj, so VRAM headroom is tighter - if you hit OOM, raise --n-cpu-moe or launch without vision."
            }
            $tunable = @('KvK','KvV','NGpuLayers','NCpuMoe','UbatchSize','BatchSize','Threads','ThreadsBatch','Mlock','NoMmap','FlashAttn','SplitMode','SwaFull','CachePrompt','CacheReuse','SpecType','SpecDraftNMax')
            foreach ($k in $tunable) {
                if ($buildParams.ContainsKey($k)) { continue }
                $val = $null
                if ($bestEntry.overrides -is [System.Collections.IDictionary]) {
                    if ($bestEntry.overrides.Contains($k)) { $val = $bestEntry.overrides[$k] }
                } else {
                    $prop = $bestEntry.overrides.PSObject.Properties[$k]
                    if ($prop) { $val = $prop.Value }
                }
                if ($null -ne $val) { $buildParams[$k] = $val }
            }
        } elseif ($bestEntry) {
            Write-Warning "AutoBest: matched saved entry has no 'overrides' field (older tuner version?). Skipping."
        } else {
            $profileHint = if ($promptProfileOverride) { $promptProfileOverride } else { 'long' }
            $visionState = if ($UseVision) { 'on' } else { 'off' }
            Write-Warning "AutoBest: no saved config matches (key=$Key contextKey=$ContextKey mode=$Mode autoBestProfile=$AutoBestProfile vision=$visionState quant=$([string]$def.Quant)). Run: findbest $Key -ContextKey $ContextKey -Mode $Mode -PromptLengths $profileHint"
        }
    }

    # Fallback: only apply the config CacheReuse default when neither a tuned
    # profile nor an explicit launch setting already bound it.
    if (-not $buildParams.ContainsKey('CacheReuse') -and $agentCacheReuse -gt 0) {
        $buildParams.CacheReuse = $agentCacheReuse
    }

    $serverArgs = Build-LlamaServerArgs @buildParams
    $backendOutLog = $null
    $backendErrLog = $null

    if ($DryRun) {
        $serverPath = switch ($Mode) {
            'turboquant' { try { Find-TurboquantServerExe } catch { $null } }
            'mtpturbo'   { try { Find-MtpTurboServerExe   } catch { $null } }
            default      { Find-LlamaServerExe }
        }
        if (-not $serverPath) { $serverPath = '<not installed>' }
    }
    else {
        $serverPath = switch ($Mode) {
            'turboquant' { Ensure-LlamaServerTurboquant }
            'mtpturbo'   { Ensure-LlamaServerMtpTurbo }
            default      { Ensure-LlamaServerNative }
        }
    }

    if (-not $DryRun) {
        $logPaths = New-LlamaServerLogPaths
        $backendOutLog = $logPaths.Out
        $backendErrLog = $logPaths.Err
        Write-Host ""
        Write-Host "Starting llama-server serve backend for $($def.Root)..." -ForegroundColor Cyan
        Write-Host "  Server   : $serverPath" -ForegroundColor DarkGray
        Write-Host "  GGUF     : $ggufPath" -ForegroundColor DarkGray
        Write-Host "  Args     : $(Format-LocalLLMArgvLine -Argv $serverArgs)" -ForegroundColor DarkGray
        Write-Host "  Port     : $port" -ForegroundColor DarkGray
        Write-Host "  Logs     : $($logPaths.Out)" -ForegroundColor DarkGray
        Write-Host "             $($logPaths.Err)" -ForegroundColor DarkGray
        Write-LaunchLog "llama-server serve backend argv: $(Format-LocalLLMArgvLine -Argv (@($serverPath) + $serverArgs))" 'SERVER'

        try {
            $proc = Start-LlamaServerNative -ServerPath $serverPath -ServerArgs $serverArgs -OutLogPath $logPaths.Out -ErrLogPath $logPaths.Err
            Set-CurrentBackendSession -Session @{
                Backend  = 'llamacpp'
                Mode     = $Mode
                Port     = $port
                BaseUrl  = "http://localhost:$port"
                Model    = $def.Root
                GgufPath = $ggufPath
                Pid      = $proc.Id
                OutLog   = $logPaths.Out
                ErrLog   = $logPaths.Err
            }
            Wait-LlamaServer -Port $port -Process $proc -OutLogPath $logPaths.Out -ErrLogPath $logPaths.Err
        }
        catch {
            Stop-LlamaServer -Quiet
            if ((Test-LlamaCppSpecFallbackEligible -ErrorRecord $_ -BuildParams $buildParams) -and (Disable-LlamaCppSpecDecode -BuildParams $buildParams)) {
                Write-Warning "llama-server failed while loading the MTP head; retrying once without speculative MTP."
                Write-LaunchLog "llama-server MTP head load failed; retrying without --spec-type" 'WARN'
                $serverArgs = Build-LlamaServerArgs @buildParams
                $logPaths = New-LlamaServerLogPaths
                $backendOutLog = $logPaths.Out
                $backendErrLog = $logPaths.Err
                Write-Host "  Retry Args: $(Format-LocalLLMArgvLine -Argv $serverArgs)" -ForegroundColor DarkGray
                Write-Host "  Retry Logs: $($logPaths.Out)" -ForegroundColor DarkGray
                Write-Host "              $($logPaths.Err)" -ForegroundColor DarkGray
                Write-LaunchLog "llama-server serve backend retry argv: $(Format-LocalLLMArgvLine -Argv (@($serverPath) + $serverArgs))" 'SERVER'

                $proc = Start-LlamaServerNative -ServerPath $serverPath -ServerArgs $serverArgs -OutLogPath $logPaths.Out -ErrLogPath $logPaths.Err
                Set-CurrentBackendSession -Session @{
                    Backend  = 'llamacpp'
                    Mode     = $Mode
                    Port     = $port
                    BaseUrl  = "http://localhost:$port"
                    Model    = $def.Root
                    GgufPath = $ggufPath
                    Pid      = $proc.Id
                    OutLog   = $logPaths.Out
                    ErrLog   = $logPaths.Err
                }
                try {
                    Wait-LlamaServer -Port $port -Process $proc -OutLogPath $logPaths.Out -ErrLogPath $logPaths.Err
                }
                catch {
                    Stop-LlamaServer -Quiet
                    throw
                }
            } else {
                throw
            }
        }
    }

    return [pscustomobject]@{
        Backend       = 'llamacpp'
        Mode          = $Mode
        Model         = $def.Root
        TargetHost    = '127.0.0.1'
        TargetPort    = $port
        TargetBaseUrl = "http://127.0.0.1:$port"
        GgufPath      = $ggufPath
        ServerPath    = $serverPath
        ServerArgs    = $serverArgs
        AutoBestProfile = $autoBestLoadedProfile
        BackendOutLog = $backendOutLog
        BackendErrLog = $backendErrLog
    }
}

function Start-LocalLLMHeadlessServe {
    # Headless model launch for CLI / agent / CI use: bring up llama-server and
    # (for reasoning models) the no-think proxy as background processes, run the
    # visible-response smoke test, then return — WITHOUT attaching an interactive
    # agent. Unlike Start-LocalPilot, nothing is torn down on exit, so the
    # endpoint stays up for a separate `localpilot`/`claude` process to drive.
    # Reuses Start-LocalLLMLlamaCppServeBackend (server) + Start-NoThinkProxy.
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$Key,
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$ContextKey,
        [ValidateSet('native', 'turboquant', 'mtpturbo')][string]$Mode,
        [string]$KvCacheK,
        [string]$KvCacheV,
        [switch]$Strict,
        [switch]$UseAutoBest,
        [ValidateSet('auto', 'pure', 'balanced', 'short', 'long')][string]$AutoBestProfile = 'auto',
        [switch]$DryRun
    )

    $resolvedMode = Resolve-LlamaCppMode -Mode $Mode
    $def = Get-ModelDef -Key $Key
    $thinkingPolicy = if ($def.Contains('ThinkingPolicy') -and -not [string]::IsNullOrWhiteSpace($def.ThinkingPolicy)) { [string]$def.ThinkingPolicy } else { 'strip' }
    $useNoThinkProxy = ($thinkingPolicy -ne 'keep')

    $backend = Start-LocalLLMLlamaCppServeBackend `
        -Key $Key -ContextKey $ContextKey -Mode $resolvedMode `
        -KvCacheK $KvCacheK -KvCacheV $KvCacheV -Strict:$Strict `
        -AutoBest:$UseAutoBest -AutoBestProfile $AutoBestProfile -DryRun:$DryRun

    if ($DryRun) {
        $proxyNote = if ($useNoThinkProxy) {
            "127.0.0.1:$($script:NoThinkProxyPort) -> llama-server:$($backend.TargetPort)"
        } else {
            'disabled (ThinkingPolicy=keep)'
        }
        $plan = @{
            Title      = "headless serve: $($def.Root) via llama.cpp ($resolvedMode)"
            Backend    = 'llamacpp'
            Mode       = $resolvedMode
            Key        = $Key
            Model      = $def.Root
            ContextKey = $ContextKey
            ServerPath = $backend.ServerPath
            ServerArgs = $backend.ServerArgs
            Port       = $backend.TargetPort
            BaseUrl    = $backend.TargetBaseUrl
            Notes      = @(
                'No agent is attached and nothing is torn down — the server (and proxy) keep running until `llmstop`.',
                "No-think proxy: $proxyNote"
            )
        }
        Show-LocalLLMLaunchPlan -Plan $plan
        return
    }

    $clientBaseUrl = $backend.TargetBaseUrl
    if ($useNoThinkProxy) {
        try {
            Start-NoThinkProxy -ListenHost '127.0.0.1' -ListenPort $script:NoThinkProxyPort -TargetHost '127.0.0.1' -TargetPort $backend.TargetPort -AuthToken ''
            $clientBaseUrl = "http://127.0.0.1:$($script:NoThinkProxyPort)"
            Write-Host "  Proxy    : no-think 127.0.0.1:$($script:NoThinkProxyPort) -> llama-server:$($backend.TargetPort)" -ForegroundColor DarkGray
        }
        catch {
            Write-Warning "No-think proxy failed to start ($_); serving the raw OpenAI endpoint at $($backend.TargetBaseUrl)."
            $useNoThinkProxy = $false
        }
    }

    $smoke = Test-ClaudeLocalVisibleResponse -BaseUrl $clientBaseUrl -Model $def.Root
    if (-not $smoke.Ok) {
        Write-Warning "Served $($def.Root) on $clientBaseUrl but the visible-response smoke test failed ($(Format-ClaudeLocalSmokeFailure -Smoke $smoke)). Check the server logs."
        # Bounded, non-blocking diagnostic: distinguish a stale proxy (up while the
        # upstream is down → a bare 502) from a genuine model fault, with the fix to
        # run. Never blocks the launch — the endpoint is already up.
        if ($useNoThinkProxy) {
            $health = Get-LocalLLMServeHealthState -ProxyPort $script:NoThinkProxyPort -UpstreamPort $backend.TargetPort
            if ($health.State -ne 'ok' -and -not [string]::IsNullOrWhiteSpace($health.Recommendation)) {
                Write-Warning "Serve health: $($health.State). $($health.Recommendation)"
            }
        }
    }

    $session = Get-CurrentBackendSession
    Write-Host ""
    Write-Host "Headless model server ready (no agent attached)." -ForegroundColor Green
    Write-Host "  Model    : $($def.Root)" -ForegroundColor Gray
    Write-Host "  Client   : $clientBaseUrl$(if ($useNoThinkProxy) { '  (Anthropic /v1/messages, think-stripped)' } else { '  (OpenAI /v1)' })" -ForegroundColor Gray
    Write-Host "  Backend  : $($backend.TargetBaseUrl)  (llama-server pid $($session.Pid), port $($backend.TargetPort))" -ForegroundColor Gray
    Write-Host "  Smoke    : $(if ($smoke.Ok) { 'ok' } else { 'FAILED (see warning)' })" -ForegroundColor Gray
    Write-Host "  Stop     : llmstop" -ForegroundColor Gray

    return [pscustomobject]@{
        Model          = $def.Root
        ClientBaseUrl  = $clientBaseUrl
        BackendBaseUrl = $backend.TargetBaseUrl
        Port           = $backend.TargetPort
        Pid            = $session.Pid
        Proxy          = $useNoThinkProxy
        SmokeOk        = [bool]$smoke.Ok
    }
}

function Test-LlamaCppSpecFallbackEligible {
    param(
        [Parameter(Mandatory = $true)]$ErrorRecord,
        [Parameter(Mandatory = $true)][System.Collections.IDictionary]$BuildParams
    )

    if (-not $BuildParams.Contains('SpecType')) { return $false }

    $msg = [string]$ErrorRecord.Exception.Message
    return ($msg -match 'failed to load MTP head|invalid vector subscript')
}

function Disable-LlamaCppSpecDecode {
    param([Parameter(Mandatory = $true)][System.Collections.IDictionary]$BuildParams)

    $changed = $false
    foreach ($key in @('SpecType', 'SpecDraftNMax')) {
        if ($BuildParams.Contains($key)) {
            $BuildParams.Remove($key)
            $changed = $true
        }
    }
    return $changed
}

function Start-LocalLLMServeGateway {
    # See Get-LocalLLMServeClientEnvCommands: the token crosses an env-var
    # boundary and cannot be a SecureString.
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute('PSAvoidUsingPlainTextForPassword', 'Password')]
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$Key,
        [Alias('Ctx')]
        [AllowEmptyString()][string]$ContextKey = '',
        [ValidateSet('native', 'turboquant', 'mtpturbo')][string]$LlamaCppMode = 'native',
        [string]$KvCacheK,
        [string]$KvCacheV,
        [switch]$Strict,
        [switch]$UseVision,
        [switch]$AutoBest,
        [ValidateSet('auto','pure','balanced','short','long')][string]$AutoBestProfile = 'auto',
        [string]$ListenHost = '0.0.0.0',
        [int]$ListenPort = $script:NoThinkProxyPort,
        [string]$AdvertiseHost,
        [string]$Password = $env:LOCAL_LLM_SERVE_PASS,
        [string[]]$ExtraArgs,
        [switch]$NoMonitor,
        [switch]$AllowPublicNoAuth,
        [switch]$DryRun
    )

    if ($ListenPort -le 0) {
        $ListenPort = $script:NoThinkProxyPort
    }

    $hosts = if ([string]::IsNullOrWhiteSpace($AdvertiseHost)) {
        Get-LocalLLMServeAdvertiseHosts -ListenHost $ListenHost
    } else {
        @($AdvertiseHost)
    }
    $baseUrls = @($hosts | ForEach-Object { "http://${_}:$ListenPort" })
    $primaryBaseUrl = $baseUrls[0]

    # Exposure guard runs before the backend spins up: refusing after a model
    # load would waste the load and leave a server to tear down.
    $guard = Get-LocalLLMServeGuardDecision -BaseUrls $baseUrls -Password $Password -AllowPublicNoAuth:$AllowPublicNoAuth
    if ($guard.Refuse -and -not $DryRun) {
        throw "Serve gateway refused: $($guard.Reason)"
    }

    $backendInfo = Start-LocalLLMLlamaCppServeBackend -Key $Key -ContextKey $ContextKey -Mode $LlamaCppMode -KvCacheK $KvCacheK -KvCacheV $KvCacheV -Strict:$Strict -UseVision:$UseVision -AutoBest:$AutoBest -AutoBestProfile $AutoBestProfile -ExtraArgs $ExtraArgs -DryRun:$DryRun

    if ($DryRun) {
        $commands = Get-LocalLLMServeClientEnvCommands -BaseUrl $primaryBaseUrl -Password $Password
        $notes = @("Example LocalPilot command: $($commands.Bash -join '; '); localpilot")
        if ($guard.Refuse) {
            $notes += "A real launch would refuse: $($guard.Reason)"
        }
        elseif (Test-LocalLLMServePublicHttp -BaseUrl $primaryBaseUrl) {
            $authNote = if ([string]::IsNullOrWhiteSpace($Password)) { "Open (no auth)" } else { "Password-only" }
            $notes += "$authNote HTTP on a public-looking address is not encrypted."
        }
        if ($backendInfo.AutoBestProfile) {
            $notes += "AutoBest: loaded saved tuner profile=$($backendInfo.AutoBestProfile) (overrides applied to server argv)"
        }
        $plan = @{
            Title       = "serve gateway via llama.cpp"
            Backend     = 'llamacpp'
            Mode        = $backendInfo.Mode
            Key         = $Key
            Model       = $backendInfo.Model
            BaseUrl     = $primaryBaseUrl
            HealthCheck = "$primaryBaseUrl/health"
            Port        = $ListenPort
            GgufPath    = $backendInfo.GgufPath
            ServerPath  = $backendInfo.ServerPath
            ServerArgs  = $backendInfo.ServerArgs
            Notes       = $notes
        }
        Show-LocalLLMLaunchPlan -Plan $plan
        return
    }

    $gatewayLogs = New-LocalLLMServeGatewayLogPaths
    Start-NoThinkProxy -ListenHost $ListenHost -ListenPort $ListenPort -TargetHost $backendInfo.TargetHost -TargetPort $backendInfo.TargetPort -AuthToken $Password -OutLogPath $gatewayLogs.Out -ErrLogPath $gatewayLogs.Err

    $script:ServeGatewaySession = @{
        Backend    = 'llamacpp'
        Model      = $backendInfo.Model
        ListenHost = $ListenHost
        ListenPort = $ListenPort
        BaseUrls   = $baseUrls
        Password   = $Password
        StartedAt  = Get-Date
        GatewayPid = if ($script:NoThinkProxyProcess) { $script:NoThinkProxyProcess.Id } else { $null }
        GatewayOutLog = $gatewayLogs.Out
        GatewayErrLog = $gatewayLogs.Err
        BackendOutLog = $backendInfo.BackendOutLog
        BackendErrLog = $backendInfo.BackendErrLog
    }

    Show-LocalLLMServeClientInstructions -BaseUrls $baseUrls -Password $Password

    if (-not $NoMonitor) {
        Watch-LocalLLMServeGateway -Session $script:ServeGatewaySession
    }
}

function Stop-LocalLLMServeGateway {
    [CmdletBinding()]
    param()

    Stop-NoThinkProxy
    $script:ServeGatewaySession = $null
}

function Test-LocalDegenerateResponseText {
    [CmdletBinding()]
    param([AllowEmptyString()][string]$Text)

    $trimmed = $Text.Trim()
    if ([string]::IsNullOrWhiteSpace($trimmed)) {
        return $false
    }
    if ($trimmed -eq '[no output]') {
        return $true
    }
    if ([regex]::IsMatch($trimmed, '(?m)([/\\#*=.~\-])\1{7,}')) {
        return $true
    }

    $previous = $null
    $run = 0
    foreach ($token in ($trimmed -split '\s+')) {
        if ($token -eq $previous) {
            $run += 1
        }
        else {
            $previous = $token
            $run = 1
        }
        if ($run -ge 10) {
            return $true
        }
    }
    return $false
}

function Test-ClaudeLocalVisibleResponse {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$BaseUrl,
        [Parameter(Mandatory = $true)][string]$Model,
        [string]$SystemPrompt,
        [int]$TimeoutSec = 0
    )

    if ($TimeoutSec -le 0) {
        $TimeoutSec = if ($script:Cfg.Contains('LlamaCppSmokeTestTimeoutSec')) { [int]$script:Cfg.LlamaCppSmokeTestTimeoutSec } else { 300 }
    }

    $payload = @{
        model = $Model
        max_tokens = 32
        stream = $false
        messages = @(
            @{
                role = 'user'
                content = 'Are you working? Reply with a short visible acknowledgement.'
            }
        )
    }
    if (-not [string]::IsNullOrWhiteSpace($SystemPrompt)) {
        $payload.system = $SystemPrompt
    }
    $body = $payload | ConvertTo-Json -Depth 8 -Compress

    try {
        $resp = Invoke-RestMethod `
            -Uri "$BaseUrl/v1/messages" `
            -Method Post `
            -Headers @{ 'anthropic-version' = '2023-06-01'; 'x-api-key' = 'local' } `
            -ContentType 'application/json' `
            -Body $body `
            -TimeoutSec $TimeoutSec
    }
    catch {
        return [pscustomobject]@{
            Ok = $false
            Text = ''
            Error = $_.Exception.Message
        }
    }

    $parts = New-Object System.Collections.Generic.List[string]
    if ($resp -and $resp.content) {
        foreach ($block in @($resp.content)) {
            if ($block -is [string]) {
                $parts.Add($block) | Out-Null
                continue
            }
            $prop = $block.PSObject.Properties['text']
            if ($prop -and -not [string]::IsNullOrWhiteSpace([string]$prop.Value)) {
                $parts.Add([string]$prop.Value) | Out-Null
            }
        }
    }

    $text = (($parts | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }) -join '').Trim()
    $withoutThink = [regex]::Replace($text, '(?is)<think>.*?</think>', '')
    $withoutThink = [regex]::Replace($withoutThink, '(?is)<think>.*$', '').Trim()
    $looksDegenerate = Test-LocalDegenerateResponseText -Text $withoutThink
    $looksAnswered = -not [string]::IsNullOrWhiteSpace($withoutThink) -and -not $looksDegenerate
    return [pscustomobject]@{
        Ok = $looksAnswered
        Text = $text
        VisibleText = $withoutThink
        Degenerate = $looksDegenerate
        Error = $(if ($looksAnswered) { '' } elseif ($looksDegenerate) { 'degenerate response text' } elseif ([string]::IsNullOrWhiteSpace($text)) { 'no response text' } else { 'no visible response text after stripping thinking output' })
    }
}

function Format-ClaudeLocalSmokeFailure {
    param([Parameter(Mandatory = $true)]$Smoke)

    if (-not [string]::IsNullOrWhiteSpace($Smoke.Error)) {
        return [string]$Smoke.Error
    }

    $snippet = if (-not [string]::IsNullOrWhiteSpace($Smoke.VisibleText)) {
        [string]$Smoke.VisibleText
    } elseif (-not [string]::IsNullOrWhiteSpace($Smoke.Text)) {
        [string]$Smoke.Text
    } else {
        ''
    }
    $snippet = ($snippet -replace '\s+', ' ').Trim()
    if ($snippet.Length -gt 160) {
        $snippet = $snippet.Substring(0, 160) + '...'
    }
    if (-not [string]::IsNullOrWhiteSpace($snippet)) {
        return "unexpected smoke response: $snippet"
    }

    return 'no visible response text'
}

function Install-LocalPilot {
    [CmdletBinding()]
    param(
        [string]$Destination = (Join-Path $HOME '.local-llm\tools\localpilot'),
        [switch]$Force
    )

    if ((Test-Path -LiteralPath (Join-Path $Destination 'Cargo.toml')) -and -not $Force) {
        Write-Host "LocalPilot already exists: $Destination" -ForegroundColor Green
        Set-LocalLLMSetting LocalPilotRoot $Destination
        return $Destination
    }

    if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
        throw "git is not on PATH; cannot clone LocalPilot."
    }

    $repoUrl = if (-not [string]::IsNullOrWhiteSpace($script:Cfg.LocalPilotRepoUrl)) {
        [string]$script:Cfg.LocalPilotRepoUrl
    } else {
        'https://github.com/C0deGeek-dev/LocalPilot'
    }

    Ensure-Directory (Split-Path -Parent $Destination)
    if (Test-Path -LiteralPath $Destination) {
        throw "Destination already exists: $Destination. Use Update-LocalPilot, or remove it and retry."
    }

    & git clone $repoUrl $Destination
    if ($LASTEXITCODE -ne 0) { throw "git clone failed for $repoUrl" }

    Set-LocalLLMSetting LocalPilotRoot $Destination
    return $Destination
}

function Update-LocalPilot {
    [CmdletBinding()]
    param([switch]$RefreshInstalled)

    $root = if (Get-Command Resolve-LocalPilotRoot -ErrorAction SilentlyContinue) {
        Resolve-LocalPilotRoot
    } else {
        $script:Cfg.LocalPilotRoot
    }

    if ([string]::IsNullOrWhiteSpace($root) -or -not (Test-Path -LiteralPath $root)) {
        throw "LocalPilot is not installed. Run Install-LocalPilot first."
    }

    $result = Invoke-LocalLLMGitFastForwardUpdate -Name 'LocalPilot' -Root $root
    if ($result.Status -in @('failed', 'not-git', 'no-upstream', 'diverged')) {
        throw $result.Reason
    }
    if ($result.Updated -or $RefreshInstalled) {
        Invoke-LocalPilotInstallFromRoot -Root $result.Root
    }
    return $result
}

function Get-LocalPilotExtraArgs {
    # Merges the -ExtraLocalPilotArgs param with $env:LOCALPILOT_EXTRA_ARGS.
    # Env-var splitting is whitespace-only — sufficient for flags like `-D` or
    # `-D --debug-file=path`. For values containing spaces, pass via param.
    param([string[]]$Param)

    $extras = @()
    if ($env:LOCALPILOT_EXTRA_ARGS) {
        $extras += ($env:LOCALPILOT_EXTRA_ARGS -split '\s+' | Where-Object { $_ })
    }
    if ($Param) { $extras += $Param }
    return ,$extras
}

function ConvertTo-CodexTomlString {
    param([AllowEmptyString()][string]$Value)

    $escaped = ([string]$Value) -replace '\\', '\\' -replace '"', '\"'
    return '"' + $escaped + '"'
}

function Get-CodexCommonArgs {
    $commonArgs = @()

    if ($script:Cfg.Contains("CodexEnableSearch") -and [bool]$script:Cfg.CodexEnableSearch) {
        $commonArgs += '--search'
    }

    # Bypass is a conscious, persisted decision (default off in non-interactive
    # sessions), not a default-on inheritance: --dangerously-bypass-approvals-and-sandbox
    # gives the local model full command/file authority with no approval gate.
    if (Resolve-AgentBypassDecision -Label 'Codex' -SettingName 'CodexBypassApprovalsAndSandbox' `
            -EnvVar 'LOCAL_LLM_CODEX_BYPASS' -FlagSummary '--dangerously-bypass-approvals-and-sandbox') {
        $commonArgs += '--dangerously-bypass-approvals-and-sandbox'
    }

    return $commonArgs
}

function Start-CodexCli {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$Model,
        [Parameter(Mandatory = $true)][string]$BaseUrl,
        [int]$ContextTokens,
        [int]$MaxOutputTokens
    )

    if (-not (Get-Command codex -ErrorAction SilentlyContinue)) {
        throw "codex is not on PATH. Install with: npm install -g @openai/codex"
    }

    $codexArgs = @()

    $providerId = 'localbox_llamacpp'
    $idleMs = if ($script:Cfg.Contains("CodexStreamIdleTimeoutMs")) {
        try { [int]$script:Cfg.CodexStreamIdleTimeoutMs } catch { 10000000 }
    } else {
        10000000
    }

    $codexArgs += @(
        '-c', ('model_provider={0}' -f (ConvertTo-CodexTomlString $providerId)),
        '-c', ('model_providers.{0}.name={1}' -f $providerId, (ConvertTo-CodexTomlString 'LocalBox llama.cpp')),
        '-c', ('model_providers.{0}.base_url={1}' -f $providerId, (ConvertTo-CodexTomlString $BaseUrl)),
        '-c', ('model_providers.{0}.wire_api="responses"' -f $providerId),
        '-c', ('model_providers.{0}.stream_idle_timeout_ms={1}' -f $providerId, $idleMs)
    )

    if ($ContextTokens -gt 0) {
        $codexArgs += @('-c', "model_context_window=$ContextTokens")
    }
    if ($MaxOutputTokens -gt 0) {
        $codexArgs += @('-c', "model_max_output_tokens=$MaxOutputTokens")
    }

    $codexArgs += @('--model', $Model)
    $codexArgs += @(Get-CodexCommonArgs)

    Write-Host ""
    Write-Host "Launching codex with $Model..." -ForegroundColor Cyan
    Write-Host "  Base URL : $BaseUrl" -ForegroundColor DarkGray
    Write-Host "  Model    : $Model" -ForegroundColor DarkGray
    Write-Host ""

    & codex @codexArgs
}

function Get-ClaudeTargetSummary {
    if ($env:ANTHROPIC_DEFAULT_OPUS_MODEL) {
        return "Local -> $($env:ANTHROPIC_DEFAULT_OPUS_MODEL) @ $($env:ANTHROPIC_BASE_URL)"
    }

    return "Default (Anthropic API)"
}

function Resolve-LocalPilotVisionModule {
    # Resolve the multimodal projector (mmproj) path for a LocalPilot vision launch.
    # Returns '' when vision is not opted in or no projector is available (the
    # launch then proceeds text-only). A real launch downloads the projector on
    # demand; a DryRun resolves the expected local path WITHOUT downloading, so a
    # preview never pulls gigabytes. Guarded by Test-ModelVisionModuleAvailable so a
    # missing projector gives a clear message rather than a broken --mmproj.
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$Key,
        [Parameter(Mandatory = $true)][System.Collections.IDictionary]$Def,
        [switch]$UseVision,
        [switch]$DryRun
    )

    if (-not $UseVision) { return '' }

    $avail = Test-ModelVisionModuleAvailable -Key $Key -Def $Def
    if (-not ($avail.Local -or $avail.AvailableOnHF)) {
        Write-Warning "Vision requested but no mmproj is available locally or on HuggingFace for $Key; launching text-only."
        return ''
    }

    if ($DryRun) {
        # Preview only: resolve the expected on-disk path without downloading.
        $folder = Get-ModelFolder -Def $Def
        return [string](Resolve-HuggingFaceLocalPath -DestinationFolder $folder -FileName $avail.Filename)
    }

    $resolved = Get-ModelVisionModulePath -Key $Key -Def $Def
    if ($resolved) {
        Write-Host "Vision: loaded mmproj $([System.IO.Path]::GetFileName($resolved))" -ForegroundColor DarkCyan
        return [string]$resolved
    }

    Write-Warning "Vision requested but no mmproj found for $Key; launching text-only."
    return ''
}

function New-LocalPilotBaseConfigToml {
    # Build the [provider] + [providers.local] head of the generated .localpilot.toml.
    # -Model pins the provider's default model: the LocalPilot REPL is the default
    # (no-arg) command and resolves its model from config (there is no `chat --model`
    # flag any more), so without this the REPL finds no model and falls back to a
    # doctor dump instead of starting. When -SupportsVision is set (LocalBox loaded
    # the projector for this launch), it auto-declares supports_vision = true so
    # LocalPilot honours image input without a hand edit. The result has no trailing
    # newline, matching the caller's incremental ` `n`-prefixed appends.
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$ProviderKind,
        [Parameter(Mandatory = $true)][string]$BaseUrl,
        [Parameter(Mandatory = $true)][string]$ApiKeyEnv,
        [string]$Model,
        [switch]$SupportsVision
    )

    $toml = @"
[provider]
default = "local"

[providers.local]
kind = "$ProviderKind"
base_url = "$BaseUrl"
api_key_env = "$ApiKeyEnv"
"@

    if (-not [string]::IsNullOrWhiteSpace($Model)) {
        $toml += "`nmodel = `"$Model`""
    }
    if ($SupportsVision) {
        $toml += "`nsupports_vision = true"
    }

    return $toml
}

function Start-LocalPilot {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$Key,
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$ContextKey,
        [Parameter(Mandatory = $true)][ValidateSet('native','turboquant','mtpturbo')][string]$LlamaCppMode,
        [string]$KvCacheK,
        [string]$KvCacheV,
        [switch]$Strict,
        [switch]$UseVision,
        [switch]$UseAutoBest,
        [ValidateSet('auto','pure','balanced','short','long')][string]$AutoBestProfile = 'auto',
        [string[]]$ExtraLocalPilotArgs,
        [switch]$DryRun
    )

    $def = Get-ModelDef -Key $Key
    $extras = @(Get-LocalPilotExtraArgs -Param $ExtraLocalPilotArgs)

    # Stop any prior llama-server we own.
    if (-not $DryRun) {
        Stop-LlamaServer -Quiet
    }

    # Resolve GGUF.
    if ($DryRun) {
        $folder = Join-Path $script:Cfg.LlamaCppGgufRoot $def.Root
        $fileName = Get-ModelFileName -Def $def
        $ggufPath = Resolve-HuggingFaceLocalPath -DestinationFolder $folder -FileName $fileName
        if (-not (Test-Path -LiteralPath $ggufPath)) {
            Write-Host "GGUF not present locally; a real launch would download from $($def.Repo)/$fileName" -ForegroundColor DarkYellow
        }
    }
    else {
        $ggufPath = Get-ModelGgufPath -Def $def
    }

    # Pick a free port.
    $defaultPort = if ($script:Cfg.Contains('LlamaCppPort')) { [int]$script:Cfg.LlamaCppPort } else { 8080 }
    $port = Find-LlamaCppFreePort -StartPort $defaultPort

    $thinkingPolicy = if ($def.Contains('ThinkingPolicy') -and -not [string]::IsNullOrWhiteSpace($def.ThinkingPolicy)) { [string]$def.ThinkingPolicy } else { 'strip' }
    $useNoThinkProxy = ($thinkingPolicy -ne 'keep')
    $agentParallel = if ($script:Cfg.Contains('LlamaCppAgentParallel')) {
        try { [int]$script:Cfg.LlamaCppAgentParallel } catch { 1 }
    } else { 1 }
    # Default wiring talks straight to llama-server's OpenAI endpoint. For
    # reasoning models the no-think proxy is started after the server is up and
    # the client is switched to the Anthropic /v1/messages path through it (see
    # below), which strips <think> blocks the same way the Claude Code wiring
    # does and avoids the degenerate output seen on the raw OpenAI path. Use
    # 127.0.0.1 (not localhost) because llama-server binds IPv4 loopback and the
    # Rust client does not fall back from a localhost->::1 resolution on Windows.
    $providerKind = 'openai-compatible'
    $effectiveBaseUrl = "http://127.0.0.1:$port"

    # Resolve the multimodal projector when vision is opted in (mirrors
    # Start-ClaudeWithLlamaCppModel); '' when off or unavailable, so the default
    # agent-launch path is unchanged. A real launch downloads on demand; DryRun
    # resolves the expected path without downloading.
    $visionModulePath = Resolve-LocalPilotVisionModule -Key $Key -Def $def -UseVision:$UseVision -DryRun:$DryRun

    # Build llama-server args and start server.
    $buildParams = @{
        Def              = $def
        ContextKey       = $ContextKey
        Mode             = $LlamaCppMode
        ModelArgPath     = $ggufPath
        Port             = $port
        ThinkingPolicy   = $thinkingPolicy
        VisionModulePath = $visionModulePath
    }
    if ($agentParallel -gt 0) { $buildParams.Parallel = $agentParallel }
    if (-not [string]::IsNullOrWhiteSpace($KvCacheK)) { $buildParams.KvK = $KvCacheK }
    if (-not [string]::IsNullOrWhiteSpace($KvCacheV)) { $buildParams.KvV = $KvCacheV }
    if ($Strict) { $buildParams.Strict = $true }

    if ($UseAutoBest) {
        # AutoBest loading is the same as in Start-ClaudeWithLlamaCppModel
        $bestEntry = $null
        $selectionProfile = if ($AutoBestProfile -in @('pure', 'balanced')) { $AutoBestProfile } else { 'auto' }
        $promptProfileOverride = if ($AutoBestProfile -in @('short', 'long')) { $AutoBestProfile } else { $null }
        if ($promptProfileOverride) {
            $bestEntry = Get-BestLlamaCppConfig -Key $Key -ContextKey $ContextKey -Mode $LlamaCppMode -PromptLength $promptProfileOverride -Profile pure -Vision $UseVision -AllowVisionFallback:$UseVision
        } else {
            $preferred = Get-PreferredLlamaCppBestConfig -Key $Key -ContextKey $ContextKey -Mode $LlamaCppMode -Profile $selectionProfile -Vision $UseVision -AllowVisionFallback:$UseVision
            if ($preferred) { $bestEntry = $preferred.Entry }
        }
        if ($bestEntry -and $bestEntry.overrides) {
            Write-Host "AutoBest: loaded saved tuner config (profile=$AutoBestProfile)." -ForegroundColor Cyan
            if ($UseVision -and -not [bool]$bestEntry.vision) {
                Write-Warning "AutoBest: no vision-tuned config exists for this model; loaded a text-only tune as fallback. It was measured without the mmproj, so VRAM headroom is tighter - if you hit OOM, raise --n-cpu-moe or launch without vision."
            }
            $tunable = @('KvK','KvV','NGpuLayers','NCpuMoe','UbatchSize','BatchSize','Threads','ThreadsBatch','Mlock','NoMmap','FlashAttn','SplitMode','SwaFull','CachePrompt','CacheReuse','SpecType','SpecDraftNMax')
            foreach ($k in $tunable) {
                if ($buildParams.ContainsKey($k)) { continue }
                $val = $null
                if ($bestEntry.overrides -is [System.Collections.IDictionary]) {
                    if ($bestEntry.overrides.Contains($k)) { $val = $bestEntry.overrides[$k] }
                } else {
                    $prop = $bestEntry.overrides.PSObject.Properties[$k]
                    if ($prop) { $val = $prop.Value }
                }
                if ($null -ne $val) { $buildParams[$k] = $val }
            }
        }
    }

    # Fallback: apply config CacheReuse default when not already set.
    $agentCacheReuse = if ($script:Cfg.Contains('LlamaCppAgentCacheReuse')) {
        try { [int]$script:Cfg.LlamaCppAgentCacheReuse } catch { 256 }
    } else { 256 }
    if (-not $buildParams.ContainsKey('CacheReuse') -and $agentCacheReuse -gt 0) {
        $buildParams.CacheReuse = $agentCacheReuse
    }

    $serverArgs = Build-LlamaServerArgs @buildParams

    # Resolve server binary.
    if ($DryRun) {
        $serverPath = switch ($LlamaCppMode) {
            'turboquant' { try { Find-TurboquantServerExe } catch { $null } }
            'mtpturbo'   { try { Find-MtpTurboServerExe   } catch { $null } }
            default      { Find-LlamaServerExe }
        }
        if (-not $serverPath) { $serverPath = '<not installed>' }
    }
    else {
        $serverPath = switch ($LlamaCppMode) {
            'turboquant' { Ensure-LlamaServerTurboquant }
            'mtpturbo'   { Ensure-LlamaServerMtpTurbo }
            default      { Ensure-LlamaServerNative }
        }
    }

    if ($DryRun) {
        $title = "localpilot via llama.cpp ($LlamaCppMode)"
        # Preview resolves the bypass decision read-only (no prompt/persist); a
        # real launch makes the first-run decision.
        $bypassArgs = @(Get-LocalPilotBypassArgs -NoPrompt)
        $notes = @()
        if ($bypassArgs.Count -eq 0 -and
            -not ($script:Cfg -and $script:Cfg.Contains('LocalPilotBypass')) -and
            [string]::IsNullOrEmpty($env:LOCAL_LLM_LOCALPILOT_BYPASS)) {
            $notes += "LocalPilot bypass is undecided; a real launch will ask once and persist the answer (default off)."
        }
        $plan = @{
            Title         = $title
            Backend       = 'llamacpp'
            Mode          = $LlamaCppMode
            Key           = $Key
            Model         = $def.Root
            ContextKey    = $ContextKey
            ServerPath    = $serverPath
            ServerArgs    = $serverArgs
            Port          = $port
            BaseUrl       = $effectiveBaseUrl
            Bypass        = Get-AgentBypassStatusText -SettingName 'LocalPilotBypass' -EnvVar 'LOCAL_LLM_LOCALPILOT_BYPASS'
            LaunchExe     = 'localpilot'
            LaunchArgs    = @($extras)
            Notes         = $notes
        }
        Show-LocalLLMLaunchPlan -Plan $plan
        return
    }

    # Start llama-server.
    $logPaths = New-LlamaServerLogPaths
    Write-Host ""
    Write-Host "Starting llama-server for $($def.Root) via llama.cpp ($LlamaCppMode)..." -ForegroundColor Cyan
    Write-Host "  Server   : $serverPath" -ForegroundColor DarkGray
    Write-Host "  GGUF     : $ggufPath" -ForegroundColor DarkGray
    Write-Host "  Port     : $port" -ForegroundColor DarkGray
    Write-Host "  Logs     : $($logPaths.Out)" -ForegroundColor DarkGray
    Write-Host "             $($logPaths.Err)" -ForegroundColor DarkGray
    Write-Host "  Args     : $(Format-LocalLLMArgvLine -Argv $serverArgs)" -ForegroundColor DarkGray

    $proc = Start-LlamaServerNative -ServerPath $serverPath -ServerArgs $serverArgs -OutLogPath $logPaths.Out -ErrLogPath $logPaths.Err
    Write-Host "  PID      : $($proc.Id)" -ForegroundColor DarkGray

    try {
        Wait-LlamaServer -Port $port -Process $proc -OutLogPath $logPaths.Out -ErrLogPath $logPaths.Err
    }
    catch {
        Stop-LlamaServer -Quiet
        throw
    }

    # Reasoning models: put the no-think proxy in front of llama-server and route
    # the client through the Anthropic /v1/messages path, so <think> output is
    # stripped before it reaches LocalPilot (mirrors the working Claude Code
    # wiring). The proxy binds local loopback only, so it runs without auth.
    if ($useNoThinkProxy) {
        try {
            Start-NoThinkProxy -ListenHost '127.0.0.1' -ListenPort $script:NoThinkProxyPort -TargetHost '127.0.0.1' -TargetPort $port -AuthToken ''
            $providerKind = 'anthropic'
            $effectiveBaseUrl = "http://127.0.0.1:$($script:NoThinkProxyPort)"
            Write-Host "  Proxy    : no-think 127.0.0.1:$($script:NoThinkProxyPort) -> llama-server:$port" -ForegroundColor DarkGray
        }
        catch {
            Write-Warning "No-think proxy failed to start ($_); falling back to the direct OpenAI endpoint."
            $useNoThinkProxy = $false
        }
    }

    $smoke = Test-ClaudeLocalVisibleResponse -BaseUrl $effectiveBaseUrl -Model $def.Root
    if (-not $smoke.Ok) {
        $detail = Format-ClaudeLocalSmokeFailure -Smoke $smoke
        if ($LlamaCppMode -ne 'native') {
            Write-Warning "llama.cpp $LlamaCppMode failed the response smoke test ($detail); retrying the Rust launch with native llama.cpp."
            if ($useNoThinkProxy) { Stop-NoThinkProxy }
            Stop-LlamaServer -Quiet

            $fallbackParams = @{}
            foreach ($name in $PSBoundParameters.Keys) {
                $fallbackParams[$name] = $PSBoundParameters[$name]
            }
            $fallbackParams.LlamaCppMode = 'native'
            $fallbackParams.UseAutoBest = $false
            Start-LocalPilot @fallbackParams
            return
        }
        throw "Native llama.cpp failed the response smoke test ($detail). Check the model, quant, and server logs."
    }

    # Generate .localpilot.toml in the current working directory. The Anthropic
    # adapter normalizes a base_url ending in /v1 to /v1/messages; the
    # OpenAI-compatible one to /v1/chat/completions.
    $apiKeyEnv = if ($providerKind -eq 'anthropic') { 'ANTHROPIC_AUTH_TOKEN' } else { 'LOCALPILOT_LOCAL_API_KEY' }
    # When the projector loaded for this launch, LocalBox is the authoritative
    # declarer that the provider accepts image input, so auto-declare it.
    $declareVision = ($UseVision -and -not [string]::IsNullOrWhiteSpace($visionModulePath))
    $tomlContent = New-LocalPilotBaseConfigToml -ProviderKind $providerKind -BaseUrl "$effectiveBaseUrl/v1" -ApiKeyEnv $apiKeyEnv -Model $def.Root -SupportsVision:$declareVision

    Write-Host ""
    Write-Host "Launching localpilot with $($def.Root) via llama.cpp ($LlamaCppMode)..." -ForegroundColor Cyan
    Write-Host "  Base URL : $effectiveBaseUrl" -ForegroundColor DarkGray
    Write-Host "  Model    : $($def.Root)" -ForegroundColor DarkGray

    # Set env vars for the local backend.
    Save-ClaudeEnvBackup
    try {
        $env:ANTHROPIC_BASE_URL = $effectiveBaseUrl
        $env:ANTHROPIC_AUTH_TOKEN = "local"
        $env:ANTHROPIC_API_KEY = ""
        $env:ANTHROPIC_MODEL = $def.Root
        $env:ANTHROPIC_DEFAULT_OPUS_MODEL = $def.Root
        $env:ANTHROPIC_DEFAULT_SONNET_MODEL = $def.Root
        $env:ANTHROPIC_DEFAULT_HAIKU_MODEL = $def.Root
        $env:CLAUDE_CODE_DISABLE_THINKING = "1"
        $env:MAX_THINKING_TOKENS = "0"
        $env:CLAUDE_CODE_DISABLE_ADAPTIVE_THINKING = "1"

        $maxOutputTokens = if ($script:Cfg.Contains("LocalModelMaxOutputTokens")) {
            try { [int]$script:Cfg.LocalModelMaxOutputTokens } catch { 4096 }
        } else { 4096 }
        if ($maxOutputTokens -gt 0) {
            $env:CLAUDE_CODE_MAX_OUTPUT_TOKENS = [string]$maxOutputTokens
            $tomlContent += "`nmax_tokens = $maxOutputTokens`n"
        }

        $contextTokens = Get-ModelContextValue -Def $def -ContextKey $ContextKey
        if ($contextTokens -gt 0) {
            $env:CLAUDE_CODE_MAX_CONTEXT_TOKENS = [string]$contextTokens
            $env:CLAUDE_CODE_AUTO_COMPACT_WINDOW = [string]$contextTokens
        }

        $env:CLAUDE_CODE_ATTRIBUTION_HEADER = "0"
        $env:DISABLE_PROMPT_CACHING = "1"
        $env:API_TIMEOUT_MS = "1800000"
        $env:CLAUDE_CODE_DISABLE_AUTO_MEMORY = "1"
        $env:CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS = "1"
        $env:ENABLE_TOOL_SEARCH = "false"

        # Per-model image cap.
        $maxImagesPerRequest = 0
        if ($def.ContainsKey('MaxImagesPerRequest')) {
            try { $maxImagesPerRequest = [int]$def.MaxImagesPerRequest } catch { $maxImagesPerRequest = 0 }
        }
        if ($maxImagesPerRequest -gt 0) {
            $env:CLAUDE_LOCAL_MAX_IMAGES = [string]$maxImagesPerRequest
        }

        # Rust-specific: API key env var (empty for local).
        $env:LOCALPILOT_LOCAL_API_KEY = ""

        # Pass the model's usable context window to LocalPilot (Rust) so it uses
        # the full context instead of its conservative default.
        if ($contextTokens -gt 0) {
            $tomlContent += "`n[harness]`ncontext_token_limit = $contextTokens`n"
        }

        # Bypass hands the local model full tool/command authority with no
        # per-action gate; default off, opt-in via a persisted first-run decision.
        # The LocalPilot REPL takes no `--bypass` flag (clap rejects it and aborts
        # the launch), so the decision is written into the config's [permissions]
        # profile instead of the command line.
        $localpilotBypass = (@(Get-LocalPilotBypassArgs).Count -gt 0)
        if ($localpilotBypass) {
            $tomlContent += "`n[permissions]`nprofile = `"bypass`"`n"
        }

        # Write .localpilot.toml to cwd.
        $tomlPath = Join-Path (Get-Location) '.localpilot.toml'
        Set-Content -Path $tomlPath -Value $tomlContent -Encoding UTF8
        Write-Host "  Config   : $tomlPath" -ForegroundColor DarkGray

        # Launch the LocalPilot REPL. The interactive REPL is the DEFAULT (no-arg)
        # command in current LocalPilot — the old `chat` subcommand is gone, and the
        # REPL resolves its model and permission profile from the .localpilot.toml
        # written above (with ANTHROPIC_MODEL as a fallback on the anthropic route),
        # not from argv. Passing `chat --model <m>` or `--bypass` makes clap error out
        # and the launch fall straight back to the model-selection menu.
        if (-not (Get-Command localpilot -ErrorAction SilentlyContinue)) {
            throw "localpilot is not on PATH. Install with: cargo install localpilot"
        }

        $launchArgs = @($extras)
        & localpilot @launchArgs
    }
    finally {
        Restore-ClaudeEnvBackup
        if ($useNoThinkProxy) { Stop-NoThinkProxy }
        Stop-LlamaServer
    }
}

function Start-ClaudeWithLlamaCppModel {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$Key,
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$ContextKey,
        [Parameter(Mandatory = $true)][ValidateSet('native', 'turboquant', 'mtpturbo')][string]$Mode,
        [string]$KvCacheK,
        [string]$KvCacheV,
        [string]$Tools,
        [Nullable[bool]]$IncludeInlineToolSchemas,
        [switch]$LimitTools,
        [switch]$LocalPilot,
        [switch]$Codex,
        [switch]$Strict,
        [switch]$UseVision,
        [switch]$AutoBest,
        [switch]$AutoBestStrict,
        [ValidateSet('auto','pure','balanced','short','long')][string]$AutoBestProfile = 'auto',
        [string[]]$ExtraArgs,
        [string[]]$ExtraLocalPilotArgs,
        [AllowEmptyString()][string]$SpecType,
        [int]$SpecDraftNMax,
        [switch]$DryRun
    )

    $def = Get-ModelDef -Key $Key
    if ($LocalPilot) {
        Start-LocalPilot `
            -Key $Key `
            -ContextKey $ContextKey `
            -LlamaCppMode $Mode `
            -KvCacheK $KvCacheK `
            -KvCacheV $KvCacheV `
            -Strict:$Strict `
            -UseVision:$UseVision `
            -UseAutoBest:$AutoBest `
            -AutoBestProfile $AutoBestProfile `
            -ExtraLocalPilotArgs $ExtraLocalPilotArgs `
            -DryRun:$DryRun
        return
    }

    # Resolve MTP params: per-call > per-model def > caller-provided default
    if ([string]::IsNullOrWhiteSpace($SpecType)) {
        $SpecType = if ($def.ContainsKey('SpecType') -and -not [string]::IsNullOrWhiteSpace($def.SpecType)) { [string]$def.SpecType } else { '' }
    }
    if ($SpecDraftNMax -le 0) {
        $SpecDraftNMax = if ($def.ContainsKey('SpecDraftNMax') -and $null -ne $def.SpecDraftNMax) { [int]$def.SpecDraftNMax } else { 0 }
    }

    if ([string]::IsNullOrWhiteSpace($Tools)) {
        $Tools = $script:Cfg.LocalModelTools
    }

    if ($null -eq $IncludeInlineToolSchemas) {
        $IncludeInlineToolSchemas = [bool]$LimitTools
    }

    # Stop any prior llama-server we own.
    if (-not $DryRun) {
        Stop-LlamaServer -Quiet
    }

    # Resolve GGUF. Real launch downloads on demand; DryRun only inspects the
    # expected path so we don't pull gigabytes for a preview.
    $dryRunGgufNote = $null
    if ($DryRun) {
        $folder = Join-Path $script:Cfg.LlamaCppGgufRoot $def.Root
        $fileName = Get-ModelFileName -Def $def
        $ggufPath = Resolve-HuggingFaceLocalPath -DestinationFolder $folder -FileName $fileName
        if (-not (Test-Path -LiteralPath $ggufPath)) {
            $dryRunGgufNote = "GGUF not present locally; a real launch would download from $($def.Repo)/$fileName"
        }
    }
    else {
        $ggufPath = Get-ModelGgufPath -Def $def
    }

    # Pick a free port from the configured default.
    $defaultPort = if ($script:Cfg.Contains('LlamaCppPort')) { [int]$script:Cfg.LlamaCppPort } else { 8080 }
    $port = Find-LlamaCppFreePort -StartPort $defaultPort

    # Both modes are native processes — same path semantics.
    $modelArgPath = $ggufPath

    $thinkingPolicy = if ($def.Contains('ThinkingPolicy') -and -not [string]::IsNullOrWhiteSpace($def.ThinkingPolicy)) { [string]$def.ThinkingPolicy } else { 'strip' }
    $agentParallel = if ($script:Cfg.Contains('LlamaCppAgentParallel')) {
        try { [int]$script:Cfg.LlamaCppAgentParallel } catch { 1 }
    } else {
        1
    }
    $agentCacheReuse = if ($script:Cfg.Contains('LlamaCppAgentCacheReuse')) {
        try { [int]$script:Cfg.LlamaCppAgentCacheReuse } catch { 256 }
    } else {
        256
    }

    # Resolve vision module (mmproj) on demand when user opts in; always log availability.
    $visionModulePath = if ($UseVision) {
        Write-LaunchLog "Resolving vision module for llama.cpp launch (model=$($def.Root))" 'VISION'
        $result = Get-ModelVisionModulePath -Key $Key -Def $def
        if ($result) {
            Write-LaunchLog "Vision module resolved: $([System.IO.Path]::GetFileName($result))" 'VISION'
        } else {
            Write-LaunchLog "No vision module found for $Key" 'WARN'
        }
        $result
    } else {
        $avail = Test-ModelVisionModuleAvailable -Key $Key -Def $def
        if ($avail.Local) {
            Write-LaunchLog "Vision available locally ($($avail.Filename)) — not loaded (no -UseVision)" 'VISION'
        } elseif ($avail.AvailableOnHF) {
            Write-LaunchLog "Vision available on HuggingFace ($($avail.Filename)) — not loaded (no -UseVision)" 'VISION'
        } else {
            Write-LaunchLog "No vision module available for $Key" 'VISION'
        }
        ''
    }

    if ($UseVision -and $visionModulePath) {
        Write-Host "Vision: loaded mmproj $([System.IO.Path]::GetFileName($visionModulePath))" -ForegroundColor DarkCyan
    } elseif ($UseVision) {
        Write-Warning "Vision requested but no mmproj found for $Key"
    }

    $buildParams = @{
        Def              = $def
        ContextKey       = $ContextKey
        Mode             = $Mode
        ModelArgPath     = $modelArgPath
        Port             = $port
        ThinkingPolicy   = $thinkingPolicy
        VisionModulePath = $(if ($visionModulePath) { $visionModulePath } else { '' })
    }
    if ($agentParallel -gt 0) { $buildParams.Parallel = $agentParallel }
    # CacheReuse is applied as a fallback AFTER the AutoBest merge (below) so a
    # tuned profile's value (including 0 = reuse disabled, needed for vision)
    # takes precedence over the hardcoded config default.

    if (-not [string]::IsNullOrWhiteSpace($KvCacheK)) { $buildParams.KvK = $KvCacheK }
    if (-not [string]::IsNullOrWhiteSpace($KvCacheV)) { $buildParams.KvV = $KvCacheV }
    if ($Strict)    { $buildParams.Strict = $true }
    if ($ExtraArgs) { $buildParams.ExtraArgs = $ExtraArgs }
    if (-not [string]::IsNullOrWhiteSpace($SpecType))       { $buildParams.SpecType = $SpecType }
    if ($SpecDraftNMax -gt 0)                                { $buildParams.SpecDraftNMax = $SpecDraftNMax }

    # -AutoBest splats saved tuner overrides into Build-LlamaServerArgs.
    # Caller-supplied args (KvCacheK/KvCacheV/ExtraArgs above) take precedence
    # because they were set before this block — we only fill in keys that
    # haven't already been bound.
    $autoBestLoadedProfile = $null
    if ($AutoBest) {
        $bestEntry = $null
        $selectionProfile = if ($AutoBestProfile -in @('pure', 'balanced')) { $AutoBestProfile } else { 'auto' }
        $promptProfileOverride = if ($AutoBestProfile -in @('short', 'long')) { $AutoBestProfile } else { $null }
        $loadedProfile = $AutoBestProfile
        if ($promptProfileOverride) {
            $bestEntry = Get-BestLlamaCppConfig -Key $Key -ContextKey $ContextKey -Mode $Mode -PromptLength $promptProfileOverride -Profile pure -Vision $UseVision -AllowVisionFallback:$UseVision
            $loadedProfile = "pure/$promptProfileOverride"
        } else {
            $preferred = Get-PreferredLlamaCppBestConfig -Key $Key -ContextKey $ContextKey -Mode $Mode -Profile $selectionProfile -Vision $UseVision -AllowVisionFallback:$UseVision
            if ($preferred) {
                $bestEntry = $preferred.Entry
                $loadedProfile = "$($preferred.Profile)/$($preferred.PromptLength)"
            }
        }
        if ($bestEntry -and $bestEntry.overrides) {
            $autoBestLoadedProfile = $loadedProfile
            Write-Host "AutoBest: loaded saved tuner config (profile=$loadedProfile, score=$($bestEntry.score) $($bestEntry.scoreUnit), trials=$($bestEntry.trial_count))." -ForegroundColor Cyan
            if ($UseVision -and -not [bool]$bestEntry.vision) {
                Write-Warning "AutoBest: no vision-tuned config exists for this model; loaded a text-only tune as fallback. It was measured without the mmproj, so VRAM headroom is tighter - if you hit OOM, raise --n-cpu-moe or launch without vision."
            }
            if ([string]$bestEntry.scoreUnit -match '^(gen|tg)_') {
                Write-Warning "AutoBest: this is a generation-only profile. Re-run: findbest $Key -ContextKey $ContextKey -Mode $Mode"
            }
            $staleReasons = @(Test-LlamaCppBestConfigStale -Entry $bestEntry -Mode $Mode)
            if ($staleReasons.Count -gt 0) {
                $msg = "AutoBest: hardware/build changed since last tune - saved config may be stale. Re-run: findbest $Key -ContextKey $ContextKey -Mode $Mode"
                if ($AutoBestStrict) { throw $msg }
                Write-Warning $msg
                foreach ($reason in $staleReasons) {
                    Write-Warning "AutoBest: $reason"
                }
            }
            $tunable = @('KvK','KvV','NGpuLayers','NCpuMoe','UbatchSize','BatchSize','Threads','ThreadsBatch','Mlock','NoMmap','FlashAttn','SplitMode','SwaFull','CachePrompt','CacheReuse','SpecType','SpecDraftNMax')
            foreach ($k in $tunable) {
                if ($buildParams.ContainsKey($k)) { continue }
                $val = $null
                if ($bestEntry.overrides -is [System.Collections.IDictionary]) {
                    if ($bestEntry.overrides.Contains($k)) { $val = $bestEntry.overrides[$k] }
                } else {
                    $prop = $bestEntry.overrides.PSObject.Properties[$k]
                    if ($prop) { $val = $prop.Value }
                }
                if ($null -ne $val) { $buildParams[$k] = $val }
            }
        } elseif ($bestEntry) {
            Write-Warning "AutoBest: matched saved entry has no 'overrides' field (older tuner version?). Skipping."
        } else {
            $currentVram = Get-LocalLLMVRAMGB
            $quant = if ($def.Contains('Quant')) { [string]$def.Quant } else { '' }
            $profilesToCheck = if ($promptProfileOverride) { @($promptProfileOverride) } else { @('long', 'short') }
            $selectionProfilesToCheck = if ($selectionProfile -eq 'auto') { @('balanced', 'pure') } else { @($selectionProfile) }
            $candidates = @()
            foreach ($selectionName in $selectionProfilesToCheck) {
                foreach ($profileName in $profilesToCheck) {
                    $candidates += @(Get-LlamaCppBestConfigCandidates -Key $Key -ContextKey $ContextKey -Mode $Mode -PromptLength $profileName -Quant $quant -Profile $selectionName -Vision $UseVision)
                }
            }
            foreach ($candidate in $candidates) {
                if ($candidate.vramGB -and [Math]::Abs([int]$candidate.vramGB - [int]$currentVram) -gt 1) {
                    Write-Warning "AutoBest: saved config VRAM was $($candidate.vramGB)GB, current detected VRAM is ${currentVram}GB."
                    break
                }
            }
            $profileHint = if ($promptProfileOverride) { $promptProfileOverride } else { 'long' }
            $visionState = if ($UseVision) { 'on' } else { 'off' }
            Write-Warning "AutoBest: no saved config matches (key=$Key contextKey=$ContextKey mode=$Mode autoBestProfile=$AutoBestProfile vision=$visionState quant=$quant vram=${currentVram}GB). Run: findbest $Key -ContextKey $ContextKey -Mode $Mode -PromptLengths $profileHint"
        }
    }

    # Fallback: only apply the config CacheReuse default when neither a tuned
    # profile nor an explicit launch setting already bound it.
    if (-not $buildParams.ContainsKey('CacheReuse') -and $agentCacheReuse -gt 0) {
        $buildParams.CacheReuse = $agentCacheReuse
    }

    $serverArgs = Build-LlamaServerArgs @buildParams

    # Resolve the server binary based on mode (upstream vs turboquant fork).
    # DryRun must not trigger an install — Find-* returns $null if absent.
    $dryRunServerNote = $null
    if ($DryRun) {
        $serverPath = switch ($Mode) {
            'turboquant' { try { Find-TurboquantServerExe } catch { $null } }
            'mtpturbo'   { try { Find-MtpTurboServerExe   } catch { $null } }
            default      { Find-LlamaServerExe }
        }
        if (-not $serverPath) {
            $serverPath = '<not installed>'
            $dryRunServerNote = if ($Mode -eq 'mtpturbo') {
                "llama-server ($Mode) is not installed; this mode requires a self-built binary under $(Get-LlamaCppMtpTurboInstallRoot)"
            } else {
                "llama-server ($Mode) is not installed; a real launch would install it first"
            }
        }
    }
    else {
        $serverPath = switch ($Mode) {
            'turboquant' { Ensure-LlamaServerTurboquant }
            'mtpturbo'   { Ensure-LlamaServerMtpTurbo }
            default      { Ensure-LlamaServerNative }
        }
    }

    if ($DryRun) {
        $thinking = if ($thinkingPolicy -eq 'keep') {
            'kept (direct to llama-server)'
        } else {
            "stripped via no-think-proxy:$($script:NoThinkProxyPort)"
        }

        $baseUrl = if ($thinkingPolicy -eq 'keep') {
            "http://localhost:$port"
        } else {
            "http://localhost:$($script:NoThinkProxyPort)"
        }

        $systemPrompt = Get-LocalModelSystemPrompt -IncludeInlineToolSchemas:$IncludeInlineToolSchemas

        $permArgs = @(Get-LocalModelPermissionArgs)
        $launchArgs = if ($LimitTools) {
            @($permArgs) + @('--tools', $Tools, '--append-system-prompt', $systemPrompt)
        }
        else {
            @($permArgs) + @('--append-system-prompt', $systemPrompt)
        }

        $title = if ($Codex) {
            "codex via llama.cpp ($Mode)"
        } elseif ($LocalPilot) {
            "localpilot via llama.cpp ($Mode)"
        } else {
            "claude via llama.cpp ($Mode)"
        }

        $snapshotMaxImages = 0
        if ($def.ContainsKey('MaxImagesPerRequest')) {
            try { $snapshotMaxImages = [int]$def.MaxImagesPerRequest } catch { $snapshotMaxImages = 0 }
        }
        $env = if ($Codex) {
            [ordered]@{}
        } else {
            Get-LocalLLMClaudeEnvSnapshot -BaseUrl $baseUrl -Model $def.Root -KeepThinking:($thinkingPolicy -eq 'keep') -MaxImagesPerRequest $snapshotMaxImages
        }

        $launchExe = if ($LocalPilot) { 'localpilot' } elseif ($Codex) { 'codex' } else { 'claude' }
        $launchExeArgs = if ($Codex) {
            @()
        } elseif ($LocalPilot) {
            @($launchArgs)
        } else {
            @('--model', $def.Root) + $launchArgs
        }

        $contextTokens = Get-ModelContextValue -Def $def -ContextKey $ContextKey
        $quantLabel    = if ($def.Contains('Quant')) { [string]$def.Quant } else { $null }
        $parserLabel   = if ($def.Contains('Parser') -and -not [string]::IsNullOrWhiteSpace([string]$def.Parser)) { [string]$def.Parser } else { 'none' }
        $vramSize      = if ($quantLabel) { Get-QuantSizeGB -Def $def -QuantKey $quantLabel } else { $null }
        $vramInfo      = Get-LocalLLMVRAMInfo
        $fit           = if ($quantLabel) { Get-QuantFitClass -Def $def -QuantKey $quantLabel } else { '' }
        $toolsLabel    = if ($LimitTools) { "limited ($Tools)" } else { 'all' }
        $healthTimeout = if ($script:Cfg.Contains('LlamaCppHealthCheckTimeoutSec')) { [int]$script:Cfg.LlamaCppHealthCheckTimeoutSec } else { 300 }

        $notes = @()
        if ($dryRunGgufNote)   { $notes += $dryRunGgufNote }
        if ($dryRunServerNote) { $notes += $dryRunServerNote }
        if ($AutoBest -and -not [string]::IsNullOrWhiteSpace($autoBestLoadedProfile)) {
            $notes += "AutoBest: loaded saved tuner profile=$autoBestLoadedProfile (overrides applied to argv)"
        }
        if (-not [string]::IsNullOrWhiteSpace($SpecType) -and $SpecDraftNMax -gt 0) {
            $emittedSpec = ConvertTo-LlamaCppSpecTypeForMode -SpecType $SpecType -Mode $Mode
            $notes += "MTP: --spec-type $emittedSpec --spec-draft-n-max $SpecDraftNMax"
        }

        $plan = @{
            Title            = $title
            Backend          = 'llamacpp'
            Mode             = $Mode
            Key              = $Key
            Model            = $def.Root
            ContextKey       = $ContextKey
            ContextTokens    = $contextTokens
            Quant            = $quantLabel
            Parser           = $parserLabel
            GgufPath         = $ggufPath
            ServerPath       = $serverPath
            ServerArgs       = $serverArgs
            Port             = $port
            BaseUrl          = $baseUrl
            HealthCheck      = "http://127.0.0.1:$port/v1/models"
            HealthTimeoutSec = $healthTimeout
            Tools            = $toolsLabel
            Thinking         = $thinking
            VramNeeded       = $vramSize
            VramAvailable    = $vramInfo.GB
            VramSource       = $vramInfo.Source
            FitClass         = $fit
            Env              = $env
            LaunchExe        = $launchExe
            LaunchArgs       = $launchExeArgs
            Notes            = $notes
        }

        Show-LocalLLMLaunchPlan -Plan $plan
        return
    }

    $logPaths = New-LlamaServerLogPaths

    Write-Host ""
    Write-Host "Starting llama-server for $($def.Root) via llama.cpp ($Mode)..." -ForegroundColor Cyan
    Write-Host "  Server   : $serverPath" -ForegroundColor DarkGray
    Write-Host "  GGUF     : $ggufPath" -ForegroundColor DarkGray
    if ($VisionModulePath) {
        Write-Host "  Vision   : $([System.IO.Path]::GetFileName($VisionModulePath))" -ForegroundColor DarkCyan
    }
    Write-Host "  Port     : $port" -ForegroundColor DarkGray
    Write-Host "  Logs     : $($logPaths.Out)" -ForegroundColor DarkGray
    Write-Host "             $($logPaths.Err)" -ForegroundColor DarkGray
    Write-Host "  Args     : $(Format-LocalLLMArgvLine -Argv $serverArgs)" -ForegroundColor DarkGray

    Write-LaunchLog "llama-server: path=$serverPath port=$port gguf=$ggufPath mode=$Mode" 'SERVER'
    Write-LaunchLog "llama-server argv: $(Format-LocalLLMArgvLine -Argv (@($serverPath) + $serverArgs))" 'SERVER'

    $proc = Start-LlamaServerNative -ServerPath $serverPath -ServerArgs $serverArgs -OutLogPath $logPaths.Out -ErrLogPath $logPaths.Err
    Write-Host "  PID      : $($proc.Id)" -ForegroundColor DarkGray
    Write-LaunchLog "llama-server started: pid=$($proc.Id) port=$port" 'SERVER'

    $session = @{
        Backend  = 'llamacpp'
        Mode     = $Mode
        Port     = $port
        BaseUrl  = "http://localhost:$port"
        Model    = $def.Root
        GgufPath = $ggufPath
        Pid      = $proc.Id
        OutLog   = $logPaths.Out
        ErrLog   = $logPaths.Err
    }

    Set-CurrentBackendSession -Session $session

    try {
        Wait-LlamaServer -Port $port -Process $proc -OutLogPath $logPaths.Out -ErrLogPath $logPaths.Err
    }
    catch {
        Stop-LlamaServer -Quiet
        if ((Test-LlamaCppSpecFallbackEligible -ErrorRecord $_ -BuildParams $buildParams) -and (Disable-LlamaCppSpecDecode -BuildParams $buildParams)) {
            Write-Warning "llama-server failed while loading the MTP head; retrying once without speculative MTP."
            Write-LaunchLog "llama-server MTP head load failed; retrying without --spec-type" 'WARN'
            $serverArgs = Build-LlamaServerArgs @buildParams
            $logPaths = New-LlamaServerLogPaths

            Write-Host "  Retry Logs : $($logPaths.Out)" -ForegroundColor DarkGray
            Write-Host "               $($logPaths.Err)" -ForegroundColor DarkGray
            Write-Host "  Retry Args : $(Format-LocalLLMArgvLine -Argv $serverArgs)" -ForegroundColor DarkGray
            Write-LaunchLog "llama-server retry argv: $(Format-LocalLLMArgvLine -Argv (@($serverPath) + $serverArgs))" 'SERVER'

            $proc = Start-LlamaServerNative -ServerPath $serverPath -ServerArgs $serverArgs -OutLogPath $logPaths.Out -ErrLogPath $logPaths.Err
            Write-Host "  Retry PID  : $($proc.Id)" -ForegroundColor DarkGray
            Write-LaunchLog "llama-server retry started: pid=$($proc.Id) port=$port" 'SERVER'

            $session = @{
                Backend  = 'llamacpp'
                Mode     = $Mode
                Port     = $port
                BaseUrl  = "http://localhost:$port"
                Model    = $def.Root
                GgufPath = $ggufPath
                Pid      = $proc.Id
                OutLog   = $logPaths.Out
                ErrLog   = $logPaths.Err
            }
            Set-CurrentBackendSession -Session $session

            try {
                Wait-LlamaServer -Port $port -Process $proc -OutLogPath $logPaths.Out -ErrLogPath $logPaths.Err
            }
            catch {
                Stop-LlamaServer -Quiet
                throw
            }
        } else {
            throw
        }
    }

    $contextTokens = Get-ModelContextValue -Def $def -ContextKey $ContextKey

    if ($Codex) {
        try {
            $maxOutputTokens = if ($script:Cfg.Contains("LocalModelMaxOutputTokens")) {
                try { [int]$script:Cfg.LocalModelMaxOutputTokens } catch { 0 }
            } else {
                0
            }

            Write-Host ""
            Write-Host "Launching codex with $($def.Root) via llama.cpp ($Mode)..." -ForegroundColor Cyan
            Write-Host "  Base URL : http://localhost:$port/v1" -ForegroundColor DarkGray
            Write-Host "  Model    : $($def.Root)" -ForegroundColor DarkGray
            Write-Host "  GGUF     : $ggufPath" -ForegroundColor DarkGray
            if ($VisionModulePath) {
                Write-Host "  Vision   : $([System.IO.Path]::GetFileName($VisionModulePath))" -ForegroundColor DarkCyan
            }
            Write-Host "  Port     : $port" -ForegroundColor DarkGray
            Write-Host "  Strict   : $([bool]$Strict)" -ForegroundColor DarkGray
            Write-Host ""

            Start-CodexCli -Model $def.Root -BaseUrl "http://localhost:$port/v1" -ContextTokens $contextTokens -MaxOutputTokens $maxOutputTokens
        }
        finally {
            Stop-LlamaServer
        }
        return
    }

    Save-ClaudeEnvBackup

    try {
    # Front llama-server with no-think-proxy unless the model opts to keep
    # thinking. The proxy strips <think>...</think> from /v1/messages
    # responses, which both reasoning-Qwen variants and Heretic merges leak
    # into the assistant text and break LocalPilot's session-title parser.
    $useNoThinkProxy = ($thinkingPolicy -ne 'keep')

    if ($useNoThinkProxy) {
        Start-NoThinkProxy -TargetPort $port
        $effectiveBaseUrl = "http://localhost:$($script:NoThinkProxyPort)"
    }
    else {
        $effectiveBaseUrl = "http://localhost:$port"
    }

    $systemPrompt = Get-LocalModelSystemPrompt -IncludeInlineToolSchemas:$IncludeInlineToolSchemas

    if ($AutoBest -and -not [string]::IsNullOrWhiteSpace($autoBestLoadedProfile)) {
        $smoke = Test-ClaudeLocalVisibleResponse -BaseUrl $effectiveBaseUrl -Model $def.Root -SystemPrompt $systemPrompt
        if (-not $smoke.Ok -and $useNoThinkProxy) {
            Write-Warning "AutoBest: launch smoke through no-think proxy produced no visible text; trying direct llama-server routing for this session."
            $directBaseUrl = "http://localhost:$port"
            $directSmoke = Test-ClaudeLocalVisibleResponse -BaseUrl $directBaseUrl -Model $def.Root -SystemPrompt $systemPrompt
            if ($directSmoke.Ok) {
                $effectiveBaseUrl = $directBaseUrl
            } else {
                $detail = Format-ClaudeLocalSmokeFailure -Smoke $directSmoke
                throw "AutoBest: saved profile failed launch smoke through proxy and direct llama-server route ($detail). Re-run tuning or launch without -AutoBest."
            }
        } elseif (-not $smoke.Ok) {
            $detail = Format-ClaudeLocalSmokeFailure -Smoke $smoke
            throw "AutoBest: saved profile failed launch smoke ($detail). Re-run tuning or launch without -AutoBest."
        }
    }

        # Lift the local-backend image cap only when the model def declares it can
        # handle more than one image per request; otherwise leave LocalPilot's
        # safe default of 1. Only meaningful for a vision (-UseVision) launch.
        $maxImagesPerRequest = 0
        if ($def.ContainsKey('MaxImagesPerRequest')) {
            try { $maxImagesPerRequest = [int]$def.MaxImagesPerRequest } catch { $maxImagesPerRequest = 0 }
        }
        Set-ClaudeLocalEnv -BaseUrl $effectiveBaseUrl -Model $def.Root -KeepThinking:($thinkingPolicy -eq 'keep') -ContextTokens $contextTokens -MaxImagesPerRequest $maxImagesPerRequest
        Set-LocalBackendTelemetryEnv -ProcessId $proc.Id -Port $port -OutLogPath $logPaths.Out -ErrLogPath $logPaths.Err -GgufPath $ggufPath -Model $def.Root -ContextKey $ContextKey -ContextTokens $contextTokens

        $backendLabel = if ($LocalPilot) { "localpilot" } else { "claude" }
        $toolsLabel = if ($LimitTools) { "limited" } else { "all" }
        $thinkingLabel = if ($thinkingPolicy -eq 'keep') {
            "kept (direct to llama-server)"
        } elseif ($effectiveBaseUrl -eq "http://localhost:$($script:NoThinkProxyPort)") {
            "stripped via no-think-proxy:$($script:NoThinkProxyPort)"
        } else {
            "disabled; direct route after proxy smoke fallback"
        }

        Write-Host ""
        Write-Host "Launching $backendLabel with $($def.Root) via llama.cpp ($Mode)..." -ForegroundColor Cyan
        Write-Host "  Base URL : $effectiveBaseUrl" -ForegroundColor DarkGray
        Write-Host "  Model    : $($def.Root)" -ForegroundColor DarkGray
        Write-Host "  GGUF     : $ggufPath" -ForegroundColor DarkGray
        if ($VisionModulePath) {
            Write-Host "  Vision   : $([System.IO.Path]::GetFileName($VisionModulePath))" -ForegroundColor DarkCyan
        }
        Write-Host "  Port     : $port" -ForegroundColor DarkGray
        $agentSlotsLabel = if ($agentParallel -gt 0) { [string]$agentParallel } else { 'auto' }
        $agentCacheReuseLabel = if ($agentCacheReuse -gt 0) { [string]$agentCacheReuse } else { 'default' }
        Write-Host "  Agent    : slots=$agentSlotsLabel cache-reuse=$agentCacheReuseLabel" -ForegroundColor DarkGray
        Write-Host "  Thinking : $thinkingLabel" -ForegroundColor DarkGray
        Write-Host "  Tools    : $toolsLabel" -ForegroundColor DarkGray
        Write-Host "  Strict   : $([bool]$Strict)" -ForegroundColor DarkGray
        Write-Host ""

        $permArgs = @(Get-LocalModelPermissionArgs)
        $launchArgs = if ($LimitTools) {
            @($permArgs) + @(
                '--tools',
                $Tools,
                '--append-system-prompt',
                $systemPrompt
            )
        }
        else {
            @($permArgs) + @(
                '--append-system-prompt',
                $systemPrompt
            )
        }

        Write-LaunchLog "Launching ${backendLabel}: model=$($def.Root) base=$effectiveBaseUrl localpilot=$LocalPilot" 'LAUNCH'

        & claude --model $def.Root @launchArgs
    }
    finally {
        Restore-ClaudeEnvBackup

        if ($useNoThinkProxy) {
            Stop-NoThinkProxy
        }

        Stop-LlamaServer
    }
}
