# Pester 5 tests for the agent launch safety posture: bypass must be a persisted
# decision that defaults OFF in non-interactive sessions, never a silent default,
# for both the LocalPilot (--bypass) and Codex (--dangerously-bypass-…) paths.
# Also pins the LocalPilot install hint to the real crate name.

BeforeAll {
    $repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path

    . (Join-Path $repoRoot 'local-llm\lib\00-settings.ps1')
    . (Join-Path $repoRoot 'local-llm\lib\10-helpers.ps1')
    . (Join-Path $repoRoot 'local-llm\lib\65-claude-launch.ps1')

    # Set-LocalLLMSetting reloads the live config after writing; the tests assert
    # on the written settings.json directly.
    function Reload-LocalLLMConfig {}

    function New-TempSettingsRoot {
        $root = Join-Path ([System.IO.Path]::GetTempPath()) ("localbox-launch-tests-" + [System.IO.Path]::GetRandomFileName())
        New-Item -ItemType Directory -Path $root | Out-Null
        return $root
    }

    $script:LaunchScriptPath = Join-Path $repoRoot 'local-llm\lib\65-claude-launch.ps1'
}

Describe 'Get-LocalPilotBypassArgs' {
    BeforeEach {
        $script:LLMProfileRoot = New-TempSettingsRoot
        $env:LOCAL_LLM_SETTINGS = Join-Path $script:LLMProfileRoot 'settings.json'
        $env:LOCAL_LLM_LOCALPILOT_BYPASS = $null
        $script:Cfg = @{}
    }

    AfterEach {
        $env:LOCAL_LLM_LOCALPILOT_BYPASS = $null
        $env:LOCAL_LLM_SETTINGS = $null
        Remove-Item -Recurse -Force $script:LLMProfileRoot -ErrorAction SilentlyContinue
    }

    It 'a non-interactive first launch adds no --bypass and persists nothing (fail-closed)' {
        Mock Read-Host { throw [System.Management.Automation.PSInvalidOperationException]::new('non-interactive') }
        @(Get-LocalPilotBypassArgs).Count | Should -Be 0
        Test-Path $env:LOCAL_LLM_SETTINGS | Should -BeFalse
    }

    It 'a persisted true adds --bypass without prompting' {
        Mock Read-Host { throw 'must not prompt' }
        $script:Cfg = @{ LocalPilotBypass = $true }
        @(Get-LocalPilotBypassArgs) | Should -Contain '--bypass'
    }

    It 'a persisted false adds no --bypass without prompting' {
        Mock Read-Host { throw 'must not prompt' }
        $script:Cfg = @{ LocalPilotBypass = $false }
        @(Get-LocalPilotBypassArgs).Count | Should -Be 0
    }

    It 'honors the env override in both directions' {
        $env:LOCAL_LLM_LOCALPILOT_BYPASS = '1'
        @(Get-LocalPilotBypassArgs) | Should -Contain '--bypass'

        $env:LOCAL_LLM_LOCALPILOT_BYPASS = '0'
        $script:Cfg = @{ LocalPilotBypass = $true }
        @(Get-LocalPilotBypassArgs).Count | Should -Be 0
    }

    It 'a first-run yes persists and the next launch does not prompt again' {
        Mock Read-Host { 'y' }
        @(Get-LocalPilotBypassArgs) | Should -Contain '--bypass'
        Should -Invoke Read-Host -Times 1 -Exactly

        $saved = Get-Content -Raw $env:LOCAL_LLM_SETTINGS | ConvertFrom-Json -AsHashtable
        $saved.LocalPilotBypass | Should -BeTrue
        $script:Cfg.LocalPilotBypass | Should -BeTrue
    }

    It 'the read-only preview never prompts and reports off when undecided' {
        Mock Read-Host { throw 'preview must not prompt' }
        @(Get-LocalPilotBypassArgs -NoPrompt).Count | Should -Be 0
    }
}

Describe 'Get-CodexCommonArgs bypass routing (default off, not on)' {
    BeforeEach {
        $script:LLMProfileRoot = New-TempSettingsRoot
        $env:LOCAL_LLM_SETTINGS = Join-Path $script:LLMProfileRoot 'settings.json'
        $env:LOCAL_LLM_CODEX_BYPASS = $null
        $script:Cfg = @{}
    }

    AfterEach {
        $env:LOCAL_LLM_CODEX_BYPASS = $null
        $env:LOCAL_LLM_SETTINGS = $null
        Remove-Item -Recurse -Force $script:LLMProfileRoot -ErrorAction SilentlyContinue
    }

    It 'a non-interactive launch with no config no longer defaults to bypass' {
        Mock Read-Host { throw [System.Management.Automation.PSInvalidOperationException]::new('non-interactive') }
        @(Get-CodexCommonArgs) | Should -Not -Contain '--dangerously-bypass-approvals-and-sandbox'
    }

    It 'adds the dangerous bypass flag only when explicitly enabled' {
        Mock Read-Host { throw 'must not prompt' }
        $script:Cfg = @{ CodexBypassApprovalsAndSandbox = $true }
        @(Get-CodexCommonArgs) | Should -Contain '--dangerously-bypass-approvals-and-sandbox'
    }
}

Describe 'Codex bypass on the real merged config (fresh machine, no settings)' {
    # Unlike the blocks above (which assign $script:Cfg = @{} directly), this loads
    # the *real* merged config through Import-LocalLLMConfig with an empty per-machine
    # settings file — the exact path a fresh install takes. It pins that an unset
    # bypass key resolves to off, so config-load can never silently re-enable the
    # dangerous flag without an explicit decision.
    BeforeEach {
        $script:LLMProfileRoot = (Resolve-Path (Join-Path $repoRoot 'local-llm')).Path
        # The per-user catalog is gitignored; the repo ships the template.
        $script:LocalLLMConfigPath = Join-Path $script:LLMProfileRoot 'llm-models.example.json'
        $script:SettingsRoot = New-TempSettingsRoot
        # A path that does not exist => Import-LocalLLMSettings returns empty (fresh machine).
        $env:LOCAL_LLM_SETTINGS = Join-Path $script:SettingsRoot 'settings.json'
        $env:LOCAL_LLM_CODEX_BYPASS = $null
        $script:Cfg = Import-LocalLLMConfig
    }

    AfterEach {
        $env:LOCAL_LLM_CODEX_BYPASS = $null
        $env:LOCAL_LLM_SETTINGS = $null
        Remove-Item -Recurse -Force $script:SettingsRoot -ErrorAction SilentlyContinue
    }

    It 'a non-interactive launch with no persisted choice adds no dangerous bypass flag' {
        Mock Read-Host { throw [System.Management.Automation.PSInvalidOperationException]::new('non-interactive') }
        @(Get-CodexCommonArgs) | Should -Not -Contain '--dangerously-bypass-approvals-and-sandbox'
    }

    It 'reports Codex bypass undecided (not ON) on a fresh merged config' {
        Get-AgentBypassStatusText -SettingName 'CodexBypassApprovalsAndSandbox' -EnvVar 'LOCAL_LLM_CODEX_BYPASS' |
            Should -Match 'undecided'
    }

    It 'leaves the bypass key absent so the resolver reaches the prompt/fail-closed branch' {
        $script:Cfg.Contains('CodexBypassApprovalsAndSandbox') | Should -BeFalse
    }

    It 'an explicitly persisted true still adds the flag (decision is honoured)' {
        Mock Read-Host { throw 'must not prompt when a choice is persisted' }
        $script:Cfg['CodexBypassApprovalsAndSandbox'] = $true
        @(Get-CodexCommonArgs) | Should -Contain '--dangerously-bypass-approvals-and-sandbox'
    }

    It 'an explicitly persisted false still suppresses the flag' {
        Mock Read-Host { throw 'must not prompt when a choice is persisted' }
        $script:Cfg['CodexBypassApprovalsAndSandbox'] = $false
        @(Get-CodexCommonArgs) | Should -Not -Contain '--dangerously-bypass-approvals-and-sandbox'
    }

    It 'the env override flips the gate decision both ways over the merged config' {
        Mock Read-Host { throw 'env override must not prompt' }
        $env:LOCAL_LLM_CODEX_BYPASS = '1'
        @(Get-CodexCommonArgs) | Should -Contain '--dangerously-bypass-approvals-and-sandbox'

        $env:LOCAL_LLM_CODEX_BYPASS = '0'
        $script:Cfg['CodexBypassApprovalsAndSandbox'] = $true
        @(Get-CodexCommonArgs) | Should -Not -Contain '--dangerously-bypass-approvals-and-sandbox'
    }
}

Describe 'LocalPilot install hint' {
    It 'points at the cargo install localpilot crate, not localpilot-cli' {
        $content = Get-Content -Raw $script:LaunchScriptPath
        $content | Should -Match 'cargo install localpilot(?!-cli)'
        $content | Should -Not -Match 'cargo install localpilot-cli'
    }
}

Describe 'New-LocalPilotBaseConfigToml' {
    It 'pins the model so the default REPL can resolve it from config' {
        # The REPL is the default (no-arg) command and reads its model from config;
        # without a model line it falls back to a doctor dump instead of starting.
        $toml = New-LocalPilotBaseConfigToml -ProviderKind 'anthropic' `
            -BaseUrl 'http://127.0.0.1:11435/v1' -ApiKeyEnv 'ANTHROPIC_AUTH_TOKEN' `
            -Model 'ornith35hapex'
        $toml | Should -Match '(?m)^model = "ornith35hapex"$'
    }

    It 'omits the model line when no model is given' {
        $toml = New-LocalPilotBaseConfigToml -ProviderKind 'anthropic' `
            -BaseUrl 'http://127.0.0.1:11435/v1' -ApiKeyEnv 'ANTHROPIC_AUTH_TOKEN'
        $toml | Should -Not -Match '(?m)^model = '
    }
}

Describe 'Start-LocalPilot invocation (no dead chat subcommand)' {
    It 'never invokes the removed `localpilot chat --model` form' {
        # Current LocalPilot has no `chat` subcommand; the REPL is the default
        # command and takes model/permissions from config, not argv.
        $content = Get-Content -Raw $script:LaunchScriptPath
        $content | Should -Not -Match "'chat', '--model'"
    }
}
