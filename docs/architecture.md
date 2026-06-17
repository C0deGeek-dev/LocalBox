# Architecture

Part of the [LocalBox documentation](README.md).

The repo ships in two folders that map to two deployed locations:

```
repo                              deployed
local-llm/      ─── install ──→   %USERPROFILE%\.local-llm\
localbox-proxy/ ─── install ──→   %USERPROFILE%\.localbox-proxy\
```

```
local-llm/
  LocalLLMProfile.ps1   minimal entry point — dot-sourced by $PROFILE
  llm-models.json       model catalog (committed, sharable)
  defaults.json         shipped launcher defaults (overlaid by settings.json)
  lib/
    00-settings.ps1     config loader, settings.json overlay, env names
    05-validate.ps1     catalog validator
    10-helpers.ps1      pwsh utility primitives
    15-updates.ps1      llm-update + proxy version check
    20-models.ps1       model-def + GGUF/mmproj resolution
    25-vram.ps1         nvidia-smi auto-detect, fit-class arithmetic
    32-llamacpp.ps1     llama-server lifecycle (port pick, health, session)
    33-llamacpp-install.ps1   resolve native/turboquant/mtpturbo llama-server binaries
    34-llamacpp-status.ps1    rich per-process llama-server inspector (llm-status)
    35-backend.ps1      Invoke-Backend dispatcher
    40-parsers.ps1      per-family chat template / sampler / strict overlay
    41-llamacpp-args.ps1   pure argv builder for llama-server
    42-llamacpp-templates.ps1  parser → llama-server flag mapping, strict file
    55-huggingface.ps1  HF repo discovery, GGUF download, quant code recognition
    60-catalog.ps1      catalog editor (addllm/updatellm/removellm)
    65-claude-launch.ps1   Claude/LocalPilot/Codex launcher; env save/restore, proxy
    70-bench.ps1        legacy bench history viewer
    71-localbench-bridge.ps1   LocalBench interop
    72-llamacpp-tuner.ps1      AutoBest config persistence
    75-display.ps1      info dashboard (Spectre + plain-text fallbacks)
    80-init.ps1         purge / unloadall
    85-shortcuts.ps1    per-model function generator, default-key resolution
    90-wizard.ps1       native selectable + Spectre interactive wizards
    99-entrypoints.ps1  llm/llmmenu/llmc/llms/reloadllm/lps/lstop

localbox-proxy/
  no-think-proxy.py     strips Anthropic thinking/reasoning blocks
```

`LocalLLMProfile.ps1` dot-sources every `lib/*.ps1` in numeric prefix order,
loads `llm-models.json` overlaid with `~/.local-llm/settings.json`, and
registers per-model shortcut functions. Everything else hangs off that.

---

## Casing convention

The repo mixes three styles intentionally:

- `kebab-case` for folders (`local-llm/`, `localbox-proxy/`) — matches their deployed path.
- `PascalCase` for the entry-point script (`LocalLLMProfile.ps1`) — PowerShell convention.
- `kebab-case` for data files (`llm-models.json`).

These names are user-visible (the deployed paths). Renaming them would break
setups, so they stay.

---
