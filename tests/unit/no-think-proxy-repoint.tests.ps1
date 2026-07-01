BeforeAll {
    $repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
    $script:NoThinkProxyPort = 11435

    # Stubs for helpers 65-claude-launch.ps1 calls but that live in other libs /
    # only make sense against a live stack. Defining them lets Pester mock them.
    function Write-LaunchLog { param([string]$Message, [string]$Level) }
    function Test-LlamaCppPortFree { param([int]$Port) return $true }

    . (Join-Path $repoRoot 'local-llm\lib\65-claude-launch.ps1')
}

Describe 'Clear-StaleNoThinkProxy (fix C: reap dead-upstream orphan)' {
    It 'reaps the proxy when the upstream model server is down' {
        Mock Get-NoThinkProxyHealth { @{ target_host = '127.0.0.1'; target_port = 8080 } }
        Mock Test-LlamaCppPortFree { $true }   # upstream port bindable => server down
        Mock Get-NetTCPConnection { [pscustomobject]@{ OwningProcess = 21348 } }
        Mock Stop-Process { }
        Mock Start-Sleep { }

        Clear-StaleNoThinkProxy -ListenPort 11435 | Should -BeTrue
        Should -Invoke Stop-Process -Times 1 -Exactly -ParameterFilter { $Id -eq 21348 }
    }

    It 'spares the proxy when the upstream model server is alive' {
        Mock Get-NoThinkProxyHealth { @{ target_host = '127.0.0.1'; target_port = 8080 } }
        Mock Test-LlamaCppPortFree { $false }  # upstream port in use => server up
        Mock Get-NetTCPConnection { [pscustomobject]@{ OwningProcess = 21348 } }
        Mock Stop-Process { }

        Clear-StaleNoThinkProxy -ListenPort 11435 | Should -BeFalse
        Should -Invoke Stop-Process -Times 0 -Exactly
    }

    It 'is a no-op when no proxy is listening' {
        Mock Get-NoThinkProxyHealth { $null }
        Mock Stop-Process { }

        Clear-StaleNoThinkProxy -ListenPort 11435 | Should -BeFalse
        Should -Invoke Stop-Process -Times 0 -Exactly
    }
}

Describe 'Stop-NoThinkProxyOnPort (llm-stop reaps orphan by port)' {
    It 'kills whatever listens on the proxy port' {
        Mock Get-NetTCPConnection { [pscustomobject]@{ OwningProcess = 21348 } }
        Mock Stop-Process { }

        Stop-NoThinkProxyOnPort -ListenPort 11435 | Should -BeTrue
        Should -Invoke Stop-Process -Times 1 -Exactly -ParameterFilter { $Id -eq 21348 }
    }

    It 'is a no-op when nothing listens on the port' {
        Mock Get-NetTCPConnection { $null }
        Mock Stop-Process { }

        Stop-NoThinkProxyOnPort -ListenPort 11435 | Should -BeFalse
        Should -Invoke Stop-Process -Times 0 -Exactly
    }
}

Describe 'Start-NoThinkProxy (fix A: repoint on target mismatch)' {
    It 'does not throw on a live but mismatched target and starts a fresh proxy' {
        # Pre-check reports a proxy pointed at a DIFFERENT target ($false), then the
        # readiness loop reports ready ($true) once the fresh proxy is up.
        $script:tntCalls = 0
        Mock Clear-StaleNoThinkProxy { $false }
        Mock Test-NoThinkProxyTarget {
            $script:tntCalls++
            if ($script:tntCalls -eq 1) { $false } else { $true }
        }
        Mock Stop-NoThinkProxy { $script:NoThinkProxyProcess = $null }
        Mock Get-NetTCPConnection { [pscustomobject]@{ OwningProcess = 21348 } }
        Mock Stop-Process { }
        Mock Start-Sleep { }
        Mock Test-Path { $true }
        Mock Start-Process { [pscustomobject]@{ Id = 4242; HasExited = $false } }

        { Start-NoThinkProxy -ListenPort 11435 -TargetPort 9999 -AuthToken '' } | Should -Not -Throw
        Should -Invoke Start-Process -Times 1
    }
}
