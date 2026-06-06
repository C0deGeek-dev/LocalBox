# Legacy benchmark history viewer. LocalBench owns benchmarking now; this
# module reads pre-existing bench-history.jsonl entries written by the
# retired `ospeed` helper.

function Get-LLMBenchHistoryFile {
    return Join-Path $script:LLMProfileRoot "bench-history.jsonl"
}

function Read-LLMBenchHistoryEntries {
    $historyFile = Get-LLMBenchHistoryFile

    if (-not (Test-Path $historyFile)) {
        return @()
    }

    $lines = Get-Content -Path $historyFile -Encoding UTF8

    $entries = foreach ($line in $lines) {
        if ([string]::IsNullOrWhiteSpace($line)) { continue }
        try { ConvertFrom-Json $line } catch { continue }
    }

    return @($entries)
}

function Show-LLMBenchHistory {
    [CmdletBinding()]
    param(
        [string]$Model,
        [int]$Last = 20
    )

    $entries = @(Read-LLMBenchHistoryEntries)

    if ($entries.Count -eq 0) {
        Write-Host "No benchmark history. Run 'findbest' to record new entries via LocalBench." -ForegroundColor DarkGray
        return
    }

    if ($Model) {
        $entries = @($entries | Where-Object { $_.model -eq $Model })
    }

    $entries = @($entries | Select-Object -Last $Last)

    if ($entries.Count -eq 0) {
        Write-Host "No matching entries." -ForegroundColor DarkGray
        return
    }

    $entries | Select-Object timestamp, model, output_tokens_per_sec, prompt_tokens_per_sec, total_seconds | Format-Table -AutoSize
}

function Trim-LLMBenchHistory {
    [CmdletBinding()]
    param(
        [int]$OlderThanDays = 90,
        [switch]$DryRun
    )

    $historyFile = Get-LLMBenchHistoryFile

    if (-not (Test-Path $historyFile)) {
        Write-Host "No benchmark history file." -ForegroundColor DarkGray
        return
    }

    $cutoff = (Get-Date).AddDays(-$OlderThanDays)
    $entries = @(Read-LLMBenchHistoryEntries)
    $kept = @()
    $dropped = 0

    foreach ($entry in $entries) {
        $ts = $null
        if ([DateTime]::TryParse($entry.timestamp, [ref]$ts) -and $ts -lt $cutoff) {
            $dropped++
            continue
        }
        $kept += $entry
    }

    if ($dropped -eq 0) {
        Write-Host "Nothing to trim. $($entries.Count) entries, none older than $OlderThanDays days." -ForegroundColor Green
        return
    }

    if ($DryRun) {
        Write-Host "[dry-run] Would drop $dropped entries older than $OlderThanDays days, keep $($kept.Count)." -ForegroundColor Cyan
        return
    }

    $tmp = "$historyFile.tmp"
    if (Test-Path $tmp) { Remove-Item $tmp -Force }

    foreach ($entry in $kept) {
        Add-Content -Path $tmp -Value ($entry | ConvertTo-Json -Compress -Depth 4) -Encoding UTF8
    }

    Move-Item -Path $tmp -Destination $historyFile -Force
    Write-Host "Dropped $dropped entries, kept $($kept.Count)." -ForegroundColor Green
}

function obench { Show-LLMBenchHistory @args }
