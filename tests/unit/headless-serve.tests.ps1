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

Describe 'llmdefaultserve recipe parity (selected quant)' {
    BeforeAll {
        $script:ParityRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("localbox-parity-tests-" + [System.IO.Path]::GetRandomFileName())
        New-Item -ItemType Directory -Path $script:ParityRoot | Out-Null
        $env:LOCAL_LLM_SETTINGS = Join-Path $script:ParityRoot 'settings.json'
        . (Join-Path $repoRoot 'local-llm\LocalLLMProfile.ps1') *> $null

        # A DefaultLaunch recipe that selects the non-default quant, so the
        # dry-run-vs-live divergence the fix closes is exercised. The catalog model
        # defaults to 'apex-balanced'; the recipe asks for 'apex-i-quality'.
        $script:DefaultQuant = (Get-ModelDef -Key 'q3635ba3bapex').Quant
        $script:Cfg.DefaultLaunch = @{ ModelKey = 'q3635ba3bapex'; ContextKey = ''; Quant = 'apex-i-quality'; LlamaCppMode = 'turboquant' }
    }

    AfterAll {
        # Restore the shared catalog quant so a live-path test cannot leak state.
        (Get-ModelDef -Key 'q3635ba3bapex').Quant = $script:DefaultQuant
        $env:LOCAL_LLM_SETTINGS = $null
        Remove-Item -Recurse -Force $script:ParityRoot -ErrorAction SilentlyContinue
    }

    It 'the selected quant differs from the catalog default (the divergence exists)' {
        $script:DefaultQuant | Should -Not -Be 'apex-i-quality'
    }

    It '-DryRun previews the selected quant and reverts it (commits no session state)' {
        # Capture the quant the launch would consume; the preview must see the
        # selected quant (parity), then the shared catalog quant must be restored.
        Mock Start-LocalLLMHeadlessServe { $script:capturedDryRunQuant = (Get-ModelDef -Key 'q3635ba3bapex').Quant }
        $before = (Get-ModelDef -Key 'q3635ba3bapex').Quant

        llmdefaultserve -DryRun *> $null

        $script:capturedDryRunQuant | Should -Be 'apex-i-quality'
        (Get-ModelDef -Key 'q3635ba3bapex').Quant | Should -Be $before
    }

    It 'a live launch applies the same selected quant' {
        Mock Start-LocalLLMHeadlessServe { $script:capturedLiveQuant = (Get-ModelDef -Key 'q3635ba3bapex').Quant }

        llmdefaultserve *> $null

        $script:capturedLiveQuant | Should -Be 'apex-i-quality'
        # The live path commits the quant; restore it for isolation.
        (Get-ModelDef -Key 'q3635ba3bapex').Quant = $script:DefaultQuant
    }
}

Describe 'Get-LocalLLMServeHealthState (stale-proxy diagnostic)' {
    BeforeAll {
        . (Join-Path $repoRoot 'local-llm\lib\32-llamacpp.ps1')
        . (Join-Path $repoRoot 'local-llm\lib\65-claude-launch.ps1')
    }

    It 'flags a stale proxy (up) with a down upstream and recommends the restart' {
        # Proxy answers /health (up); the upstream port is bindable (free => nothing
        # listening => down). This is the bare-502 condition.
        Mock Get-NoThinkProxyHealth { @{ status = 'ok'; target_host = '127.0.0.1'; target_port = 8080 } }
        Mock Test-LlamaCppPortFree { $true }

        $health = Get-LocalLLMServeHealthState -ProxyPort 11435 -UpstreamPort 8080

        $health.State | Should -Be 'stale-proxy'
        $health.ProxyUp | Should -BeTrue
        $health.UpstreamUp | Should -BeFalse
        $health.Recommendation | Should -Match 'llmstop; llmdefaultserve'
    }

    It 'reports a healthy stack with no recommendation' {
        Mock Get-NoThinkProxyHealth { @{ status = 'ok' } }
        Mock Test-LlamaCppPortFree { $false }   # upstream port in use => server up

        $health = Get-LocalLLMServeHealthState -ProxyPort 11435 -UpstreamPort 8080

        $health.State | Should -Be 'ok'
        $health.Recommendation | Should -BeNullOrEmpty
    }

    It 'reports a fully down stack' {
        Mock Get-NoThinkProxyHealth { $null }   # proxy not answering
        Mock Test-LlamaCppPortFree { $true }    # upstream free => down

        $health = Get-LocalLLMServeHealthState -ProxyPort 11435 -UpstreamPort 8080

        $health.State | Should -Be 'down'
        $health.Recommendation | Should -Match 'llmdefaultserve'
    }
}
