BeforeAll {
    $repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
    function Write-Section { param([string]$Title) }
    . (Join-Path $repoRoot 'local-llm\lib\91-launch-board.ps1')
    . (Join-Path $repoRoot 'local-llm\lib\92-launch-board-loop.ps1')
    $script:LoopLib = Join-Path $repoRoot 'local-llm\lib\92-launch-board-loop.ps1'
}

Describe 'Test-LaunchBoardCapable' {
    AfterEach { Remove-Item Env:LOCALBOX_NO_BOARD -ErrorAction SilentlyContinue }

    It 'forces the fallback when LOCALBOX_NO_BOARD=1' {
        $env:LOCALBOX_NO_BOARD = '1'
        Test-LaunchBoardCapable | Should -BeFalse
    }
}

Describe 'Build-LaunchSelectionArgs' {
    It 'maps a full plan to the Invoke-LLMSelection parameter set' {
        $plan = [pscustomobject]@{
            ModelKey = 'ornith35hapex'; Target = 'localpilot'; Quant = 'apex-i-quality'
            ContextKey = '256k'; Mode = 'turboquant'; AutoBestProfile = 'balanced'
            UseAutoBest = $true; Vision = $true; Strict = $true
        }
        $a = Build-LaunchSelectionArgs -Plan $plan
        $a.ModelKey | Should -Be 'ornith35hapex'
        $a.Action | Should -Be 'localpilot'
        $a.LlamaCppMode | Should -Be 'turboquant'
        $a.Quant | Should -Be 'apex-i-quality'
        $a.ContextKey | Should -Be '256k'
        $a.UseAutoBest | Should -BeTrue
        $a.AutoBestProfile | Should -Be 'balanced'
        $a.Strict | Should -BeTrue
        $a.UseVision | Should -BeTrue
    }

    It 'omits switch args and autobest profile when off' {
        $plan = [pscustomobject]@{
            ModelKey = 'm'; Target = 'claude'; Quant = ''; ContextKey = ''
            Mode = 'native'; AutoBestProfile = 'auto'; UseAutoBest = $false; Vision = $false; Strict = $false
        }
        $a = Build-LaunchSelectionArgs -Plan $plan
        $a.ContainsKey('Quant') | Should -BeFalse
        $a.ContainsKey('Strict') | Should -BeFalse
        $a.ContainsKey('UseVision') | Should -BeFalse
        $a.ContainsKey('UseAutoBest') | Should -BeFalse
        $a.ContainsKey('AutoBestProfile') | Should -BeFalse
    }
}

Describe 'Launch board loop layer' {
    It 'never clears the screen or enters the alternate buffer' {
        $content = Get-Content -Raw $script:LoopLib
        $content | Should -Not -Match '(?m)^\s*Clear-Host\b'
        $content | Should -Not -Match '\?1049'
    }
}
