# Pester 5 tests for Get-LocalModelPermissionArgs: permission skipping must be
# a persisted decision, never a silent inheritance. New installs (no env var,
# no config key) prompt once, default No, and persist the answer.

BeforeAll {
    $repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path

    . (Join-Path $repoRoot 'local-llm\lib\00-settings.ps1')
    . (Join-Path $repoRoot 'local-llm\lib\10-helpers.ps1')

    # Set-LocalLLMSetting reloads the live config after writing; the tests
    # assert on the written settings.json directly.
    function Reload-LocalLLMConfig {}

    function New-TempSettingsRoot {
        $root = Join-Path ([System.IO.Path]::GetTempPath()) ("localbox-perm-tests-" + [System.IO.Path]::GetRandomFileName())
        New-Item -ItemType Directory -Path $root | Out-Null
        return $root
    }
}

Describe 'Get-LocalModelPermissionArgs' {
    BeforeEach {
        $script:LLMProfileRoot = New-TempSettingsRoot
        $env:LOCAL_LLM_SETTINGS = Join-Path $script:LLMProfileRoot 'settings.json'
        $env:LOCAL_LLM_SKIP_PERMISSIONS = $null
        $script:Cfg = @{}
    }

    AfterEach {
        $env:LOCAL_LLM_SKIP_PERMISSIONS = $null
        $env:LOCAL_LLM_SETTINGS = $null
        Remove-Item -Recurse -Force $script:LLMProfileRoot -ErrorAction SilentlyContinue
    }

    It 'honors the env var over everything, in both directions' {
        $script:Cfg = @{ LocalModelSkipPermissions = $false }
        $env:LOCAL_LLM_SKIP_PERMISSIONS = '1'
        @(Get-LocalModelPermissionArgs) | Should -Contain '--dangerously-skip-permissions'

        $script:Cfg = @{ LocalModelSkipPermissions = $true }
        $env:LOCAL_LLM_SKIP_PERMISSIONS = '0'
        @(Get-LocalModelPermissionArgs).Count | Should -Be 0
    }

    It 'uses a persisted true without prompting' {
        Mock Read-Host { throw 'must not prompt' }
        $script:Cfg = @{ LocalModelSkipPermissions = $true }
        @(Get-LocalModelPermissionArgs) | Should -Contain '--dangerously-skip-permissions'
    }

    It 'uses a persisted false without prompting' {
        Mock Read-Host { throw 'must not prompt' }
        $script:Cfg = @{ LocalModelSkipPermissions = $false }
        @(Get-LocalModelPermissionArgs).Count | Should -Be 0
    }

    Context 'first run (no env var, no config key)' {
        It 'prompts, defaults to keeping permission prompts on, and persists the No' {
            Mock Read-Host { '' }
            @(Get-LocalModelPermissionArgs).Count | Should -Be 0

            $saved = Get-Content -Raw $env:LOCAL_LLM_SETTINGS | ConvertFrom-Json -AsHashtable
            $saved.LocalModelSkipPermissions | Should -BeFalse
        }

        It 'persists a yes and skips from then on' {
            Mock Read-Host { 'y' }
            @(Get-LocalModelPermissionArgs) | Should -Contain '--dangerously-skip-permissions'

            $saved = Get-Content -Raw $env:LOCAL_LLM_SETTINGS | ConvertFrom-Json -AsHashtable
            $saved.LocalModelSkipPermissions | Should -BeTrue
            $script:Cfg.LocalModelSkipPermissions | Should -BeTrue
        }

        It 'asks only once: the persisted answer short-circuits the next launch' {
            Mock Read-Host { 'n' }
            Get-LocalModelPermissionArgs | Out-Null
            Should -Invoke Read-Host -Times 1 -Exactly

            Get-LocalModelPermissionArgs | Out-Null
            Should -Invoke Read-Host -Times 1 -Exactly
        }

        It 'keeps prompts on without persisting when the session cannot ask' {
            Mock Read-Host { throw [System.Management.Automation.PSInvalidOperationException]::new('non-interactive') }
            @(Get-LocalModelPermissionArgs).Count | Should -Be 0
            Test-Path $env:LOCAL_LLM_SETTINGS | Should -BeFalse
        }
    }
}

Describe 'Set-LocalLLMSetting boolean persistence' {
    BeforeEach {
        $script:LLMProfileRoot = New-TempSettingsRoot
        $env:LOCAL_LLM_SETTINGS = Join-Path $script:LLMProfileRoot 'settings.json'
    }

    AfterEach {
        $env:LOCAL_LLM_SETTINGS = $null
        Remove-Item -Recurse -Force $script:LLMProfileRoot -ErrorAction SilentlyContinue
    }

    It 'persists a literal $false instead of unsetting the key' {
        Set-LocalLLMSetting LocalModelSkipPermissions $false
        $saved = Get-Content -Raw $env:LOCAL_LLM_SETTINGS | ConvertFrom-Json -AsHashtable
        $saved.Contains('LocalModelSkipPermissions') | Should -BeTrue
        $saved.LocalModelSkipPermissions | Should -BeFalse
    }

    It 'still unsets on $null and empty string' {
        Set-LocalLLMSetting SomeKey 'value'
        Set-LocalLLMSetting SomeKey ''
        Test-Path $env:LOCAL_LLM_SETTINGS | Should -BeFalse
    }
}
