# Generic filesystem / console / HuggingFace download primitives shared by
# everything else. No dependencies on the catalog or model defs.

function Get-LocalModelPermissionArgs {
    # Resolve whether agent launches pass '--dangerously-skip-permissions'.
    # Resolution order:
    #   1. LOCAL_LLM_SKIP_PERMISSIONS in the environment (0/false/no/off = keep
    #      the agent's per-action permission prompts; anything else = skip).
    #   2. LocalModelSkipPermissions from config (settings.json per machine).
    #   3. Neither set (first run): ask once and persist the answer, defaulting
    #      to keeping prompts on. Non-interactive sessions keep prompts on for
    #      the launch without persisting a choice.
    # Permission prompts are the human-in-the-loop that breaks prompt-injection
    # / runaway tool calls, which matter more with smaller, less-aligned local
    # models — so skipping them is a decision, never an inheritance.
    # Callers wrap the result with @(...), so a scalar (skip on) or empty (off)
    # both normalize cleanly without injecting a stray $null arg.
    if (-not [string]::IsNullOrEmpty($env:LOCAL_LLM_SKIP_PERMISSIONS)) {
        if ($env:LOCAL_LLM_SKIP_PERMISSIONS -notin @('0', 'false', 'no', 'off')) {
            return @('--dangerously-skip-permissions')
        }
        return @()
    }

    if ($script:Cfg -and $script:Cfg.Contains('LocalModelSkipPermissions')) {
        if ([bool]$script:Cfg.LocalModelSkipPermissions) {
            return @('--dangerously-skip-permissions')
        }
        return @()
    }

    if (Request-LocalModelPermissionDecision) {
        return @('--dangerously-skip-permissions')
    }
    return @()
}

function Show-LocalBoxSecuritySummary {
    # One screen turning the repo's invisible security postures into visible
    # ones: permission-skip status, proxy exposure, and supply-chain pin
    # status. Shown on the first agent launch (alongside the permission
    # decision) and callable directly any time.
    [CmdletBinding()]
    param()

    $cfg = $script:Cfg

    $permission = if ($cfg -and $cfg.Contains('LocalModelSkipPermissions')) {
        if ([bool]$cfg.LocalModelSkipPermissions) { 'SKIPPED (agents run without per-action prompts)' } else { 'prompts on (per-action approval)' }
    } else {
        'undecided - this launch will ask'
    }

    $localPilotBypass = Get-AgentBypassStatusText -SettingName 'LocalPilotBypass' -EnvVar 'LOCAL_LLM_LOCALPILOT_BYPASS'
    $codexBypass = Get-AgentBypassStatusText -SettingName 'CodexBypassApprovalsAndSandbox' -EnvVar 'LOCAL_LLM_CODEX_BYPASS'

    $proxyPort = if ($cfg -and $cfg.Contains('NoThinkProxyPort')) { $cfg.NoThinkProxyPort } else { 11435 }
    $serveAuth = if ([string]::IsNullOrWhiteSpace($env:LOCAL_LLM_SERVE_PASS)) { 'no token set (gateway would be open)' } else { 'token set' }

    $pins = if ($cfg -and $cfg.Contains('LlamaCppDownloadPins') -and $cfg.LlamaCppDownloadPins) { @($cfg.LlamaCppDownloadPins.Keys).Count } else { 0 }
    $requirePins = $cfg -and $cfg.Contains('LlamaCppRequireDownloadPins') -and [bool]$cfg.LlamaCppRequireDownloadPins
    $pinnedTag = if ($cfg -and $cfg.Contains('LlamaCppPinnedTag') -and $cfg.LlamaCppPinnedTag) { [string]$cfg.LlamaCppPinnedTag } else { 'none (latest, unpinned)' }
    $pinLine = if ($requirePins) {
        "$pins asset pin(s), unpinned downloads blocked, llama.cpp tag $pinnedTag"
    } else {
        "$pins asset pin(s), unpinned downloads ALLOWED (trust-on-first-use), llama.cpp tag $pinnedTag"
    }

    Write-Host ""
    Write-Host "=== LocalBox security posture ===" -ForegroundColor Cyan
    Write-Host ("  Agent permission prompts : {0}" -f $permission) -ForegroundColor Gray
    Write-Host ("  LocalPilot bypass        : {0}" -f $localPilotBypass) -ForegroundColor Gray
    Write-Host ("  Codex bypass             : {0}" -f $codexBypass) -ForegroundColor Gray
    Write-Host ("  Agent proxy              : 127.0.0.1:{0} (local only)" -f $proxyPort) -ForegroundColor Gray
    Write-Host ("  Serve gateway (if used)  : listens on 0.0.0.0; auth: {0}" -f $serveAuth) -ForegroundColor Gray
    Write-Host "                             LAN/VPN only; HTTPS in front for off-LAN; public no-auth HTTP is refused." -ForegroundColor DarkGray
    Write-Host ("  Binary download pins     : {0}" -f $pinLine) -ForegroundColor Gray
    Write-Host "  Change via Set-LocalLLMSetting; see README 'Verified binary downloads'." -ForegroundColor DarkGray
}

function Request-LocalModelPermissionDecision {
    # First agent launch on a machine with no persisted choice: make
    # permission skipping a conscious decision. Returns $true to skip prompts.
    # The answer persists to settings.json, so this asks exactly once.
    Show-LocalBoxSecuritySummary
    Write-Host ""
    Write-Host "Agent launches can skip the harness's per-action permission prompts" -ForegroundColor Yellow
    Write-Host "(--dangerously-skip-permissions). Keeping the prompts gives you a" -ForegroundColor DarkGray
    Write-Host "human-in-the-loop that catches runaway or injected tool calls from" -ForegroundColor DarkGray
    Write-Host "less-aligned local models. You can change this later with:" -ForegroundColor DarkGray
    Write-Host "  Set-LocalLLMSetting LocalModelSkipPermissions `$true|`$false" -ForegroundColor DarkGray

    try {
        $answer = (Read-Host "Skip permission prompts for agent launches? [y/N]").Trim().ToLowerInvariant()
    }
    catch {
        Write-Host "Non-interactive session — keeping permission prompts on for this launch." -ForegroundColor DarkGray
        return $false
    }

    $skip = $answer -in @('y', 'yes')
    Set-LocalLLMSetting LocalModelSkipPermissions $skip
    if ($script:Cfg) { $script:Cfg['LocalModelSkipPermissions'] = $skip }
    return $skip
}

function Resolve-AgentBypassDecision {
    # Whether an agent launch requests its bypass / all-permissions mode. Mirrors
    # Get-LocalModelPermissionArgs resolution: an explicit env override, then the
    # persisted per-machine config, then a one-time first-run prompt that defaults
    # OFF. Bypass hands the local model full tool/command authority with no
    # per-action gate, so — like skip-permissions — it is a conscious decision,
    # never an inheritance, and a non-interactive session never silently enables
    # it. With -NoPrompt (preview/dry-run), an undecided machine resolves to the
    # safe default (off) without prompting or persisting.
    param(
        [Parameter(Mandatory = $true)][string]$Label,
        [Parameter(Mandatory = $true)][string]$SettingName,
        [Parameter(Mandatory = $true)][string]$EnvVar,
        [Parameter(Mandatory = $true)][string]$FlagSummary,
        [switch]$NoPrompt
    )
    $envVal = [Environment]::GetEnvironmentVariable($EnvVar)
    if (-not [string]::IsNullOrEmpty($envVal)) {
        return ($envVal -notin @('0', 'false', 'no', 'off'))
    }
    if ($script:Cfg -and $script:Cfg.Contains($SettingName)) {
        return [bool]$script:Cfg[$SettingName]
    }
    if ($NoPrompt) { return $false }
    return (Request-AgentBypassDecision -Label $Label -SettingName $SettingName -FlagSummary $FlagSummary)
}

function Request-AgentBypassDecision {
    # First agent launch on a machine with no persisted bypass choice: make
    # enabling bypass a conscious, persisted decision. Returns $true to bypass.
    # Defaults OFF; a non-interactive session keeps bypass off for the launch
    # without persisting a choice.
    param(
        [Parameter(Mandatory = $true)][string]$Label,
        [Parameter(Mandatory = $true)][string]$SettingName,
        [Parameter(Mandatory = $true)][string]$FlagSummary
    )
    Show-LocalBoxSecuritySummary
    Write-Host ""
    Write-Host "$Label can launch in bypass mode ($FlagSummary), giving the local" -ForegroundColor Yellow
    Write-Host "model full tool/command authority with no per-action approval. Keeping" -ForegroundColor DarkGray
    Write-Host "bypass off preserves the agent's own permission gate against runaway or" -ForegroundColor DarkGray
    Write-Host "injected tool calls from less-aligned local models. Change later with:" -ForegroundColor DarkGray
    Write-Host "  Set-LocalLLMSetting $SettingName `$true|`$false" -ForegroundColor DarkGray

    try {
        $answer = (Read-Host "Enable bypass for $Label launches? [y/N]").Trim().ToLowerInvariant()
    }
    catch {
        Write-Host "Non-interactive session — keeping bypass off for this launch." -ForegroundColor DarkGray
        return $false
    }

    $bypass = $answer -in @('y', 'yes')
    Set-LocalLLMSetting $SettingName $bypass
    if ($script:Cfg) { $script:Cfg[$SettingName] = $bypass }
    return $bypass
}

function Get-LocalPilotBypassArgs {
    # The LocalPilot launch flags for the resolved bypass decision: '--bypass'
    # when on, nothing when off. Callers wrap with @(...), so an empty result
    # injects no stray arg. -NoPrompt resolves read-only for previews.
    param([switch]$NoPrompt)
    if (Resolve-AgentBypassDecision -Label 'LocalPilot' -SettingName 'LocalPilotBypass' `
            -EnvVar 'LOCAL_LLM_LOCALPILOT_BYPASS' -FlagSummary '--bypass' -NoPrompt:$NoPrompt) {
        return @('--bypass')
    }
    return @()
}

function Get-AgentBypassStatusText {
    # Human-readable bypass posture for the security summary / launch plan, read
    # WITHOUT prompting: 'on', 'off', or 'undecided' when neither env nor config
    # has set it (a real launch will ask). Env overrides config for this launch.
    param(
        [Parameter(Mandatory = $true)][string]$SettingName,
        [Parameter(Mandatory = $true)][string]$EnvVar
    )
    $envVal = [Environment]::GetEnvironmentVariable($EnvVar)
    if (-not [string]::IsNullOrEmpty($envVal)) {
        if ($envVal -notin @('0', 'false', 'no', 'off')) { return "ON via $EnvVar (bypass — no per-action approval)" }
        return "off via $EnvVar (agent permission gate on)"
    }
    if ($script:Cfg -and $script:Cfg.Contains($SettingName)) {
        if ([bool]$script:Cfg[$SettingName]) { return 'ON (bypass — no per-action approval)' }
        return 'off (agent permission gate on)'
    }
    return 'undecided - this launch will ask (defaults off)'
}

function Ensure-Directory {
    param([Parameter(Mandatory = $true)][string]$Path)

    if ([string]::IsNullOrWhiteSpace($Path)) {
        return
    }

    if (-not (Test-Path $Path)) {
        New-Item -ItemType Directory -Force -Path $Path | Out-Null
    }
}

function Convert-ToPosixPath {
    param([Parameter(Mandatory = $true)][string]$Path)

    return ($Path -replace '\\', '/')
}

function Write-Section {
    param([Parameter(Mandatory = $true)][string]$Title)

    Write-Host ""
    Write-Host "=== $Title ===" -ForegroundColor Cyan
}

function Pause-Menu {
    Read-Host "Press Enter to continue" | Out-Null
}

function Resolve-HuggingFaceLocalPath {
    param(
        [Parameter(Mandatory = $true)][string]$DestinationFolder,
        [Parameter(Mandatory = $true)][string]$FileName
    )

    $normalizedFileName = ($FileName -replace '\\', '/')
    $localRelativePath = ($normalizedFileName -replace '/', [System.IO.Path]::DirectorySeparatorChar)
    return Join-Path $DestinationFolder $localRelativePath
}

function Convert-HuggingFaceFileNameToUrlPath {
    param([Parameter(Mandatory = $true)][string]$FileName)

    $normalizedFileName = ($FileName -replace '\\', '/')

    return (($normalizedFileName -split '/') | ForEach-Object {
            [System.Uri]::EscapeDataString($_)
        }) -join '/'
}

function Download-HuggingFaceFile {
    param(
        [Parameter(Mandatory = $true)][string]$Repo,
        [Parameter(Mandatory = $true)][string]$FileName,
        [Parameter(Mandatory = $true)][string]$DestinationFolder
    )

    Ensure-Directory $DestinationFolder

    $normalizedFileName = ($FileName -replace '\\', '/')
    $destinationFile = Resolve-HuggingFaceLocalPath -DestinationFolder $DestinationFolder -FileName $normalizedFileName
    $destinationParent = Split-Path -Parent $destinationFile

    Ensure-Directory $destinationParent

    if (Test-Path $destinationFile) {
        Write-Host "Using existing file: $destinationFile" -ForegroundColor Green
        return $destinationFile
    }

    $downloaders = @()

    if (Get-Command uvx -ErrorAction SilentlyContinue) {
        $downloaders += "uvx-hf"
    }

    # hf/huggingface-cli are intentionally disabled because broken local Python
    # environments commonly fail on Windows. uvx or direct download is safer.
    $downloaders += "direct"

    foreach ($downloader in $downloaders) {
        Write-Host "Downloading $normalizedFileName using $downloader..." -ForegroundColor Cyan

        try {
            switch ($downloader) {
                "uvx-hf" {
                    $oldPythonUtf8 = $env:PYTHONUTF8
                    $oldPythonIoEncoding = $env:PYTHONIOENCODING
                    $oldHfSsl = $env:HF_HUB_DISABLE_SSL_VERIFICATION

                    try {
                        $env:PYTHONUTF8 = "1"
                        $env:PYTHONIOENCODING = "utf-8"
                        $env:HF_HUB_DISABLE_SSL_VERIFICATION = "1"

                        & uvx hf download $Repo $normalizedFileName --local-dir $DestinationFolder | Out-Host

                        if (Test-Path $destinationFile) {
                            Write-Host "Download completed: $destinationFile" -ForegroundColor Green
                            return $destinationFile
                        }

                        if ($LASTEXITCODE -ne 0) {
                            throw "uvx hf download failed with exit code $LASTEXITCODE"
                        }
                    }
                    finally {
                        if ($null -ne $oldPythonUtf8) {
                            $env:PYTHONUTF8 = $oldPythonUtf8
                        }
                        else {
                            Remove-Item Env:PYTHONUTF8 -ErrorAction SilentlyContinue
                        }

                        if ($null -ne $oldPythonIoEncoding) {
                            $env:PYTHONIOENCODING = $oldPythonIoEncoding
                        }
                        else {
                            Remove-Item Env:PYTHONIOENCODING -ErrorAction SilentlyContinue
                        }

                        if ($null -ne $oldHfSsl) {
                            $env:HF_HUB_DISABLE_SSL_VERIFICATION = $oldHfSsl
                        }
                        else {
                            Remove-Item Env:HF_HUB_DISABLE_SSL_VERIFICATION -ErrorAction SilentlyContinue
                        }
                    }
                }

                "direct" {
                    $urlFileName = Convert-HuggingFaceFileNameToUrlPath -FileName $normalizedFileName
                    $url = "https://huggingface.co/$Repo/resolve/main/$urlFileName"
                    $partialFile = "$destinationFile.partial"

                    $existingBytes = 0L

                    if (Test-Path $partialFile) {
                        $existingBytes = (Get-Item $partialFile).Length
                        Write-Host "Resuming from $([math]::Round($existingBytes / 1MB, 1)) MB at $partialFile" -ForegroundColor DarkCyan
                    }

                    Ensure-Directory $destinationParent

                    $oldProgress = $global:ProgressPreference
                    $global:ProgressPreference = 'SilentlyContinue'

                    try {
                        $request = [System.Net.HttpWebRequest]::Create($url)
                        $request.Method = "GET"
                        $request.AllowAutoRedirect = $true
                        $request.UserAgent = "LocalLLMProfile/1.0"
                        $request.ServerCertificateValidationCallback = { $true }

                        if ($existingBytes -gt 0) {
                            $request.AddRange($existingBytes)
                        }

                        $response = $null

                        try {
                            $response = $request.GetResponse()
                        }
                        catch [System.Net.WebException] {
                            # 416 Requested Range Not Satisfiable means the partial is already
                            # the full size; treat that as completion.
                            $errResponse = $_.Exception.Response

                            if ($errResponse -and [int]$errResponse.StatusCode -eq 416) {
                                Write-Host "Server reports already complete; finalizing." -ForegroundColor DarkCyan
                                Move-Item -Path $partialFile -Destination $destinationFile -Force
                                break
                            }

                            throw
                        }

                        try {
                            $appendMode = ($existingBytes -gt 0 -and [int]$response.StatusCode -eq 206)

                            if (-not $appendMode -and (Test-Path $partialFile)) {
                                Remove-Item $partialFile -Force -ErrorAction SilentlyContinue
                            }

                            $fileMode = if ($appendMode) { [System.IO.FileMode]::Append } else { [System.IO.FileMode]::Create }
                            $output = [System.IO.File]::Open($partialFile, $fileMode, [System.IO.FileAccess]::Write)

                            try {
                                $stream = $response.GetResponseStream()
                                $buffer = New-Object byte[] 1048576
                                $totalRead = $existingBytes
                                $expectedTotal = $existingBytes + [int64]$response.ContentLength
                                $lastReport = Get-Date

                                while ($true) {
                                    $read = $stream.Read($buffer, 0, $buffer.Length)
                                    if ($read -le 0) { break }
                                    $output.Write($buffer, 0, $read)
                                    $totalRead += $read

                                    if (((Get-Date) - $lastReport).TotalSeconds -ge 5) {
                                        $mb = [math]::Round($totalRead / 1MB, 1)
                                        $totalMb = if ($expectedTotal -gt 0) { [math]::Round($expectedTotal / 1MB, 1) } else { "?" }
                                        Write-Host "  ... $mb / $totalMb MB" -ForegroundColor DarkGray
                                        $lastReport = Get-Date
                                    }
                                }
                            }
                            finally {
                                $output.Close()
                            }
                        }
                        finally {
                            if ($response) { $response.Close() }
                        }
                    }
                    finally {
                        $global:ProgressPreference = $oldProgress
                    }

                    if (Test-Path $partialFile) {
                        Move-Item -Path $partialFile -Destination $destinationFile -Force
                    }
                }
            }

            if (Test-Path $destinationFile) {
                Write-Host "Download completed: $destinationFile" -ForegroundColor Green
                return $destinationFile
            }

            Write-Warning "$downloader completed but file was not found: $destinationFile"
        }
        catch {
            Write-Warning "$downloader failed: $($_.Exception.Message)"
            continue
        }
    }

    throw "All download methods failed for $Repo / $normalizedFileName"
}
