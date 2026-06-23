# Structured API for Terminal.Gui and other machine clients.
# These functions intentionally return objects only; do not write formatted
# console output here.

function ConvertTo-LocalBoxTuiContext {
    param(
        [Parameter(Mandatory = $true)][System.Collections.IDictionary]$Def,
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$ContextKey
    )

    $resolved = Resolve-ModelContextKey -Def $Def -ContextKey $ContextKey
    $label = if ([string]::IsNullOrWhiteSpace($resolved)) { 'default' } else { [string]$resolved }

    [pscustomobject]@{
        key = [string]$resolved
        label = $label
        tokens = [int](Get-ModelContextValue -Def $Def -ContextKey $resolved)
        note = Get-ModelContextNote -Def $Def -ContextKey $resolved
        isDefault = [string]::IsNullOrWhiteSpace($resolved)
    }
}

function ConvertTo-LocalBoxTuiQuant {
    param(
        [Parameter(Mandatory = $true)][System.Collections.IDictionary]$Def,
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$QuantKey
    )

    $file = ''
    if ($Def.ContainsKey('Quants') -and $Def.Quants.Contains($QuantKey)) {
        $file = [string]$Def.Quants[$QuantKey]
    }

    $size = Get-QuantSizeGB -Def $Def -QuantKey $QuantKey
    [pscustomobject]@{
        key = [string]$QuantKey
        file = $file
        sizeGB = $size
        fit = Get-QuantFitClass -Def $Def -QuantKey $QuantKey
        note = Get-ModelQuantNote -Def $Def -QuantKey $QuantKey
        isDefault = ($QuantKey -eq $Def.Quant)
    }
}

function Get-LocalBoxTuiModelSummary {
    param(
        [Parameter(Mandatory = $true)][string]$Key,
        [Parameter(Mandatory = $true)][System.Collections.IDictionary]$Def
    )

    $contexts = @()
    if ($Def.ContainsKey('Contexts')) {
        $contexts = @($Def.Contexts.Keys | ForEach-Object { ConvertTo-LocalBoxTuiContext -Def $Def -ContextKey ([string]$_) })
    }

    $quants = @()
    if ($Def.ContainsKey('Quants')) {
        $quants = @($Def.Quants.Keys | ForEach-Object { ConvertTo-LocalBoxTuiQuant -Def $Def -QuantKey ([string]$_) })
    }

    $sourceType = if ($Def.ContainsKey('SourceType') -and -not [string]::IsNullOrWhiteSpace([string]$Def.SourceType)) {
        [string]$Def.SourceType
    } elseif ($Def.ContainsKey('Repo')) {
        'gguf'
    } else {
        'ollama'
    }

    $backendModes = @('ollama')
    if ($sourceType -eq 'gguf' -or $Def.ContainsKey('Quants') -or $Def.ContainsKey('Repo')) {
        $backendModes = @('ollama', 'llamacpp:native', 'llamacpp:turboquant', 'llamacpp:mtpturbo')
    }

    [pscustomobject]@{
        key = $Key
        displayName = if ($Def.ContainsKey('DisplayName')) { [string]$Def.DisplayName } else { $Key }
        description = Get-ModelDescription -Def $Def
        tier = Get-ModelTier -Def $Def
        sourceType = $sourceType
        parser = if ($Def.ContainsKey('Parser')) { [string]$Def.Parser } else { '' }
        defaultQuant = if ($Def.ContainsKey('Quant')) { [string]$Def.Quant } else { '' }
        defaultContextKey = ''
        strict = Get-ModelStrictEnabled -Def $Def
        limitTools = ($Def.ContainsKey('LimitTools') -and [bool]$Def.LimitTools)
        hasVision = ($Def.ContainsKey('VisionModule') -and -not [string]::IsNullOrWhiteSpace([string]$Def.VisionModule))
        contexts = $contexts
        quants = $quants
        backendModes = $backendModes
    }
}

function Get-LocalBoxTuiStatus {
    [CmdletBinding()]
    param()

    $vram = Get-LocalLLMVRAMInfo
    $localBench = $null
    if (Get-Command Test-LocalBenchIntegrationAvailable -ErrorAction SilentlyContinue) {
        $localBench = Test-LocalBenchIntegrationAvailable -Quiet
    }

    [pscustomobject]@{
        name = 'LocalBox'
        profileRoot = $script:LLMProfileRoot
        configPath = $script:LocalLLMConfigPath
        modelCount = @($script:Cfg.Models.Keys).Count
        defaultModel = if ($script:Cfg.ContainsKey('Default')) { [string]$script:Cfg.Default } else { '' }
        vramGB = [int]$vram.GB
        vramSource = [string]$vram.Source
        localBench = $localBench
    }
}

function Get-LocalBoxTuiSettings {
    [CmdletBinding()]
    param()

    $settingsPath = Get-LocalLLMSettingsPath
    $settings = [ordered]@{}
    if (Test-Path -LiteralPath $settingsPath) {
        try {
            $loaded = Get-Content -Raw -LiteralPath $settingsPath | ConvertFrom-Json -AsHashtable
            foreach ($key in $loaded.Keys) {
                if ($key -notin @('Models', 'CommandAliases')) {
                    $settings[$key] = $loaded[$key]
                }
            }
        }
        catch {
            return [pscustomobject]@{
                path = $settingsPath
                readable = $false
                error = $_.Exception.Message
                values = [pscustomobject]@{}
            }
        }
    }

    [pscustomobject]@{
        path = $settingsPath
        readable = $true
        error = ''
        values = [pscustomobject]$settings
    }
}

function Get-LocalBoxTuiLocalBenchStatus {
    [CmdletBinding()]
    param()

    if (-not (Get-Command Test-LocalBenchIntegrationAvailable -ErrorAction SilentlyContinue)) {
        return [pscustomobject]@{
            available = $false
            reason = 'LocalBench bridge is not loaded.'
            version = ''
            root = ''
            modulePath = ''
            source = ''
        }
    }

    $status = Test-LocalBenchIntegrationAvailable -Quiet
    $resolved = $null
    try { $resolved = Resolve-LocalBenchRoot } catch { $resolved = $null }

    [pscustomobject]@{
        available = [bool]$status.Available
        reason = [string]$status.Reason
        version = [string]$status.Version
        apiVersion = $status.ApiVersion
        launcherExportVersion = $status.LauncherExportVersion
        root = if ($resolved) { [string]$resolved.Root } else { '' }
        modulePath = if ($resolved) { [string]$resolved.ModulePath } else { '' }
        source = [string]$status.Source
    }
}

function Get-LocalBoxTuiModels {
    [CmdletBinding()]
    param([switch]$All)

    $keys = @(Get-FilteredModelKeys -IncludeAll:$All | Sort-Object)
    @($keys | ForEach-Object {
        $key = [string]$_
        Get-LocalBoxTuiModelSummary -Key $key -Def (Get-ModelDef -Key $key)
    })
}

function Get-LocalBoxTuiModelDetail {
    [CmdletBinding()]
    param([Parameter(Mandatory = $true)][string]$Key)

    $def = Get-ModelDef -Key $Key
    $summary = Get-LocalBoxTuiModelSummary -Key $Key -Def $def
    $aliases = @()
    if (Get-Command Get-RegisteredShortcutNamesForModel -ErrorAction SilentlyContinue) {
        $aliases = @(Get-RegisteredShortcutNamesForModel -Def $def)
    }
    $fileName = try { Get-ModelFileName -Def $def } catch { '' }

    [pscustomobject]@{
        summary = $summary
        repo = if ($def.ContainsKey('Repo')) { [string]$def.Repo } else { '' }
        root = if ($def.ContainsKey('Root')) { [string]$def.Root } else { '' }
        file = $fileName
        aliases = $aliases
        raw = $def
    }
}

function Get-LocalBoxTuiLaunchOptions {
    [CmdletBinding()]
    param([Parameter(Mandatory = $true)][string]$Key)

    $def = Get-ModelDef -Key $Key
    $summary = Get-LocalBoxTuiModelSummary -Key $Key -Def $def

    [pscustomobject]@{
        key = $Key
        contexts = $summary.contexts
        quants = $summary.quants
        backendModes = $summary.backendModes
        actions = @(
            [pscustomobject]@{ key = 'claude'; label = 'Claude Code' }
            [pscustomobject]@{ key = 'codex'; label = 'Codex' }
            [pscustomobject]@{ key = 'localpilot'; label = 'LocalPilot' }
            [pscustomobject]@{ key = 'serve'; label = 'Serve' }
            [pscustomobject]@{ key = 'chat'; label = 'Ollama chat' }
            [pscustomobject]@{ key = 'setup'; label = 'Setup/download only' }
        )
    }
}

function Get-LocalBoxTuiAutoBestProfiles {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$Key,
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$ContextKey,
        [ValidateSet('native','turboquant','mtpturbo')][string]$Mode = 'native',
        [string]$Quant
    )

    $profiles = @()
if (Get-Command Get-LlamaCppBestConfigCandidates -ErrorAction SilentlyContinue) {
        foreach ($profileName in @('balanced', 'pure')) {
            $profiles += @(Get-LlamaCppBestConfigCandidates -Key $Key -ContextKey $ContextKey -Mode $Mode -Quant $Quant -Profile $profileName | ForEach-Object {
                $staleReasons = @(try { Test-LlamaCppBestConfigStale -Entry $_ -Mode $Mode } catch { @() })
                [pscustomobject]@{
                    profile = $profileName
                    score = $_.score
                    scoreUnit = $_.scoreUnit
                    quant = $_.quant
                    mode = $_.mode
                    contextKey = $_.contextKey
                    promptLength = if ($_.prompt_length) { $_.prompt_length } else { 'short' }
                    measuredAt = $_.measured_at
                    source = $_.source
                    reportPath = $_.report_path
                    launcherProfilePath = Get-LlamaCppTunerBestFile -Key $Key
                    staleReasons = $staleReasons
                    overrides = $_.overrides
                }
            })
        }
    }

    @($profiles)
}

function ConvertTo-LocalBoxTuiPowerShellLiteral {
    param([AllowEmptyString()][string]$Value)

    if ($null -eq $Value) { return "''" }
    return "'" + ([string]$Value -replace "'", "''") + "'"
}

function New-LocalBoxTuiSelectionCommand {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute('PSAvoidAssignmentToAutomaticVariable', 'Profile', Justification = 'shipped -Profile parameter name; renaming would break existing callers')]
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$Key,
        [AllowEmptyString()][string]$ContextKey = '',
        [ValidateSet('claude','codex','localpilot','serve','chat','setup','findbest','resetbest')][string]$Action = 'claude',
        [ValidateSet('native','turboquant','mtpturbo')][string]$Mode = 'native',
        [string]$Quant,
        [switch]$Strict,
        [switch]$UseVision,
        [switch]$UseAutoBest,
        [ValidateSet('auto','pure','balanced','short','long')][string]$AutoBestProfile = 'auto',
        [int]$Budget = 0,
        [int]$Runs = 0,
        [ValidateSet('gen','prompt','both','coding-agent')][string]$Optimize = 'coding-agent',
        [ValidateSet('pure','balanced','both')][string]$Profile = 'pure',
        [ValidateSet('','greedy','beam')][string]$SearchStrategy = '',
        [int]$BeamWidth = 0,
        [int[]]$NCpuMoeCandidates,
        [switch]$DryRun
    )

    if ($Action -eq 'findbest') {
        $parts = @(
            'Invoke-LocalBoxTuiFindBest',
            '-Key', (ConvertTo-LocalBoxTuiPowerShellLiteral $Key),
            '-ContextKey', (ConvertTo-LocalBoxTuiPowerShellLiteral $ContextKey),
            '-Mode', (ConvertTo-LocalBoxTuiPowerShellLiteral $Mode)
        )
        if (-not [string]::IsNullOrWhiteSpace($Quant)) {
            $parts += @('-Quant', (ConvertTo-LocalBoxTuiPowerShellLiteral $Quant))
        }
        if ($Budget -gt 0) { $parts += @('-Budget', [string]$Budget) }
        if ($Runs -gt 0) { $parts += @('-Runs', [string]$Runs) }
        if ($Optimize -ne 'coding-agent') { $parts += @('-Optimize', (ConvertTo-LocalBoxTuiPowerShellLiteral $Optimize)) }
        if ($Profile -ne 'pure') { $parts += @('-Profile', (ConvertTo-LocalBoxTuiPowerShellLiteral $Profile)) }
        if (-not [string]::IsNullOrWhiteSpace($SearchStrategy)) { $parts += @('-SearchStrategy', (ConvertTo-LocalBoxTuiPowerShellLiteral $SearchStrategy)) }
        if ($BeamWidth -gt 0) { $parts += @('-BeamWidth', [string]$BeamWidth) }
        if ($NCpuMoeCandidates -and $NCpuMoeCandidates.Count -gt 0) {
            $parts += @('-NCpuMoeCandidates', (($NCpuMoeCandidates | ForEach-Object { [string][int]$_ }) -join ','))
        }
        if ($DryRun) { $parts += '-DryRun' }
        return ($parts -join ' ')
    }

    if ($Action -eq 'resetbest') {
        $parts = @(
            'Invoke-LocalBoxTuiResetBest',
            '-Key', (ConvertTo-LocalBoxTuiPowerShellLiteral $Key),
            '-ContextKey', (ConvertTo-LocalBoxTuiPowerShellLiteral $ContextKey),
            '-Mode', (ConvertTo-LocalBoxTuiPowerShellLiteral $Mode)
        )
        if (-not [string]::IsNullOrWhiteSpace($Quant)) {
            $parts += @('-Quant', (ConvertTo-LocalBoxTuiPowerShellLiteral $Quant))
        }
        if ($DryRun) { $parts += '-DryRun' }
        return ($parts -join ' ')
    }

    $parts = @(
        'Invoke-LLMSelection',
        '-ModelKey', (ConvertTo-LocalBoxTuiPowerShellLiteral $Key),
        '-ContextKey', (ConvertTo-LocalBoxTuiPowerShellLiteral $ContextKey),
        '-Action', (ConvertTo-LocalBoxTuiPowerShellLiteral $Action),
        '-LlamaCppMode', (ConvertTo-LocalBoxTuiPowerShellLiteral $Mode),
        '-AutoBestProfile', (ConvertTo-LocalBoxTuiPowerShellLiteral $AutoBestProfile)
    )
    if (-not [string]::IsNullOrWhiteSpace($Quant)) {
        $parts += @('-Quant', (ConvertTo-LocalBoxTuiPowerShellLiteral $Quant))
    }
    if ($Strict) { $parts += '-Strict' }
    if ($UseVision) { $parts += '-UseVision' }
    if ($UseAutoBest) { $parts += '-UseAutoBest' }
    if ($DryRun) { $parts += '-DryRun' }

    return ($parts -join ' ')
}

function Invoke-LocalBoxTuiFindBest {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute('PSAvoidAssignmentToAutomaticVariable', 'Profile', Justification = 'shipped -Profile parameter name; renaming would break existing callers')]
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$Key,
        [AllowEmptyString()][string]$ContextKey = '',
        [ValidateSet('native','turboquant','mtpturbo')][string]$Mode = 'native',
        [string]$Quant,
        [int]$Budget = 0,
        [int]$Runs = 0,
        [ValidateSet('gen','prompt','both','coding-agent')][string]$Optimize = 'coding-agent',
        [ValidateSet('pure','balanced','both')][string]$Profile = 'pure',
        [ValidateSet('','greedy','beam')][string]$SearchStrategy = '',
        [int]$BeamWidth = 0,
        [int[]]$NCpuMoeCandidates,
        [switch]$DryRun
    )

    $def = Get-ModelDef -Key $Key
    $resolvedContext = Resolve-ModelContextKey -Def $def -ContextKey $ContextKey
    $quant = if (-not [string]::IsNullOrWhiteSpace($Quant)) {
        Resolve-ModelQuantKey -Def $def -Quant $Quant
    } elseif ($def.Contains('Quant')) {
        [string]$def.Quant
    } else {
        ''
    }
    $contextLabel = if ([string]::IsNullOrWhiteSpace($resolvedContext)) { 'default' } else { $resolvedContext }

    if ($DryRun) {
        $command = "findbest $(ConvertTo-LocalBoxTuiPowerShellLiteral $Key) -ContextKey $(ConvertTo-LocalBoxTuiPowerShellLiteral $resolvedContext) -Mode $(ConvertTo-LocalBoxTuiPowerShellLiteral $Mode)"
        if (-not [string]::IsNullOrWhiteSpace($quant)) {
            $command += " -Quant $(ConvertTo-LocalBoxTuiPowerShellLiteral $quant)"
        }
        if ($Budget -gt 0) { $command += " -Budget $Budget" }
        if ($Runs -gt 0) { $command += " -Runs $Runs" }
        if ($Optimize -ne 'coding-agent') { $command += " -Optimize $(ConvertTo-LocalBoxTuiPowerShellLiteral $Optimize)" }
        if ($Profile -ne 'pure') { $command += " -Profile $(ConvertTo-LocalBoxTuiPowerShellLiteral $Profile)" }
        if (-not [string]::IsNullOrWhiteSpace($SearchStrategy)) { $command += " -SearchStrategy $(ConvertTo-LocalBoxTuiPowerShellLiteral $SearchStrategy)" }
        if ($BeamWidth -gt 0) { $command += " -BeamWidth $BeamWidth" }
        if ($NCpuMoeCandidates -and $NCpuMoeCandidates.Count -gt 0) {
            $command += " -NCpuMoeCandidates $(($NCpuMoeCandidates | ForEach-Object { [string][int]$_ }) -join ',')"
        }
        [pscustomobject]@{
            action = 'findbest'
            key = $Key
            contextKey = $resolvedContext
            contextLabel = $contextLabel
            mode = $Mode
            quant = $quant
            budget = $Budget
            runs = $Runs
            optimize = $Optimize
            profile = $Profile
            searchStrategy = $SearchStrategy
            beamWidth = $BeamWidth
            nCpuMoeCandidates = @($NCpuMoeCandidates)
            command = $command
        }
        return
    }

    Write-Host "Running LocalBench AutoBest tuning..." -ForegroundColor Cyan
    Write-Host "  model   : $Key" -ForegroundColor DarkGray
    Write-Host "  quant   : $quant" -ForegroundColor DarkGray
    Write-Host "  context : $contextLabel" -ForegroundColor DarkGray
    Write-Host "  mode    : $Mode" -ForegroundColor DarkGray
    Write-Host ""

    $params = @{
        Key = $Key
        ContextKey = $resolvedContext
        Mode = $Mode
        Quant = $quant
        Optimize = 'coding-agent'
        Profile = 'pure'
    }
    if ($Budget -gt 0) { $params.Budget = $Budget }
    if ($Runs -gt 0) { $params.Runs = $Runs }
    if (-not [string]::IsNullOrWhiteSpace($Optimize)) { $params.Optimize = $Optimize }
    if (-not [string]::IsNullOrWhiteSpace($Profile)) { $params.Profile = $Profile }
    if (-not [string]::IsNullOrWhiteSpace($SearchStrategy)) { $params.SearchStrategy = $SearchStrategy }
    if ($BeamWidth -gt 0) { $params.BeamWidth = $BeamWidth }
    if ($NCpuMoeCandidates -and $NCpuMoeCandidates.Count -gt 0) { $params.NCpuMoeCandidates = [int[]]$NCpuMoeCandidates }

    $results = @(Find-BestLlamaCppConfig @params | Where-Object { $_ })
    if ($results.Count -eq 0) {
        Write-Warning "LocalBench did not return a saved tuning result."
        return
    }

    Write-Host ""
    Write-Host "AutoBest tuning complete." -ForegroundColor Green
    foreach ($item in $results) {
        $profileLabel = if ($item.Profile) { [string]$item.Profile } else { 'pure' }
        Write-Host ("  [{0}] score     : {1:N2} ({2})" -f $profileLabel, $item.Score, $item.ScoreUnit) -ForegroundColor Green
        if ($item.Overrides) {
            Write-Host ("  [{0}] overrides : {1}" -f $profileLabel, (Format-LlamaCppOverrides -Overrides $item.Overrides)) -ForegroundColor DarkGray
        }
        if ($item.report_path) {
            Write-Host ("  [{0}] report    : {1}" -f $profileLabel, $item.report_path) -ForegroundColor DarkGray
        }
    }
}

function Invoke-LocalBoxTuiResetBest {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$Key,
        [AllowEmptyString()][string]$ContextKey = '',
        [ValidateSet('native','turboquant','mtpturbo')][string]$Mode = 'native',
        [string]$Quant,
        [switch]$DryRun
    )

    $def = Get-ModelDef -Key $Key
    $resolvedContext = Resolve-ModelContextKey -Def $def -ContextKey $ContextKey
    $quant = if (-not [string]::IsNullOrWhiteSpace($Quant)) {
        Resolve-ModelQuantKey -Def $def -Quant $Quant
    } elseif ($def.Contains('Quant')) {
        [string]$def.Quant
    } else {
        ''
    }
    $contextLabel = if ([string]::IsNullOrWhiteSpace($resolvedContext)) { 'default' } else { $resolvedContext }

    if ($DryRun) {
        [pscustomobject]@{
            action = 'resetbest'
            key = $Key
            contextKey = $resolvedContext
            contextLabel = $contextLabel
            mode = $Mode
            quant = $quant
            command = "Remove-LlamaCppBestConfig -Key $(ConvertTo-LocalBoxTuiPowerShellLiteral $Key) -ContextKey $(ConvertTo-LocalBoxTuiPowerShellLiteral $resolvedContext) -Mode $(ConvertTo-LocalBoxTuiPowerShellLiteral $Mode) -Quant $(ConvertTo-LocalBoxTuiPowerShellLiteral $quant) -AllPromptLengths"
        }
        return
    }

    $result = Remove-LlamaCppBestConfig -Key $Key -ContextKey $resolvedContext -Mode $Mode -Quant $quant -AllPromptLengths
    if ($result.Removed -gt 0) {
        Write-Host "Deleted $($result.Removed) saved AutoBest setting(s)." -ForegroundColor Green
        if ($result.DeletedFile) {
            Write-Host "Removed $($result.Path)" -ForegroundColor DarkGray
        } else {
            Write-Host "$($result.Remaining) saved setting(s) remain in $($result.Path)" -ForegroundColor DarkGray
        }
        return
    }

    Write-Host "No matching saved AutoBest settings found for $Key / $contextLabel / $Mode / $quant." -ForegroundColor DarkGray
}

function New-LocalBoxTuiLaunchPlan {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute('PSAvoidAssignmentToAutomaticVariable', 'Profile', Justification = 'shipped -Profile parameter name; renaming would break existing callers')]
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$Key,
        [AllowEmptyString()][string]$ContextKey = '',
        [ValidateSet('claude','codex','localpilot','serve','chat','setup','findbest','resetbest')][string]$Action = 'claude',
        [ValidateSet('native','turboquant','mtpturbo')][string]$Mode = 'native',
        [string]$Quant,
        [switch]$Strict,
        [switch]$UseVision,
        [switch]$UseAutoBest,
        [ValidateSet('auto','pure','balanced','short','long')][string]$AutoBestProfile = 'auto',
        [int]$Budget = 0,
        [int]$Runs = 0,
        [ValidateSet('gen','prompt','both','coding-agent')][string]$Optimize = 'coding-agent',
        [ValidateSet('pure','balanced','both')][string]$Profile = 'pure',
        [ValidateSet('','greedy','beam')][string]$SearchStrategy = '',
        [int]$BeamWidth = 0,
        [int[]]$NCpuMoeCandidates
    )

    $def = Get-ModelDef -Key $Key
    $context = ConvertTo-LocalBoxTuiContext -Def $def -ContextKey $ContextKey
    $resolvedQuant = ''
    if (-not [string]::IsNullOrWhiteSpace($Quant)) {
        $resolvedQuant = Resolve-ModelQuantKey -Def $def -Quant $Quant
    } elseif ($def.ContainsKey('Quant')) {
        $resolvedQuant = [string]$def.Quant
    }
    $dryRunCommand = New-LocalBoxTuiSelectionCommand -Key $Key -ContextKey $context.key -Action $Action -Mode $Mode -Quant $resolvedQuant -Strict:$Strict -UseVision:$UseVision -UseAutoBest:$UseAutoBest -AutoBestProfile $AutoBestProfile -Budget $Budget -Runs $Runs -Optimize $Optimize -Profile $Profile -SearchStrategy $SearchStrategy -BeamWidth $BeamWidth -NCpuMoeCandidates $NCpuMoeCandidates -DryRun
    $launchCommand = New-LocalBoxTuiSelectionCommand -Key $Key -ContextKey $context.key -Action $Action -Mode $Mode -Quant $resolvedQuant -Strict:$Strict -UseVision:$UseVision -UseAutoBest:$UseAutoBest -AutoBestProfile $AutoBestProfile -Budget $Budget -Runs $Runs -Optimize $Optimize -Profile $Profile -SearchStrategy $SearchStrategy -BeamWidth $BeamWidth -NCpuMoeCandidates $NCpuMoeCandidates

    [pscustomobject]@{
        key = $Key
        model = if ($def.ContainsKey('DisplayName')) { [string]$def.DisplayName } else { $Key }
        action = $Action
        mode = $Mode
        contextKey = $context.key
        contextLabel = $context.label
        contextTokens = $context.tokens
        quant = $resolvedQuant
        strict = [bool]$Strict
        useVision = [bool]$UseVision
        useAutoBest = [bool]$UseAutoBest
        autoBestProfile = $AutoBestProfile
        tune = [pscustomobject]@{
            budget = $Budget
            runs = $Runs
            optimize = $Optimize
            profile = $Profile
            searchStrategy = $SearchStrategy
            beamWidth = $BeamWidth
            nCpuMoeCandidates = @($NCpuMoeCandidates)
        }
        dryRunCommand = $dryRunCommand
        launchCommand = $launchCommand
    }
}

function Invoke-LocalBoxTuiLaunchPreview {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute('PSAvoidAssignmentToAutomaticVariable', 'Profile', Justification = 'shipped -Profile parameter name; renaming would break existing callers')]
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$Key,
        [AllowEmptyString()][string]$ContextKey = '',
        [ValidateSet('claude','codex','localpilot','serve','chat','setup','findbest','resetbest')][string]$Action = 'claude',
        [ValidateSet('native','turboquant','mtpturbo')][string]$Mode = 'native',
        [string]$Quant,
        [switch]$Strict,
        [switch]$UseVision,
        [switch]$UseAutoBest,
        [ValidateSet('auto','pure','balanced','short','long')][string]$AutoBestProfile = 'auto',
        [int]$Budget = 0,
        [int]$Runs = 0,
        [ValidateSet('gen','prompt','both','coding-agent')][string]$Optimize = 'coding-agent',
        [ValidateSet('pure','balanced','both')][string]$Profile = 'pure',
        [ValidateSet('','greedy','beam')][string]$SearchStrategy = '',
        [int]$BeamWidth = 0,
        [int[]]$NCpuMoeCandidates
    )

    $cmd = New-LocalBoxTuiSelectionCommand -Key $Key -ContextKey $ContextKey -Action $Action -Mode $Mode -Quant $Quant -Strict:$Strict -UseVision:$UseVision -UseAutoBest:$UseAutoBest -AutoBestProfile $AutoBestProfile -Budget $Budget -Runs $Runs -Optimize $Optimize -Profile $Profile -SearchStrategy $SearchStrategy -BeamWidth $BeamWidth -NCpuMoeCandidates $NCpuMoeCandidates -DryRun
    $output = (& ([scriptblock]::Create($cmd)) *>&1 | Out-String).Trim()
    [pscustomobject]@{
        command = $cmd
        output = $output
    }
}
