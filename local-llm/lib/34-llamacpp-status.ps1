# Rich llama-server status — per-process inspector that combines launch-arg
# parsing, /props + /slots + /v1/models endpoint queries, nvidia-smi per-PID
# memory, and Windows GPU performance counters into one view. Powers
# `llm-status` (the llama.cpp half) and `Invoke-LlamaCppStatus` as a public
# entry point. Supports table, list, JSON, and watch modes.
#
# Adapted from a standalone Get-LlamaCppStatus.ps1 utility. Kept as one
# self-contained file so the script-form helpers and the function-form
# wrappers stay co-located.
#
# Requires pwsh 7+ (Get-CimInstance Win32_Process, Get-NetTCPConnection,
# Get-Counter \GPU Process Memory) — LocalBox's baseline.

#Requires -Version 7.0

function Convert-LlamaCppBytesToGB {
    param($ValueBytes)
    if ($null -eq $ValueBytes) { return $null }
    [math]::Round(([double]$ValueBytes / 1GB), 2)
}

function Convert-LlamaCppMBToGB {
    param($ValueMB)
    if ($null -eq $ValueMB) { return $null }
    [math]::Round(([double]$ValueMB / 1024), 2)
}

function Convert-LlamaCppToInt64OrNull {
    param($Value)
    if ($null -eq $Value) { return $null }
    $result = 0L
    if ([int64]::TryParse([string]$Value, [ref]$result)) { return $result }
    return $null
}

function Format-LlamaCppUnknown {
    param($Value)
    if ($null -eq $Value -or [string]::IsNullOrWhiteSpace([string]$Value)) { return "?" }
    return $Value
}

function Format-LlamaCppGB {
    param($Value)
    if ($null -eq $Value) { return "?" }
    return "$Value GB"
}

function Shorten-LlamaCppText {
    param([string]$Text, [int]$MaxLength = 60)
    if ([string]::IsNullOrWhiteSpace($Text)) { return $null }
    if ($Text.Length -le $MaxLength) { return $Text }
    return $Text.Substring(0, $MaxLength - 1) + "…"
}

function Get-LlamaCppFileLeafSafe {
    param([string]$Path)
    if ([string]::IsNullOrWhiteSpace($Path)) { return $null }
    try { return Split-Path -Path $Path -Leaf } catch { return $Path }
}

function Get-LlamaCppFileSizeGB {
    param([string]$Path)
    if ([string]::IsNullOrWhiteSpace($Path)) { return $null }
    try {
        if (Test-Path -LiteralPath $Path) {
            return Convert-LlamaCppBytesToGB -ValueBytes (Get-Item -LiteralPath $Path).Length
        }
    } catch { return $null }
    return $null
}

function Split-LlamaCppCommandLine {
    param([Parameter(Mandatory)][string]$CommandLine)
    if ([string]::IsNullOrWhiteSpace($CommandLine)) { return @() }

    # Tokenize: double-quoted, single-quoted, or bare. Good enough for normal
    # llama-server launches.
    $pattern = '"([^"\\]*(?:\\.[^"\\]*)*)"|''([^'']*)''|(\S+)'
    $tokens = foreach ($match in [regex]::Matches($CommandLine, $pattern)) {
        if ($match.Groups[1].Success) { $match.Groups[1].Value -replace '\\"', '"' }
        elseif ($match.Groups[2].Success) { $match.Groups[2].Value }
        else { $match.Groups[3].Value }
    }
    return @($tokens)
}

function Get-LlamaCppArgValue {
    param(
        [Parameter(Mandatory)][string[]]$Tokens,
        [Parameter(Mandatory)][string[]]$Names,
        $Default = $null
    )
    for ($i = 0; $i -lt $Tokens.Count; $i++) {
        $token = $Tokens[$i]
        foreach ($name in $Names) {
            if ($token -eq $name) {
                if (($i + 1) -lt $Tokens.Count) { return $Tokens[$i + 1] }
                return $Default
            }
            $prefix = "$name="
            if ($token.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
                return $token.Substring($prefix.Length)
            }
        }
    }
    return $Default
}

function Test-LlamaCppArgPresent {
    param(
        [Parameter(Mandatory)][string[]]$Tokens,
        [Parameter(Mandatory)][string[]]$Names
    )
    foreach ($token in $Tokens) {
        foreach ($name in $Names) {
            if ($token -eq $name -or $token.StartsWith("$name=", [StringComparison]::OrdinalIgnoreCase)) {
                return $true
            }
        }
    }
    return $false
}

function Get-LlamaCppNvidiaGpuSummary {
    $nvidiaSmi = Get-Command "nvidia-smi" -ErrorAction SilentlyContinue
    if ($null -eq $nvidiaSmi) { return @() }

    try {
        $lines = & $nvidiaSmi.Source `
            "--query-gpu=index,name,memory.total,memory.used,memory.free,utilization.gpu" `
            "--format=csv,noheader,nounits" 2>$null

        $items = foreach ($line in $lines) {
            if ([string]::IsNullOrWhiteSpace($line)) { continue }
            $parts = $line -split ",", 6
            if ($parts.Count -lt 6) { continue }
            [pscustomobject]@{
                GPU         = $parts[0].Trim()
                Name        = $parts[1].Trim()
                TotalGB     = Convert-LlamaCppMBToGB ([int]$parts[2].Trim())
                UsedGB      = Convert-LlamaCppMBToGB ([int]$parts[3].Trim())
                FreeGB      = Convert-LlamaCppMBToGB ([int]$parts[4].Trim())
                Utilization = "$($parts[5].Trim())%"
            }
        }
        return @($items)
    } catch { return @() }
}

function Get-LlamaCppNvidiaProcessMemory {
    $map = @{}
    $nvidiaSmi = Get-Command "nvidia-smi" -ErrorAction SilentlyContinue
    if ($null -eq $nvidiaSmi) { return $map }

    try {
        $lines = & $nvidiaSmi.Source `
            "--query-compute-apps=pid,process_name,used_memory" `
            "--format=csv,noheader,nounits" 2>$null

        foreach ($line in $lines) {
            if ([string]::IsNullOrWhiteSpace($line)) { continue }
            $parts = $line -split ",", 3
            if ($parts.Count -lt 3) { continue }
            $pidText = $parts[0].Trim()
            $memText = $parts[2].Trim()
            $pidValue = 0
            $memValue = 0
            if ([int]::TryParse($pidText, [ref]$pidValue) -and [int]::TryParse($memText, [ref]$memValue)) {
                $map[$pidValue] = [pscustomobject]@{
                    PID          = $pidValue
                    ProcessName  = $parts[1].Trim()
                    UsedMemoryMB = $memValue
                    UsedMemoryGB = Convert-LlamaCppMBToGB $memValue
                }
            }
        }
    } catch {
        # Some Windows/NVIDIA combinations don't report per-process compute memory.
        Write-Verbose "nvidia-smi per-process compute memory unavailable: $($_.Exception.Message)"
    }
    return $map
}

function Get-LlamaCppWindowsGpuMemoryByPid {
    # Fallback when nvidia-smi --query-compute-apps doesn't return per-PID data.
    $map = @{}
    try {
        $counters = Get-Counter '\GPU Process Memory(*)\Dedicated Usage' -ErrorAction Stop
        foreach ($sample in $counters.CounterSamples) {
            # Instance names look like: pid_5304_luid_0x00000000_0x00018f71_phys_0
            if ($sample.InstanceName -match 'pid_(\d+)') {
                $pidValue = [int]$matches[1]
                $bytes = [double]$sample.CookedValue
                if (-not $map.ContainsKey($pidValue)) { $map[$pidValue] = 0.0 }
                $map[$pidValue] += $bytes
            }
        }
        $result = @{}
        foreach ($key in $map.Keys) {
            $result[$key] = [pscustomobject]@{
                PID          = $key
                UsedMemoryGB = [math]::Round($map[$key] / 1GB, 2)
            }
        }
        return $result
    } catch { return @{} }
}

function Get-LlamaCppListeningPortsForProcess {
    param([Parameter(Mandatory)][int]$ProcessId)
    try {
        return @(
            Get-NetTCPConnection -OwningProcess $ProcessId -State Listen -ErrorAction SilentlyContinue |
                Select-Object -ExpandProperty LocalPort -Unique |
                Sort-Object
        )
    } catch { return @() }
}

function Invoke-LlamaCppJsonEndpoint {
    param(
        [string]$HostName,
        [int]$Port,
        [string]$Path,
        [int]$TimeoutSeconds = 2,
        [switch]$NoEndpointQuery
    )
    if ($NoEndpointQuery) { return $null }
    if ($Port -le 0) { return $null }

    $callHost = $HostName
    if ([string]::IsNullOrWhiteSpace($callHost) -or $callHost -eq "0.0.0.0" -or $callHost -eq "::") {
        $callHost = "127.0.0.1"
    }

    $uri = "http://$callHost`:$Port$Path"
    try {
        return Invoke-RestMethod -Uri $uri -Method Get -TimeoutSec $TimeoutSeconds -ErrorAction Stop
    } catch { return $null }
}

function Test-LlamaCppMetricsEndpoint {
    param(
        [string]$HostName,
        [int]$Port,
        [int]$TimeoutSeconds = 2,
        [switch]$NoEndpointQuery
    )
    if ($NoEndpointQuery) { return $false }
    if ($Port -le 0) { return $false }

    $callHost = $HostName
    if ([string]::IsNullOrWhiteSpace($callHost) -or $callHost -eq "0.0.0.0" -or $callHost -eq "::") {
        $callHost = "127.0.0.1"
    }

    $uri = "http://$callHost`:$Port/metrics"
    try {
        $response = Invoke-WebRequest -Uri $uri -Method Get -TimeoutSec $TimeoutSeconds -ErrorAction Stop
        return $response.StatusCode -ge 200 -and $response.StatusCode -lt 300
    } catch { return $false }
}

function Get-LlamaCppServerProcesses {
    param([int[]]$ProcessId)
    $processes = @(
        Get-CimInstance Win32_Process -Filter "Name = 'llama-server.exe'" -ErrorAction SilentlyContinue
    )
    if ($ProcessId -and $ProcessId.Count -gt 0) {
        $processes = @($processes | Where-Object { [int]$_.ProcessId -in $ProcessId })
    }
    return $processes
}

function Get-LlamaCppStatus {
    # Returns an array of per-process status objects. The shape is documented
    # at the bottom of this function so callers can pipe to Format-Table or
    # ConvertTo-Json without surprise.
    #
    # The empty catches here are deliberate presence probes: every
    # `try { $x = $props.<field> } catch {}` reads an optional field that
    # older/other llama-server builds simply don't ship, and absence is the
    # informative outcome ($null stays in place). Logging each miss would
    # spam every status call on every non-bleeding-edge server.
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute('PSAvoidUsingEmptyCatchBlock', '',
        Justification = 'optional endpoint fields; absence is the probed state, $null is the result')]
    [CmdletBinding()]
    param(
        [int[]]$ProcessId,
        [int[]]$Port,
        [int]$TimeoutSeconds = 2,
        [switch]$NoEndpointQuery
    )

    $gpuSummary        = Get-LlamaCppNvidiaGpuSummary
    $gpuMemByPid       = Get-LlamaCppNvidiaProcessMemory
    $windowsGpuMemByPid = Get-LlamaCppWindowsGpuMemoryByPid
    $llamaProcesses    = Get-LlamaCppServerProcesses -ProcessId $ProcessId

    $rows = foreach ($proc in $llamaProcesses) {
        $tokens = Split-LlamaCppCommandLine -CommandLine $proc.CommandLine

        $modelPath       = Get-LlamaCppArgValue -Tokens $tokens -Names @("-m", "--model")
        $ctxArg          = Get-LlamaCppArgValue -Tokens $tokens -Names @("-c", "--ctx-size", "--context-size")
        $hostArg         = Get-LlamaCppArgValue -Tokens $tokens -Names @("--host") -Default "127.0.0.1"
        $portArg         = Get-LlamaCppArgValue -Tokens $tokens -Names @("--port", "-p")
        $gpuLayersArg    = Get-LlamaCppArgValue -Tokens $tokens -Names @("-ngl", "--n-gpu-layers", "--gpu-layers")
        $cpuMoeArg       = Get-LlamaCppArgValue -Tokens $tokens -Names @("--n-cpu-moe")
        $cacheKArg       = Get-LlamaCppArgValue -Tokens $tokens -Names @("--cache-type-k")
        $cacheVArg       = Get-LlamaCppArgValue -Tokens $tokens -Names @("--cache-type-v")
        $parallelArg     = Get-LlamaCppArgValue -Tokens $tokens -Names @("--parallel", "-np")
        $batchArg        = Get-LlamaCppArgValue -Tokens $tokens -Names @("--batch-size", "-b")
        $uBatchArg       = Get-LlamaCppArgValue -Tokens $tokens -Names @("--ubatch-size", "-ub")
        $mmprojPath      = Get-LlamaCppArgValue -Tokens $tokens -Names @("--mmproj")
        $specType        = Get-LlamaCppArgValue -Tokens $tokens -Names @("--spec-type")
        $specDraftNMax   = Get-LlamaCppArgValue -Tokens $tokens -Names @("--spec-draft-n-max")
        $tensorSplit     = Get-LlamaCppArgValue -Tokens $tokens -Names @("--tensor-split")
        $mainGpu         = Get-LlamaCppArgValue -Tokens $tokens -Names @("--main-gpu")
        $threads         = Get-LlamaCppArgValue -Tokens $tokens -Names @("--threads", "-t")
        $threadsBatch    = Get-LlamaCppArgValue -Tokens $tokens -Names @("--threads-batch", "-tb")
        $reasoning       = Get-LlamaCppArgValue -Tokens $tokens -Names @("--reasoning")
        $reasoningBudget = Get-LlamaCppArgValue -Tokens $tokens -Names @("--reasoning-budget")
        $reasoningFormat = Get-LlamaCppArgValue -Tokens $tokens -Names @("--reasoning-format")
        $cacheReuse      = Get-LlamaCppArgValue -Tokens $tokens -Names @("--cache-reuse")

        $configuredPort = Convert-LlamaCppToInt64OrNull $portArg

        $candidatePorts = @()
        if ($configuredPort) {
            $candidatePorts = @([int]$configuredPort)
        } else {
            $candidatePorts = Get-LlamaCppListeningPortsForProcess -ProcessId ([int]$proc.ProcessId)
        }
        if ($candidatePorts.Count -eq 0) { $candidatePorts = @($null) }

        foreach ($candidatePort in $candidatePorts) {
            if ($Port -and $Port.Count -gt 0) {
                if ($null -eq $candidatePort -or [int]$candidatePort -notin $Port) { continue }
            }

            $props = $null
            $slots = $null
            $models = $null
            $metricsEnabled = $false

            if ($null -ne $candidatePort) {
                $props = Invoke-LlamaCppJsonEndpoint -HostName $hostArg -Port ([int]$candidatePort) -Path "/props" -TimeoutSeconds $TimeoutSeconds -NoEndpointQuery:$NoEndpointQuery
                $slots = Invoke-LlamaCppJsonEndpoint -HostName $hostArg -Port ([int]$candidatePort) -Path "/slots" -TimeoutSeconds $TimeoutSeconds -NoEndpointQuery:$NoEndpointQuery
                $models = Invoke-LlamaCppJsonEndpoint -HostName $hostArg -Port ([int]$candidatePort) -Path "/v1/models" -TimeoutSeconds $TimeoutSeconds -NoEndpointQuery:$NoEndpointQuery
                $metricsEnabled = Test-LlamaCppMetricsEndpoint -HostName $hostArg -Port ([int]$candidatePort) -TimeoutSeconds $TimeoutSeconds -NoEndpointQuery:$NoEndpointQuery
            }

            $procInfo = Get-Process -Id ([int]$proc.ProcessId) -ErrorAction SilentlyContinue

            $vramGB = $null
            $vramSource = "Unavailable"
            if ($gpuMemByPid.ContainsKey([int]$proc.ProcessId)) {
                $vramGB = $gpuMemByPid[[int]$proc.ProcessId].UsedMemoryGB
                $vramSource = "NvidiaPerProcess"
            } elseif ($windowsGpuMemByPid.ContainsKey([int]$proc.ProcessId)) {
                $vramGB = $windowsGpuMemByPid[[int]$proc.ProcessId].UsedMemoryGB
                $vramSource = "WindowsGpuCounter"
            } elseif ($llamaProcesses.Count -eq 1 -and $gpuSummary.Count -eq 1) {
                $vramGB = $gpuSummary[0].UsedGB
                $vramSource = "ApproxGpuTotal"
            }

            $propsCtx = $null; $propsSlots = $null; $modelAlias = $null
            $propsModelPath = $null; $vision = $null; $audio = $null
            $buildInfo = $null; $isSleeping = $null
            $endpointSlots = $null; $endpointMetrics = $null
            if ($null -ne $props) {
                try { $propsCtx = $props.default_generation_settings.n_ctx } catch {}
                try { $propsSlots = $props.total_slots } catch {}
                try { $modelAlias = $props.model_alias } catch {}
                try { $propsModelPath = $props.model_path } catch {}
                try { $vision = $props.modalities.vision } catch {}
                try { $audio = $props.modalities.audio } catch {}
                try { $buildInfo = $props.build_info } catch {}
                try { $isSleeping = $props.is_sleeping } catch {}
                try { $endpointSlots = $props.endpoint_slots } catch {}
                try { $endpointMetrics = $props.endpoint_metrics } catch {}
            }

            $slotCount = $null; $processingSlots = $null
            $slotCtx = $null; $decodedTokens = $null; $remainingTokens = $null
            if ($null -ne $slots) {
                $slotArray = @($slots)
                $slotCount = $slotArray.Count
                try { $processingSlots = @($slotArray | Where-Object { $_.is_processing -eq $true }).Count } catch { $processingSlots = $null }
                $firstSlot = $slotArray | Select-Object -First 1
                if ($null -ne $firstSlot) {
                    try { $slotCtx = $firstSlot.n_ctx } catch {}
                    try { $decodedTokens = $firstSlot.next_token.n_decoded } catch {}
                    try { $remainingTokens = $firstSlot.next_token.n_remain } catch {}
                }
            }

            $trainCtx = $null; $params = $null; $apiModelSize = $null
            if ($null -ne $models) {
                try {
                    $firstModel = @($models.data) | Select-Object -First 1
                    if ($null -ne $firstModel) {
                        try { $trainCtx = $firstModel.meta.n_ctx_train } catch {}
                        try { $params = $firstModel.meta.n_params } catch {}
                        try { $apiModelSize = $firstModel.meta.size } catch {}
                    }
                } catch {}
            }

            $effectiveModelPath = if ($propsModelPath) { $propsModelPath } else { $modelPath }
            $effectiveContext = if ($propsCtx) { $propsCtx } elseif ($slotCtx) { $slotCtx } else { Convert-LlamaCppToInt64OrNull $ctxArg }
            $kvText = if ($cacheKArg -or $cacheVArg) { "$cacheKArg/$cacheVArg" } else { $null }
            $endpointOnline = $null -ne $props

            [pscustomobject]@{
                PID              = [int]$proc.ProcessId
                Port             = $candidatePort
                Host             = $hostArg
                Online           = $endpointOnline

                Model            = Shorten-LlamaCppText -Text (Get-LlamaCppFileLeafSafe $effectiveModelPath) -MaxLength 64
                ModelFile        = Get-LlamaCppFileLeafSafe $effectiveModelPath
                ModelAlias       = $modelAlias
                ModelFullPath    = $effectiveModelPath
                ModelSizeGB      = Get-LlamaCppFileSizeGB $effectiveModelPath
                ApiModelSize     = $apiModelSize
                Params           = $params

                Context          = $effectiveContext
                LaunchContext    = Convert-LlamaCppToInt64OrNull $ctxArg
                TrainContext     = $trainCtx

                Slots            = if ($propsSlots) { $propsSlots } else { $slotCount }
                SlotContext      = $slotCtx
                ProcessingSlots  = $processingSlots
                Decoded          = $decodedTokens
                Remaining        = $remainingTokens

                GpuLayers        = $gpuLayersArg
                CpuMoE           = $cpuMoeArg
                TensorSplit      = $tensorSplit
                MainGpu          = $mainGpu
                KV               = $kvText
                CacheK           = $cacheKArg
                CacheV           = $cacheVArg
                CacheReuse       = $cacheReuse

                VRAM_GB          = $vramGB
                VRAM_Source      = $vramSource
                RAM_GB           = if ($procInfo) { Convert-LlamaCppBytesToGB $procInfo.WorkingSet64 } else { $null }
                PrivateRAM_GB    = if ($procInfo) { Convert-LlamaCppBytesToGB $procInfo.PrivateMemorySize64 } else { $null }

                Parallel         = $parallelArg
                Batch            = $batchArg
                UBatch           = $uBatchArg
                Threads          = $threads
                ThreadsBatch     = $threadsBatch

                Vision           = $vision
                Audio            = $audio
                MMProj           = if ($mmprojPath) { Shorten-LlamaCppText -Text (Get-LlamaCppFileLeafSafe $mmprojPath) -MaxLength 56 } else { $null }
                MMProjFile       = Get-LlamaCppFileLeafSafe $mmprojPath
                MMProjFullPath   = $mmprojPath
                MMProjSizeGB     = Get-LlamaCppFileSizeGB $mmprojPath

                MLock            = Test-LlamaCppArgPresent -Tokens $tokens -Names @("--mlock")
                NoMMap           = Test-LlamaCppArgPresent -Tokens $tokens -Names @("--no-mmap")
                Jinja            = Test-LlamaCppArgPresent -Tokens $tokens -Names @("--jinja")

                SpecType         = $specType
                SpecDraftNMax    = $specDraftNMax

                Reasoning        = $reasoning
                ReasoningBudget  = $reasoningBudget
                ReasoningFormat  = $reasoningFormat

                Metrics          = $metricsEnabled
                EndpointSlots    = $endpointSlots
                EndpointMetrics  = $endpointMetrics
                Build            = $buildInfo
                Sleeping         = $isSleeping

                CommandLine      = $proc.CommandLine
                OffloadNote      = "GpuLayers/CpuMoE are launch configuration values. Exact model/KV/compute split requires startup log."
            }
        }
    }

    return @($rows)
}

function Show-LlamaCppGpuSummary {
    $gpuSummary = Get-LlamaCppNvidiaGpuSummary
    Write-Host ""
    Write-Host "== NVIDIA GPU ==" -ForegroundColor Cyan
    if ($gpuSummary.Count -eq 0) {
        Write-Host "nvidia-smi not found or no NVIDIA GPU data available." -ForegroundColor Yellow
        return
    }

    $gpuSummary |
        Select-Object `
            GPU,
            Name,
            @{Name="Total";Expression={Format-LlamaCppGB $_.TotalGB}},
            @{Name="Used";Expression={Format-LlamaCppGB $_.UsedGB}},
            @{Name="Free";Expression={Format-LlamaCppGB $_.FreeGB}},
            Utilization |
        Format-Table -AutoSize |
        Out-String -Width 300 |
        Write-Host
}

function Show-LlamaCppStatusTable {
    param(
        [AllowNull()][object[]]$Rows,
        [switch]$Detailed
    )

    Write-Host ""
    Write-Host "== llama.cpp / llama-server ==" -ForegroundColor Cyan

    if (-not $Rows -or $Rows.Count -eq 0) {
        Write-Host "No running llama-server.exe processes found." -ForegroundColor Yellow
        return
    }

    if ($Detailed) {
        foreach ($row in $Rows | Sort-Object PID, Port) {
            Write-Host ""
            Write-Host "llama-server PID $($row.PID) / port $($row.Port)" -ForegroundColor Green
            $row |
                Select-Object PID, Port, Host, Online, ModelFile, ModelSizeGB,
                    Context, LaunchContext, TrainContext, Slots, SlotContext,
                    ProcessingSlots, Decoded, Remaining, GpuLayers, CpuMoE,
                    TensorSplit, MainGpu, KV, CacheReuse,
                    @{Name="VRAM";Expression={Format-LlamaCppGB $_.VRAM_GB}},
                    VRAM_Source,
                    @{Name="RAM";Expression={Format-LlamaCppGB $_.RAM_GB}},
                    @{Name="PrivateRAM";Expression={Format-LlamaCppGB $_.PrivateRAM_GB}},
                    Parallel, Batch, UBatch, Threads, ThreadsBatch,
                    Vision, Audio, MMProjFile, MMProjSizeGB,
                    MLock, NoMMap, Jinja, SpecType, SpecDraftNMax,
                    Reasoning, ReasoningBudget, ReasoningFormat,
                    Metrics, EndpointSlots, EndpointMetrics, Build, Sleeping,
                    OffloadNote |
                Format-List
            Write-Host "Model path:" -ForegroundColor DarkCyan
            Write-Host "  $($row.ModelFullPath)"
            if ($row.MMProjFullPath) {
                Write-Host "MMProj path:" -ForegroundColor DarkCyan
                Write-Host "  $($row.MMProjFullPath)"
            }
            Write-Host "Command line:" -ForegroundColor DarkCyan
            Write-Host "  $($row.CommandLine)"
        }
        return
    }

    Write-Host ""
    Write-Host "-- Runtime --" -ForegroundColor DarkCyan
    $Rows |
        Sort-Object PID, Port |
        Select-Object PID, Port, Online,
            @{Name="Ctx";Expression={Format-LlamaCppUnknown $_.Context}},
            @{Name="TrainCtx";Expression={Format-LlamaCppUnknown $_.TrainContext}},
            @{Name="Slots";Expression={Format-LlamaCppUnknown $_.Slots}},
            @{Name="Busy";Expression={Format-LlamaCppUnknown $_.ProcessingSlots}},
            @{Name="Vision";Expression={Format-LlamaCppUnknown $_.Vision}},
            @{Name="MMProj";Expression={if ($_.MMProj) { "Yes" } else { "No" }}},
            @{Name="Model";Expression={$_.Model}} |
        Format-Table -AutoSize |
        Out-String -Width 220 |
        Write-Host

    Write-Host "-- Memory / Offload --" -ForegroundColor DarkCyan
    $Rows |
        Sort-Object PID, Port |
        Select-Object PID,
            @{Name="ngl";Expression={Format-LlamaCppUnknown $_.GpuLayers}},
            @{Name="CPU-MoE";Expression={Format-LlamaCppUnknown $_.CpuMoE}},
            @{Name="KV";Expression={Format-LlamaCppUnknown $_.KV}},
            @{Name="VRAM";Expression={
                if ($_.VRAM_Source -eq "ApproxGpuTotal") { "$(Format-LlamaCppGB $_.VRAM_GB)*" }
                else { Format-LlamaCppGB $_.VRAM_GB }
            }},
            @{Name="RAM";Expression={Format-LlamaCppGB $_.RAM_GB}},
            @{Name="PrivateRAM";Expression={Format-LlamaCppGB $_.PrivateRAM_GB}},
            @{Name="Parallel";Expression={Format-LlamaCppUnknown $_.Parallel}},
            @{Name="Spec";Expression={Format-LlamaCppUnknown $_.SpecType}} |
        Format-Table -AutoSize |
        Out-String -Width 220 |
        Write-Host

    $hasApproxVram = @($Rows | Where-Object { $_.VRAM_Source -eq "ApproxGpuTotal" }).Count -gt 0
    if ($hasApproxVram) {
        Write-Host "* VRAM approximated from total GPU usage (nvidia-smi did not report per-process memory)." -ForegroundColor DarkYellow
    }
    Write-Host "Note: ngl/CPU-MoE are launch configuration values. Exact model/KV/compute split requires the llama-server startup log." -ForegroundColor DarkYellow
}

function Show-LlamaCppStatusInterpretation {
    param([AllowNull()][object[]]$Rows)
    if (-not $Rows -or $Rows.Count -eq 0) { return }

    Write-Host ""
    Write-Host "== Interpretation ==" -ForegroundColor Cyan
    foreach ($row in $Rows | Sort-Object PID, Port) {
        $ctx = Format-LlamaCppUnknown $row.Context
        $train = Format-LlamaCppUnknown $row.TrainContext
        $ngl = Format-LlamaCppUnknown $row.GpuLayers
        $cpuMoe = Format-LlamaCppUnknown $row.CpuMoE
        $kv = Format-LlamaCppUnknown $row.KV
        Write-Host "- PID $($row.PID): ctx=$ctx / train_ctx=$train / ngl=$ngl / cpu_moe=$cpuMoe / kv=$kv"
        if ($row.GpuLayers -eq "999" -or $row.GpuLayers -eq "all" -or $row.GpuLayers -eq "auto") {
            Write-Host "  Requested maximum GPU layer offload. Actual result depends on VRAM and llama.cpp startup allocation." -ForegroundColor DarkGray
        }
        if ($row.CpuMoE) {
            Write-Host "  MoE CPU offload is configured via --n-cpu-moe $($row.CpuMoE)." -ForegroundColor DarkGray
        }
        if ($row.VRAM_Source -eq "ApproxGpuTotal") {
            Write-Host "  VRAM value is total GPU usage, not exact per-process usage." -ForegroundColor DarkGray
        }
    }
}

function Invoke-LlamaCppStatus {
    # Public entry point. Runs once unless -Watch is supplied.
    # Detailed/Json/NoGpuSummary are consumed inside the $runOnce closure;
    # the analyzer's unused-parameter rule does not track scriptblock captures.
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute('PSReviewUnusedParameter', 'Detailed', Justification = 'used inside the $runOnce closure')]
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute('PSReviewUnusedParameter', 'Json', Justification = 'used inside the $runOnce closure')]
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute('PSReviewUnusedParameter', 'NoGpuSummary', Justification = 'used inside the $runOnce closure')]
    [CmdletBinding()]
    param(
        [Alias("Pid")][int[]]$ProcessId,
        [int[]]$Port,
        [switch]$Detailed,
        [switch]$Json,
        [switch]$NoEndpointQuery,
        [switch]$Watch,
        [int]$IntervalSeconds = 3,
        [int]$TimeoutSeconds = 2,
        [switch]$NoGpuSummary
    )

    $statusParams = @{
        ProcessId        = $ProcessId
        Port             = $Port
        TimeoutSeconds   = $TimeoutSeconds
        NoEndpointQuery  = $NoEndpointQuery
    }

    $runOnce = {
        $rows = @(Get-LlamaCppStatus @statusParams)
        if ($Json) {
            if ($rows.Count -eq 0) { '[]' } else { $rows | ConvertTo-Json -Depth 10 }
            return
        }
        if (-not $NoGpuSummary) { Show-LlamaCppGpuSummary }
        Show-LlamaCppStatusTable -Rows $rows -Detailed:$Detailed
        if (-not $Detailed) { Show-LlamaCppStatusInterpretation -Rows $rows }
    }

    if (-not $Watch) {
        & $runOnce
        return
    }

    while ($true) {
        Clear-Host
        Write-Host "Refreshing every $IntervalSeconds second(s). Press Ctrl+C to stop." -ForegroundColor DarkGray
        & $runOnce
        Start-Sleep -Seconds $IntervalSeconds
    }
}
