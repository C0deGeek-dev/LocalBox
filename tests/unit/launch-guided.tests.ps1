BeforeAll {
    $repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
    . (Join-Path $repoRoot 'local-llm\lib\91-launch-board.ps1')
    . (Join-Path $repoRoot 'local-llm\lib\93-launch-guided.ps1')
    $script:GuidedLib = Join-Path $repoRoot 'local-llm\lib\93-launch-guided.ps1'

    $script:Def = @{
        DisplayName  = 'Ornith 1.0 35B heretic APEX GGUF'
        Quant        = 'apex-i-quality'
        Quants       = @{ 'apex-i-quality' = 'a.gguf'; 'apex-i-compact' = 'b.gguf' }
        QuantSizesGB = @{ 'apex-i-quality' = 22.8; 'apex-i-compact' = 16.5 }
        Contexts     = @{ '' = 16384; '256k' = 262144 }
    }
}

Describe 'Plain-language vocabulary' {
    It 'names run targets in plain words' {
        Get-GuidedTargetLabel -Value 'localpilot' | Should -Be 'LocalPilot (recommended)'
        Get-GuidedTargetLabel -Value 'serve' | Should -Be 'Share to other apps'
    }
    It 'names engines in plain words' {
        Get-GuidedEngineLabel -Value 'turboquant' | Should -Match 'Turbo'
        Get-GuidedEngineLabel -Value 'native' | Should -Be 'Standard'
    }
    It 'describes memory as standard vs large' {
        Get-GuidedMemoryLabel -Def $script:Def -ContextKey '' | Should -Match '^Standard'
        Get-GuidedMemoryLabel -Def $script:Def -ContextKey '256k' | Should -Match '^Large'
    }
    It 'describes quality with a plain hint and size' {
        Get-GuidedQualityLabel -Def $script:Def -Quant 'apex-i-quality' | Should -Match 'best quality .* 22\.8 GB'
        Get-GuidedQualityLabel -Def $script:Def -Quant 'apex-i-compact' | Should -Match 'smaller & faster .* 16\.5 GB'
    }
}

Describe 'Format-GuidedPlanSummary' {
    It 'renders the plan in friendly words with no raw jargon surfaced' {
        $plan = Resolve-LaunchPlan -ModelKey 'ornith35hapex' -Def $script:Def -Defaults @{ Action = 'localpilot'; LlamaCppMode = 'turboquant'; UseAutoBest = $true }
        $summary = Format-GuidedPlanSummary -Plan $plan -Def $script:Def
        $summary | Should -Match 'Run with:  LocalPilot'
        $summary | Should -Match 'Quality:'
        $summary | Should -Match 'Memory:'
        $summary | Should -Match 'Speed:.*Turbo'
        # No developer field names on the surface.
        $summary | Should -Not -Match 'quant'
        $summary | Should -Not -Match 'AutoBest'
        $summary | Should -Not -Match 'turboquant'
    }
}

Describe 'Get-GuidedGlossary' {
    It 'explains the choices in plain language' {
        $g = Get-GuidedGlossary
        $g | Should -Match 'Run with'
        $g | Should -Match 'graphics memory'
        $g | Should -Match 'Launch now'
    }
}

Describe 'Guided launcher hygiene' {
    It 'does not Clear-Host or use the Spectre transition cooldown' {
        $content = Get-Content -Raw $script:GuidedLib
        $content | Should -Not -Match '(?m)^\s*Clear-Host\b'
        $content | Should -Not -Match 'Invoke-LLMSpectreTransitionCooldown'
    }
}
