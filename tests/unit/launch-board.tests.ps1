BeforeAll {
    $repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
    function Write-Section { param([string]$Title) }
    . (Join-Path $repoRoot 'local-llm\lib\91-launch-board.ps1')

    $script:LaunchBoardLib = Join-Path $repoRoot 'local-llm\lib\91-launch-board.ps1'

    # A representative model def mirroring llm-models.json shape.
    $script:OrnithDef = @{
        DisplayName   = 'Ornith 1.0 35B heretic APEX GGUF'
        Tier          = 'experimental'
        Quant         = 'apex-i-quality'
        Quants        = @{ 'apex-i-quality' = 'a.gguf'; 'apex-i-compact' = 'b.gguf' }
        QuantSizesGB  = @{ 'apex-i-quality' = 22.8; 'apex-i-compact' = 16.5 }
        Contexts      = @{ '' = 16384; '256k' = 262144 }
        Strict        = $true
    }
    $script:DefaultLaunch = @{
        ModelKey = 'q3635ba3bapex'; Action = 'localpilot'; LlamaCppMode = 'turboquant'
        AutoBestProfile = 'balanced'; UseAutoBest = $true; Quant = 'apex-i-quality'; ContextKey = '256k'
    }
}

Describe 'Resolve-LaunchPlan' {
    It 'applies cross-model preferences (target/mode/autobest) but per-model quant/context for a non-default model' {
        $plan = Resolve-LaunchPlan -ModelKey 'ornith35hapex' -Def $script:OrnithDef -Defaults $script:DefaultLaunch
        $plan.Target | Should -Be 'localpilot'          # preference
        $plan.Mode | Should -Be 'turboquant'            # preference
        $plan.AutoBestProfile | Should -Be 'balanced'   # preference
        $plan.UseAutoBest | Should -BeTrue
        $plan.Quant | Should -Be 'apex-i-quality'       # model default (NOT seeded from default model)
        $plan.ContextKey | Should -Be ''                # not the default model -> model base context
        $plan.Strict | Should -BeTrue                   # model def
        $plan.Vision | Should -BeFalse
    }

    It 'seeds quant and context from DefaultLaunch when the selected model IS the default model' {
        $plan = Resolve-LaunchPlan -ModelKey 'q3635ba3bapex' -Def $script:OrnithDef -Defaults $script:DefaultLaunch
        $plan.Quant | Should -Be 'apex-i-quality'
        $plan.ContextKey | Should -Be '256k'
    }

    It 'lets explicit overrides win over everything' {
        $plan = Resolve-LaunchPlan -ModelKey 'ornith35hapex' -Def $script:OrnithDef -Defaults $script:DefaultLaunch `
            -Overrides @{ Target = 'claude'; Mode = 'native'; Quant = 'apex-i-compact'; Vision = $true }
        $plan.Target | Should -Be 'claude'
        $plan.Mode | Should -Be 'native'
        $plan.Quant | Should -Be 'apex-i-compact'
        $plan.Vision | Should -BeTrue
    }

    It 'falls back to hard defaults with no preferences' {
        $plan = Resolve-LaunchPlan -ModelKey 'ornith35hapex' -Def $script:OrnithDef
        $plan.Target | Should -Be 'localpilot'
        $plan.Mode | Should -Be 'native'
        $plan.AutoBestProfile | Should -Be 'auto'
        $plan.UseAutoBest | Should -BeFalse
        $plan.Quant | Should -Be 'apex-i-quality'
    }
}

Describe 'Format-LaunchPlanPanel' {
    It 'renders the plan fields with quant size + fit against a VRAM budget' {
        $plan = Resolve-LaunchPlan -ModelKey 'ornith35hapex' -Def $script:OrnithDef -Defaults $script:DefaultLaunch
        $lines = Format-LaunchPlanPanel -Plan $plan -Def $script:OrnithDef -VramGB 24
        ($lines -join "`n") | Should -Match 'target   LocalPilot'
        ($lines -join "`n") | Should -Match 'quant    apex-i-quality   22\.8 GB   fits'
        ($lines -join "`n") | Should -Match 'mode     turboquant'
        ($lines -join "`n") | Should -Match 'autobest balanced \(on\)'
    }

    It 'marks a quant that does not fit as tight' {
        $plan = Resolve-LaunchPlan -ModelKey 'ornith35hapex' -Def $script:OrnithDef
        $lines = Format-LaunchPlanPanel -Plan $plan -Def $script:OrnithDef -VramGB 16
        ($lines -join "`n") | Should -Match '22\.8 GB   tight'
    }
}

Describe 'Format-BoardModelRow' {
    It 'marks the selected row and truncates long names' {
        $row = Format-BoardModelRow -Model @{ Key = 'ornith35hapex'; Tier = 'experimental'; DisplayName = 'Ornith 1.0 35B heretic APEX GGUF' } -Selected
        $row | Should -Match '^>'
        $row | Should -Match 'ornith35hapex'
    }
    It 'leaves an unselected row unmarked' {
        (Format-BoardModelRow -Model @{ Key = 'x'; Tier = 't'; DisplayName = 'X' }) | Should -Match '^\s'
    }
}

Describe 'Get-LaunchBoardAction' {
    It 'maps launch/quit/move keys' {
        Get-LaunchBoardAction -KeyName 'Enter' | Should -Be 'Launch'
        Get-LaunchBoardAction -KeyName 'Escape' | Should -Be 'Quit'
        Get-LaunchBoardAction -KeyName 'UpArrow' | Should -Be 'MoveUp'
        Get-LaunchBoardAction -KeyChar 'j' | Should -Be 'MoveDown'
    }
    It 'maps field-edit letters, with q = quant (not quit)' {
        Get-LaunchBoardAction -KeyChar 't' | Should -Be 'Edit:Target'
        Get-LaunchBoardAction -KeyChar 'q' | Should -Be 'Edit:Quant'
        Get-LaunchBoardAction -KeyChar 'M' | Should -Be 'Edit:Mode'   # case-insensitive
        Get-LaunchBoardAction -KeyChar '/' | Should -Be 'Search'
    }
    It 'returns empty for an unbound key' {
        Get-LaunchBoardAction -KeyChar 'z' | Should -Be ''
    }
}

Describe 'Launch board render layer' {
    It 'never clears the screen or enters the alternate buffer (scrollback-safe)' {
        $content = Get-Content -Raw $script:LaunchBoardLib
        $content | Should -Not -Match '(?m)^\s*Clear-Host\b'   # a call, not the word in a comment
        $content | Should -Not -Match '\?1049'                 # alt-screen enable
    }
    It 'composes a full frame string with both columns and the legend' {
        $plan = Resolve-LaunchPlan -ModelKey 'ornith35hapex' -Def $script:OrnithDef -Defaults $script:DefaultLaunch
        $models = @(@{ Key = 'ornith35hapex'; Tier = 'exp'; DisplayName = 'Ornith' }, @{ Key = 'other'; Tier = 'q'; DisplayName = 'Other' })
        $frame = Format-LaunchBoardFrame -Models $models -SelectedIndex 0 -Plan $plan -Def $script:OrnithDef -VramGB 24
        $frame | Should -Match 'models'
        $frame | Should -Match 'launch plan'
        $frame | Should -Match '> ornith35hapex'
        $frame | Should -Match 'Enter launch'
    }
}
