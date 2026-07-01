# Init / teardown lifecycle. With the llama-server backend, there are no
# aliases to materialize up front — GGUFs are downloaded on demand at launch
# time. `purge` removes downloaded GGUF blobs; `unloadall` / `llm-stop` frees
# VRAM by stopping every running llama-server.

function Remove-AllLocalLLM {
    param([switch]$DeleteFiles)

    Write-Host ""

    if ($DeleteFiles) {
        Write-Host "=== Full Purge (GGUF files) ===" -ForegroundColor Red
    }
    else {
        Write-Host "=== Cleanup (stop servers only) ===" -ForegroundColor Yellow
    }

    Write-Host ""

    Stop-LlamaServer -Quiet

    if ($DeleteFiles) {
        foreach ($key in (Get-ModelKeys)) {
            Remove-ModelFiles -Key $key
        }
    }

    Write-Host ""
    Write-Host "Done." -ForegroundColor Green
    Write-Host ""
}

function Remove-ModelFiles {
    param([Parameter(Mandatory = $true)][string]$Key)

    $def = Get-ModelDef -Key $Key
    $folder = Get-ModelFolder -Def $def
    Remove-Item -Recurse -Force $folder -ErrorAction SilentlyContinue
}

function Set-ModelQuant {
    param(
        [Parameter(Mandatory = $true)][string]$Key,
        [Parameter(Mandatory = $true)][string]$Quant
    )

    $def = Get-ModelDef -Key $Key

    if (-not $def.ContainsKey("Quants")) {
        throw "$Key does not support quant switching."
    }

    $resolvedQuant = Resolve-ModelQuantKey -Def $def -Quant $Quant
    $def.Quant = $resolvedQuant

    Write-Host "$Key now set to $resolvedQuant -> $($def.Quants[$resolvedQuant])" -ForegroundColor Green
}

function Unload-LocalLLM {
    # Frees all VRAM by stopping any running LocalBox-managed llama-server
    # process, and tearing down the serve gateway if one is up.
    if (Get-Command Stop-LocalLLMServeGateway -ErrorAction SilentlyContinue) {
        Stop-LocalLLMServeGateway
    }
    Stop-AllLlamaServers

    # Also reap the launch-path no-think proxy. Stop-NoThinkProxy clears this shell's
    # owned handle; the by-port kill catches an orphan left by a prior/other session
    # (which otherwise survives — llm-stop is where a full teardown is expected).
    if (Get-Command Stop-NoThinkProxy -ErrorAction SilentlyContinue) { Stop-NoThinkProxy }
    if ((Get-Command Stop-NoThinkProxyOnPort -ErrorAction SilentlyContinue) -and $script:NoThinkProxyPort) {
        Stop-NoThinkProxyOnPort -ListenPort ([int]$script:NoThinkProxyPort) | Out-Null
    }
}

function purge { Remove-AllLocalLLM -DeleteFiles }
