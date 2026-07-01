# Inline launch board — pure resolution, rendering, and input mapping.
#
# Everything in this file is side-effect-free (no console I/O, no Clear-Host, no
# alternate screen) so it is unit-testable without a live terminal. The
# interactive loop that consumes these helpers lives alongside it; the loop stays
# thin because all decisions are made by the pure functions here.

function Select-BoardValue {
    # First non-null, non-empty candidate from an ordered list. The precedence
    # helper the plan resolver leans on so each field reads as "override, else
    # preference, else model default, else hard default".
    param([Parameter(Mandatory = $true)][AllowNull()][object[]]$Candidates)
    foreach ($c in $Candidates) {
        if ($null -ne $c -and -not ($c -is [string] -and [string]::IsNullOrEmpty($c))) {
            return $c
        }
    }
    return $null
}

function Resolve-LaunchPlan {
    # Resolve a fully-populated launch plan for one model from three inputs, in
    # precedence order: explicit user Overrides > cross-model preferences from the
    # saved DefaultLaunch (target/mode/autobest) > per-model definition
    # (quant/context/strict/vision) > hard defaults. Quant and context also seed
    # from DefaultLaunch when the selected model IS the default model. Pure: no I/O.
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$ModelKey,
        [Parameter(Mandatory = $true)][System.Collections.IDictionary]$Def,
        [System.Collections.IDictionary]$Defaults = @{},
        [System.Collections.IDictionary]$Overrides = @{}
    )

    $sameModel = ($ModelKey -eq [string]$Defaults['ModelKey'])
    $defQuant = if ($Def.Contains('Quant')) { [string]$Def['Quant'] } else { '' }
    $firstQuant = if ($Def.Contains('Quants') -and $Def['Quants']) { @($Def['Quants'].Keys | Sort-Object)[0] } else { '' }
    $defStrict = if ($Def.Contains('Strict')) { [bool]$Def['Strict'] } else { $false }

    $target = [string](Select-BoardValue -Candidates @($Overrides['Target'], $Defaults['Action'], 'localpilot'))
    $mode = [string](Select-BoardValue -Candidates @($Overrides['Mode'], $Defaults['LlamaCppMode'], 'native'))
    $autoBestProfile = [string](Select-BoardValue -Candidates @($Overrides['AutoBestProfile'], $Defaults['AutoBestProfile'], 'auto'))
    $quant = [string](Select-BoardValue -Candidates @($Overrides['Quant'], $(if ($sameModel) { $Defaults['Quant'] }), $defQuant, $firstQuant))
    $contextKey = [string](Select-BoardValue -Candidates @($Overrides['ContextKey'], $(if ($sameModel) { $Defaults['ContextKey'] }), ''))

    $useAutoBest = if ($Overrides.Contains('UseAutoBest')) { [bool]$Overrides['UseAutoBest'] }
    elseif ($Defaults.Contains('UseAutoBest')) { [bool]$Defaults['UseAutoBest'] }
    else { $false }

    $vision = if ($Overrides.Contains('Vision')) { [bool]$Overrides['Vision'] } else { $false }
    $strict = if ($Overrides.Contains('Strict')) { [bool]$Overrides['Strict'] } else { $defStrict }

    [pscustomobject]@{
        ModelKey        = $ModelKey
        Target          = $target
        Quant           = $quant
        ContextKey      = $contextKey
        Mode            = $mode
        AutoBestProfile = $autoBestProfile
        UseAutoBest     = $useAutoBest
        Vision          = $vision
        Strict          = $strict
    }
}

function Get-BoardTargetLabel {
    param([Parameter(Mandatory = $true)][string]$Target)
    switch ($Target) {
        'localpilot' { 'LocalPilot' }
        'claude' { 'Claude Code' }
        'codex' { 'Codex' }
        'serve' { 'Serve' }
        default { $Target }
    }
}

function Format-BoardModelRow {
    # One model-list row. `>` marks the selected row; columns are key, tier, name.
    param(
        [Parameter(Mandatory = $true)][System.Collections.IDictionary]$Model,
        [switch]$Selected
    )
    $marker = if ($Selected) { '>' } else { ' ' }
    $key = [string]$Model['Key']
    $tier = if ($Model.Contains('Tier')) { [string]$Model['Tier'] } else { '' }
    $name = if ($Model.Contains('DisplayName')) { [string]$Model['DisplayName'] } else { $key }
    if ($name.Length -gt 22) { $name = $name.Substring(0, 22) }
    '{0} {1,-16} {2,-11} {3}' -f $marker, $key, $tier, $name
}

function Format-LaunchPlanPanel {
    # The right-hand launch-plan panel as an array of lines. Pure: takes the
    # resolved plan + the model def (for quant sizing/context tokens) and an
    # optional VRAM budget for the fit note.
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][pscustomobject]$Plan,
        [Parameter(Mandatory = $true)][System.Collections.IDictionary]$Def,
        [double]$VramGB = 0
    )

    $name = if ($Def.Contains('DisplayName')) { [string]$Def['DisplayName'] } else { $Plan.ModelKey }

    $quantLine = $Plan.Quant
    if ($Def.Contains('QuantSizesGB') -and $Plan.Quant -and $Def['QuantSizesGB'].Contains($Plan.Quant)) {
        $sizeGb = [double]$Def['QuantSizesGB'][$Plan.Quant]
        $fit = if ($VramGB -gt 0) { if ($sizeGb -le $VramGB) { 'fits' } else { 'tight' } } else { '' }
        $quantLine = ('{0}   {1:N1} GB   {2}' -f $Plan.Quant, $sizeGb, $fit).TrimEnd()
    }

    $ctxLabel = if ([string]::IsNullOrEmpty($Plan.ContextKey)) { 'default' } else { $Plan.ContextKey }
    $ctxTokens = ''
    if ($Def.Contains('Contexts') -and $Def['Contexts'].Contains($Plan.ContextKey)) {
        $ctxTokens = '{0} tok' -f [int]$Def['Contexts'][$Plan.ContextKey]
    }

    $autoBest = if ($Plan.UseAutoBest) { '{0} (on)' -f $Plan.AutoBestProfile } else { '{0} (off)' -f $Plan.AutoBestProfile }
    $visionStrict = 'vision {0}   strict {1}' -f $(if ($Plan.Vision) { 'on' } else { 'off' }), $(if ($Plan.Strict) { 'on' } else { 'off' })

    @(
        ('model    {0} — {1}' -f $Plan.ModelKey, $name)
        ('target   {0}' -f (Get-BoardTargetLabel -Target $Plan.Target))
        ('quant    {0}' -f $quantLine)
        ('context  {0}   {1}' -f $ctxLabel, $ctxTokens).TrimEnd()
        ('mode     {0}' -f $Plan.Mode)
        ('autobest {0}' -f $autoBest)
        ($visionStrict)
    )
}

function Format-LaunchBoardFrame {
    # Compose the whole board into one frame string: model list on the left, the
    # resolved launch plan on the right, legend underneath. Pure — the interactive
    # loop paints this in place (cursor-home + clear-to-end), never Clear-Host, so
    # terminal scrollback survives.
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][object[]]$Models,
        [Parameter(Mandatory = $true)][int]$SelectedIndex,
        [Parameter(Mandatory = $true)][pscustomobject]$Plan,
        [Parameter(Mandatory = $true)][System.Collections.IDictionary]$Def,
        [double]$VramGB = 0,
        [int]$LeftWidth = 36
    )

    $rows = @()
    for ($i = 0; $i -lt $Models.Count; $i++) {
        $rows += Format-BoardModelRow -Model $Models[$i] -Selected:($i -eq $SelectedIndex)
    }
    $panel = @(Format-LaunchPlanPanel -Plan $Plan -Def $Def -VramGB $VramGB)

    $lines = @(('  {0}{1}' -f 'models'.PadRight($LeftWidth - 2), 'launch plan'))
    $height = [Math]::Max($rows.Count, $panel.Count)
    for ($i = 0; $i -lt $height; $i++) {
        $left = if ($i -lt $rows.Count) { [string]$rows[$i] } else { '' }
        $right = if ($i -lt $panel.Count) { [string]$panel[$i] } else { '' }
        $lines += ('{0}  {1}' -f $left.PadRight($LeftWidth), $right).TrimEnd()
    }
    $lines += ''
    $lines += (Get-LaunchBoardLegend)
    return ($lines -join "`n")
}

function Get-LaunchBoardAction {
    # Map a keystroke to a board action. Table-driven: special keys by ConsoleKey
    # name, letters by character (case-insensitive). Returns an action string
    # ('Launch','Quit','MoveUp','MoveDown','Search','Preview','FindBest',
    # 'ToggleAll','Help','Edit:<Field>') or '' when the key is unbound. Pure.
    [CmdletBinding()]
    param(
        [string]$KeyName = '',
        [string]$KeyChar = ''
    )

    switch ($KeyName) {
        'Enter' { return 'Launch' }
        'Escape' { return 'Quit' }
        'UpArrow' { return 'MoveUp' }
        'DownArrow' { return 'MoveDown' }
    }

    switch -CaseSensitive ($KeyChar) {
        'k' { return 'MoveUp' }
        'j' { return 'MoveDown' }
    }

    $letterActions = @{
        't' = 'Edit:Target'; 'q' = 'Edit:Quant';   'c' = 'Edit:Context'
        'm' = 'Edit:Mode';   'b' = 'Edit:AutoBest'; 'v' = 'Edit:Vision'
        's' = 'Edit:Strict'; 'p' = 'Preview';       'f' = 'FindBest'
        'a' = 'ToggleAll';   '/' = 'Search';        '?' = 'Help'
    }
    $lc = ([string]$KeyChar).ToLowerInvariant()
    if ($letterActions.Contains($lc)) { return $letterActions[$lc] }
    return ''
}

function Get-LaunchBoardLegend {
    # The one-line key legend shown under the board.
    'Enter launch  t target  q quant  c context  m mode  b autobest  v vision  s strict  p preview  / search  Esc quit'
}
