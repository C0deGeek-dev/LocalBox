# Per-model shortcut function generator. For every catalog entry we bind a
# global function (named after the model's Root or ShortName) that takes
# -Ctx / -Unshackled / -Codex / -Strict / -Quant / -Mode flags and dispatches
# to the llama-server launcher.

function Invoke-ModelShortcut {
    param(
        [Parameter(Mandatory = $true)][string]$Key,
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$ContextKey,
        [switch]$Unshackled,
        [switch]$Codex,
        [switch]$Strict,
        [switch]$UseVision,
        [ValidateSet('native', 'turboquant', 'mtpturbo')][string]$Mode,
        [string]$KvCacheK,
        [string]$KvCacheV,
        [switch]$AutoBest,
        [string[]]$ExtraUnshackledArgs,
        [switch]$DryRun
    )

    $def = Get-ModelDef -Key $Key

    if ($Strict -and -not (Get-ModelStrictEnabled -Def $def)) {
        throw "Model '$Key' has Strict=false in the catalog; refuse -Strict. Set Strict=true on the model def to enable the engineering overlay."
    }

    $resolvedMode = Resolve-LlamaCppMode -Mode $Mode

    Write-LaunchLog "Shortcut launch: key=$Key mode=$resolvedMode unshackled=$Unshackled codex=$Codex strict=$Strict" 'LAUNCH'

    Start-ClaudeWithLlamaCppModel `
        -Key $Key `
        -ContextKey $ContextKey `
        -Mode $resolvedMode `
        -KvCacheK $KvCacheK `
        -KvCacheV $KvCacheV `
        -LimitTools:([bool]$def.LimitTools) `
        -Unshackled:$Unshackled `
        -Codex:$Codex `
        -Strict:$Strict `
        -UseVision:$UseVision `
        -AutoBest:$AutoBest `
        -ExtraUnshackledArgs $ExtraUnshackledArgs `
        -DryRun:$DryRun
}

function Register-ShortcutFunction {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][scriptblock]$ScriptBlock
    )

    Set-Item -Path ("function:global:{0}" -f $Name) -Value $ScriptBlock -Force
}

function Get-ModelShortcutName {
    param([Parameter(Mandatory = $true)][System.Collections.IDictionary]$Def)

    if ($Def.Contains("ShortName") -and -not [string]::IsNullOrWhiteSpace($Def.ShortName)) {
        return $Def.ShortName
    }

    return $Def.Root
}

function Unregister-AllModelShortcuts {
    foreach ($key in (Get-ModelKeys)) {
        $def = Get-ModelDef -Key $key

        $names = New-Object System.Collections.Generic.HashSet[string]
        $names.Add((Get-ModelShortcutName -Def $def)) | Out-Null
        $names.Add($def.Root) | Out-Null

        if ($def.ContainsKey("Quants") -and $def.Contains("QuantShortcut")) {
            foreach ($quantKey in $def.Quants.Keys) {
                $names.Add("set$($def.QuantShortcut)$quantKey") | Out-Null
            }
        }

        foreach ($name in $names) {
            Remove-Item -Path "function:global:$name" -Force -ErrorAction SilentlyContinue
            Remove-Item -Path "alias:$name" -Force -ErrorAction SilentlyContinue
        }
    }
}

function Register-ModelShortcuts {
    Unregister-AllModelShortcuts

    foreach ($key in (Get-ModelKeys)) {
        $def = Get-ModelDef -Key $key
        $name = Get-ModelShortcutName -Def $def
        $k = $key

        Register-ShortcutFunction -Name $name -ScriptBlock ({
                [CmdletBinding()]
                param(
                    [string]$Ctx = "",
                    [string]$Quant,
                    [switch]$Unshackled,
                    [switch]$Codex,
                    [switch]$Strict,
                    [ValidateSet('native', 'turboquant', 'mtpturbo')][string]$Mode,
                    [string]$KvK,
                    [string]$KvV,
                    [switch]$AutoBest,
                    [Alias('WhatIf')]
                    [switch]$DryRun
                )

                if ($Quant) {
                    Set-ModelQuant -Key $k -Quant $Quant
                    return
                }

                Invoke-ModelShortcut -Key $k -ContextKey $Ctx -Unshackled:$Unshackled -Codex:$Codex -Strict:$Strict -Mode $Mode -KvCacheK $KvK -KvCacheV $KvV -AutoBest:$AutoBest -DryRun:$DryRun
            }.GetNewClosure())
    }

    if ($script:Cfg.CommandAliases) {
        foreach ($alias in @($script:Cfg.CommandAliases.Keys)) {
            $target = $script:Cfg.CommandAliases[$alias]

            if ($alias -ne $target) {
                Set-Alias -Name $alias -Value $target -Scope Global -Force
            }
        }
    }
}

function Resolve-ModelKeyByAnyName {
    param([Parameter(Mandatory = $true)][string]$Name)

    if ($script:Cfg.Models.Contains($Name)) {
        return $Name
    }

    foreach ($key in (Get-ModelKeys)) {
        $def = Get-ModelDef -Key $key

        if ($def.Contains("ShortName") -and $def.ShortName -eq $Name) {
            return $key
        }

        if ($def.Root -eq $Name) {
            return $key
        }
    }

    return $null
}

function Find-WorkspaceDefaultModelKey {
    # Walk up from $PWD looking for a .llm-default file. First match wins.
    # Stops at filesystem root. Returns $null if nothing found.
    # File contents may be a key, ShortName, or Root.
    $dir = (Get-Location).Path

    while (-not [string]::IsNullOrWhiteSpace($dir)) {
        $marker = Join-Path $dir ".llm-default"

        if (Test-Path $marker) {
            $value = (Get-Content -Raw -Path $marker -ErrorAction SilentlyContinue).Trim()

            if (-not [string]::IsNullOrWhiteSpace($value)) {
                $resolved = Resolve-ModelKeyByAnyName -Name $value

                if ($resolved) {
                    return $resolved
                }

                Write-Warning "$marker references unknown model '$value'; ignoring."
                return $null
            }
        }

        $parent = Split-Path -Parent $dir

        if ([string]::IsNullOrWhiteSpace($parent) -or $parent -eq $dir) {
            break
        }

        $dir = $parent
    }

    return $null
}

function Get-DefaultModelKey {
    $workspace = Find-WorkspaceDefaultModelKey

    if ($workspace) {
        return $workspace
    }

    if ($script:Cfg.Contains("Default") -and -not [string]::IsNullOrWhiteSpace($script:Cfg.Default)) {
        $resolved = Resolve-ModelKeyByAnyName -Name ([string]$script:Cfg.Default)
        if ($resolved) {
            return $resolved
        }

        Write-Warning "Configured Default '$($script:Cfg.Default)' is not a known model key, ShortName, or Root; falling back to the first recommended model."
    }

    $recommended = @(Get-FilteredModelKeys)

    if ($recommended.Count -gt 0) {
        return $recommended[0]
    }

    throw "No default model: create a .llm-default file in this workspace, set 'Default' in llm-models.json, or add a recommended model."
}

function ConvertTo-LLMDefaultLaunchHashtable {
    param([Parameter(Mandatory = $true)]$Value)

    if ($Value -is [System.Collections.IDictionary]) {
        return $Value
    }

    $result = @{}
    foreach ($prop in @($Value.PSObject.Properties)) {
        $result[$prop.Name] = $prop.Value
    }
    return $result
}

function Save-LLMDefaultLaunch {
    param(
        [Parameter(Mandatory = $true)][string]$ModelKey,
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$ContextKey,
        [Parameter(Mandatory = $true)][ValidateSet('claude','unshackled','codex')][string]$Action,
        [string]$LlamaCppMode,
        [string]$KvCacheK,
        [string]$KvCacheV,
        [switch]$Strict,
        [switch]$UseAutoBest,
        [string]$AutoBestProfile = 'auto'
    )

    $def = Get-ModelDef -Key $ModelKey
    $launch = [ordered]@{
        ModelKey        = $ModelKey
        ContextKey      = $ContextKey
        Action          = $Action
        Strict          = [bool]$Strict
        UseAutoBest     = [bool]$UseAutoBest
        AutoBestProfile = $AutoBestProfile
    }

    if ($def.ContainsKey("Quants")) {
        $launch.Quant = [string]$def.Quant
    }
    if (-not [string]::IsNullOrWhiteSpace($LlamaCppMode)) {
        $launch.LlamaCppMode = $LlamaCppMode
    }
    if (-not [string]::IsNullOrWhiteSpace($KvCacheK)) {
        $launch.KvCacheK = $KvCacheK
    }
    if (-not [string]::IsNullOrWhiteSpace($KvCacheV)) {
        $launch.KvCacheV = $KvCacheV
    }

    Set-LocalLLMSetting Default $ModelKey
    Set-LocalLLMSetting DefaultLaunch $launch
    Write-Host "llmdefault saved: $(Format-LLMDefaultLaunchSummary -Launch $launch)" -ForegroundColor Green
}

function Format-LLMDefaultLaunchSummary {
    param([Parameter(Mandatory = $true)]$Launch)

    $launchMap = ConvertTo-LLMDefaultLaunchHashtable -Value $Launch
    $parts = @(
        [string]$launchMap.ModelKey,
        [string]$launchMap.Action
    )

    $ctx = if ($launchMap.Contains("ContextKey")) { [string]$launchMap.ContextKey } else { "" }
    $parts += "ctx=$(if ([string]::IsNullOrWhiteSpace($ctx)) { 'default' } else { $ctx })"

    if ($launchMap.Contains("Quant") -and -not [string]::IsNullOrWhiteSpace([string]$launchMap.Quant)) {
        $parts += "quant=$($launchMap.Quant)"
    }
    if ($launchMap.Contains("LlamaCppMode") -and -not [string]::IsNullOrWhiteSpace([string]$launchMap.LlamaCppMode)) {
        $parts += "mode=$($launchMap.LlamaCppMode)"
    }
    if ($launchMap.Contains("AutoBestProfile") -and [string]$launchMap.AutoBestProfile -ne 'auto') {
        $parts += "profile=$($launchMap.AutoBestProfile)"
    }
    if ($launchMap.Contains("UseAutoBest") -and [bool]$launchMap.UseAutoBest) {
        $parts += "autobest"
    }
    if ($launchMap.Contains("Strict") -and [bool]$launchMap.Strict) {
        $parts += "strict"
    }

    return ($parts -join " | ")
}

function Invoke-LLMDefaultLaunch {
    param(
        [switch]$Strict,
        [switch]$DryRun
    )

    if (-not $script:Cfg.Contains("DefaultLaunch") -or -not $script:Cfg.DefaultLaunch) {
        Invoke-ModelShortcut -Key (Get-DefaultModelKey) -ContextKey "" -Strict:$Strict -DryRun:$DryRun
        return
    }

    $workspace = Find-WorkspaceDefaultModelKey
    if ($workspace) {
        Invoke-ModelShortcut -Key $workspace -ContextKey "" -Strict:$Strict -DryRun:$DryRun
        return
    }

    $launch = ConvertTo-LLMDefaultLaunchHashtable -Value $script:Cfg.DefaultLaunch
    $modelKey = [string]$launch.ModelKey
    $def = Get-ModelDef -Key $modelKey

    if (-not $DryRun -and $def.ContainsKey("Quants") -and $launch.Contains("Quant") -and -not [string]::IsNullOrWhiteSpace([string]$launch.Quant)) {
        Set-ModelQuantForSelectedLaunch -ModelKey $modelKey -QuantKey ([string]$launch.Quant)
    }

    $contextKey = if ($launch.Contains("ContextKey")) { [string]$launch.ContextKey } else { "" }
    $action = if ($launch.Contains("Action")) { [string]$launch.Action } else { "claude" }
    $llamaCppMode = if ($launch.Contains("LlamaCppMode")) { [string]$launch.LlamaCppMode } else { $null }
    $kvK = if ($launch.Contains("KvCacheK")) { [string]$launch.KvCacheK } else { $null }
    $kvV = if ($launch.Contains("KvCacheV")) { [string]$launch.KvCacheV } else { $null }
    $useStrict = ($launch.Contains("Strict") -and [bool]$launch.Strict) -or [bool]$Strict
    $useAutoBest = $launch.Contains("UseAutoBest") -and [bool]$launch.UseAutoBest
    $autoBestProfile = if ($launch.Contains("AutoBestProfile") -and -not [string]::IsNullOrWhiteSpace([string]$launch.AutoBestProfile)) {
        [string]$launch.AutoBestProfile
    } else {
        "auto"
    }

    Invoke-LLMSelection -ModelKey $modelKey -ContextKey $contextKey -Action $action `
        -LlamaCppMode $llamaCppMode `
        -KvCacheK $kvK -KvCacheV $kvV -Strict:$useStrict `
        -UseAutoBest:$useAutoBest -AutoBestProfile $autoBestProfile -DryRun:$DryRun
}

function llmdefault {
    [CmdletBinding()]
    param(
        [switch]$Strict,
        [Alias('WhatIf')][switch]$DryRun
    )
    Invoke-LLMDefaultLaunch -Strict:$Strict -DryRun:$DryRun
}

function llmdefaultunshackled {
    [CmdletBinding()]
    param(
        [switch]$Strict,
        [Alias('WhatIf')][switch]$DryRun
    )
    Invoke-ModelShortcut -Key (Get-DefaultModelKey) -ContextKey "" -Unshackled -Strict:$Strict -DryRun:$DryRun
}

function llmdefaultcodex {
    [CmdletBinding()]
    param(
        [switch]$Strict,
        [Alias('WhatIf')][switch]$DryRun
    )
    Invoke-ModelShortcut -Key (Get-DefaultModelKey) -ContextKey "" -Codex -Strict:$Strict -DryRun:$DryRun
}
