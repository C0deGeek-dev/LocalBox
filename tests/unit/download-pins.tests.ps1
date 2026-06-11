# Pester 5 tests for the supply-chain pin posture shipped in defaults.json and
# the pin-or-latest release resolution in 33-llamacpp-install.ps1.
#
# These pin the security contract: a fresh install must target a fixed release
# tag, every asset that tag can install must have a SHA-256 on record, and an
# unpinned download must be a hard failure unless the user opts out.

BeforeAll {
    $repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
    $script:Defaults = Get-Content -Raw (Join-Path $repoRoot 'local-llm\defaults.json') | ConvertFrom-Json -AsHashtable

    # The install lib only needs $script:Cfg and Ensure-Directory at parse time;
    # dot-source it with a stub so release resolution is testable in isolation.
    function Ensure-Directory { param([string]$Path) }
    . (Join-Path $repoRoot 'local-llm\lib\33-llamacpp-install.ps1')
}

Describe 'shipped pin posture (defaults.json)' {
    It 'pins the llama.cpp release tag' {
        $Defaults.LlamaCppPinnedTag | Should -Match '^b\d+$'
    }

    It 'pins the turboquant release tag' {
        $Defaults.LlamaCppTurboquantPinnedTag | Should -Not -BeNullOrEmpty
    }

    It 'pins the mtpturbo build to a full commit sha' {
        $Defaults.LlamaCppMtpTurboCommit | Should -Match '^[0-9a-f]{40}$'
    }

    It 'requires pins by default' {
        $Defaults.LlamaCppRequireDownloadPins | Should -BeTrue
    }

    It 'ships a non-empty pin table of well-formed sha256 values' {
        $pins = $Defaults.LlamaCppDownloadPins
        $pins.Keys.Count | Should -BeGreaterThan 0
        foreach ($key in $pins.Keys) {
            $pins[$key] | Should -Match '^[0-9a-f]{64}$' -Because "pin for $key must be a lowercase sha256"
        }
    }

    It 'pins every asset name against the pinned llama.cpp tag' {
        $tag = $Defaults.LlamaCppPinnedTag
        $llamaAssets = @($Defaults.LlamaCppDownloadPins.Keys | Where-Object { $_ -like 'llama-*' })
        $llamaAssets.Count | Should -BeGreaterThan 0
        foreach ($name in $llamaAssets) {
            $name | Should -BeLike "llama-$tag-*" -Because 'a pin for a different tag than LlamaCppPinnedTag can never match a download'
        }
    }

    It 'covers every variant the installer can select, plus both cudart bundles' {
        $keys = @($Defaults.LlamaCppDownloadPins.Keys)
        foreach ($needle in @('-cuda-12', '-vulkan-', '-cpu-')) {
            ($keys | Where-Object { $_ -like "llama-*$needle*" }) | Should -Not -BeNullOrEmpty -Because "the $needle variant is downloadable"
        }
        ($keys | Where-Object { $_ -like 'cudart-*' }).Count | Should -BeGreaterOrEqual 1
        ($keys | Where-Object { $_ -like '*turboquant*windows*' }) | Should -Not -BeNullOrEmpty
    }

    It 'pins a turboquant asset that matches the pinned turboquant tag' {
        $tag = $Defaults.LlamaCppTurboquantPinnedTag
        ($Defaults.LlamaCppDownloadPins.Keys | Where-Object { $_ -like "*$tag*" }) | Should -Not -BeNullOrEmpty
    }
}

Describe 'release resolution honors the pinned tag' {
    BeforeEach {
        $script:CapturedUri = $null
        Mock Invoke-RestMethod {
            $script:CapturedUri = $Uri
            return [pscustomobject]@{ tag_name = 'mocked'; assets = @() }
        }
    }

    It 'fetches the pinned llama.cpp tag when LlamaCppPinnedTag is set' {
        $script:Cfg = @{ LlamaCppPinnedTag = 'b1234' }
        Resolve-LlamaCppRelease | Out-Null
        $script:CapturedUri | Should -Be 'https://api.github.com/repos/ggerganov/llama.cpp/releases/tags/b1234'
    }

    It 'falls back to latest when no tag is pinned' {
        $script:Cfg = @{}
        Resolve-LlamaCppRelease | Out-Null
        $script:CapturedUri | Should -Be 'https://api.github.com/repos/ggerganov/llama.cpp/releases/latest'
    }

    It 'fetches the pinned turboquant tag when LlamaCppTurboquantPinnedTag is set' {
        $script:Cfg = @{ LlamaCppTurboquantPinnedTag = 'tqp-v9.9.9'; LlamaCppTurboquantRepo = 'example/turbo' }
        Resolve-LlamaCppTurboquantRelease | Out-Null
        $script:CapturedUri | Should -Be 'https://api.github.com/repos/example/turbo/releases/tags/tqp-v9.9.9'
    }

    It 'selects the single -cpu- x64 asset shipped by current releases, never the arm64 one' {
        $release = [pscustomobject]@{
            tag_name = 'b9999'
            assets   = @(
                [pscustomobject]@{ name = 'llama-b9999-bin-win-cpu-arm64.zip' },
                [pscustomobject]@{ name = 'llama-b9999-bin-win-cuda-12.4-x64.zip' },
                [pscustomobject]@{ name = 'llama-b9999-bin-win-cpu-x64.zip' }
            )
        }
        (Select-LlamaCppReleaseAsset -Release $release -Variant 'cpu').name | Should -Be 'llama-b9999-bin-win-cpu-x64.zip'
    }
}
