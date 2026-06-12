# LocalBench discovery/import bridge. The launcher owns launch-time AutoBest
# loading; LocalBench owns benchmark execution and compatible profile export.

function Resolve-LocalBenchModulePath {
    [CmdletBinding()]
    param([Parameter(Mandatory = $true)][string]$Root)

    if ([string]::IsNullOrWhiteSpace($Root)) { return $null }

    $expanded = Expand-LocalLLMPath $Root
    if (-not (Test-Path -LiteralPath $expanded -ErrorAction SilentlyContinue)) { return $null }

    if (Test-Path -LiteralPath $expanded -PathType Leaf -ErrorAction SilentlyContinue) {
        $leaf = Split-Path -Leaf $expanded
        if ($leaf -in @('LocalBench.psm1', 'LocalBench.psd1')) {
            return (Resolve-Path -LiteralPath $expanded).Path
        }
    }

    $candidates = @(
        (Join-Path $expanded 'src\LocalBench.psm1'),
        (Join-Path $expanded 'LocalBench.psd1'),
        (Join-Path $expanded 'LocalBench.psm1')
    )

    foreach ($path in $candidates) {
        if (Test-Path -LiteralPath $path -PathType Leaf -ErrorAction SilentlyContinue) {
            return (Resolve-Path -LiteralPath $path).Path
        }
    }

    return $null
}

function Resolve-LocalBenchRoot {
    [CmdletBinding()]
    param()

    $candidates = New-Object System.Collections.Generic.List[object]

    if (-not [string]::IsNullOrWhiteSpace($env:LOCALBENCH_ROOT)) {
        $candidates.Add([pscustomobject]@{ Source = 'env:LOCALBENCH_ROOT'; Root = $env:LOCALBENCH_ROOT; ModulePath = $null }) | Out-Null
    }

    if ($script:Cfg -and $script:Cfg.ContainsKey('LocalBenchRoot') -and -not [string]::IsNullOrWhiteSpace($script:Cfg.LocalBenchRoot)) {
        $candidates.Add([pscustomobject]@{ Source = 'setting:LocalBenchRoot'; Root = $script:Cfg.LocalBenchRoot; ModulePath = $null }) | Out-Null
    }

    $module = Get-Module -ListAvailable -Name LocalBench -ErrorAction SilentlyContinue | Sort-Object Version -Descending | Select-Object -First 1
    if ($module) {
        $candidates.Add([pscustomobject]@{ Source = 'module:LocalBench'; Root = $module.ModuleBase; ModulePath = $module.Path }) | Out-Null
    }

    $managed = Join-Path $HOME '.local-llm\tools\localbench'
    $candidates.Add([pscustomobject]@{ Source = 'managed'; Root = $managed; ModulePath = $null }) | Out-Null

    if (-not [string]::IsNullOrWhiteSpace($script:LLMProfileRoot)) {
        $launcherDir   = Split-Path -Parent $script:LLMProfileRoot
        $workspaceDir  = Split-Path -Parent $launcherDir
        if (-not [string]::IsNullOrWhiteSpace($workspaceDir)) {
            $candidates.Add([pscustomobject]@{ Source = 'heuristic:workspace-sibling'; Root = (Join-Path $workspaceDir 'localbench'); ModulePath = $null }) | Out-Null
        }
    }

    foreach ($folder in @('IdeaProjects', 'repos', 'projects', 'code', 'dev', 'src', 'git')) {
        $candidates.Add([pscustomobject]@{ Source = "heuristic:$folder"; Root = (Join-Path $HOME "$folder\localbench"); ModulePath = $null }) | Out-Null
    }

    foreach ($candidate in $candidates) {
        $modulePath = if ($candidate.ModulePath) { $candidate.ModulePath } else { Resolve-LocalBenchModulePath -Root $candidate.Root }
        if ($modulePath -and (Test-Path -LiteralPath $modulePath -PathType Leaf)) {
            $root = if ($candidate.Root) { Expand-LocalLLMPath $candidate.Root } else { Split-Path -Parent $modulePath }
            return [pscustomobject]@{
                Source = $candidate.Source
                Root = $root
                ModulePath = $modulePath
            }
        }
    }

    return $null
}

function Import-LocalBenchModule {
    [CmdletBinding()]
    param([string]$Root)

    $resolved = if ([string]::IsNullOrWhiteSpace($Root)) {
        Resolve-LocalBenchRoot
    } else {
        $modulePath = Resolve-LocalBenchModulePath -Root $Root
        if (-not $modulePath) { throw "LocalBench module not found under $Root" }
        [pscustomobject]@{ Source = 'explicit'; Root = (Expand-LocalLLMPath $Root); ModulePath = $modulePath }
    }

    if (-not $resolved) {
        throw "LocalBench was not found. Set LOCALBENCH_ROOT, setllm LocalBenchRoot <path>, install the LocalBench module, or clone to ~/.local-llm/tools/localbench."
    }

    Import-Module $resolved.ModulePath -Force -ErrorAction Stop | Out-Null
    return $resolved
}

function Test-LocalBenchIntegrationAvailable {
    [CmdletBinding()]
    param([switch]$Quiet)

    $minimum = if ($script:Cfg -and $script:Cfg.ContainsKey('LocalBenchMinimumVersion')) {
        [string]$script:Cfg.LocalBenchMinimumVersion
    } else {
        '0.1.0'
    }

    $result = [ordered]@{
        Available = $false
        Found = $false
        Source = ''
        Root = ''
        ModulePath = ''
        Version = ''
        ApiVersion = 0
        LauncherExportVersion = 0
        MinimumVersion = $minimum
        Reason = ''
    }

    try {
        $resolved = Import-LocalBenchModule
        $result.Found = $true
        $result.Source = $resolved.Source
        $result.Root = $resolved.Root
        $result.ModulePath = $resolved.ModulePath

        if (-not (Get-Command Get-LocalBenchVersion -ErrorAction SilentlyContinue)) {
            $result.Reason = 'LocalBench module imported, but Get-LocalBenchVersion is missing.'
            return [pscustomobject]$result
        }

        $version = Get-LocalBenchVersion
        $result.Version = [string]$version.version
        $result.ApiVersion = [int]$version.api_version
        $result.LauncherExportVersion = [int]$version.launcher_export_version

        $versionOk = $true
        try {
            $versionOk = ([version]$result.Version -ge [version]$minimum)
        }
        catch {
            $versionOk = $false
        }

        if (-not $versionOk) {
            $result.Reason = "LocalBench $($result.Version) is below required $minimum."
            return [pscustomobject]$result
        }
        if ($result.ApiVersion -lt 1) {
            $result.Reason = "LocalBench API version $($result.ApiVersion) is below required 1."
            return [pscustomobject]$result
        }
        if ($result.LauncherExportVersion -lt 1) {
            $result.Reason = "LocalBench launcher export version $($result.LauncherExportVersion) is below required 1."
            return [pscustomobject]$result
        }

        $result.Available = $true
        $result.Reason = 'OK'
        return [pscustomobject]$result
    }
    catch {
        $result.Reason = $_.Exception.Message
        if (-not $Quiet) {
            Write-LaunchLog "LocalBench check failed: $($result.Reason)" 'WARN'
        }
        return [pscustomobject]$result
    }
}

function Invoke-LocalBenchLauncherFindBest {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute('PSAvoidAssignmentToAutomaticVariable', 'Profile', Justification = 'shipped -Profile parameter name; renaming would break existing callers')]
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$Key,
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$ContextKey,
        [ValidateSet('native','turboquant','mtpturbo')][string]$Mode = 'native',
        [string]$Quant,
        [string[]]$AllowedKvTypes,
        [int]$Budget = 100,
        [ValidateSet('gen','prompt','both','coding-agent')][string]$Optimize = 'coding-agent',
        [int]$Runs = 3,
        [switch]$Quick,
        [switch]$Deep,
        [switch]$Aggressive,
        [switch]$AggressiveKv,
        [switch]$AllowKvQualityRegression,
        [ValidateSet('short','long')][string[]]$PromptLengths = @(),
        [ValidateSet('pure','balanced','both')][string]$Profile = 'pure',
        [ValidateSet('greedy','beam')][string]$SearchStrategy,
        [int]$BeamWidth = 1,
        [int[]]$NCpuMoeCandidates,
        [switch]$UseVision,
        [switch]$NoTrialCache,
        [switch]$ClearTrialCache,
        [switch]$NoSave
    )

    Import-LocalBenchModule | Out-Null
    if (-not (Get-Command Find-LocalBenchBestConfig -ErrorAction SilentlyContinue)) {
        throw "LocalBench is available, but Find-LocalBenchBestConfig is not implemented by this version."
    }

    if (-not $PromptLengths -or $PromptLengths.Count -eq 0) {
        $PromptLengths = if ($Optimize -eq 'coding-agent') { @('long') } else { @('short') }
    }

    $params = @{
        Target = 'LocalBox'
        Runtime = 'llamacpp'
        Key = $Key
        ContextKey = $ContextKey
        Mode = $Mode
        Quant = $Quant
        PromptLengths = $PromptLengths
        AllowedKvTypes = $AllowedKvTypes
        Optimize = $Optimize
        Budget = $Budget
        Runs = $Runs
        Quick = $Quick
        Deep = $Deep
        Aggressive = $Aggressive
        AggressiveKv = $AggressiveKv
        AllowKvQualityRegression = $AllowKvQualityRegression
        Profile = $Profile
        NoSave = $NoSave
        UseVision = $UseVision
        LauncherRoot = $script:LLMProfileRoot
    }
    if ($PSBoundParameters.ContainsKey('SearchStrategy') -and -not [string]::IsNullOrWhiteSpace($SearchStrategy)) {
        $params.SearchStrategy = $SearchStrategy
    }
    if ($PSBoundParameters.ContainsKey('BeamWidth')) {
        $params.BeamWidth = $BeamWidth
    }
    if ($PSBoundParameters.ContainsKey('NCpuMoeCandidates') -and $NCpuMoeCandidates -and $NCpuMoeCandidates.Count -gt 0) {
        $params.NCpuMoeCandidates = $NCpuMoeCandidates
    }
    if ($NoTrialCache) { $params.NoTrialCache = $true }
    if ($ClearTrialCache) { $params.ClearTrialCache = $true }

    Find-LocalBenchBestConfig @params
}

function Get-LocalBenchTopNCpuMoeValues {
    param(
        [Parameter(Mandatory = $true)][string]$Key,
        [AllowEmptyString()][string]$ContextKey = '',
        [int]$TopN = 5
    )
    try { Import-LocalBenchModule | Out-Null } catch { return @() }
    if (Get-Command Get-LlamaCppTopNCpuMoeFromCandidates -ErrorAction SilentlyContinue) {
        return @(Get-LlamaCppTopNCpuMoeFromCandidates -Key $Key -ContextKey $ContextKey -TopN $TopN)
    }
    return @()
}

function Get-LocalBenchLauncherBestConfig {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute('PSAvoidAssignmentToAutomaticVariable', 'Profile', Justification = 'shipped -Profile parameter name; renaming would break existing callers')]
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$Key,
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$ContextKey,
        [Parameter(Mandatory = $true)][string]$Mode,
        [ValidateSet('short','long')][string]$PromptLength = 'short',
        [string]$Quant,
        [ValidateSet('pure','balanced')][string]$Profile = 'pure'
    )

    Import-LocalBenchModule | Out-Null
    if (-not (Get-Command Get-LocalBenchBestConfig -ErrorAction SilentlyContinue)) {
        throw "LocalBench is available, but Get-LocalBenchBestConfig is not implemented by this version."
    }

    Get-LocalBenchBestConfig -Target LocalBox -Runtime llamacpp -Key $Key -ContextKey $ContextKey -Mode $Mode -PromptLength $PromptLength -Quant $Quant -Profile $Profile
}

function Get-LocalBenchLauncherBestConfigCandidates {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute('PSAvoidAssignmentToAutomaticVariable', 'Profile', Justification = 'shipped -Profile parameter name; renaming would break existing callers')]
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$Key,
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$ContextKey,
        [Parameter(Mandatory = $true)][string]$Mode,
        [ValidateSet('short','long')][string]$PromptLength = 'short',
        [string]$Quant,
        [ValidateSet('pure','balanced')][string]$Profile = 'pure'
    )

    Import-LocalBenchModule | Out-Null
    if (-not (Get-Command Get-LocalBenchBestConfigCandidates -ErrorAction SilentlyContinue)) {
        throw "LocalBench is available, but Get-LocalBenchBestConfigCandidates is not implemented by this version."
    }

    Get-LocalBenchBestConfigCandidates -Target LocalBox -Runtime llamacpp -Key $Key -ContextKey $ContextKey -Mode $Mode -PromptLength $PromptLength -Quant $Quant -Profile $Profile
}

function Show-LocalBenchLauncherHistory {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$Key,
        [int]$Last = 50
    )

    Import-LocalBenchModule | Out-Null
    if (-not (Get-Command Show-LocalBenchHistory -ErrorAction SilentlyContinue)) {
        throw "LocalBench is available, but Show-LocalBenchHistory is not implemented by this version."
    }

    Show-LocalBenchHistory -Target LocalBox -Runtime llamacpp -Key $Key -Last $Last
}

function Show-LocalBenchLauncherStatus {
    [CmdletBinding()]
    param([switch]$Quiet)

    $status = Test-LocalBenchIntegrationAvailable -Quiet

    if (-not $Quiet) {
        Write-Section "LocalBench"
    }

    if ($status.Available) {
        Write-Host "LocalBench : available $($status.Version) ($($status.Source))" -ForegroundColor Green
        Write-Host "Root       : $($status.Root)" -ForegroundColor DarkGray
        Write-Host "API/export : $($status.ApiVersion) / $($status.LauncherExportVersion)" -ForegroundColor DarkGray
        return $status
    }

    if ($status.Found) {
        Write-Host "LocalBench : found but unavailable" -ForegroundColor Yellow
        Write-Host "Reason     : $($status.Reason)" -ForegroundColor DarkYellow
        Write-Host "Root       : $($status.Root)" -ForegroundColor DarkGray
    }
    else {
        Write-Host "LocalBench : not found" -ForegroundColor DarkGray
        Write-Host "Tuning     : unavailable until LocalBench is installed" -ForegroundColor DarkGray
        Write-Host "Configure  : setllm LocalBenchRoot <path-to-localbench>" -ForegroundColor DarkGray
    }

    return $status
}

function lbstatus {
    [CmdletBinding()]
    param()

    Show-LocalBenchLauncherStatus | Out-Null
}

function Install-LocalBench {
    [CmdletBinding()]
    param(
        [string]$Destination = (Join-Path $HOME '.local-llm\tools\localbench'),
        [switch]$Force
    )

    if ((Resolve-LocalBenchModulePath -Root $Destination) -and -not $Force) {
        Write-Host "LocalBench already exists: $Destination" -ForegroundColor Green
        Set-LocalLLMSetting LocalBenchRoot $Destination
        return $Destination
    }

    if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
        throw "git is not on PATH; cannot clone LocalBench."
    }

    $repoUrl = if ($script:Cfg.ContainsKey('LocalBenchRepoUrl') -and -not [string]::IsNullOrWhiteSpace($script:Cfg.LocalBenchRepoUrl)) {
        [string]$script:Cfg.LocalBenchRepoUrl
    } else {
        'https://github.com/C0deGeek-dev/LocalBench'
    }

    Ensure-Directory (Split-Path -Parent $Destination)
    if (Test-Path -LiteralPath $Destination) {
        throw "Destination already exists: $Destination. Use Update-LocalBench, or remove it and retry."
    }

    & git clone $repoUrl $Destination
    if ($LASTEXITCODE -ne 0) { throw "git clone failed for $repoUrl" }

    Set-LocalLLMSetting LocalBenchRoot $Destination
    return $Destination
}

function Update-LocalBench {
    [CmdletBinding()]
    param()

    $resolved = Resolve-LocalBenchRoot
    if (-not $resolved) {
        throw "LocalBench is not installed. Run Install-LocalBench first."
    }

    $root = $resolved.Root
    $result = Invoke-LocalLLMGitFastForwardUpdate -Name 'LocalBench' -Root $root
    if ($result.Status -in @('failed', 'not-git', 'no-upstream', 'diverged')) {
        throw $result.Reason
    }
    return $result
}
