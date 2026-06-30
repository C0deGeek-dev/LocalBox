# Pester 5 tests for the LocalPilot agent-launch vision wiring: the projector
# resolver (Resolve-LocalPilotVisionModule), the auto-declared .localpilot.toml
# head (New-LocalPilotBaseConfigToml), and that a resolved projector reaches
# llama-server as --mmproj. Vision stays opt-in: the default path loads no
# projector and declares nothing.

BeforeAll {
    $repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path

    . (Join-Path $repoRoot 'local-llm\lib\00-settings.ps1')
    . (Join-Path $repoRoot 'local-llm\lib\10-helpers.ps1')
    . (Join-Path $repoRoot 'local-llm\lib\40-parsers.ps1')
    . (Join-Path $repoRoot 'local-llm\lib\41-llamacpp-args.ps1')
    . (Join-Path $repoRoot 'local-llm\lib\42-llamacpp-templates.ps1')
    . (Join-Path $repoRoot 'local-llm\lib\65-claude-launch.ps1')

    # Stubs so the resolver's model-layer dependencies can be mocked without
    # dot-sourcing the whole catalog/HuggingFace stack (their real bodies never run).
    function Test-ModelVisionModuleAvailable { param($Key, $Def) }
    function Get-ModelVisionModulePath { param($Key, $Def) }
    function Get-ModelFolder { param($Def) }
    function Resolve-HuggingFaceLocalPath { param($DestinationFolder, $FileName) }
}

Describe 'Resolve-LocalPilotVisionModule' {
    It 'returns empty when vision is not opted in (default agent launch)' {
        Mock Test-ModelVisionModuleAvailable { throw 'must not probe availability when vision is off' }
        Resolve-LocalPilotVisionModule -Key 'm' -Def @{} | Should -BeNullOrEmpty
        Should -Invoke Test-ModelVisionModuleAvailable -Times 0 -Exactly
    }

    It 'returns empty when no projector is available (guarded, not a broken launch)' {
        Mock Test-ModelVisionModuleAvailable { @{ Local = $false; AvailableOnHF = $false; Filename = '' } }
        Mock Get-ModelVisionModulePath { throw 'must not download when nothing is available' }
        $p = Resolve-LocalPilotVisionModule -Key 'm' -Def @{} -UseVision -WarningAction SilentlyContinue
        $p | Should -BeNullOrEmpty
        Should -Invoke Get-ModelVisionModulePath -Times 0 -Exactly
    }

    It 'resolves the expected projector path on a DryRun without downloading' {
        Mock Test-ModelVisionModuleAvailable { @{ Local = $true; AvailableOnHF = $false; Filename = 'mmproj-x.gguf' } }
        Mock Get-ModelFolder { 'C:\models\m' }
        Mock Resolve-HuggingFaceLocalPath { Join-Path $DestinationFolder $FileName }
        Mock Get-ModelVisionModulePath { throw 'must not download on a DryRun preview' }
        $p = Resolve-LocalPilotVisionModule -Key 'm' -Def @{} -UseVision -DryRun
        $p | Should -Be 'C:\models\m\mmproj-x.gguf'
        Should -Invoke Get-ModelVisionModulePath -Times 0 -Exactly
    }

    It 'downloads/resolves the projector on a real opted-in launch' {
        Mock Test-ModelVisionModuleAvailable { @{ Local = $false; AvailableOnHF = $true; Filename = 'mmproj-x.gguf' } }
        Mock Get-ModelVisionModulePath { 'C:\models\m\mmproj-x.gguf' }
        Resolve-LocalPilotVisionModule -Key 'm' -Def @{} -UseVision -WarningAction SilentlyContinue |
            Should -Be 'C:\models\m\mmproj-x.gguf'
    }
}

Describe 'New-LocalPilotBaseConfigToml' {
    It 'auto-declares supports_vision = true only when the projector loaded' {
        $withVision = New-LocalPilotBaseConfigToml `
            -ProviderKind 'openai-compatible' `
            -BaseUrl 'http://127.0.0.1:8080/v1' `
            -ApiKeyEnv 'LOCALPILOT_LOCAL_API_KEY' `
            -SupportsVision
        $withVision | Should -Match '\[providers\.local\]'
        $withVision | Should -Match 'supports_vision = true'
    }

    It 'writes no supports_vision on the default (text-only) path' {
        $noVision = New-LocalPilotBaseConfigToml `
            -ProviderKind 'openai-compatible' `
            -BaseUrl 'http://127.0.0.1:8080/v1' `
            -ApiKeyEnv 'LOCALPILOT_LOCAL_API_KEY'
        $noVision | Should -Match '\[providers\.local\]'
        $noVision | Should -Not -Match 'supports_vision'
    }
}

Describe 'Build-LlamaServerArgs vision wiring' {
    BeforeAll {
        $script:Cfg = @{}
        # Isolate the --mmproj branch from KV-type, template, reasoning and sampler
        # resolution — each has its own coverage. `,@()` returns a real empty array
        # (not $null) so the `-Lines` binding the builder performs still succeeds.
        Mock Get-LlamaCppKvTypes { @{ K = 'f16'; V = 'f16' } }
        Mock Test-LlamaCppKvType { }
        Mock Resolve-LlamaCppChatTemplate { @() }
        Mock Get-LlamaCppReasoningArgs { @() }
        Mock Get-ParserLines { , @() }
        Mock ConvertFrom-OllamaParameter { @() }
    }

    It 'emits --mmproj <path> when a projector path is set' {
        $argv = Build-LlamaServerArgs -Def @{} -ContextKey '' -Mode 'native' `
            -ModelArgPath 'C:\models\m\model.gguf' -Port 8080 `
            -VisionModulePath 'C:\models\m\mmproj-x.gguf'
        $argv | Should -Contain '--mmproj'
        $argv | Should -Contain 'C:\models\m\mmproj-x.gguf'
    }

    It 'omits --mmproj on the default text-only launch (empty path)' {
        $argv = Build-LlamaServerArgs -Def @{} -ContextKey '' -Mode 'native' `
            -ModelArgPath 'C:\models\m\model.gguf' -Port 8080 `
            -VisionModulePath ''
        $argv | Should -Not -Contain '--mmproj'
    }
}
