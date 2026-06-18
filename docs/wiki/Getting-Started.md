# Getting started

LocalBox puts a local GGUF model behind the Claude Code / LocalPilot / Codex
agent harness, served by llama.cpp's `llama-server`, with the right chat
template, KV-cache type, sampler, and tool allowlist per model family.

> **Do not edit on github.com.** This wiki is generated from in-repo Markdown
> under `docs/wiki/` and synced one-way on every push to `main`. Edit the source
> in `docs/wiki/`; web edits are overwritten on the next sync.

## Prerequisites

- **Windows with PowerShell.** LocalBox manages the `llama-server` lifecycle,
  drives `Start-Process`, reads `nvidia-smi`, and touches `$PROFILE`. It does not
  run in WSL/bash.
- An NVIDIA GPU is recommended; LocalBox auto-detects VRAM and tags each quant
  `[fits]` / `[tight]` / `[over]`.
- `llama-server` binaries download (pinned + checksum-verified) on first use;
  GGUF weights download from Hugging Face on first launch.

## Install

From the repo root:

```powershell
. .\install.ps1                  # copy files to deployed locations + wire $PROFILE
. .\install.ps1 -DryRun          # preview without changing anything
```

Open a fresh PowerShell so `$PROFILE` loads the launcher. Full options (symlink
mode, companion checkouts, the Terminal.Gui TUI) are in
[install.md](https://github.com/C0deGeek-dev/LocalBox/blob/main/docs/install.md).

## First run

```powershell
llm                              # interactive wizard: pick model, mode, action
info                             # dashboard: VRAM fit, default model, parser freshness
info -Commands                   # full LocalBox + LocalBench command list
```

Then launch a model. Each catalog model gets its own function:

```powershell
qcoder -Ctx 32k -LocalPilot      # Qwen3-Coder at 32k context, via LocalPilot
q36p -Ctx 128k                   # Qwen 3.6 Plus at 128k, via Claude Code
```

## Next steps

- [[How-To|How-To guides]] — manage models, run the proxy/serve, AutoBest.
- [[Examples]] — copy-pasteable launch recipes.
- [[Reference]] — the full in-repo documentation index.
- [[Troubleshooting]] — common problems and fixes.
