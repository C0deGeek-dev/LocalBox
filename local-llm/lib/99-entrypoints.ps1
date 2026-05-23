# Top-level command surface (one-liners that wrap into the wizard / reload).

function llm     { Start-LLMWizard @args }
function llmmenu { Start-LLMWizard @args }
function llmc    { Start-LLMWizardClassic }
function llms    { Start-LLMWizardSpectreExplicit }
function llmremote { Start-LocalLLMRemoteGateway @args }
function reloadllm { Reload-LocalLLMConfig }

# llama.cpp: status + stop. The wizard handles launch interactively;
# these are escape hatches for an already-running session.
#   lps   = ops parallel    (show status of the running llama-server)
#   lstop = ostop parallel  (stop every llama-server.exe; no restart)
function lps    { Get-LlamaServerStatus }
function lstop  { Stop-AllLlamaServers }
function bp     { bpstatus }

# Cross-backend nuclear option: free all VRAM by stopping Ollama and every
# llama-server.exe. Neither backend is restarted afterwards.
function unloadall { Unload-LocalLLM }
function llmstop   { Unload-LocalLLM }
function llm-stop  { Unload-LocalLLM }

# Cross-backend status: shows running models for both backends, regardless
# of DefaultBackend. The llama.cpp half uses Invoke-LlamaCppStatus — rich
# per-process inspector with launch-arg parsing, /props + /slots queries,
# nvidia-smi per-PID memory, and Windows GPU performance counters. Forwards
# the same switches as Invoke-LlamaCppStatus (-Detailed, -Json, -Watch, etc.).
function llm-status {
    [CmdletBinding()]
    param(
        [Alias("Pid")][int[]]$ProcessId,
        [int[]]$Port,
        [switch]$Detailed,
        [switch]$Json,
        [switch]$NoEndpointQuery,
        [switch]$Watch,
        [int]$IntervalSeconds = 3,
        [int]$TimeoutSeconds = 2,
        [switch]$OllamaOnly,
        [switch]$LlamaCppOnly
    )

    if ($Json) {
        # JSON path: skip Ollama (its `ollama ps` is text-only) and emit the
        # llama.cpp rows directly so the output is machine-parseable.
        Invoke-LlamaCppStatus @PSBoundParameters
        return
    }

    if (-not $LlamaCppOnly) {
        Write-Host "== Ollama ==" -ForegroundColor Cyan
        & ollama ps
        Write-Host ""
    }

    if (-not $OllamaOnly) {
        $forward = @{}
        foreach ($k in 'ProcessId','Port','Detailed','NoEndpointQuery','Watch','IntervalSeconds','TimeoutSeconds') {
            if ($PSBoundParameters.ContainsKey($k)) { $forward[$k] = $PSBoundParameters[$k] }
        }
        Invoke-LlamaCppStatus @forward
    }
}

# Shorter alias that targets just the llama.cpp side.
function llmstatus { Invoke-LlamaCppStatus @args }
