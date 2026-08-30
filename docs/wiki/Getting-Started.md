# Getting started

LocalBox puts a local GGUF model behind the Claude Code / LocalPilot / Codex
agent harness, served by llama.cpp's `llama-server`, with the right chat
template, KV-cache type, sampler, and tool allowlist per model family.

> **Do not edit on github.com.** This wiki is generated from in-repo Markdown
> under `docs/wiki/` and synced one-way on every push to `main`. Edit the source
> in `docs/wiki/`; web edits are overwritten on the next sync.

## Prerequisites

- **Windows, Linux, or macOS.** LocalBox is a single native binary — no
  PowerShell, .NET, or Python at runtime.
- An NVIDIA GPU is recommended; LocalBox auto-detects VRAM and tags each quant
  fits / tight / over. macOS uses Metal builds; Linux CUDA is best-effort with
  a CPU fallback.
- `llama-server` binaries download (pinned + checksum-verified) on first use;
  GGUF weights download from Hugging Face on first launch.

## Install

From the repo root (or use a release binary):

```text
cargo install --path crates/localbox --locked
```

Full details (per-platform notes, bring-your-own `llama-server`) are in
[install.md](https://github.com/C0deGeek-dev/LocalBox/blob/main/docs/install.md).

## First run

```text
localbox                         # guided launcher: pick a model, confirm, go
localbox info                    # list the configured models by tier
localbox status                  # serve health and the remedy when down
```

The first run seeds `~/.local-llm` with the defaults and an editable model
catalog. Then launch a model directly when you know what you want:

```text
localbox launch q3635ba3bapex --context 32k --agent localpilot
localbox launch q36plus --context 128k          # via Claude Code (default)
```

## Next steps

- [[How-To]] — manage models, serve headless, AutoBest replay.
- [[Examples]] — copy-pasteable launch recipes.
- [[Reference]] — the full in-repo documentation index.
- [[Troubleshooting]] — common problems and fixes.
