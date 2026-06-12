BeforeAll {
    $repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
    $script:LLMProfileRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("localbox-tests-" + [System.IO.Path]::GetRandomFileName())
    New-Item -ItemType Directory -Path $script:LLMProfileRoot | Out-Null
    $script:LocalLLMConfigPath = Join-Path $script:LLMProfileRoot 'llm-models.json'
    $script:Cfg = @{ LocalModelMaxOutputTokens = 4096 }
    $script:NoThinkProxyPort = 11435

    . (Join-Path $repoRoot 'local-llm\lib\00-settings.ps1')
    . (Join-Path $repoRoot 'local-llm\lib\65-claude-launch.ps1')

    function Write-LaunchLog { param([string]$Message, [string]$Level) }
}

AfterAll {
    Remove-Item -Recurse -Force $script:LLMProfileRoot -ErrorAction SilentlyContinue
}

Describe 'Serve gateway exposure guard' {
    Context 'public-looking address classification' {
        It 'treats loopback, RFC1918, and link-local addresses as private' {
            foreach ($url in @(
                    'http://localhost:11435',
                    'http://127.0.0.1:11435',
                    'http://10.0.0.5:11435',
                    'http://192.168.1.20:11435',
                    'http://172.16.0.9:11435',
                    'http://172.31.255.1:11435',
                    'http://169.254.10.10:11435'
                )) {
                Test-LocalLLMServePublicHttp -BaseUrl $url | Should -BeFalse -Because $url
            }
        }

        It 'treats non-private IPv4 and bare hostnames as public-looking' {
            foreach ($url in @(
                    'http://203.0.113.7:11435',
                    'http://myhost.example.com:11435',
                    'http://workstation:11435'
                )) {
                Test-LocalLLMServePublicHttp -BaseUrl $url | Should -BeTrue -Because $url
            }
        }
    }

    Context 'guard decision' {
        It 'refuses a public-looking URL with no password and no opt-in' {
            $decision = Get-LocalLLMServeGuardDecision -BaseUrls @('http://203.0.113.7:11435') -Password ''

            $decision.Refuse | Should -BeTrue
            $decision.Reason | Should -Match 'AllowPublicNoAuth'
            $decision.PublicUrls | Should -Be @('http://203.0.113.7:11435')
        }

        It 'refuses when any advertised URL is public-looking' {
            $decision = Get-LocalLLMServeGuardDecision -BaseUrls @('http://192.168.1.20:11435', 'http://203.0.113.7:11435') -Password ''

            $decision.Refuse | Should -BeTrue
            $decision.PublicUrls | Should -Be @('http://203.0.113.7:11435')
        }

        It 'allows private-only URLs without a password' {
            $decision = Get-LocalLLMServeGuardDecision -BaseUrls @('http://127.0.0.1:11435', 'http://192.168.1.20:11435') -Password ''

            $decision.Refuse | Should -BeFalse
            $decision.OptedIn | Should -BeFalse
        }

        It 'allows a public-looking URL when a password is set' {
            $decision = Get-LocalLLMServeGuardDecision -BaseUrls @('http://203.0.113.7:11435') -Password 'secret'

            $decision.Refuse | Should -BeFalse
            $decision.OptedIn | Should -BeFalse
        }

        It 'allows a public-looking URL without a password under explicit opt-in' {
            $decision = Get-LocalLLMServeGuardDecision -BaseUrls @('http://203.0.113.7:11435') -Password '' -AllowPublicNoAuth

            $decision.Refuse | Should -BeFalse
            $decision.OptedIn | Should -BeTrue
        }

        It 'does not flag opt-in when it was unnecessary' {
            $decision = Get-LocalLLMServeGuardDecision -BaseUrls @('http://127.0.0.1:11435') -Password '' -AllowPublicNoAuth

            $decision.Refuse | Should -BeFalse
            $decision.OptedIn | Should -BeFalse
        }

        It 'treats a whitespace password as no auth' {
            $decision = Get-LocalLLMServeGuardDecision -BaseUrls @('http://203.0.113.7:11435') -Password '   '

            $decision.Refuse | Should -BeTrue
        }
    }
}
