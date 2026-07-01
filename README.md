```
╔══════════════╗		██╗      ██████╗  ██████╗ █████╗ ██╗     ██████╗  ██████╗ ██╗  ██╗
║ ╔═══╗        ║		██║     ██╔═══██╗██╔════╝██╔══██╗██║     ██╔══██╗██╔═══██╗╚██╗██╔╝
║ ║███║  ████  ║║		██║     ██║   ██║██║     ███████║██║     ██████╔╝██║   ██║ ╚███╔╝
║ ╚═══╝        ║║		██║     ██║   ██║██║     ██╔══██║██║     ██╔══██╗██║   ██║ ██╔██╗
╚══════════════╝║		███████╗╚██████╔╝╚██████╗██║  ██║███████╗██████╔╝╚██████╔╝██╔╝ ██╗
 ╚══════════════╝		╚══════╝ ╚═════╝  ╚═════╝╚═╝  ╚═╝╚══════╝╚═════╝  ╚═════╝ ╚═╝  ╚═╝
  Put a local LLM behind the Claude Code / LocalPilot harness
```

<div align="center">
  <h1>LocalBox</h1>
  <p><strong>Run local GGUF models through a real coding-agent harness.</strong></p>
  <p>
    <a href="docs/README.md">Documentation</a> ·
    <a href="docs/install.md">Install</a> ·
    <a href="docs/troubleshooting.md">Troubleshooting</a> ·
    <a href="https://c0degeek-dev.github.io/LocalStack/">LocalX</a>
  </p>
  <p>
    <img alt="version 1.2.1" src="https://img.shields.io/badge/version-1.2.1-38bdae?style=flat-square">
    <img alt="Windows PowerShell" src="https://img.shields.io/badge/platform-Windows%20PowerShell-4d8df7?style=flat-square">
    <img alt="llama.cpp runtime" src="https://img.shields.io/badge/runtime-llama.cpp-59636e?style=flat-square">
    <img alt="GitHub stars" src="https://img.shields.io/github/stars/C0deGeek-dev/LocalBox?style=flat-square&amp;label=stars">
  </p>
</div>

LocalBox turns a local model into something you can actually use for coding. It
starts `llama-server`, chooses safe settings for your hardware and model, and
connects the result to Claude Code, Codex, or
[LocalPilot](https://github.com/C0deGeek-dev/LocalPilot).

| At a glance | |
|---|---|
| **Use it when** | You have a GGUF model and want an agent-ready local runtime |
| **It handles** | Server lifecycle, chat templates, parsers, sampling, context, KV cache, VRAM checks, and harness setup |
| **You control** | Model, quant, context size, runtime mode, and target harness |
| **Runs on** | Windows PowerShell (not WSL or bash) |

## Privacy by design

LocalBox runs your model on hardware you control and keeps the normal inference
path local.

- **No usage telemetry is sent.** LocalBox does not report your prompts, code,
  models, hardware measurements, or usage to us.
- **Your runtime data stays yours.** Models, profiles, logs, and generated
  configuration remain on the machine and paths you choose.
- **Network access is deliberate.** Model or runtime downloads happen only when
  you request them; exposing a server beyond loopback is an explicit, guarded
  action.
- **You remain in control.** The configuration is readable, portable, and yours
  to inspect, back up, move, or delete.

> [!IMPORTANT]
> LocalBox uses `llama-server`. Ollama support ended after the
> [`ollama-classic`](https://github.com/C0deGeek-dev/LocalBox/tree/ollama-classic)
> tag.

## Quick start

From a PowerShell window in this repository:

```powershell
. .\install.ps1
```

Open a new PowerShell window, then launch the guided model picker:

```powershell
llm
```

That is the shortest path. The installer wires your profile, deploys the
launcher, and can connect companion LocalX checkouts. Preview every change first
with:

```powershell
. .\install.ps1 -DryRun
```

See the [installation guide](docs/install.md) for symlink mode, verified
downloads, companion tools, and the Terminal.Gui interface.

## Everyday commands

| Goal | Command |
|---|---|
| Pick a model interactively | `llm` |
| Use the native selectable wizard | `llmc` |
| Open the status dashboard | `info` |
| Show every LocalBox and LocalBench command | `info -Commands` |
| Run Qwen3-Coder through LocalPilot | `qcoder -Ctx 32k -LocalPilot` |
| Run Qwen 3.6 Plus through Claude Code | `q36p -Ctx 128k` |
| Run through Codex | `qcoder -Ctx 32k -Codex` |
| Replay the best measured profile | `q36p -AutoBest` |
| Use the configured default | `llmdefault` |

Model aliases come from the catalog, so the exact list on your machine may be
different. `info -Commands` is the source of truth for the installed command
surface.

## What LocalBox does for you

- Chooses the correct chat template, parser, sampler, stop set, and reasoning
  policy for each supported model family.
- Estimates weight and KV-cache pressure against your actual GPU and blocks
  combinations likely to run out of VRAM.
- Keeps agent sessions predictable with single-session defaults and prompt-cache
  reuse.
- Sets up one consistent dispatch path for Claude Code, Codex, LocalPilot, and
  plain server mode.
- Saves measured [LocalBench](https://github.com/C0deGeek-dev/LocalBench)
  recommendations as reusable AutoBest profiles.

```text
GGUF model ──> LocalBox ──> llama-server ──> Claude Code / Codex / LocalPilot
                    │
                    └── VRAM guardrails, templates, parsers, cache and sampling
```

## Choose your next guide

| I want to… | Read |
|---|---|
| Install or repair LocalBox | [Install](docs/install.md) |
| Learn the day-to-day flags | [Usage](docs/usage.md) |
| Connect a coding-agent harness | [Harness mode](docs/harness-mode.md) |
| Add or size a model | [Model management](docs/model-management.md) |
| Tune a model automatically | [Auto-tuner](docs/auto-tuner.md) and [AutoBest profiles](docs/autobest-profile.md) |
| Configure a machine | [Settings](docs/settings.md) |
| Choose a llama.cpp runtime mode | [llama.cpp modes](docs/llamacpp-modes.md) |
| Configure MCP servers | [MCP](docs/mcp.md) |
| Understand the repository | [Architecture](docs/architecture.md) |
| Fix a problem | [Troubleshooting](docs/troubleshooting.md) |

## LocalX

LocalBox is the runtime layer in the
[LocalX toolchain](https://c0degeek-dev.github.io/LocalStack/):

| Project | Role |
|---|---|
| **LocalBox** | Run local models |
| [LocalBench](https://github.com/C0deGeek-dev/LocalBench) | Find fast, stable settings |
| [LocalPilot](https://github.com/C0deGeek-dev/LocalPilot) | Code through the agent harness |
| [LocalMind](https://github.com/C0deGeek-dev/LocalMind) | Turn reviewed sessions into reusable project memory |

Release history lives in [CHANGELOG.md](CHANGELOG.md).
