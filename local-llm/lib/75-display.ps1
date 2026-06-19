# Status / dashboard / per-model detail. Prefers PwshSpectreConsole when
# installed; falls back to plain Write-Host. The fallback path stays usable on
# fresh machines without any module installs.

$script:LocalLLMSpectreState = $null  # $true / $false / $null (unprobed)

function Format-LocalLLMArgvLine {
    param([Parameter(Mandatory = $true)][AllowEmptyCollection()][string[]]$Argv)

    # Quote tokens that contain whitespace or quote characters so the preview
    # round-trips back to a usable command line if a user copy-pastes it.
    $quoted = foreach ($a in $Argv) {
        if ([string]::IsNullOrEmpty($a)) {
            '""'
        }
        elseif ($a -match '[\s"]') {
            '"' + ($a -replace '"', '\"') + '"'
        }
        else {
            $a
        }
    }

    return ($quoted -join ' ')
}

function Show-LocalLLMLaunchPlan {
    # Preview-mode renderer used by every -DryRun path. Callers build a plan
    # hashtable describing the resolved launch (argv, env, VRAM estimate, etc.)
    # and pass it here in place of actually spawning anything.
    [CmdletBinding()]
    param([Parameter(Mandatory = $true)][hashtable]$Plan)

    $title = if ($Plan.Contains('Title') -and -not [string]::IsNullOrWhiteSpace([string]$Plan.Title)) {
        [string]$Plan.Title
    } else {
        'launch preview'
    }

    Write-Host ""
    Write-Host "=== DryRun: $title (nothing spawned) ===" -ForegroundColor Cyan

    $rows = [System.Collections.Generic.List[object]]::new()
    $add = {
        param($label, $value)
        if ($null -eq $value) { return }
        $text = [string]$value
        if ([string]::IsNullOrWhiteSpace($text)) { return }
        $rows.Add(@{ Label = $label; Value = $text }) | Out-Null
    }

    & $add 'Backend'  $Plan.Backend
    & $add 'Mode'     $Plan.Mode
    & $add 'Key'      $Plan.Key
    & $add 'Model'    $Plan.Model
    if ($Plan.Contains('ContextKey')) {
        $ctxLabel = [string]$Plan.ContextKey
        if ([string]::IsNullOrWhiteSpace($ctxLabel)) { $ctxLabel = 'default' }
        if ($Plan.Contains('ContextTokens') -and [int]$Plan.ContextTokens -gt 0) {
            $ctxLabel = "$ctxLabel ($([int]$Plan.ContextTokens) tokens)"
        }
        & $add 'Context' $ctxLabel
    } elseif ($Plan.Contains('ContextTokens') -and [int]$Plan.ContextTokens -gt 0) {
        & $add 'Context' ("{0} tokens" -f [int]$Plan.ContextTokens)
    }
    & $add 'Quant'    $Plan.Quant
    & $add 'Parser'   $Plan.Parser
    & $add 'GGUF'     $Plan.GgufPath
    & $add 'Server'   $Plan.ServerPath
    & $add 'Port'     $Plan.Port
    & $add 'BaseUrl'  $Plan.BaseUrl
    & $add 'Bypass'   $Plan.Bypass
    & $add 'Health'   $Plan.HealthCheck
    if ($Plan.Contains('HealthTimeoutSec') -and [int]$Plan.HealthTimeoutSec -gt 0) {
        & $add 'HealthTimeout' ("{0}s" -f [int]$Plan.HealthTimeoutSec)
    }
    & $add 'Tools'    $Plan.Tools
    & $add 'Thinking' $Plan.Thinking

    if ($Plan.Contains('VramAvailable') -or $Plan.Contains('VramNeeded')) {
        $parts = @()
        if ($Plan.Contains('VramNeeded') -and $null -ne $Plan.VramNeeded) {
            $parts += ("{0:N1} GB needed" -f [double]$Plan.VramNeeded)
        }
        if ($Plan.Contains('VramAvailable') -and $null -ne $Plan.VramAvailable) {
            $availLabel = ("{0} GB available" -f [int]$Plan.VramAvailable)
            if ($Plan.Contains('VramSource') -and -not [string]::IsNullOrWhiteSpace([string]$Plan.VramSource)) {
                $availLabel += " ($($Plan.VramSource))"
            }
            $parts += $availLabel
        }
        if ($Plan.Contains('FitClass') -and -not [string]::IsNullOrWhiteSpace([string]$Plan.FitClass)) {
            $parts += "fit=$($Plan.FitClass)"
        }
        if ($parts.Count -gt 0) {
            & $add 'VRAM' ($parts -join ' / ')
        }
    }

    foreach ($row in $rows) {
        Write-Host ("  {0,-13}: " -f $row.Label) -ForegroundColor DarkGray -NoNewline
        Write-Host $row.Value
    }

    if ($Plan.Contains('ServerArgs') -and $Plan.ServerArgs) {
        Write-Host ""
        Write-Host "  Server argv:" -ForegroundColor DarkGray
        $argv = @($Plan.ServerArgs)
        $serverPath = if ($Plan.Contains('ServerPath') -and -not [string]::IsNullOrWhiteSpace([string]$Plan.ServerPath)) {
            [string]$Plan.ServerPath
        } else {
            'llama-server'
        }
        $line = Format-LocalLLMArgvLine -Argv (@($serverPath) + $argv)
        Write-Host "    $line" -ForegroundColor Gray
    }

    if ($Plan.Contains('LaunchCmd') -and -not [string]::IsNullOrWhiteSpace([string]$Plan.LaunchCmd)) {
        Write-Host ""
        Write-Host "  Agent command:" -ForegroundColor DarkGray
        Write-Host "    $($Plan.LaunchCmd)" -ForegroundColor Gray
    } elseif ($Plan.Contains('LaunchArgs') -and $Plan.LaunchArgs) {
        Write-Host ""
        Write-Host "  Agent argv:" -ForegroundColor DarkGray
        $exe = if ($Plan.Contains('LaunchExe') -and -not [string]::IsNullOrWhiteSpace([string]$Plan.LaunchExe)) {
            [string]$Plan.LaunchExe
        } else {
            'claude'
        }
        $line = Format-LocalLLMArgvLine -Argv (@($exe) + @($Plan.LaunchArgs))
        Write-Host "    $line" -ForegroundColor Gray
    }

    if ($Plan.Contains('Env') -and $Plan.Env -and ([System.Collections.IDictionary]$Plan.Env).Count -gt 0) {
        Write-Host ""
        Write-Host "  Env (would set):" -ForegroundColor DarkGray
        $envMap = [System.Collections.IDictionary]$Plan.Env
        $names = @($envMap.Keys | Sort-Object)
        foreach ($name in $names) {
            $val = [string]$envMap[$name]
            Write-Host ("    {0,-38} = {1}" -f $name, $val) -ForegroundColor Gray
        }
    }

    if ($Plan.Contains('Notes') -and $Plan.Notes) {
        Write-Host ""
        foreach ($note in @($Plan.Notes)) {
            if (-not [string]::IsNullOrWhiteSpace([string]$note)) {
                Write-Host "  ! $note" -ForegroundColor Yellow
            }
        }
    }

    Write-Host ""
}

function Get-LocalLLMClaudeEnvSnapshot {
    # Capture the env vars Set-ClaudeLocalEnv would write, without actually
    # touching the process environment. Mirrors the logic in
    # Set-ClaudeLocalEnv in lib/65-claude-launch.ps1.
    param(
        [Parameter(Mandatory = $true)][string]$BaseUrl,
        [Parameter(Mandatory = $true)][string]$Model,
        [bool]$KeepThinking = $false,
        [string]$AuthToken = 'local',
        [int]$MaxImagesPerRequest = 0
    )

    $env = [ordered]@{
        ANTHROPIC_BASE_URL              = $BaseUrl
        ANTHROPIC_AUTH_TOKEN            = $AuthToken
        ANTHROPIC_API_KEY               = ''
        ANTHROPIC_MODEL                 = $Model
        ANTHROPIC_DEFAULT_OPUS_MODEL    = $Model
        ANTHROPIC_DEFAULT_SONNET_MODEL  = $Model
        ANTHROPIC_DEFAULT_HAIKU_MODEL   = $Model
    }

    if (-not $KeepThinking) {
        $env.CLAUDE_CODE_DISABLE_THINKING          = '1'
        $env.MAX_THINKING_TOKENS                   = '0'
        $env.CLAUDE_CODE_DISABLE_ADAPTIVE_THINKING = '1'
    }

    $maxOutputTokens = if ($script:Cfg.Contains('LocalModelMaxOutputTokens')) {
        try { [int]$script:Cfg.LocalModelMaxOutputTokens } catch { 4096 }
    } else {
        4096
    }
    if ($maxOutputTokens -gt 0) {
        $env.CLAUDE_CODE_MAX_OUTPUT_TOKENS = [string]$maxOutputTokens
    }

    $env.CLAUDE_CODE_ATTRIBUTION_HEADER = '0'
    $env.DISABLE_PROMPT_CACHING         = '1'
    $env.API_TIMEOUT_MS                 = '1800000'
    $env.CLAUDE_CODE_DISABLE_AUTO_MEMORY = '1'
    $env.CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS = '1'
    $env.ENABLE_TOOL_SEARCH = 'false'

    # Mirror Set-ClaudeLocalEnv: emit the image cap only when a model raises it
    # above LocalPilot's default of 1.
    if ($MaxImagesPerRequest -gt 0) {
        $env.CLAUDE_LOCAL_MAX_IMAGES = [string]$MaxImagesPerRequest
    }

    return $env
}

function Show-ClaudeTarget {
    Write-Section "Claude"
    Write-Host "Target : $(Get-ClaudeTargetSummary)" -ForegroundColor Yellow
}

function Show-LocalBackendStatus {
    Write-Section "llama-server"

    if (Get-Command Get-LlamaServerStatus -ErrorAction SilentlyContinue) {
        Get-LlamaServerStatus
    } else {
        Write-Host "(status helper unavailable — load lib/34-llamacpp-status.ps1)" -ForegroundColor DarkGray
    }
}

function Show-ConfiguredGgufQuants {
    param([switch]$All)

    Write-Host ""
    Write-Host "Configured GGUF quants/files:" -ForegroundColor Yellow

    foreach ($key in (Get-FilteredModelKeys -IncludeAll:$All)) {
        $def = Get-ModelDef -Key $key

        if ($def.ContainsKey("Quants")) {
            Write-Host "  $key -> $($def.Quant) ($($def.Quants[$def.Quant]))"
        } else {
            Write-Host "  $key -> $(Get-ModelFileName -Def $def)"
        }
    }
}

# Spectre.Console renderer (soft dependency)
# Tries to import PwshSpectreConsole on first use. If absent, the dashboard
# falls back to the legacy Write-Host renderer and we surface a one-line install
# hint. Set $env:LOCAL_LLM_NO_SPECTRE=1 to disable Spectre even when installed.

function Test-LocalLLMSpectreAvailable {
    if ($env:LOCAL_LLM_NO_SPECTRE -eq '1') { return $false }
    if ($null -ne $script:LocalLLMSpectreState) { return $script:LocalLLMSpectreState }

    if (Get-Module -Name PwshSpectreConsole) {
        $script:LocalLLMSpectreState = $true
        return $true
    }

    $available = Get-Module -ListAvailable -Name PwshSpectreConsole -ErrorAction SilentlyContinue
    if (-not $available) {
        $script:LocalLLMSpectreState = $false
        return $false
    }

    try {
        Import-Module PwshSpectreConsole -ErrorAction Stop -DisableNameChecking | Out-Null
        $script:LocalLLMSpectreState = $true
        return $true
    } catch {
        Write-LaunchLog "PwshSpectreConsole import failed: $($_.Exception.Message)" 'WARN'
        $script:LocalLLMSpectreState = $false
        return $false
    }
}

function Show-LocalLLMSpectreInstallHint {
    Write-Host ""
    Write-Host "Tip: install PwshSpectreConsole for a nicer dashboard:" -ForegroundColor DarkGray
    Write-Host "       Install-Module PwshSpectreConsole -Scope CurrentUser" -ForegroundColor DarkGray
    Write-Host "     Reload your profile, or run 'reloadllm', and 'info' will switch to the rich UI." -ForegroundColor DarkGray
}

function ConvertTo-LocalLLMSpectreSafe {
    # Spectre markup is `[color]text[/]`. Square brackets in arbitrary text
    # (e.g. tier badges "[recommended]") collide. Escape with `[[` / `]]`.
    param([AllowNull()][AllowEmptyString()][string]$Text)

    if ($null -eq $Text) { return "" }
    return ($Text -replace '\[', '[[') -replace '\]', ']]'
}

function Format-LocalLLMSpectreFitCell {
    # Single-quant fit cell for the summary table: short label + colored marker.
    # marker uses Spectre markup; the bracket/square-bracket text is plain.
    param(
        [Parameter(Mandatory = $true)][string]$QuantKey,
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$FitClass,
        [switch]$IsDefault
    )

    $star = if ($IsDefault) { '*' } else { ' ' }

    $marker, $color = switch ($FitClass) {
        'fits'  { '+',  'green'  }
        'tight' { '~',  'yellow' }
        'over'  { '!',  'red'    }
        default { '?',  'grey50' }
    }

    return "[$color]$marker[/]$star$QuantKey"
}

function Format-LocalLLMSpectreCatalogQuants {
    param(
        [Parameter(Mandatory = $true)]$Def,
        [int]$Limit = 10
    )

    if (-not $Def.ContainsKey("Quants")) {
        return "[grey50](single file)[/]"
    }

    $quantCells = @(
        foreach ($qk in $Def.Quants.Keys) {
            $fit = Get-QuantFitClass -Def $Def -QuantKey $qk
            Format-LocalLLMSpectreFitCell -QuantKey $qk -FitClass $fit -IsDefault:($qk -eq $def.Quant)
        }
    )

    if ($quantCells.Count -le $Limit) {
        return ($quantCells -join '  ')
    }

    $hidden = $quantCells.Count - $Limit
    return (@($quantCells | Select-Object -First $Limit) -join '  ') + "  [grey50]+$hidden[/]"
}

function Format-LocalLLMSpectreCatalogContexts {
    param([Parameter(Mandatory = $true)]$Def)

    $contextLabels = @(
        $Def.Contexts.Keys | ForEach-Object {
            if ([string]::IsNullOrWhiteSpace($_)) { "def" } else { $_ }
        }
    )

    return ConvertTo-LocalLLMSpectreSafe ($contextLabels -join ' ')
}

function Show-ModelCatalogSpectre {
    param([switch]$All)

    $vramInfo = Get-LocalLLMVRAMInfo
    $sourceLabel = switch ($vramInfo.Source) {
        "configured" { "set in settings.json" }
        "auto"       { "nvidia-smi auto-detect" }
        "fallback"   { "fallback — nvidia-smi unavailable" }
        default      { $vramInfo.Source }
    }

    Write-Host ""
    Format-SpectrePanel -Header "Models" -Color Blue -Data ("VRAM: [yellow]{0} GB[/] ({1})" -f $vramInfo.GB, (ConvertTo-LocalLLMSpectreSafe $sourceLabel)) | Out-Host

    $visibleKeys = @(Get-FilteredModelKeys -IncludeAll:$All)

    $rows = New-Object System.Collections.Generic.List[object]

    foreach ($key in $visibleKeys) {
        $def = Get-ModelDef -Key $key
        $quants = Format-LocalLLMSpectreCatalogQuants -Def $def

        $rows.Add([pscustomobject]@{
            Key    = "[white]$key[/]"
            Name   = ConvertTo-LocalLLMSpectreSafe $def.DisplayName
            Quants = $quants
            Ctx    = Format-LocalLLMSpectreCatalogContexts -Def $def
        }) | Out-Null
    }

    $keyWidth = 14
    $nameWidth = 36
    $ctxWidth = 30
    $quantWidth = 44

    $properties = @(
        @{ Name = 'Key'; Expression = { $_.Key }; Width = $keyWidth }
        @{ Name = 'Name'; Expression = { $_.Name }; Width = $nameWidth }
        @{ Name = 'Quants'; Expression = { $_.Quants }; Width = $quantWidth }
        @{ Name = 'Ctx'; Expression = { $_.Ctx }; Width = $ctxWidth }
    )

    $rows | Format-SpectreTable -Property $properties -Border Rounded -Color Blue -AllowMarkup -Wrap | Out-Host

    Write-Host ""
    Write-Host "  Quant cells: " -ForegroundColor DarkGray -NoNewline
    Write-Host "+" -ForegroundColor Green -NoNewline; Write-Host " fits  " -ForegroundColor DarkGray -NoNewline
    Write-Host "~" -ForegroundColor Yellow -NoNewline; Write-Host " tight  " -ForegroundColor DarkGray -NoNewline
    Write-Host "!" -ForegroundColor Red -NoNewline; Write-Host " over  " -ForegroundColor DarkGray -NoNewline
    Write-Host "?" -ForegroundColor DarkGray -NoNewline; Write-Host " size unknown   " -ForegroundColor DarkGray -NoNewline
    Write-Host "*name = current default quant" -ForegroundColor DarkGray

    if (-not $All) {
        $hiddenCount = (@(Get-ModelKeys)).Count - $visibleKeys.Count
        if ($hiddenCount -gt 0) {
            Write-Host ""
            Write-Host "$hiddenCount more model(s) hidden. Run 'info -All' to show experimental + legacy, or 'info <key>' for one." -ForegroundColor DarkGray
        }
    }

    Write-Host ""
    Write-Host "Drill in:" -ForegroundColor White
    Write-Host "  info <key>                     Per-model detail (description, quants, contexts)" -ForegroundColor DarkGray
    Write-Host "Manage:" -ForegroundColor White
    Write-Host "  addllm <hf-url> -Key <key>     Add a model from HuggingFace (auto-fills size + description)" -ForegroundColor DarkGray
    Write-Host "  removellm <key>                Remove a model + its files" -ForegroundColor DarkGray
    Write-Host "  reloadllm, purge, lps, lstop, llm, llmdocs" -ForegroundColor DarkGray
    Write-Host "  Config: $script:LocalLLMConfigPath" -ForegroundColor DarkGray
}

function Show-ModelDetailSpectre {
    param([Parameter(Mandatory = $true)][string]$Key)

    $def = Get-ModelDef -Key $Key
    $tier = Get-ModelTier -Def $def
    $tierColor = switch ($tier) {
        'recommended'  { 'green' }
        'experimental' { 'yellow' }
        'legacy'       { 'grey50' }
        default        { 'grey70' }
    }

    $description = Get-ModelDescription -Def $def
    $source = "GGUF · $($def.Repo)"
    $parser = if ($def.Parser) { $def.Parser } else { 'none' }
    $limitTools = if ($def.ContainsKey('LimitTools')) { [bool]$def.LimitTools } else { $true }

    $headerLines = New-Object System.Collections.Generic.List[string]
    if ($description) {
        $headerLines.Add((ConvertTo-LocalLLMSpectreSafe $description)) | Out-Null
        $headerLines.Add('') | Out-Null
    }
    $headerLines.Add(("[grey70]Source[/]    : {0}" -f (ConvertTo-LocalLLMSpectreSafe $source))) | Out-Null
    $headerLines.Add(("[grey70]Parser[/]    : {0}    [grey70]LimitTools[/]: {1}" -f (ConvertTo-LocalLLMSpectreSafe $parser), $limitTools)) | Out-Null

    if ($def.ContainsKey('VisionModule') -and -not [string]::IsNullOrWhiteSpace($def.VisionModule)) {
        $headerLines.Add(("[grey70]Vision[/]   : {0}" -f (ConvertTo-LocalLLMSpectreSafe $def.VisionModule))) | Out-Null
    }

    if ($def.ContainsKey('ParserNote') -and $def.ParserNote) {
        $headerLines.Add(("[grey50]Note[/]      : {0}" -f (ConvertTo-LocalLLMSpectreSafe $def.ParserNote))) | Out-Null
    }

    $panelHeader = ("[white]{0}[/] · [{1}]{2}[/]" -f (ConvertTo-LocalLLMSpectreSafe $def.DisplayName), $tierColor, $tier)
    Write-Host ""
    Format-SpectrePanel -Header $panelHeader -Color $tierColor -Data ($headerLines -join "`n") | Out-Host

    if ($def.ContainsKey('Quants')) {
        $quantRows = foreach ($qk in $def.Quants.Keys) {
            $isDefault = ($qk -eq $def.Quant)
            $fit = Get-QuantFitClass -Def $def -QuantKey $qk
            $fitMark, $fitColor = switch ($fit) {
                'fits'  { 'fits',  'green' }
                'tight' { 'tight', 'yellow' }
                'over'  { 'over',  'red' }
                default { '?',     'grey50' }
            }
            $size = Get-QuantSizeGB -Def $def -QuantKey $qk
            $sizeText = if ($null -eq $size) { '—' } else { "{0:N1} GB" -f $size }
            $note = Get-ModelQuantNote -Def $def -QuantKey $qk
            if (-not $note) { $note = $def.Quants[$qk] }

            [pscustomobject]@{
                ' '    = if ($isDefault) { '[cyan]*[/]' } else { ' ' }
                Quant  = if ($isDefault) { "[cyan]$qk[/]" } else { $qk }
                Fit    = "[$fitColor]$fitMark[/]"
                Size   = $sizeText
                Note   = ConvertTo-LocalLLMSpectreSafe $note
            }
        }

        Write-Host ""
        Write-Host "Quants" -ForegroundColor White
        $quantRows | Format-SpectreTable -Border Rounded -Color $tierColor -AllowMarkup -Wrap | Out-Host
    }

    $ctxRows = foreach ($ck in $def.Contexts.Keys) {
        $label = if ([string]::IsNullOrWhiteSpace($ck)) { 'default' } else { $ck }
        $tokens = Get-ModelContextValue -Def $def -ContextKey $ck
        $note = Get-ModelContextNote -Def $def -ContextKey $ck

        [pscustomobject]@{
            Context = $label
            Tokens  = "{0:N0}" -f [int]$tokens
            Note    = ConvertTo-LocalLLMSpectreSafe $note
        }
    }

    Write-Host ""
    Write-Host "Contexts" -ForegroundColor White
    $ctxRows | Format-SpectreTable -Border Rounded -Color $tierColor -AllowMarkup -Wrap | Out-Host

    if ($def.Contains('Tools') -and -not [string]::IsNullOrWhiteSpace($def.Tools)) {
        Write-Host ""
        Write-Host "Tools : $($def.Tools)" -ForegroundColor DarkGray
    }

    $cmdName = Get-ModelShortcutName -Def $def
    $contextLabels = @($def.Contexts.Keys | ForEach-Object {
        if ([string]::IsNullOrWhiteSpace($_)) { "default" } else { $_ }
    })
    $ctxFlag = if ($contextLabels.Count -gt 1) { "[-Ctx $($contextLabels -join '|')]" } else { '' }
    $usage = "$cmdName $ctxFlag [-LocalPilot] [-Codex] [-Strict] [-Mode native|turboquant|mtpturbo]".Trim()
    if ($def.ContainsKey('Quants')) {
        $usage += " [-Quant $((@($def.Quants.Keys)) -join '|')]"
    }
    Write-Host ""
    Write-Host "Usage : $usage" -ForegroundColor White
}

function Show-ModelCatalog {
    param([switch]$All)

    if (Test-LocalLLMSpectreAvailable) {
        Show-ModelCatalogSpectre -All:$All
        return
    }

    Write-Section "Commands"

    $vramInfo = Get-LocalLLMVRAMInfo
    $sourceLabel = switch ($vramInfo.Source) {
        "configured" { "set in settings.json" }
        "auto"       { "nvidia-smi auto-detect" }
        "fallback"   { "fallback — nvidia-smi unavailable" }
        default      { $vramInfo.Source }
    }
    Write-Host ("VRAM   : {0} GB ({1})" -f $vramInfo.GB, $sourceLabel) -ForegroundColor Yellow
    if ($vramInfo.Source -ne "configured") {
        Write-Host "         Override: Set-LocalLLMSetting VRAMGB <value>" -ForegroundColor DarkGray
    }
    Write-Host ""

    $visibleKeys = @(Get-FilteredModelKeys -IncludeAll:$All)

    foreach ($key in $visibleKeys) {
        $def = Get-ModelDef -Key $key
        $cmdName = Get-ModelShortcutName -Def $def

        $contextKeys = @($def.Contexts.Keys)
        $contextLabels = $contextKeys | ForEach-Object {
            if ([string]::IsNullOrWhiteSpace($_)) { "default" } else { $_ }
        }
        $ctxFlag = if ($contextKeys.Count -gt 1) {
            "[-Ctx $($contextLabels -join '|')]"
        } else {
            ""
        }

        $tierBadge = Format-ModelTierBadge -Def $def

        Write-Host "$($def.DisplayName) " -ForegroundColor White -NoNewline
        Write-Host $tierBadge -ForegroundColor DarkYellow

        $description = Get-ModelDescription -Def $def
        if (-not [string]::IsNullOrWhiteSpace($description)) {
            Write-Host "  $description" -ForegroundColor Gray
        }

        $usage = "$cmdName $ctxFlag [-LocalPilot] [-Codex] [-Strict] [-Mode native|turboquant|mtpturbo]".Trim()

        if ($def.ContainsKey("Quants")) {
            $quantNames = @($def.Quants.Keys) -join '|'
            $usage += " [-Quant $quantNames]"
        }

        Write-Host "  $usage" -ForegroundColor White

        if ($def.Contains("Tools") -and -not [string]::IsNullOrWhiteSpace($def.Tools)) {
            Write-Host "  Tools  : $($def.Tools)" -ForegroundColor DarkGray
        }

        if ($def.ContainsKey("Quants")) {
            $hasQuantNotes = $false
            foreach ($qk in $def.Quants.Keys) {
                if (-not [string]::IsNullOrWhiteSpace((Get-ModelQuantNote -Def $def -QuantKey $qk))) {
                    $hasQuantNotes = $true
                    break
                }
            }

            if ($hasQuantNotes) {
                Write-Host "  Quants :" -ForegroundColor DarkGray
                foreach ($qk in $def.Quants.Keys) {
                    $marker = if ($qk -eq $def.Quant) { "*" } else { " " }
                    $note = Get-ModelQuantNote -Def $def -QuantKey $qk
                    $fitClass = Get-QuantFitClass -Def $def -QuantKey $qk
                    $badge = Format-QuantFitBadge -FitClass $fitClass

                    $body = if ([string]::IsNullOrWhiteSpace($note)) { $def.Quants[$qk] } else { $note }
                    $prefix = "    {0} {1,-8} " -f $marker, $qk

                    if ([string]::IsNullOrWhiteSpace($badge)) {
                        Write-Host ("$prefix $body") -ForegroundColor DarkGray
                    } else {
                        Write-Host -NoNewline $prefix -ForegroundColor DarkGray
                        Write-Host -NoNewline (" {0,-7}" -f $badge) -ForegroundColor (Get-QuantFitBadgeColor -FitClass $fitClass)
                        Write-Host (" $body") -ForegroundColor DarkGray
                    }
                }
            }
        }

        $hasCtxNotes = $false
        foreach ($ck in $contextKeys) {
            if (-not [string]::IsNullOrWhiteSpace((Get-ModelContextNote -Def $def -ContextKey $ck))) {
                $hasCtxNotes = $true
                break
            }
        }

        if ($hasCtxNotes) {
            Write-Host "  Ctx    :" -ForegroundColor DarkGray
            foreach ($ck in $contextKeys) {
                $label = if ([string]::IsNullOrWhiteSpace($ck)) { "default" } else { $ck }
                $note = Get-ModelContextNote -Def $def -ContextKey $ck
                if ([string]::IsNullOrWhiteSpace($note)) {
                    $tokens = Get-ModelContextValue -Def $def -ContextKey $ck
                    Write-Host ("    {0,-8}  {1} tokens" -f $label, $tokens) -ForegroundColor DarkGray
                } else {
                    Write-Host ("    {0,-8}  {1}" -f $label, $note) -ForegroundColor DarkGray
                }
            }
        }

        Write-Host ""
    }

    if (-not $All) {
        $hiddenCount = (@(Get-ModelKeys)).Count - $visibleKeys.Count

        if ($hiddenCount -gt 0) {
            Write-Host "$hiddenCount more model(s) hidden. Run 'info -All' to see them, or set Tier in llm-models.json." -ForegroundColor DarkGray
            Write-Host ""
        }
    }

    Write-Host "Quant-fit legend: [fits] weights + ~7 GB headroom for KV  [tight] weights only  [over] partial offload" -ForegroundColor DarkGray
    Write-Host ""
    Write-Host "Manage:" -ForegroundColor White
    Write-Host "  addllm <hf-url> -Key <key>     Add a model from HuggingFace" -ForegroundColor DarkGray
    Write-Host "  removellm <key>                Remove a model + its files" -ForegroundColor DarkGray
    Write-Host "  reloadllm, purge, lps, lstop, llm, llmdocs" -ForegroundColor DarkGray
    Write-Host "  Config: $script:LocalLLMConfigPath" -ForegroundColor DarkGray
}

function Show-ModelDetailFallback {
    # Per-model detail without Spectre. Mirrors Show-ModelDetailSpectre's fields.
    param([Parameter(Mandatory = $true)][string]$Key)

    $def = Get-ModelDef -Key $Key
    $tierBadge = Format-ModelTierBadge -Def $def

    Write-Host ""
    Write-Host "$($def.DisplayName) " -ForegroundColor White -NoNewline
    Write-Host $tierBadge -ForegroundColor DarkYellow

    $description = Get-ModelDescription -Def $def
    if ($description) {
        Write-Host "  $description" -ForegroundColor Gray
    }

    Write-Host "  Source : GGUF · $($def.Repo)" -ForegroundColor DarkGray
    Write-Host "  Parser : $($def.Parser)    LimitTools: $([bool]$def.LimitTools)" -ForegroundColor DarkGray

    if ($def.ContainsKey('VisionModule') -and -not [string]::IsNullOrWhiteSpace($def.VisionModule)) {
        Write-Host "  Vision : $($def.VisionModule)" -ForegroundColor DarkGray
    }

    if ($def.ContainsKey('ParserNote') -and $def.ParserNote) {
        Write-Host "  Note   : $($def.ParserNote)" -ForegroundColor DarkGray
    }

    if ($def.ContainsKey("Quants")) {
        Write-Host "  Quants :" -ForegroundColor White
        foreach ($qk in $def.Quants.Keys) {
            $marker = if ($qk -eq $def.Quant) { "*" } else { " " }
            $fit = Get-QuantFitClass -Def $def -QuantKey $qk
            $badge = Format-QuantFitBadge -FitClass $fit
            $size = Get-QuantSizeGB -Def $def -QuantKey $qk
            $sizeText = if ($null -eq $size) { '' } else { "{0,5:N1} GB" -f $size }
            $note = Get-ModelQuantNote -Def $def -QuantKey $qk
            if (-not $note) { $note = $def.Quants[$qk] }

            Write-Host -NoNewline ("    {0} {1,-8} " -f $marker, $qk)
            if ($badge) {
                Write-Host -NoNewline (" {0,-7}" -f $badge) -ForegroundColor (Get-QuantFitBadgeColor -FitClass $fit)
            }
            Write-Host -NoNewline (" {0,9} " -f $sizeText) -ForegroundColor DarkGray
            Write-Host $note -ForegroundColor DarkGray
        }
    }

    Write-Host "  Ctx    :" -ForegroundColor White
    foreach ($ck in $def.Contexts.Keys) {
        $label = if ([string]::IsNullOrWhiteSpace($ck)) { "default" } else { $ck }
        $tokens = Get-ModelContextValue -Def $def -ContextKey $ck
        $note = Get-ModelContextNote -Def $def -ContextKey $ck
        if ($note) {
            Write-Host ("    {0,-8}  {1}" -f $label, $note) -ForegroundColor DarkGray
        } else {
            Write-Host ("    {0,-8}  {1,7} tokens" -f $label, $tokens) -ForegroundColor DarkGray
        }
    }

    if ($def.Contains('Tools') -and -not [string]::IsNullOrWhiteSpace($def.Tools)) {
        Write-Host "  Tools  : $($def.Tools)" -ForegroundColor DarkGray
    }
    Write-Host ""
}

function Show-LLMProfileInfo {
    param([switch]$All)

    Clear-Host
    Write-Host "LocalBox dashboard" -ForegroundColor Green

    Show-ClaudeTarget
    Show-LocalBackendStatus
    Show-ConfiguredGgufQuants -All:$All
    Show-LocalBenchLauncherStatus -Quiet | Out-Null
    Show-ModelCatalog -All:$All

    if (-not (Test-LocalLLMSpectreAvailable)) {
        Show-LocalLLMSpectreInstallHint
    }

    Write-Host ""
}

function Show-LocalBoxCommandReference {
    Write-Section "Commands"

    function Write-CommandRow {
        param(
            [Parameter(Mandatory = $true)][string]$Command,
            [Parameter(Mandatory = $true)][string]$Description
        )

        Write-Host ("  {0,-34} {1}" -f $Command, $Description) -ForegroundColor Gray
    }

    Write-Host "LocalBox model commands" -ForegroundColor Green
    Write-Host "  One function is generated for each configured model. Use -Ctx, -Codex, -Strict, -LocalPilot, -Mode, -KvK/-KvV, -AutoBest, and -Quant where supported." -ForegroundColor DarkGray
    foreach ($key in (@(Get-ModelKeys) | Sort-Object)) {
        $def = Get-ModelDef -Key $key
        $name = Get-ModelShortcutName -Def $def
        $contexts = @($def.Contexts.Keys | Sort-Object | ForEach-Object {
                if ([string]::IsNullOrWhiteSpace($_)) { "default" } else { $_ }
            }) -join ", "
        if ([string]::IsNullOrWhiteSpace($contexts)) { $contexts = "default" }

        $extra = if ($def.ContainsKey("Quants")) { "; supports -Quant <name>" } else { "" }
        Write-CommandRow -Command $name -Description ("{0}; contexts: {1}{2}" -f $def.DisplayName, $contexts, $extra)
    }

    Write-Host ""
    Write-Host "LocalBox launcher and dashboard" -ForegroundColor Green
    Write-CommandRow -Command "llm, llmmenu" -Description "Open the launcher wizard (Spectre when available)."
    Write-CommandRow -Command "llmtui" -Description "Open the Terminal.Gui LocalBox TUI preview."
    Write-CommandRow -Command "llmc" -Description "Open the native selectable launcher wizard."
    Write-CommandRow -Command "llms" -Description "Open the Spectre launcher wizard explicitly."
    Write-CommandRow -Command "llmserve -Key <key> [-Ctx <context>] [-NoMonitor]" -Description "Serve a local model to any agentic client."
    Write-CommandRow -Command "info [-All] [<model>]" -Description "Show the dashboard or model details."
    Write-CommandRow -Command "info -Commands" -Description "Show this LocalBox and LocalBench command list."
    Write-CommandRow -Command "llminfo" -Description "Alias for info."
    Write-CommandRow -Command "llmdocs, docs, llmhelp" -Description "Show the quick reference."
    Write-CommandRow -Command "reloadllm" -Description "Reload llm-models.json and regenerate model commands."
    Write-CommandRow -Command "llmdefault" -Description "Launch the configured default recipe, or the default model when no recipe is saved."
    Write-CommandRow -Command "llmdefaultlocalpilot" -Description "Launch the default model through LocalPilot."
    Write-CommandRow -Command "llmdefaultcodex" -Description "Launch the default model through Codex."
    Write-CommandRow -Command "llmlogerr, llmlogerrclear" -Description "Show or clear wizard error logs."
    Write-CommandRow -Command "llmlog" -Description "Show launch debug log (~/.local-llm/launch.log)."

    Write-Host ""
    Write-Host "LocalBox model setup and catalog" -ForegroundColor Green
    Write-CommandRow -Command "addllm <hf-url-or-repo> -Key <key>" -Description "Add a GGUF model to llm-models.json."
    Write-CommandRow -Command "updatellm <key>" -Description "Refresh quant metadata for a catalog model."
    Write-CommandRow -Command "removellm, rmllm" -Description "Remove a configured model."
    Write-CommandRow -Command "purge" -Description "Stop running llama-server and delete cached GGUF files."

    Write-Host ""
    Write-Host "LocalBox runtime operations" -ForegroundColor Green
    Write-CommandRow -Command "lps" -Description "Show llama-server status."
    Write-CommandRow -Command "lstop" -Description "Stop every llama-server.exe."
    Write-CommandRow -Command "unloadall, llmstop, llm-stop" -Description "Free local model VRAM by stopping every running llama-server."
    Write-CommandRow -Command "obench" -Description "Show legacy bench history."

    Write-Host ""
    Write-Host "LocalBox companion tools" -ForegroundColor Green
    Write-CommandRow -Command "findbest, tunellm" -Description "Run LocalBench-backed llama.cpp AutoBest tuning."
    Write-CommandRow -Command "lbtui" -Description "Open the Terminal.Gui LocalBench TUI when available."
    Write-CommandRow -Command "lb, lbstatus" -Description "Show LocalBench discovery and version status."
    Write-CommandRow -Command "Install-LocalBench" -Description "Clone/configure the managed LocalBench checkout."
    Write-CommandRow -Command "Update-LocalBench" -Description "Fast-forward the configured LocalBench checkout."
    Write-CommandRow -Command "Install-LocalPilot" -Description "Clone/configure the managed LocalPilot checkout."
    Write-CommandRow -Command "Update-LocalPilot [-RefreshInstalled]" -Description "Fast-forward LocalPilot and reinstall its CLI when needed."
    Write-CommandRow -Command "llm-update [-InstallTui] [-RefreshInstalled], llmupdate" -Description "Update LocalBox plus companions, then refresh installed artifacts."

    Write-Host ""
    Write-Host "LocalBench commands" -ForegroundColor Green
    Write-CommandRow -Command "localbench info commands" -Description "Show LocalBench's command reference."
    Write-CommandRow -Command "localbench detect" -Description "Detect hardware and save a hardware profile."
    Write-CommandRow -Command "localbench list-models" -Description "List discoverable local model candidates."
    Write-CommandRow -Command "localbench help" -Description "Show LocalBench help."
    Write-CommandRow -Command "Find-LocalBenchBestConfig" -Description "Module API used by LocalBox findbest."
    Write-CommandRow -Command "Get-LocalBenchBestConfig" -Description "Read the best exported launcher profile."
    Write-CommandRow -Command "Get-LocalBenchBestConfigCandidates" -Description "Read matching exported launcher profiles."
    Write-CommandRow -Command "Show-LocalBenchHistory" -Description "Show LocalBench/LocalBox tuner history for a model."
}

function info {
    [CmdletBinding()]
    param(
        [Parameter(Position = 0)][string]$Key,
        [switch]$All,
        [switch]$Commands
    )

    if ($Commands -or ($Key -in @('commands', 'command', 'cmds'))) {
        Show-LocalBoxCommandReference
        return
    }

    if ($Key) {
        $resolved = Resolve-ModelKeyByAnyName -Name $Key
        if (-not $resolved) {
            Write-Host "Unknown model: $Key" -ForegroundColor Red
            Write-Host "Known keys: $((@(Get-ModelKeys)) -join ', ')" -ForegroundColor DarkGray
            return
        }

        if (Test-LocalLLMSpectreAvailable) {
            Show-ModelDetailSpectre -Key $resolved
        } else {
            Show-ModelDetailFallback -Key $resolved
            Show-LocalLLMSpectreInstallHint
        }
        return
    }

    Show-LLMProfileInfo -All:$All
}

function llminfo {
    [CmdletBinding()]
    param(
        [Parameter(Position = 0)][string]$Key,
        [switch]$All,
        [switch]$Commands
    )

    if ($Commands -or $Key) {
        info -Key $Key -Commands:$Commands
        return
    }

    Show-LLMProfileInfo -All:$All
}

function llmdocs { Show-LLMQuickReference }
function docs { Show-LLMQuickReference }
function llmhelp { Show-LLMQuickReference }

function Show-LLMDynamicModelSummary {
    Write-Section "Configured models (by tier)"

    $tierOrder = @("recommended", "experimental", "legacy")
    $byTier = @{}

    foreach ($tier in $tierOrder) {
        $byTier[$tier] = New-Object System.Collections.Generic.List[string]
    }

    foreach ($key in (Get-ModelKeys)) {
        $def = Get-ModelDef -Key $key
        $tier = Get-ModelTier -Def $def

        if (-not $byTier.ContainsKey($tier)) {
            $byTier[$tier] = New-Object System.Collections.Generic.List[string]
            $tierOrder += $tier
        }

        $byTier[$tier].Add($key) | Out-Null
    }

    foreach ($tier in $tierOrder) {
        if ($byTier[$tier].Count -eq 0) { continue }

        Write-Host ""
        Write-Host ("[{0}]" -f $tier) -ForegroundColor DarkYellow

        foreach ($key in $byTier[$tier]) {
            $def = Get-ModelDef -Key $key
            $source = "GGUF: $($def.Repo)"
            $contexts = @($def.Contexts.Keys | ForEach-Object {
                    $label = if ([string]::IsNullOrWhiteSpace($_)) { "default" } else { [string]$_ }
                    $ctx = Get-ModelContextValue -Def $def -ContextKey $_
                    "$label=$ctx"
                }) -join ", "

            Write-Host "$key" -ForegroundColor White
            Write-Host "  Name     : $($def.DisplayName)"

            $description = Get-ModelDescription -Def $def
            if (-not [string]::IsNullOrWhiteSpace($description)) {
                Write-Host "  About    : $description" -ForegroundColor Gray
            }

            Write-Host "  Source   : $source"
            Write-Host "  Contexts : $contexts"

            if ($def.ContainsKey("Quants")) {
                $quantList = @($def.Quants.Keys | ForEach-Object {
                        if ($_ -eq $def.Quant) { "$_ [current]" } else { $_ }
                    }) -join ", "
                Write-Host "  Quants   : $quantList"
                $shortcutName = Get-ModelShortcutName -Def $def
                Write-Host "  Switch   : $shortcutName -Quant <quant>"
            }

            Write-Host ""
        }
    }
}

function Show-LLMQuickReference {
    Write-Section "Quick Reference"

    Write-Host @"
One function per model — flags select what to do.
  qcoder -Ctx 32k -LocalPilot    Code agent (Qwen3-Coder, 32k, LocalPilot)
  q36p -Ctx 32k -LocalPilot      General Qwen 3.6 agent (32k, LocalPilot)
  dev -Ctx 32k                   Smaller / faster (Devstral 24B, 32k)
  q36p -Ctx 128k -LocalPilot     Big context (Qwen 3.6 Plus, 128k)
  qcoder -Ctx 256 -Quant iq4xs   256k coder context (4090 ceiling)
  q36p -Quant q6kp               Switch the GGUF quant
  q36p -Mode turboquant -KvK turbo4 -KvV turbo4   turbo KV via fork binary
  q36p -AutoBest                 Replay the saved best tuner config
  llmdefault                     Launch the configured Default model
  llm                            Guided wizard (Spectre when available)
  llmtui                         Terminal.Gui LocalBox TUI preview
  llmc                           Native selectable wizard
  llms                           Spectre wizard (explicit)

Flags
  -Ctx <name>     One of the model's contexts (e.g. 32k, 64k, 128k, 256k). Omit for default.
  -LocalPilot     Use LocalPilot instead of Claude Code.
  -Codex          Use OpenAI Codex instead of Claude Code.
  -Strict         Apply the strict engineering overlay (tighter sampler + system prompt).
  -Mode <name>    Pick the llama.cpp binary flavor: native | turboquant | mtpturbo.
  -KvK / -KvV     Override the KV cache types passed to llama-server.
  -AutoBest       Replay the latest saved tuner profile for this (model, ctx, mode).
  -Quant <name>   Switch the model's selected GGUF quant.

Tradeoffs / sizes
  Per-quant and per-context tradeoffs (file size, KV pressure, when to pick what)
  are shown inline by 'info' and the 'llm' wizard. Set them in llm-models.json
  as Description, QuantNotes, and ContextNotes fields.

Manage
  info                  Dashboard, recommended models only (rich UI if PwshSpectreConsole is installed)
  info -All             Dashboard with experimental + legacy
  info <key>            Per-model detail: description, quants table (with fit + size), contexts table
  info -Commands        Full LocalBox and LocalBench command list
  reloadllm             Reload llm-models.json and regenerate commands
  llm-update [-InstallTui] [-RefreshInstalled]
                        Update LocalBox, LocalPilot, and LocalBench; refresh installed artifacts
  lbtui                 Open LocalBench.Tui when available
  lps, lstop            llama-server: status / stop
  purge                 Stop running llama-server and delete cached GGUF files
  lbstatus              Show LocalBench discovery/version status
  Install-LocalBench    Clone/configure the managed LocalBench checkout
  Update-LocalBench     Pull the configured LocalBench checkout
  Install-LocalPilot    Clone/configure the managed LocalPilot checkout
  Update-LocalPilot [-RefreshInstalled]
                        Pull the configured LocalPilot checkout and reinstall its CLI
  obench [-Model name]  Show legacy bench history (~/.local-llm/bench-history.jsonl)
  findbest <key> -ContextKey <ctx> [-Mode native|turboquant|mtpturbo] [-Quick|-Deep] [-Budget 100]
                        Auto-tune llama.cpp launch flags for this box via LocalBench.
                        Saved profiles are picked up by -AutoBest at launch time.
                        The wizard also exposes Find best settings and
                        Delete best settings for llama.cpp models.

Add or remove a model
  addllm <hf-url-or-repo> -Key <key>
  addllm <hf-url-or-repo> -Key <key> -Quants Q4_K_P,IQ4_XS -DefaultQuant Q4_K_P -Tier recommended
  addllm <hf-url-or-repo> -Key <key> -Description '...' -QuantNotes @{q4='~17 GB'} -ContextNotes @{'128'='131k'}
  removellm <key> [-KeepFiles] [-Force]

  Auto-fill on add: Description (from base_model README), QuantSizesGB (from HF blob sizes),
  and a baseline QuantNotes per quant. Override any field by passing -Description / -QuantNotes etc.

Tiers
  recommended    Daily drivers, known to work. Shown by default.
  experimental   Works but uncensored / abliterated / niche; hidden by default.
  legacy         Kept for comparison; hidden by default.

Notes
  Thinking: models with ThinkingPolicy=keep skip the no-think proxy and route
            directly at llama-server; the proxy still strips <think>...</think>
            blocks for everything else.
"@

    Show-LLMDynamicModelSummary
}
