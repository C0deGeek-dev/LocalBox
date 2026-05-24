# Top-level command surface (one-liners that wrap into the wizard / reload).

function llm     { Start-LLMWizard @args }
function llmmenu { Start-LLMWizard @args }
function llmc    { Start-LLMWizardClassic }
function llms    { Start-LLMWizardSpectreExplicit }
function llmremote { Start-LocalLLMRemoteGateway @args }
function reloadllm { Reload-LocalLLMConfig }

# llama.cpp: status + stop. The wizard handles launch interactively;
# these are escape hatches for an already-running session.
#   lps   = show status of the running llama-server
#   lstop = stop every llama-server.exe; no restart
function lps    { Get-LlamaServerStatus }
function lstop  { Stop-AllLlamaServers }
function bp     { bpstatus }

# Frees all VRAM by stopping every LocalBox-managed llama-server.
function unloadall { Unload-LocalLLM }
function llmstop   { Unload-LocalLLM }
function llm-stop  { Unload-LocalLLM }

# Status: rich per-process llama-server inspector with launch-arg parsing,
# /props + /slots queries, nvidia-smi per-PID memory, and Windows GPU
# performance counters. Forwards the same switches as Invoke-LlamaCppStatus
# (-Detailed, -Json, -Watch, etc.).
function llm-status { Invoke-LlamaCppStatus @args }
function llmstatus  { Invoke-LlamaCppStatus @args }
