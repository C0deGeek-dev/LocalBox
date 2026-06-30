# Catalog access + helpers for the llama-server backend. Reload-LocalLLMConfig
# lives here too (it's the public reload; depends on Import-LocalLLMConfig from
# settings and on Register-ModelShortcuts from the shortcuts module).
# Get-ModelGgufPath calls Download-HuggingFaceFile (helpers) and Get-ModelFolder
# (this file).

function Reload-LocalLLMConfig {
    $script:Cfg = Import-LocalLLMConfig
    $script:NoThinkProxyPort = [int]$script:Cfg.NoThinkProxyPort
    Register-ModelShortcuts
    Write-Host "Reloaded LocalBox config: $script:LocalLLMConfigPath" -ForegroundColor Green
}

function Get-ModelDef {
    param([Parameter(Mandatory = $true)][string]$Key)

    if ($script:Cfg.Models.ContainsKey($Key)) {
        return $script:Cfg.Models[$Key]
    }

    throw "Unknown model key: $Key"
}

function Get-ModelKeys {
    return @($script:Cfg.Models.Keys)
}

function Get-ModelTier {
    param([Parameter(Mandatory = $true)][System.Collections.IDictionary]$Def)

    if ($Def.Contains("Tier") -and -not [string]::IsNullOrWhiteSpace($Def.Tier)) {
        return $Def.Tier.ToLowerInvariant()
    }

    return "experimental"
}

function Get-FilteredModelKeys {
    param([switch]$IncludeAll)

    $keys = @(Get-ModelKeys)

    if ($IncludeAll) {
        return $keys
    }

    return @(
        $keys | Where-Object {
            $def = Get-ModelDef -Key $_
            (Get-ModelTier -Def $def) -eq "recommended"
        }
    )
}

function Format-ModelTierBadge {
    param([Parameter(Mandatory = $true)][System.Collections.IDictionary]$Def)

    switch ((Get-ModelTier -Def $Def)) {
        "recommended"  { return "[recommended]" }
        "experimental" { return "[experimental]" }
        "legacy"       { return "[legacy]" }
        default        { return "[$($Def.Tier)]" }
    }
}

function Get-ModelFolder {
    # The folder where this model's GGUF (or downloaded artifacts) live.
    param(
        [Parameter(Mandatory = $true)][System.Collections.IDictionary]$Def
    )

    $folder = Join-Path $script:Cfg.LlamaCppGgufRoot $Def.Root
    Ensure-Directory $folder
    return $folder
}

function Get-ModelFileName {
    param([Parameter(Mandatory = $true)][System.Collections.IDictionary]$Def)

    if ($Def.ContainsKey("Quants")) {
        return $Def.Quants[$Def.Quant]
    }

    return $Def.File
}

function Get-ModelContextValue {
    param(
        [Parameter(Mandatory = $true)][System.Collections.IDictionary]$Def,
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$ContextKey
    )

    $ContextKey = Resolve-ModelContextKey -Def $Def -ContextKey $ContextKey
    return $Def.Contexts[$ContextKey]
}

function Resolve-ModelContextKey {
    param(
        [Parameter(Mandatory = $true)][System.Collections.IDictionary]$Def,
        [AllowEmptyString()][string]$ContextKey
    )

    if ([string]::IsNullOrWhiteSpace($ContextKey)) {
        $ContextKey = ''
    }
    elseif ($ContextKey -ieq 'default') {
        $ContextKey = ''
    }

    if ($Def.Contexts.Contains($ContextKey)) {
        return $ContextKey
    }

    foreach ($key in $Def.Contexts.Keys) {
        if ([string]$key -ieq $ContextKey) {
            return [string]$key
        }
    }

    $legacyAliases = @{
        'fast' = '32k'
        'deep' = '64k'
        '128'  = '128k'
    }

    $aliasKey = $ContextKey.ToLowerInvariant()
    if ($legacyAliases.ContainsKey($aliasKey)) {
        $target = $legacyAliases[$aliasKey]
        if ($Def.Contexts.Contains($target)) {
            return $target
        }
    }

    $available = @($Def.Contexts.Keys | ForEach-Object {
        if ([string]::IsNullOrWhiteSpace($_)) { 'default' } else { [string]$_ }
    }) -join ', '
    throw "Unknown context '$ContextKey'. Available: $available"
}

function Resolve-ModelQuantKey {
    param(
        [Parameter(Mandatory = $true)][System.Collections.IDictionary]$Def,
        [Parameter(Mandatory = $true)][string]$Quant
    )

    if (-not $Def.ContainsKey("Quants")) {
        throw "This model does not support quant switching."
    }

    foreach ($key in $Def.Quants.Keys) {
        if ($key -ieq $Quant) {
            return $key
        }
    }

    $available = @($Def.Quants.Keys) -join ", "
    throw "Unknown quant '$Quant'. Available: $available"
}

# Optional fields on a model def: Description, QuantNotes (qkey -> string),
# ContextNotes (ctxkey -> string). Always read through these helpers — they
# tolerate missing fields and odd casing.

function Get-ModelDescription {
    param([Parameter(Mandatory = $true)][System.Collections.IDictionary]$Def)

    if ($Def.Contains("Description") -and -not [string]::IsNullOrWhiteSpace($Def.Description)) {
        return [string]$Def.Description
    }

    return ""
}

function Get-ModelQuantNote {
    param(
        [Parameter(Mandatory = $true)][System.Collections.IDictionary]$Def,
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$QuantKey
    )

    if (-not $Def.Contains("QuantNotes") -or -not $Def.QuantNotes) { return "" }
    if ([string]::IsNullOrEmpty($QuantKey)) { return "" }

    foreach ($k in $Def.QuantNotes.Keys) {
        if ($k -ieq $QuantKey) { return [string]$Def.QuantNotes[$k] }
    }

    return ""
}

function Get-ModelContextNote {
    param(
        [Parameter(Mandatory = $true)][System.Collections.IDictionary]$Def,
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$ContextKey
    )

    if (-not $Def.Contains("ContextNotes") -or -not $Def.ContextNotes) { return "" }

    $ContextKey = Resolve-ModelContextKey -Def $Def -ContextKey $ContextKey

    # Empty string is a valid key (the "default" context). Match it literally first.
    foreach ($k in $Def.ContextNotes.Keys) {
        if ($k -eq $ContextKey) { return [string]$Def.ContextNotes[$k] }
    }

    foreach ($k in $Def.ContextNotes.Keys) {
        if ($k -ieq $ContextKey) { return [string]$Def.ContextNotes[$k] }
    }

    return ""
}

function Get-ModelStrictEnabled {
    param([Parameter(Mandatory = $true)][System.Collections.IDictionary]$Def)

    if ($Def.Contains("Strict")) {
        return [bool]$Def.Strict
    }

    return $false
}

function Get-ModelGgufPath {
    param(
        [Parameter(Mandatory = $true)][System.Collections.IDictionary]$Def
    )

    $folder = Get-ModelFolder -Def $Def
    $fileName = Get-ModelFileName -Def $Def

    $ggufPath = Download-HuggingFaceFile -Repo $Def.Repo -FileName $fileName -DestinationFolder $folder

    if ($ggufPath -is [array]) {
        $ggufPath = $ggufPath[-1]
    }

    if (-not ($ggufPath -is [string])) {
        throw "Expected GGUF path to be a string."
    }

    return $ggufPath
}

function Get-ModelVisionModulePath {
    # Resolves the full path to the mmproj.gguf (multimodal vision module) for a model.
    # Downloads on demand if not already present locally. Returns $null when no
    # VisionModule is configured or the file does not exist.
    param(
        [Parameter(Mandatory = $true)][string]$Key,
        [Parameter(Mandatory = $true)][System.Collections.IDictionary]$Def
    )

    $mmprojFile = $null
    $autoDetected = $false

    if ($Def.ContainsKey('VisionModule') -and -not [string]::IsNullOrWhiteSpace($Def.VisionModule)) {
        $mmprojFile = [string]$Def.VisionModule
        Write-LaunchLog "VisionModule configured: $mmprojFile" 'VISION'
    } else {
        $folder = Get-ModelFolder -Def $Def
        Write-LaunchLog "No VisionModule configured — scanning for mmproj*.gguf in $folder" 'VISION'
        $localMmproj = Get-ChildItem -Path $folder -Filter 'mmproj*.gguf' -File | Select-Object -First 1
        if ($localMmproj) {
            $mmprojFile = $localMmproj.Name
            $autoDetected = $true
            Write-LaunchLog "Auto-detected mmproj: $($localMmproj.Name)" 'VISION'
        }
        if (-not $mmprojFile) {
            if ($Def.ContainsKey('Repo') -and -not [string]::IsNullOrWhiteSpace($Def.Repo)) {
                Write-LaunchLog "No local mmproj found, querying HF: $($Def.Repo)" 'VISION'
                $hfFiles = Get-HuggingFaceMmprojFiles -Repo $Def.Repo
                if ($null -eq $hfFiles) {
                    Write-LaunchLog "HF query failed (network/SSL) — skipping HF fallback for $Key" 'WARN'
                } elseif ($hfFiles.Count -gt 0) {
                    $mmprojFile = @($hfFiles.Keys)[0]
                    Write-LaunchLog "Found mmproj on HF: $mmprojFile" 'VISION'
                }
            }
            if (-not $mmprojFile) {
                Write-LaunchLog "No mmproj found locally or on HF for $Key" 'WARN'
                return $null
            }
        }
    }

    $folder = Get-ModelFolder -Def $Def

    if ($autoDetected) {
        Write-LaunchLog "Reusing auto-detected mmproj: $mmprojFile" 'VISION'
        $localPath = Resolve-HuggingFaceLocalPath -DestinationFolder $folder -FileName $mmprojFile
        if (Test-Path $localPath) {
            return $localPath
        }
    }

    Write-LaunchLog "Downloading mmproj from HF repo: $($Def.Repo), file: $mmprojFile" 'VISION'
    $mmprojPath = Download-HuggingFaceFile -Repo $Def.Repo -FileName $mmprojFile -DestinationFolder $folder

    if ($mmprojPath -is [array]) {
        $mmprojPath = $mmprojPath[-1]
    }

    if (-not ($mmprojPath -is [string])) {
        throw "Expected mmproj path to be a string."
    }

    Write-LaunchLog "Resolved mmproj path: $mmprojPath" 'VISION'
    return $mmprojPath
}

function Test-ModelVisionModuleAvailable {
    # Checks whether the mmproj.gguf for a model exists locally, and if not,
    # whether it is available on HuggingFace. Returns a hashtable with:
    #   Local        : $true/$false  (file exists in the model folder)
    #   AvailableOnHF: $true/$false  (mmproj file listed on the HF repo)
    #   Filename     : ''            (the mmproj filename, when known)
    param(
        [Parameter(Mandatory = $true)][string]$Key,
        [Parameter(Mandatory = $true)][System.Collections.IDictionary]$Def
    )

    $result = @{
        Local           = $false
        AvailableOnHF   = $false
        Filename        = ''
    }

    $mmprojFile = if ($Def.ContainsKey('VisionModule') -and -not [string]::IsNullOrWhiteSpace($def.VisionModule)) {
        Write-LaunchLog "[vision/test] VisionModule configured: $($def.VisionModule)"  'VISION'
        [string]$Def.VisionModule
    } else {
        Write-LaunchLog "[vision/test] No VisionModule configured, will auto-detect"  'VISION'
        ''
    }

    if ($mmprojFile) {
        $result.Filename = $mmprojFile
    }
    $folder = Get-ModelFolder -Def $Def
    Write-LaunchLog "[vision/test] Checking local mmproj for $Key (folder=$folder)"  'VISION'

    if ($mmprojFile) {
        $localPath = Resolve-HuggingFaceLocalPath -DestinationFolder $folder -FileName $mmprojFile
        Write-LaunchLog "[vision/test]  llama.cpp: checking $($localPath) ..."  'VISION'
        if (Test-Path $localPath) {
            Write-LaunchLog "[vision/test]  Found in llama.cpp folder"  'VISION'
            $result.Local = $true
            return $result
        }
    } else {
        $localMmproj = Get-ChildItem -Path $folder -Filter 'mmproj*.gguf' -File | Select-Object -First 1
        if ($localMmproj) {
            Write-LaunchLog "[vision/test]  Auto-detected $($localMmproj.Name) in llama.cpp folder"  'VISION'
            $result.Local = $true
            $result.Filename = $localMmproj.Name
            return $result
        }
    }

    Write-LaunchLog "[vision/test] No local mmproj found for $Key, checking HuggingFace..."  'VISION'

    if ($Def.ContainsKey('Repo') -and -not [string]::IsNullOrWhiteSpace($Def.Repo)) {
        $mmprojFiles = Get-HuggingFaceMmprojFiles -Repo $Def.Repo
        if ($null -eq $mmprojFiles) {
            Write-LaunchLog "[vision/test] HF check skipped for $Key (network/SSL error)" 'WARN'
        } elseif ($mmprojFiles.Count -gt 0) {
            Write-LaunchLog "[vision/test] HF has $($mmprojFiles.Count) mmproj file(s): $($mmprojFiles.Keys -join ', ')"  'VISION'
            $result.AvailableOnHF = $true
            if (-not $mmprojFile) {
                $mmprojFile = @($mmprojFiles.Keys)[0]
                $result.Filename = $mmprojFile
            } elseif ($mmprojFiles.Contains($mmprojFile)) {
                $result.AvailableOnHF = $true
            }
        } else {
            Write-LaunchLog "[vision/test] No mmproj files on HF for $($Def.Repo)"  'VISION'
        }
    }

    Write-LaunchLog "[vision/test] Result for ${Key}: Local=$($result.Local), HF=$($result.AvailableOnHF), File='$($result.Filename)'"  'VISION'
    return $result
}
