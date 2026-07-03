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
    <img alt="version 2.1.2" src="https://img.shields.io/badge/version-2.1.2-38bdae?style=flat-square">
    <img alt="Windows, Linux, macOS" src="https://img.shields.io/badge/platform-Windows%20%7C%20Linux%20%7C%20macOS-4d8df7?style=flat-square">
    <img alt="llama.cpp runtime" src="https://img.shields.io/badge/runtime-llama.cpp-59636e?style=flat-square">
    <img alt="GitHub stars" src="https://img.shields.io/github/stars/C0deGeek-dev/LocalBox?style=flat-square&label=stars">
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
| **Runs on** | Windows, Linux, and macOS — a single native binary, run from any shell |

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

LocalBox is a single native binary — no PowerShell, .NET, or Python needed.
Build it from this repository (or use a release binary):

```text
cargo install --path crates/localbox --locked
```

Then launch the guided model picker:

```text
localbox
```

That is the shortest path. The first run seeds `~/.local-llm` with the
defaults and an editable model catalog, never overwriting anything you
already have. See the [installation guide](docs/install.md) for details.

## Everyday commands

| Goal | Command |
|---|---|
| Pick a model interactively | `localbox` |
| Guided launcher with plain-text menus | `localbox --plain` |
| Launch a model into Claude Code | `localbox launch <model>` |
| Run through LocalPilot | `localbox launch <model> --agent localpilot` |
| Run through Codex | `localbox launch <model> --agent codex` |
| Serve headless (no agent) | `localbox serve <model>` |
| List the configured models | `localbox info` |
| Serve health and the remedy | `localbox status` |
| Stop everything | `localbox stop` |

Model aliases come from the catalog, so the exact list on your machine may be
different. `localbox help` is the source of truth for the installed command
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
| Upgrade from a 1.x install | [Install → Upgrading from 1.x](docs/install.md#upgrading-from-1x) |
| Learn the day-to-day flags | [Usage](docs/usage.md) |
| Connect a coding-agent harness | [Harness mode](docs/harness-mode.md) |
| Add or size a model | [Model management](docs/model-management.md) |
| Tune a model automatically | [Auto-tuner](docs/auto-tuner.md) and [AutoBest profiles](docs/autobest-profile.md) |
| Configure a machine | [Settings](docs/settings.md) |
| Choose a llama.cpp runtime mode | [llama.cpp modes](docs/llamacpp-modes.md) |
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
