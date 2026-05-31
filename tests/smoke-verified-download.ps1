# Plain self-contained test for Invoke-LocalBoxVerifiedDownload (33-llamacpp-install.ps1).
# Overrides Invoke-WebRequest with a function that writes fixed bytes, then checks
# the SHA-256 pin behaviour: no-pin TOFU, correct pin, mismatch abort, require-pins.

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
. (Join-Path $repoRoot 'local-llm\lib\33-llamacpp-install.ps1')

$script:fail = 0
function Check($name, $cond) {
    if ($cond) { Write-Host "PASS $name" } else { Write-Host "FAIL $name"; $script:fail++ }
}

# Shadow the real cmdlet: write deterministic content instead of hitting the network.
function Invoke-WebRequest {
    param($Uri, $OutFile, [switch]$UseBasicParsing, $TimeoutSec)
    Set-Content -LiteralPath $OutFile -Value 'localbox-verified-download-payload' -NoNewline
}

$tmp = Join-Path $env:TEMP 'lbx-verify-test.bin'
Remove-Item $tmp -Force -ErrorAction SilentlyContinue

# 1. No pin -> trust-on-first-use, file kept.
$script:Cfg = @{}
Invoke-LocalBoxVerifiedDownload -Uri 'http://x/a.zip' -OutFile $tmp -Name 'a.zip' | Out-Null
Check 'no-pin downloads (TOFU)' (Test-Path $tmp)
$hash = (Get-FileHash -LiteralPath $tmp -Algorithm SHA256).Hash.ToLower()

# 2. Correct pin -> verifies, file kept.
$script:Cfg = @{ LlamaCppDownloadPins = @{ 'a.zip' = $hash } }
Invoke-LocalBoxVerifiedDownload -Uri 'http://x/a.zip' -OutFile $tmp -Name 'a.zip' | Out-Null
Check 'correct pin verifies' (Test-Path $tmp)

# 3. Wrong pin -> throws and removes the file.
$script:Cfg = @{ LlamaCppDownloadPins = @{ 'a.zip' = ('0' * 64) } }
$threw = $false
try { Invoke-LocalBoxVerifiedDownload -Uri 'http://x/a.zip' -OutFile $tmp -Name 'a.zip' | Out-Null }
catch { $threw = $true }
Check 'wrong pin throws' $threw
Check 'wrong pin removes file' (-not (Test-Path $tmp))

# 4. Require-pins on + no pin -> throws.
$script:Cfg = @{ LlamaCppRequireDownloadPins = $true }
$threw = $false
try { Invoke-LocalBoxVerifiedDownload -Uri 'http://x/a.zip' -OutFile $tmp -Name 'a.zip' | Out-Null }
catch { $threw = $true }
Check 'require-pins blocks unpinned download' $threw

Remove-Item $tmp -Force -ErrorAction SilentlyContinue

if ($script:fail -gt 0) { Write-Host "FAILURES: $($script:fail)"; exit 1 }
Write-Host 'all verified-download tests passed'; exit 0
