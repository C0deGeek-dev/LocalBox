# Inline launch board — interactive loop, capability probe, and launch dispatch.
#
# The pure resolution/render/keymap helpers live in 91-launch-board.ps1. This file
# is the thin interactive shell around them: it probes terminal capability, paints
# the board in place (no Clear-Host, no alternate screen), reads keys, opens the
# existing pickers as drill-downs, and dispatches the chosen plan through
# Invoke-LLMSelection — looping back to the board when the agent exits.

function Test-LaunchBoardCapable {
    # True when the terminal can host the repaint-in-place board: an interactive
    # session with a non-redirected console. `LOCALBOX_NO_BOARD=1` forces the
    # plan-card fallback (Proposal B). Kept side-effect-light so it is testable.
    [CmdletBinding()]
    param()

    if ($env:LOCALBOX_NO_BOARD -eq '1') { return $false }
    if (-not [Environment]::UserInteractive) { return $false }
    try {
        if ([Console]::IsOutputRedirected -or [Console]::IsInputRedirected) { return $false }
    }
    catch { return $false }
    return $true
}

function Build-LaunchSelectionArgs {
    # Map a resolved plan to the Invoke-LLMSelection parameter set (splattable).
    # Pure — pinning this decouples the launch call from the interactive loop and
    # keeps the board honest about exactly what it will run.
    [CmdletBinding()]
    param([Parameter(Mandatory = $true)][pscustomobject]$Plan)

    $selectionArgs = @{
        ModelKey     = $Plan.ModelKey
        ContextKey   = $Plan.ContextKey
        Action       = $Plan.Target
        LlamaCppMode = $Plan.Mode
    }
    if (-not [string]::IsNullOrWhiteSpace($Plan.Quant)) { $selectionArgs.Quant = $Plan.Quant }
    if ($Plan.Strict) { $selectionArgs.Strict = $true }
    if ($Plan.Vision) { $selectionArgs.UseVision = $true }
    if ($Plan.UseAutoBest) {
        $selectionArgs.UseAutoBest = $true
        $selectionArgs.AutoBestProfile = $Plan.AutoBestProfile
    }
    return $selectionArgs
}

function Get-LaunchBoardModels {
    # The board's model list: one dictionary per model with the fields the row
    # formatter needs. Reuses the wizard's filtered-key source so board and
    # wizards agree on which models show.
    [CmdletBinding()]
    param([switch]$All)

    $keys = @(Get-FilteredModelKeys -IncludeAll:$All)
    $models = @()
    foreach ($key in $keys) {
        $def = Get-ModelDef -Key $key
        $models += @{
            Key         = $key
            Tier        = if ($def.ContainsKey('Tier')) { [string]$def.Tier } else { '' }
            DisplayName = if ($def.ContainsKey('DisplayName')) { [string]$def.DisplayName } else { $key }
        }
    }
    # Stream the rows (no unary-comma wrap): callers collect with @(...), and a
    # `, $models` return double-wraps under @() so the first element becomes the
    # whole inner array (its .Key then reads empty).
    return $models
}

function Write-LaunchBoardFrame {
    # Repaint the board in place: home the cursor to the captured origin row and
    # overwrite each line padded to the window width (clearing the previous frame),
    # then blank any leftover rows from a taller previous frame. Never Clear-Host,
    # never the alternate screen — so terminal scrollback above the origin survives.
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$Frame,
        [Parameter(Mandatory = $true)][int]$OriginTop,
        [int]$PreviousHeight = 0
    )

    $lines = @($Frame -split "`n")
    $width = try { [Console]::WindowWidth } catch { 100 }
    $pad = [Math]::Max(1, $width - 1)
    try { [Console]::SetCursorPosition(0, $OriginTop) } catch { Write-Verbose $_.Exception.Message }

    foreach ($line in $lines) {
        $s = [string]$line
        if ($s.Length -gt $pad) { $s = $s.Substring(0, $pad) }
        Write-Host $s.PadRight($pad)
    }
    for ($i = $lines.Count; $i -lt $PreviousHeight; $i++) {
        Write-Host (' ' * $pad)
    }
    return $lines.Count
}

function Invoke-LaunchBoardFieldEdit {
    # Open the existing picker/toggle for one field as a drill-down and return the
    # override to merge (empty hashtable = no change). Target/quant/context/mode
    # reuse the Select-* pickers; vision/strict/autobest toggle inline.
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$Field,
        [Parameter(Mandatory = $true)][string]$ModelKey,
        [Parameter(Mandatory = $true)][pscustomobject]$Plan
    )

    switch ($Field) {
        'Target' {
            $v = Select-LLMAction
            if ($v -and $v -in @('localpilot', 'claude', 'codex', 'serve')) { return @{ Target = $v } }
        }
        'Quant' {
            $v = Select-LLMQuantKey -ModelKey $ModelKey
            if ($v -and $v -ne '__keep__') { return @{ Quant = $v } }
        }
        'Context' {
            $v = Select-LLMContextKey -ModelKey $ModelKey
            if ($null -ne $v) { return @{ ContextKey = $v } }
        }
        'Mode' {
            $v = Select-LLMMode
            if ($v) { return @{ Mode = $v } }
        }
        'Vision' { return @{ Vision = (-not $Plan.Vision) } }
        'Strict' { return @{ Strict = (-not $Plan.Strict) } }
        'AutoBest' {
            # Cycle off -> auto -> balanced -> pure -> off.
            $order = @('off', 'auto', 'balanced', 'pure')
            $cur = if ($Plan.UseAutoBest) { $Plan.AutoBestProfile } else { 'off' }
            $idx = [Math]::Max(0, [Array]::IndexOf($order, $cur))
            $next = $order[($idx + 1) % $order.Count]
            if ($next -eq 'off') { return @{ UseAutoBest = $false } }
            return @{ UseAutoBest = $true; AutoBestProfile = $next }
        }
    }
    return @{}
}

function Invoke-LaunchBoard {
    # The interactive board loop. Returns a launch decision (the resolved plan) or
    # $null on quit. Does not launch — the caller runs Invoke-LLMSelection and loops.
    [CmdletBinding()]
    param([System.Collections.IDictionary]$Defaults = @{})

    $showAll = $false
    $models = @(Get-LaunchBoardModels -All:$showAll)
    if ($models.Count -eq 0) {
        Write-Host "No models configured. Use 'addllm <hf-url> -Key <key>' to add one." -ForegroundColor Yellow
        return $null
    }

    $vram = try { [double](Get-LocalLLMVRAMGB) } catch { 0 }
    $selected = 0
    $overrides = @{}
    $originTop = try { [Console]::CursorTop } catch { 0 }
    $prevHeight = 0
    try { [Console]::CursorVisible = $false } catch { Write-Verbose $_.Exception.Message }

    try {
        while ($true) {
            $model = $models[$selected]
            $def = Get-ModelDef -Key $model.Key
            $plan = Resolve-LaunchPlan -ModelKey $model.Key -Def $def -Defaults $Defaults -Overrides $overrides
            $frame = Format-LaunchBoardFrame -Models $models -SelectedIndex $selected -Plan $plan -Def $def -VramGB $vram
            $prevHeight = Write-LaunchBoardFrame -Frame $frame -OriginTop $originTop -PreviousHeight $prevHeight

            $keyInfo = [Console]::ReadKey($true)
            $action = Get-LaunchBoardAction -KeyName ([string]$keyInfo.Key) -KeyChar ([string]$keyInfo.KeyChar)

            switch -Wildcard ($action) {
                'MoveUp' { $selected = ($selected - 1 + $models.Count) % $models.Count; $overrides = @{} }
                'MoveDown' { $selected = ($selected + 1) % $models.Count; $overrides = @{} }
                'Launch' { return $plan }
                'Quit' { return $null }
                'ToggleAll' {
                    $showAll = -not $showAll
                    $models = @(Get-LaunchBoardModels -All:$showAll)
                    if ($models.Count -eq 0) { $models = @(Get-LaunchBoardModels -All) }
                    $selected = 0; $overrides = @{}
                }
                'Search' {
                    try { [Console]::CursorVisible = $true } catch { Write-Verbose $_.Exception.Message }
                    $term = Read-Host "`nFilter models (blank = clear)"
                    try { [Console]::CursorVisible = $false } catch { Write-Verbose $_.Exception.Message }
                    $all = @(Get-LaunchBoardModels -All:$showAll)
                    if ([string]::IsNullOrWhiteSpace($term)) { $models = $all }
                    else { $models = @($all | Where-Object { $_.Key -like "*$term*" -or $_.DisplayName -like "*$term*" }) }
                    if ($models.Count -eq 0) { $models = $all }
                    $selected = 0; $overrides = @{}; $originTop = try { [Console]::CursorTop } catch { 0 }; $prevHeight = 0
                }
                'Edit:*' {
                    $field = $action.Split(':')[1]
                    $delta = Invoke-LaunchBoardFieldEdit -Field $field -ModelKey $model.Key -Plan $plan
                    foreach ($k in $delta.Keys) { $overrides[$k] = $delta[$k] }
                    # A picker drill-down may have cleared the screen; re-anchor.
                    $originTop = try { [Console]::CursorTop } catch { 0 }; $prevHeight = 0
                }
                'Help' {
                    try { [Console]::CursorVisible = $true } catch { Write-Verbose $_.Exception.Message }
                    Write-Host "`n$(Get-LaunchBoardLegend)`n  arrows/jk move · letters edit a field · Enter launch · Esc quit" -ForegroundColor DarkGray
                    [void][Console]::ReadKey($true)
                    try { [Console]::CursorVisible = $false } catch { Write-Verbose $_.Exception.Message }
                    $originTop = try { [Console]::CursorTop } catch { 0 }; $prevHeight = 0
                }
                default { }
            }
        }
    }
    finally {
        try { [Console]::CursorVisible = $true } catch { Write-Verbose $_.Exception.Message }
    }
}

function Invoke-LaunchBoardCard {
    # Proposal B fallback for terminals that can't host the repaint board: pick a
    # model, then a plan card whose first item is Launch and the rest edit a field.
    # Returns a launch decision (plan) or $null. Reuses the existing pickers.
    [CmdletBinding()]
    param([System.Collections.IDictionary]$Defaults = @{})

    $modelKey = Select-LLMModelKey
    if ([string]::IsNullOrWhiteSpace($modelKey)) { return $null }
    $def = Get-ModelDef -Key $modelKey
    $overrides = @{}

    while ($true) {
        $plan = Resolve-LaunchPlan -ModelKey $modelKey -Def $def -Defaults $Defaults -Overrides $overrides
        $panel = (Format-LaunchPlanPanel -Plan $plan -Def $def) -join "`n"
        $items = @('Launch', 'Target', 'Quant', 'Context', 'Mode', 'AutoBest', 'Vision', 'Strict')
        $idx = Read-LLMChoiceIndex -Title "Launch plan — $modelKey`n$panel" -Items $items -ZeroLabel 'Back to models' -Label { param($x) $x }
        if ($idx -lt 0) { return (Invoke-LaunchBoardCard -Defaults $Defaults) }
        if ($items[$idx] -eq 'Launch') { return $plan }
        $delta = Invoke-LaunchBoardFieldEdit -Field $items[$idx] -ModelKey $modelKey -Plan $plan
        foreach ($k in $delta.Keys) { $overrides[$k] = $delta[$k] }
    }
}

function Start-LaunchBoard {
    # Default `llm` experience: show the board (or the card fallback), launch the
    # chosen plan, then loop back to the board when the agent exits.
    [CmdletBinding()]
    param([switch]$UseVision)

    $defaults = if ($script:Cfg -and $script:Cfg.Contains('DefaultLaunch')) { $script:Cfg['DefaultLaunch'] } else { @{} }

    while ($true) {
        $plan = if (Test-LaunchBoardCapable) { Invoke-LaunchBoard -Defaults $defaults } else { Invoke-LaunchBoardCard -Defaults $defaults }
        if ($null -eq $plan) { return }
        if ($UseVision) { $plan.Vision = $true }

        $selArgs = Build-LaunchSelectionArgs -Plan $plan
        Write-Host ("`n▶ {0} · {1} · {2} · {3}" -f (Get-BoardTargetLabel -Target $plan.Target), $plan.ModelKey, $plan.Quant, $plan.Mode) -ForegroundColor Cyan
        try { Invoke-LLMSelection @selArgs }
        catch { Write-Warning "Launch failed: $($_.Exception.Message)" }
    }
}
