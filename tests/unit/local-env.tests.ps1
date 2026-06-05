BeforeAll {
    $repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
    $script:LLMProfileRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("localbox-tests-" + [System.IO.Path]::GetRandomFileName())
    New-Item -ItemType Directory -Path $script:LLMProfileRoot | Out-Null
    $script:LocalLLMConfigPath = Join-Path $script:LLMProfileRoot 'llm-models.json'
    $script:Cfg = @{ LocalModelMaxOutputTokens = 4096 }
    $script:NoThinkProxyPort = 11435

    . (Join-Path $repoRoot 'local-llm\lib\00-settings.ps1')
    . (Join-Path $repoRoot 'local-llm\lib\65-claude-launch.ps1')
    . (Join-Path $repoRoot 'local-llm\lib\75-display.ps1')

    function Write-LaunchLog { param([string]$Message, [string]$Level) }
}

AfterAll {
    Remove-Item -Recurse -Force $script:LLMProfileRoot -ErrorAction SilentlyContinue
}

Describe 'Local Claude environment' {
    It 'disables beta tool shapes and ToolSearch for local proxy-compatible launches' {
        Set-ClaudeLocalEnv -BaseUrl 'http://localhost:11435' -Model 'local-test'

        $env:CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS | Should -Be '1'
        $env:ENABLE_TOOL_SEARCH | Should -Be 'false'
    }

    It 'restores local-only env vars after launch cleanup' {
        $env:CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS = 'original-beta'
        $env:ENABLE_TOOL_SEARCH = 'auto'
        $env:API_TIMEOUT_MS = '123'
        $env:CLAUDE_CODE_DISABLE_AUTO_MEMORY = '0'

        Save-ClaudeEnvBackup
        Set-ClaudeLocalEnv -BaseUrl 'http://localhost:11435' -Model 'local-test'
        Restore-ClaudeEnvBackup

        $env:CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS | Should -Be 'original-beta'
        $env:ENABLE_TOOL_SEARCH | Should -Be 'auto'
        $env:API_TIMEOUT_MS | Should -Be '123'
        $env:CLAUDE_CODE_DISABLE_AUTO_MEMORY | Should -Be '0'
    }

    It 'shows the beta/tool-search kill switches in dry-run env snapshots' {
        $snapshot = Get-LocalLLMClaudeEnvSnapshot -BaseUrl 'http://localhost:11435' -Model 'local-test'

        $snapshot.CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS | Should -Be '1'
        $snapshot.ENABLE_TOOL_SEARCH | Should -Be 'false'
    }

    It 'emits CLAUDE_LOCAL_MAX_IMAGES when a model raises the image cap' {
        Set-ClaudeLocalEnv -BaseUrl 'http://localhost:11435' -Model 'local-test' -MaxImagesPerRequest 4

        $env:CLAUDE_LOCAL_MAX_IMAGES | Should -Be '4'
    }

    It 'leaves CLAUDE_LOCAL_MAX_IMAGES unset when the model does not raise the cap' {
        Remove-Item Env:CLAUDE_LOCAL_MAX_IMAGES -ErrorAction SilentlyContinue
        Set-ClaudeLocalEnv -BaseUrl 'http://localhost:11435' -Model 'local-test'

        $env:CLAUDE_LOCAL_MAX_IMAGES | Should -BeNullOrEmpty
    }

    It 'mirrors the image cap in dry-run env snapshots only when raised' {
        $raised = Get-LocalLLMClaudeEnvSnapshot -BaseUrl 'http://localhost:11435' -Model 'local-test' -MaxImagesPerRequest 4
        $raised.CLAUDE_LOCAL_MAX_IMAGES | Should -Be '4'

        $default = Get-LocalLLMClaudeEnvSnapshot -BaseUrl 'http://localhost:11435' -Model 'local-test'
        $default.Contains('CLAUDE_LOCAL_MAX_IMAGES') | Should -BeFalse
    }

    It 'does not advertise ToolSearch in inline deferred schemas' {
        $prompt = Get-LocalModelSystemPrompt -IncludeInlineToolSchemas

        $prompt | Should -Not -Match 'ToolSearch'
    }

    It 'tells local models to force text for PowerShell object projections' {
        $prompt = Get-LocalModelSystemPrompt

        $prompt | Should -Match 'Select-Object'
        $prompt | Should -Match 'Out-String -Width 4096'
    }
}

Describe 'Local response smoke classification' {
    It 'rejects repeated punctuation floods and empty-output markers' {
        Test-LocalDegenerateResponseText -Text ('/' * 32) | Should -BeTrue
        Test-LocalDegenerateResponseText -Text '[no output]' | Should -BeTrue
    }

    It 'rejects repeated token loops' {
        Test-LocalDegenerateResponseText -Text (('again ' * 12).Trim()) | Should -BeTrue
    }

    It 'accepts a normal visible response' {
        Test-LocalDegenerateResponseText -Text 'pong' | Should -BeFalse
    }
}
