# Pester 5 tests for the constrained-decoding capability the llama.cpp launch
# profile reports (lib/41-llamacpp-args.ps1).

BeforeAll {
    $repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
    . (Join-Path $repoRoot 'local-llm\lib\41-llamacpp-args.ps1')
}

Describe 'Get-LlamaCppConstrainedDecoding' {
    It 'reports json_schema for a llama.cpp runtime build' {
        Get-LlamaCppConstrainedDecoding -Mode 'native' | Should -Be 'json_schema'
        Get-LlamaCppConstrainedDecoding -Mode 'turboquant' | Should -Be 'json_schema'
        Get-LlamaCppConstrainedDecoding -Mode 'mtpturbo' | Should -Be 'json_schema'
    }

    It 'is case-insensitive on the mode' {
        Get-LlamaCppConstrainedDecoding -Mode 'Native' | Should -Be 'json_schema'
    }

    It 'reports none for an unrecognized runtime' {
        Get-LlamaCppConstrainedDecoding -Mode 'some-other-runtime' | Should -Be 'none'
    }
}
