# Pester 5 tests for the CPU embedding server recipe (local-llm/lib/86-embed-serve.ps1).
# The served llama-server command is a recorded contract: these pin the exact
# argv (CPU-only `-ngl 0`, `--embeddings`, `--pooling last`) and that a dry run
# renders a plan without acquiring a model or starting a server.

BeforeAll {
    $script:repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
    # 75-display provides Format-LocalLLMArgvLine (pure, no settings dependency),
    # used by the dry-run preview; the embed-serve module is the unit under test.
    . (Join-Path $repoRoot 'local-llm\lib\75-display.ps1')
    . (Join-Path $repoRoot 'local-llm\lib\86-embed-serve.ps1')
}

Describe 'Get-LocalLLMEmbedServerArgs' {
    It 'forces the model onto the CPU (-ngl 0) and enables embeddings' {
        $argv = Get-LocalLLMEmbedServerArgs -ModelPath 'C:\m\embed.gguf' -Port 8090
        # -ngl 0 is the load-bearing arm-fairness flag: zero GPU layers => no VRAM.
        $i = [array]::IndexOf($argv, '-ngl')
        $i | Should -BeGreaterOrEqual 0
        $argv[$i + 1] | Should -Be '0'
        $argv | Should -Contain '--embeddings'
    }

    It 'serves the requested model and port on loopback' {
        $argv = Get-LocalLLMEmbedServerArgs -ModelPath 'C:\m\embed.gguf' -Port 8091
        $mi = [array]::IndexOf($argv, '-m'); $argv[$mi + 1] | Should -Be 'C:\m\embed.gguf'
        $pi = [array]::IndexOf($argv, '--port'); $argv[$pi + 1] | Should -Be '8091'
        $hi = [array]::IndexOf($argv, '--host'); $argv[$hi + 1] | Should -Be '127.0.0.1'
    }

    It 'defaults pooling to last (required by Qwen3-Embedding) and is overridable' {
        $last = Get-LocalLLMEmbedServerArgs -ModelPath 'm.gguf' -Port 8090
        $pi = [array]::IndexOf($last, '--pooling'); $last[$pi + 1] | Should -Be 'last'

        $mean = Get-LocalLLMEmbedServerArgs -ModelPath 'm.gguf' -Port 8090 -Pooling 'mean'
        $pj = [array]::IndexOf($mean, '--pooling'); $mean[$pj + 1] | Should -Be 'mean'
    }

    It 'omits --pooling when explicitly blanked' {
        $argv = Get-LocalLLMEmbedServerArgs -ModelPath 'm.gguf' -Port 8090 -Pooling ''
        $argv | Should -Not -Contain '--pooling'
    }
}

Describe 'Resolve-LocalLLMEmbedDefaults' {
    It 'defaults to the license-cleared Qwen3-Embedding-0.6B Q8_0 on port 8090' {
        # No $script:Cfg overrides in this fixture => the hardcoded defaults.
        $d = Resolve-LocalLLMEmbedDefaults
        $d.Repo | Should -Be 'Qwen/Qwen3-Embedding-0.6B-GGUF'
        $d.File | Should -Be 'Qwen3-Embedding-0.6B-Q8_0.gguf'
        $d.Pooling | Should -Be 'last'
        $d.Port | Should -Be 8090
    }
}

Describe 'Get-LocalLLMEmbedBaseUrl' {
    It 'is a loopback url on the embed port' {
        Get-LocalLLMEmbedBaseUrl -Port 8090 | Should -Be 'http://127.0.0.1:8090'
    }
}

Describe 'Start-LocalLLMEmbedServe -DryRun' {
    It 'renders the plan and acquires/launches nothing' {
        # A dry run touches no llama.cpp install and no network: it resolves the
        # exe best-effort (here unavailable => placeholder) and returns the recipe.
        $result = Start-LocalLLMEmbedServe -DryRun
        $result.DryRun | Should -BeTrue
        $result.BaseUrl | Should -Be 'http://127.0.0.1:8090'
        ($result.Args -join ' ') | Should -Match '--embeddings'
        ($result.Args -join ' ') | Should -Match '-ngl 0'
    }
}
