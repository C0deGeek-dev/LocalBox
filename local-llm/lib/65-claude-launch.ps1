# Claude Code / Unshackled launcher path. Backs up Claude env vars, points
# them at the no-think strip proxy in front of llama-server, launches the
# agent, restores the env on exit.

$script:ClaudeEnvBackup = @{}
$script:NoThinkProxyProcess = $null
$script:RemoteGatewaySession = $null

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

function Get-LocalLLMRemoteClientEnvCommands {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$BaseUrl,
        [Parameter(Mandatory = $true)][string]$Password
    )

    $vars = [ordered]@{
        ANTHROPIC_BASE_URL   = $BaseUrl
        ANTHROPIC_AUTH_TOKEN = $Password
        ANTHROPIC_API_KEY    = $Password
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

function Test-LocalLLMRemotePublicHttp {
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
        [int]$ContextTokens = 0
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
    # Unshackled may withhold real tools behind ToolSearch or send schema
    # fields that local proxies tolerate inconsistently.
    $env:CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS = "1"
    $env:ENABLE_TOOL_SEARCH = "false"
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
    $targetMatches = Test-NoThinkProxyTarget -ListenPort $ListenPort -TargetHost $TargetHost -TargetPort $TargetPort -AuthToken $AuthToken
    if ($targetMatches -eq $true) {
        if ($logsRequested) {
            if ($script:NoThinkProxyProcess -and -not $script:NoThinkProxyProcess.HasExited) {
                Write-LaunchLog "Restarting owned no-think proxy so remote gateway logs are captured." 'PROXY'
                Stop-NoThinkProxy
            }
            else {
                throw "No-think proxy port $ListenPort is already running for target $target, but remote gateway logs were requested and this shell does not own that process. Stop the existing proxy with llm-stop or free the port, then start Remote again."
            }
        }
        else {
            Write-LaunchLog "No-think proxy already running for target=$target" 'PROXY'
            return
        }
    }
    if ($targetMatches -eq $false) {
        throw "No-think proxy port $ListenPort is already in use by a proxy for a different or unverifiable target. Stop that process or change NoThinkProxyPort."
    }

    $proxyScript = Join-Path $HOME ".localbox-proxy\no-think-proxy.py"

    if ($script:NoThinkProxyProcess -and -not $script:NoThinkProxyProcess.HasExited) {
        Write-LaunchLog "Reusing existing no-think proxy process (PID=$($script:NoThinkProxyProcess.Id))" 'PROXY'
        return
    }

    Write-LaunchLog "Starting no-think proxy: python $proxyScript $ListenPort $target $ListenHost" 'PROXY'

    if (-not (Test-Path $proxyScript)) {
        throw "No-think proxy not found: $proxyScript. Re-run install.ps1 so Claude/Unshackled launches do not point at a dead proxy URL."
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

    $deadline = (Get-Date).AddSeconds(3)
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

function New-LocalLLMRemoteGatewayLogPaths {
    $dir = Join-Path $HOME ".local-llm\logs"
    if (-not (Test-Path -LiteralPath $dir)) {
        New-Item -ItemType Directory -Force -Path $dir | Out-Null
    }

    $stamp = Get-Date -Format "yyyyMMdd-HHmmss-fff"
    return @{
        Out = Join-Path $dir "remote-gateway-$stamp.out.log"
        Err = Join-Path $dir "remote-gateway-$stamp.err.log"
    }
}

function Format-LocalLLMRemoteGatewayStatus {
    [CmdletBinding()]
    param([Parameter(Mandatory = $true)][hashtable]$Session)

    $lines = New-Object System.Collections.Generic.List[string]
    $lines.Add("Remote gateway running") | Out-Null
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
        catch {}
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

function Show-LocalLLMRemoteGatewayStatus {
    [CmdletBinding()]
    param([Parameter(Mandatory = $true)][hashtable]$Session)

    Write-Host ""
    foreach ($line in (Format-LocalLLMRemoteGatewayStatus -Session $Session)) {
        Write-Host $line -ForegroundColor Cyan
    }
}

function Watch-LocalLLMRemoteGateway {
    [CmdletBinding()]
    param([Parameter(Mandatory = $true)][hashtable]$Session)

    Show-LocalLLMRemoteGatewayStatus -Session $Session
    Write-Host ""
    Write-Host "Controls: Q = return to menu and keep running, S = stop gateway/backend, R = reprint client env vars" -ForegroundColor Yellow
    Write-Host "Gateway log follows: $($Session.GatewayOutLog)" -ForegroundColor DarkGray
    Write-Host ""

    if ([Console]::IsInputRedirected -or [Console]::IsOutputRedirected) {
        Write-Host "Non-interactive console detected; leaving remote gateway running. Use llm-stop to stop it." -ForegroundColor Yellow
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
            Write-Host "Remote gateway process has exited. Check logs above." -ForegroundColor Red
            return
        }

        $deadline = (Get-Date).AddMilliseconds(1000)
        while ((Get-Date) -lt $deadline) {
            if ([Console]::KeyAvailable) {
                $key = [Console]::ReadKey($true).Key
                switch ($key) {
                    'Q' {
                        Write-Host "Remote gateway left running. Use llm-stop to stop it." -ForegroundColor Green
                        return
                    }
                    'S' {
                        Stop-LocalLLMRemoteGateway
                        Stop-LlamaServer -Quiet
                        Write-Host "Remote gateway stopped." -ForegroundColor Yellow
                        return
                    }
                    'R' {
                        if ($Session.BaseUrls -and $Session.Password) {
                            Show-LocalLLMRemoteClientInstructions -BaseUrls $Session.BaseUrls -Password $Session.Password
                        }
                    }
                }
            }
            Start-Sleep -Milliseconds 100
        }
    }
}

function Get-LocalLLMRemoteAdvertiseHosts {
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
    catch {}

    if ($hosts.Count -eq 0) {
        $hosts.Add('<server-ip>') | Out-Null
    }

    return @($hosts)
}

function Show-LocalLLMRemoteClientInstructions {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string[]]$BaseUrls,
        [Parameter(Mandatory = $true)][string]$Password
    )

    $baseUrl = $BaseUrls[0]
    $commands = Get-LocalLLMRemoteClientEnvCommands -BaseUrl $baseUrl -Password $Password

    Write-Host ""
    Write-Host "Remote gateway ready." -ForegroundColor Green
    Write-Host "  Base URL : $baseUrl" -ForegroundColor DarkGray
    if ($BaseUrls.Count -gt 1) {
        Write-Host "  Other LAN URLs:" -ForegroundColor DarkGray
        foreach ($url in @($BaseUrls | Select-Object -Skip 1)) {
            Write-Host "    $url" -ForegroundColor Gray
        }
    }

    if (Test-LocalLLMRemotePublicHttp -BaseUrl $baseUrl) {
        Write-Host ""
        Write-Host "WARNING: this is password-only HTTP on a public-looking address. Passwords and prompts are not encrypted." -ForegroundColor Yellow
    }

    Write-Host ""
    Write-Host "On the client, run normal Unshackled with:" -ForegroundColor Cyan
    foreach ($line in $commands.Bash) {
        Write-Host "  $line" -ForegroundColor Gray
    }
    Write-Host "  unshackled" -ForegroundColor Gray
    Write-Host ""
    Write-Host "PowerShell client equivalent:" -ForegroundColor Cyan
    foreach ($line in $commands.PowerShell) {
        Write-Host "  $line" -ForegroundColor Gray
    }
    Write-Host "  unshackled" -ForegroundColor Gray
    Write-Host ""
}

function Start-LocalLLMLlamaCppRemoteBackend {
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
        $ggufPath = Get-ModelGgufPath -Key $Key -Def $def
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
    if ($agentCacheReuse -gt 0) { $buildParams.CacheReuse = $agentCacheReuse }
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
            $bestEntry = Get-BestLlamaCppConfig -Key $Key -ContextKey $ContextKey -Mode $Mode -PromptLength $promptProfileOverride -Profile pure
            $loadedProfile = "pure/$promptProfileOverride"
        } else {
            $preferred = Get-PreferredLlamaCppBestConfig -Key $Key -ContextKey $ContextKey -Mode $Mode -Profile $selectionProfile
            if ($preferred) {
                $bestEntry = $preferred.Entry
                $loadedProfile = "$($preferred.Profile)/$($preferred.PromptLength)"
            }
        }

        if ($bestEntry -and $bestEntry.overrides) {
            $autoBestLoadedProfile = $loadedProfile
            Write-Host "AutoBest: loaded saved tuner config (profile=$loadedProfile, score=$($bestEntry.score) $($bestEntry.scoreUnit), trials=$($bestEntry.trial_count))." -ForegroundColor Cyan
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
            Write-Warning "AutoBest: no saved config matches (key=$Key contextKey=$ContextKey mode=$Mode autoBestProfile=$AutoBestProfile). Run: findbest $Key -ContextKey $ContextKey -Mode $Mode -PromptLengths $profileHint"
        }
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
        Write-Host "Starting llama-server remote backend for $($def.Root)..." -ForegroundColor Cyan
        Write-Host "  Server   : $serverPath" -ForegroundColor DarkGray
        Write-Host "  GGUF     : $ggufPath" -ForegroundColor DarkGray
        Write-Host "  Args     : $(Format-LocalLLMArgvLine -Argv $serverArgs)" -ForegroundColor DarkGray
        Write-Host "  Port     : $port" -ForegroundColor DarkGray
        Write-Host "  Logs     : $($logPaths.Out)" -ForegroundColor DarkGray
        Write-Host "             $($logPaths.Err)" -ForegroundColor DarkGray
        Write-LaunchLog "llama-server remote backend argv: $(Format-LocalLLMArgvLine -Argv (@($serverPath) + $serverArgs))" 'SERVER'

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

function Start-LocalLLMRemoteGateway {
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
        [string]$Password = $env:LOCAL_LLM_REMOTE_PASS,
        [string[]]$ExtraArgs,
        [switch]$NoMonitor,
        [switch]$DryRun
    )

    if ([string]::IsNullOrWhiteSpace($Password)) {
        throw "Remote gateway password is required. Set `$env:LOCAL_LLM_REMOTE_PASS or pass -Password."
    }

    if ($ListenPort -le 0) {
        $ListenPort = $script:NoThinkProxyPort
    }

    $backendInfo = Start-LocalLLMLlamaCppRemoteBackend -Key $Key -ContextKey $ContextKey -Mode $LlamaCppMode -KvCacheK $KvCacheK -KvCacheV $KvCacheV -Strict:$Strict -UseVision:$UseVision -AutoBest:$AutoBest -AutoBestProfile $AutoBestProfile -ExtraArgs $ExtraArgs -DryRun:$DryRun

    $hosts = if ([string]::IsNullOrWhiteSpace($AdvertiseHost)) {
        Get-LocalLLMRemoteAdvertiseHosts -ListenHost $ListenHost
    } else {
        @($AdvertiseHost)
    }
    $baseUrls = @($hosts | ForEach-Object { "http://${_}:$ListenPort" })
    $primaryBaseUrl = $baseUrls[0]

    if ($DryRun) {
        $commands = Get-LocalLLMRemoteClientEnvCommands -BaseUrl $primaryBaseUrl -Password $Password
        $notes = @("Client command: $($commands.Bash -join '; '); unshackled")
        if (Test-LocalLLMRemotePublicHttp -BaseUrl $primaryBaseUrl) {
            $notes += "Password-only HTTP on a public-looking address is not encrypted."
        }
        if ($backendInfo.AutoBestProfile) {
            $notes += "AutoBest: loaded saved tuner profile=$($backendInfo.AutoBestProfile) (overrides applied to server argv)"
        }
        $plan = @{
            Title       = "remote gateway via llama.cpp"
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

    $gatewayLogs = New-LocalLLMRemoteGatewayLogPaths
    Start-NoThinkProxy -ListenHost $ListenHost -ListenPort $ListenPort -TargetHost $backendInfo.TargetHost -TargetPort $backendInfo.TargetPort -AuthToken $Password -OutLogPath $gatewayLogs.Out -ErrLogPath $gatewayLogs.Err

    $script:RemoteGatewaySession = @{
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

    Show-LocalLLMRemoteClientInstructions -BaseUrls $baseUrls -Password $Password

    if (-not $NoMonitor) {
        Watch-LocalLLMRemoteGateway -Session $script:RemoteGatewaySession
    }
}

function Stop-LocalLLMRemoteGateway {
    [CmdletBinding()]
    param()

    Stop-NoThinkProxy
    $script:RemoteGatewaySession = $null
}

function Test-ClaudeLocalVisibleResponse {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$BaseUrl,
        [Parameter(Mandatory = $true)][string]$Model,
        [string]$SystemPrompt,
        [int]$TimeoutSec = 90
    )

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
    $looksAnswered = -not [string]::IsNullOrWhiteSpace($withoutThink)
    return [pscustomobject]@{
        Ok = $looksAnswered
        Text = $text
        VisibleText = $withoutThink
        Error = $(if ($looksAnswered) { '' } elseif ([string]::IsNullOrWhiteSpace($text)) { 'no response text' } else { 'no visible response text after stripping thinking output' })
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

function Ensure-UnshackledInstalled {
    # Confirms an Unshackled checkout exists at $script:Cfg.UnshackledRoot.
    # If not, asks before cloning from $script:Cfg.UnshackledRepoUrl.
    $root = $script:Cfg.UnshackledRoot

    if ([string]::IsNullOrWhiteSpace($root)) {
        throw "UnshackledRoot is not set. Run: Set-LocalLLMSetting UnshackledRoot '<path>'"
    }

    $cliPath = try {
        Join-Path $root "src\entrypoints\cli.tsx"
    }
    catch {
        throw "UnshackledRoot is not accessible: $root. Run: Set-LocalLLMSetting UnshackledRoot '<path>'"
    }

    if (Test-Path -LiteralPath $cliPath -PathType Leaf -ErrorAction SilentlyContinue) {
        return
    }

    $qualifier = Split-Path -Qualifier $root
    if (-not [string]::IsNullOrWhiteSpace($qualifier) -and -not (Test-Path -LiteralPath $qualifier -ErrorAction SilentlyContinue)) {
        throw "UnshackledRoot points at an unavailable drive or path: $root. Run: Set-LocalLLMSetting UnshackledRoot '<path>'"
    }

    $repoUrl = if (-not [string]::IsNullOrWhiteSpace($script:Cfg.UnshackledRepoUrl)) {
        $script:Cfg.UnshackledRepoUrl
    } else {
        "https://github.com/David-c0degeek/unshackled"
    }

    Write-Host ""
    Write-Host "Unshackled not found at $root" -ForegroundColor Yellow
    Write-Host "  Source: $repoUrl" -ForegroundColor DarkGray
    $answer = (Read-Host "Clone it now? [y/N]").Trim().ToLowerInvariant()

    if ($answer -notin @("y", "yes")) {
        throw "Unshackled is not installed at $root. Aborting. Override with: Set-LocalLLMSetting UnshackledRoot '<path>'"
    }

    if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
        throw "git is not on PATH; cannot clone Unshackled."
    }

    $parent = Split-Path -Parent $root

    if (-not [string]::IsNullOrWhiteSpace($parent)) {
        Ensure-Directory $parent
    }

    Write-Host "Cloning $repoUrl -> $root" -ForegroundColor Cyan
    & git clone $repoUrl $root

    if ($LASTEXITCODE -ne 0) {
        throw "git clone failed for $repoUrl"
    }

    if (-not (Test-Path $cliPath)) {
        throw "Cloned but $cliPath is missing — wrong repo URL? Check Set-LocalLLMSetting UnshackledRepoUrl."
    }
}

function Install-Unshackled {
    [CmdletBinding()]
    param(
        [string]$Destination = (Join-Path $HOME '.local-llm\tools\unshackled'),
        [switch]$Force
    )

    if ((Test-Path -LiteralPath (Join-Path $Destination 'src\entrypoints\cli.tsx')) -and -not $Force) {
        Write-Host "Unshackled already exists: $Destination" -ForegroundColor Green
        Set-LocalLLMSetting UnshackledRoot $Destination
        return $Destination
    }

    if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
        throw "git is not on PATH; cannot clone Unshackled."
    }

    $repoUrl = if (-not [string]::IsNullOrWhiteSpace($script:Cfg.UnshackledRepoUrl)) {
        [string]$script:Cfg.UnshackledRepoUrl
    } else {
        'https://github.com/David-c0degeek/unshackled'
    }

    Ensure-Directory (Split-Path -Parent $Destination)
    if (Test-Path -LiteralPath $Destination) {
        throw "Destination already exists: $Destination. Use Update-Unshackled, or remove it and retry."
    }

    & git clone $repoUrl $Destination
    if ($LASTEXITCODE -ne 0) { throw "git clone failed for $repoUrl" }

    Set-LocalLLMSetting UnshackledRoot $Destination
    return $Destination
}

function Update-Unshackled {
    [CmdletBinding()]
    param()

    $root = if (Get-Command Resolve-UnshackledRoot -ErrorAction SilentlyContinue) {
        Resolve-UnshackledRoot
    } else {
        $script:Cfg.UnshackledRoot
    }

    if ([string]::IsNullOrWhiteSpace($root) -or -not (Test-Path -LiteralPath $root)) {
        throw "Unshackled is not installed. Run Install-Unshackled first."
    }

    $result = Invoke-LocalLLMGitFastForwardUpdate -Name 'Unshackled' -Root $root
    if ($result.Status -in @('failed', 'not-git', 'no-upstream', 'diverged')) {
        throw $result.Reason
    }
    return $result
}

function Get-UnshackledExtraArgs {
    # Merges the -ExtraUnshackledArgs param with $env:UNSHACKLED_EXTRA_ARGS.
    # Env-var splitting is whitespace-only — sufficient for flags like `-D` or
    # `-D --debug-file=path`. For values containing spaces, pass via param.
    param([string[]]$Param)

    $extras = @()
    if ($env:UNSHACKLED_EXTRA_ARGS) {
        $extras += ($env:UNSHACKLED_EXTRA_ARGS -split '\s+' | Where-Object { $_ })
    }
    if ($Param) { $extras += $Param }
    return ,$extras
}

function Invoke-UnshackledCli {
    [CmdletBinding()]
    param(
        [Parameter(ValueFromRemainingArguments)]
        [string[]]$CliArgs
    )

    Ensure-UnshackledInstalled

    $root = $script:Cfg.UnshackledRoot

    if (-not (Get-Command bun -ErrorAction SilentlyContinue)) {
        throw "bun is not on PATH."
    }

    $nodeModules = Join-Path $root "node_modules"

    if (-not (Test-Path $nodeModules)) {
        Write-Host "Installing Unshackled dependencies..." -ForegroundColor Cyan

        & bun install --cwd $root

        if ($LASTEXITCODE -ne 0) {
            throw "bun install failed for Unshackled"
        }
    }

    & bun (Join-Path $root "src\entrypoints\cli.tsx") @CliArgs
}

function ConvertTo-CodexTomlString {
    param([AllowEmptyString()][string]$Value)

    $escaped = ([string]$Value) -replace '\\', '\\' -replace '"', '\"'
    return '"' + $escaped + '"'
}

function Get-CodexCommonArgs {
    $args = @()

    if ($script:Cfg.Contains("CodexEnableSearch") -and [bool]$script:Cfg.CodexEnableSearch) {
        $args += '--search'
    }

    $bypass = if ($script:Cfg.Contains("CodexBypassApprovalsAndSandbox")) {
        [bool]$script:Cfg.CodexBypassApprovalsAndSandbox
    } else {
        $true
    }
    if ($bypass) {
        $args += '--dangerously-bypass-approvals-and-sandbox'
    }

    return $args
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

    $args = @()

    $providerId = 'localbox_llamacpp'
    $idleMs = if ($script:Cfg.Contains("CodexStreamIdleTimeoutMs")) {
        try { [int]$script:Cfg.CodexStreamIdleTimeoutMs } catch { 10000000 }
    } else {
        10000000
    }

    $args += @(
        '-c', ('model_provider={0}' -f (ConvertTo-CodexTomlString $providerId)),
        '-c', ('model_providers.{0}.name={1}' -f $providerId, (ConvertTo-CodexTomlString 'LocalBox llama.cpp')),
        '-c', ('model_providers.{0}.base_url={1}' -f $providerId, (ConvertTo-CodexTomlString $BaseUrl)),
        '-c', ('model_providers.{0}.wire_api="responses"' -f $providerId),
        '-c', ('model_providers.{0}.stream_idle_timeout_ms={1}' -f $providerId, $idleMs)
    )

    if ($ContextTokens -gt 0) {
        $args += @('-c', "model_context_window=$ContextTokens")
    }
    if ($MaxOutputTokens -gt 0) {
        $args += @('-c', "model_max_output_tokens=$MaxOutputTokens")
    }

    $args += @('--model', $Model)
    $args += @(Get-CodexCommonArgs)

    Write-Host ""
    Write-Host "Launching codex with $Model..." -ForegroundColor Cyan
    Write-Host "  Base URL : $BaseUrl" -ForegroundColor DarkGray
    Write-Host "  Model    : $Model" -ForegroundColor DarkGray
    Write-Host ""

    & codex @args
}

function Get-ClaudeTargetSummary {
    if ($env:ANTHROPIC_DEFAULT_OPUS_MODEL) {
        return "Local -> $($env:ANTHROPIC_DEFAULT_OPUS_MODEL) @ $($env:ANTHROPIC_BASE_URL)"
    }

    return "Default (Anthropic API)"
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
        [switch]$Unshackled,
        [switch]$Codex,
        [switch]$Strict,
        [switch]$UseVision,
        [switch]$AutoBest,
        [switch]$AutoBestStrict,
        [ValidateSet('auto','pure','balanced','short','long')][string]$AutoBestProfile = 'auto',
        [string[]]$ExtraArgs,
        [string[]]$ExtraUnshackledArgs,
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
        $ggufPath = Get-ModelGgufPath -Key $Key -Def $def
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
    if ($agentCacheReuse -gt 0) { $buildParams.CacheReuse = $agentCacheReuse }

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
            $bestEntry = Get-BestLlamaCppConfig -Key $Key -ContextKey $ContextKey -Mode $Mode -PromptLength $promptProfileOverride -Profile pure
            $loadedProfile = "pure/$promptProfileOverride"
        } else {
            $preferred = Get-PreferredLlamaCppBestConfig -Key $Key -ContextKey $ContextKey -Mode $Mode -Profile $selectionProfile
            if ($preferred) {
                $bestEntry = $preferred.Entry
                $loadedProfile = "$($preferred.Profile)/$($preferred.PromptLength)"
            }
        }
        if ($bestEntry -and $bestEntry.overrides) {
            $autoBestLoadedProfile = $loadedProfile
            Write-Host "AutoBest: loaded saved tuner config (profile=$loadedProfile, score=$($bestEntry.score) $($bestEntry.scoreUnit), trials=$($bestEntry.trial_count))." -ForegroundColor Cyan
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
                    $candidates += @(Get-LlamaCppBestConfigCandidates -Key $Key -ContextKey $ContextKey -Mode $Mode -PromptLength $profileName -Quant $quant -Profile $selectionName)
                }
            }
            foreach ($candidate in $candidates) {
                if ($candidate.vramGB -and [Math]::Abs([int]$candidate.vramGB - [int]$currentVram) -gt 1) {
                    Write-Warning "AutoBest: saved config VRAM was $($candidate.vramGB)GB, current detected VRAM is ${currentVram}GB."
                    break
                }
            }
            $profileHint = if ($promptProfileOverride) { $promptProfileOverride } else { 'long' }
            Write-Warning "AutoBest: no saved config matches (key=$Key contextKey=$ContextKey mode=$Mode autoBestProfile=$AutoBestProfile vram=${currentVram}GB). Run: findbest $Key -ContextKey $ContextKey -Mode $Mode -PromptLengths $profileHint"
        }
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

        $launchArgs = if ($LimitTools) {
            @('--dangerously-skip-permissions', '--tools', $Tools, '--append-system-prompt', $systemPrompt)
        }
        else {
            @('--dangerously-skip-permissions', '--append-system-prompt', $systemPrompt)
        }

        $title = if ($Codex) {
            "codex via llama.cpp ($Mode)"
        } elseif ($Unshackled) {
            "unshackled via llama.cpp ($Mode)"
        } else {
            "claude via llama.cpp ($Mode)"
        }

        $env = if ($Codex) {
            [ordered]@{}
        } else {
            Get-LocalLLMClaudeEnvSnapshot -BaseUrl $baseUrl -Model $def.Root -KeepThinking:($thinkingPolicy -eq 'keep')
        }

        $launchExe = if ($Unshackled) { 'unshackled' } elseif ($Codex) { 'codex' } else { 'claude' }
        $launchExeArgs = if ($Codex) {
            @()
        } elseif ($Unshackled) {
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
        throw
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
    # into the assistant text and break Unshackled's session-title parser.
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

        Set-ClaudeLocalEnv -BaseUrl $effectiveBaseUrl -Model $def.Root -KeepThinking:($thinkingPolicy -eq 'keep') -ContextTokens $contextTokens
        Set-LocalBackendTelemetryEnv -ProcessId $proc.Id -Port $port -OutLogPath $logPaths.Out -ErrLogPath $logPaths.Err -GgufPath $ggufPath -Model $def.Root -ContextKey $ContextKey -ContextTokens $contextTokens

        $backendLabel = if ($Unshackled) { "unshackled" } else { "claude" }
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

        $launchArgs = if ($LimitTools) {
            @(
                '--dangerously-skip-permissions',
                '--tools',
                $Tools,
                '--append-system-prompt',
                $systemPrompt
            )
        }
        else {
            @(
                '--dangerously-skip-permissions',
                '--append-system-prompt',
                $systemPrompt
            )
        }

        Write-LaunchLog "Launching ${backendLabel}: model=$($def.Root) base=$effectiveBaseUrl unshackled=$Unshackled" 'LAUNCH'

        if ($Unshackled) {
            $extras = Get-UnshackledExtraArgs -Param $ExtraUnshackledArgs
            Invoke-UnshackledCli @launchArgs @extras
        }
        else {
            & claude --model $def.Root @launchArgs
        }
    }
    finally {
        Restore-ClaudeEnvBackup

        if ($useNoThinkProxy) {
            Stop-NoThinkProxy
        }

        Stop-LlamaServer
    }
}

