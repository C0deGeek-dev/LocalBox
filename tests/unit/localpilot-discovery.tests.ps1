BeforeAll {
    $repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
    $script:LLMProfileRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("localbox-tests-" + [System.IO.Path]::GetRandomFileName())
    New-Item -ItemType Directory -Path $script:LLMProfileRoot | Out-Null

    . (Join-Path $repoRoot 'local-llm\lib\00-settings.ps1')

    # Sandbox layout: <root>\workspace\LocalBox plus an optional sibling
    # checkout at <root>\workspace\LocalPilot, and a fake managed tools dir.
    $script:Sandbox = Join-Path ([System.IO.Path]::GetTempPath()) ("localbox-lp-disc-" + [System.IO.Path]::GetRandomFileName())
    $script:WorkspaceRoot = Join-Path $script:Sandbox 'workspace'
    $script:FakeLocalBoxRoot = Join-Path $script:WorkspaceRoot 'LocalBox'
    $script:SiblingRoot = Join-Path $script:WorkspaceRoot 'LocalPilot'
    New-Item -ItemType Directory -Path $script:FakeLocalBoxRoot -Force | Out-Null

    $script:ManagedDefault = Join-Path $HOME '.local-llm\tools\localpilot'
}

AfterAll {
    Remove-Item -Recurse -Force $script:Sandbox -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force $script:LLMProfileRoot -ErrorAction SilentlyContinue
}

Describe 'LocalPilot checkout discovery' {
    It 'prefers a sibling checkout next to the LocalBox repo when it looks like a Rust checkout' {
        New-Item -ItemType Directory -Path $script:SiblingRoot -Force | Out-Null
        Set-Content -Path (Join-Path $script:SiblingRoot 'Cargo.toml') -Value '[workspace]'

        Find-LocalLLMLocalPilotRoot -LocalBoxRoot $script:FakeLocalBoxRoot | Should -Be $script:SiblingRoot
    }

    It 'ignores a sibling directory that is not a Rust checkout' {
        Remove-Item -Recurse -Force $script:SiblingRoot -ErrorAction SilentlyContinue
        New-Item -ItemType Directory -Path $script:SiblingRoot -Force | Out-Null

        Find-LocalLLMLocalPilotRoot -LocalBoxRoot $script:FakeLocalBoxRoot | Should -Be $script:ManagedDefault
    }

    It 'falls back to the managed tools dir when no sibling exists' {
        Remove-Item -Recurse -Force $script:SiblingRoot -ErrorAction SilentlyContinue

        Find-LocalLLMLocalPilotRoot -LocalBoxRoot $script:FakeLocalBoxRoot | Should -Be $script:ManagedDefault
    }

    It 'falls back to the managed tools dir when LocalBoxRoot is unknown' {
        Find-LocalLLMLocalPilotRoot -LocalBoxRoot '' | Should -Be $script:ManagedDefault
    }

    It 'never returns a hardcoded machine-local path' {
        $resolved = Find-LocalLLMLocalPilotRoot -LocalBoxRoot ''
        $resolved | Should -Not -Match '^[A-Z]:\\repos\\'
    }
}
