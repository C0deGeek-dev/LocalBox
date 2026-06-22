# Pester 5 tests for the headless serve path: the argv preview must tolerate the
# nested empty extra-args element, and `llmdefaultserve -DryRun` must render a plan
# without ever starting a llama-server.

BeforeAll {
    $script:repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
    $env:LOCALBOX_SKIP_PROXY_CHECK = '1'
}

Describe 'Format-LocalLLMArgvLine' {
    BeforeAll {
        . (Join-Path $repoRoot 'local-llm\lib\75-display.ps1')
    }

    It 'flattens a nested empty extra-args element instead of failing to bind' {
        # An empty extra-args list arrives as a nested empty array (helpers return
        # `,$extras`); the preview must not choke on it (regression for the
        # "Cannot bind argument to parameter 'Argv'" DryRun error).
        $argv = @('localpilot') + @('chat', '--model', 'm') + (, @())
        { Format-LocalLLMArgvLine -Argv $argv } | Should -Not -Throw
        Format-LocalLLMArgvLine -Argv $argv | Should -Be 'localpilot chat --model m'
    }

    It 'quotes tokens containing whitespace' {
        Format-LocalLLMArgvLine -Argv @('a', 'b c') | Should -Be 'a "b c"'
    }
}

Describe 'llmdefaultserve' {
    BeforeAll {
        $script:SettingsRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("localbox-serve-tests-" + [System.IO.Path]::GetRandomFileName())
        New-Item -ItemType Directory -Path $script:SettingsRoot | Out-Null
        # A non-existent settings file => no DefaultLaunch recipe, so the shortcut
        # resolves the catalog default model (fresh-machine path).
        $env:LOCAL_LLM_SETTINGS = Join-Path $script:SettingsRoot 'settings.json'
        . (Join-Path $repoRoot 'local-llm\LocalLLMProfile.ps1') *> $null
    }

    AfterAll {
        $env:LOCAL_LLM_SETTINGS = $null
        Remove-Item -Recurse -Force $script:SettingsRoot -ErrorAction SilentlyContinue
    }

    It 'is registered and distinct from the LAN serve gateway llmserve' {
        Get-Command llmdefaultserve -ErrorAction SilentlyContinue | Should -Not -BeNullOrEmpty
    }

    It 'a dry run renders a plan and starts no llama-server or proxy' {
        # Drive the worker with an explicit catalog key so the test does not depend
        # on a resolvable machine default. DryRun must render the plan and spawn
        # nothing.
        Mock Start-LlamaServerNative { throw 'a dry run must not start a server' }
        Mock Start-NoThinkProxy { throw 'a dry run must not start the proxy' }

        # `*>&1` so the Write-Host plan (Information stream) is captured too.
        $output = (Start-LocalLLMHeadlessServe -Key 'q3635ba3bapex' -ContextKey '' -Mode 'native' -DryRun *>&1 | Out-String)

        Should -Invoke Start-LlamaServerNative -Times 0 -Exactly
        Should -Invoke Start-NoThinkProxy -Times 0 -Exactly
        $output | Should -Match 'headless serve'
        $output | Should -Match 'No agent is attached'
    }
}
