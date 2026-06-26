```
╔══════════════╗		██╗      ██████╗  ██████╗ █████╗ ██╗     ██████╗  ██████╗ ██╗  ██╗
║ ╔═══╗        ║		██║     ██╔═══██╗██╔════╝██╔══██╗██║     ██╔══██╗██╔═══██╗╚██╗██╔╝
║ ║███║  ████  ║║		██║     ██║   ██║██║     ███████║██║     ██████╔╝██║   ██║ ╚███╔╝
║ ╚═══╝        ║║		██║     ██║   ██║██║     ██╔══██║██║     ██╔══██╗██║   ██║ ██╔██╗
╚══════════════╝║		███████╗╚██████╔╝╚██████╗██║  ██║███████╗██████╔╝╚██████╔╝██╔╝ ██╗
 ╚══════════════╝		╚══════╝ ╚═════╝  ╚═════╝╚═╝  ╚═╝╚══════╝╚═════╝  ╚═════╝ ╚═╝  ╚═╝
  Put a local LLM behind the Claude Code / LocalPilot harness
```

# LocalBox

[![install](https://img.shields.io/badge/install-one--liner-555?style=flat-square)](docs/install.md)
[![stars](https://img.shields.io/github/stars/C0deGeek-dev/LocalBox?style=flat-square&label=stars&color=007ec6)](https://github.com/C0deGeek-dev/LocalBox/stargazers)
[![issues](https://img.shields.io/github/issues/C0deGeek-dev/LocalBox?style=flat-square&label=issues&color=4c1)](https://github.com/C0deGeek-dev/LocalBox/issues)
[![version](https://img.shields.io/badge/version-1.0.0-4c1?style=flat-square)](CHANGELOG.md)
[![runtime](https://img.shields.io/badge/runtime-llama.cpp-555?style=flat-square)](docs/llamacpp-modes.md)
[![models](https://img.shields.io/badge/models-GGUF%20catalog-orange?style=flat-square)](docs/model-management.md)
[![harnesses](https://img.shields.io/badge/harnesses-3%20targets-4c1?style=flat-square)](docs/harness-mode.md)
[![platform](https://img.shields.io/badge/platform-Windows%20PowerShell-007ec6?style=flat-square)](docs/install.md)

A PowerShell-driven launcher that runs [Claude Code](https://claude.com/claude-code)
or [LocalPilot](https://github.com/C0deGeek-dev/LocalPilot) against a
**local model** served by [llama.cpp](https://github.com/ggerganov/llama.cpp)'s
`llama-server`, with the right chat template, KV-cache type, sampling, system
prompt, and tool allowlist for each model family.

> **Ollama support was removed.** Earlier versions also drove Ollama; that path
> is preserved at the `ollama-classic` git tag (`git checkout ollama-classic`) for
> anyone who still needs it. Everything below assumes `llama-server`.

> **Windows / PowerShell only.** Does not work in WSL/bash. The launcher
> manages the `llama-server` lifecycle, drives `Start-Process`, reads
> `nvidia-smi`, and touches `$PROFILE`. None of that travels cleanly across
> shells.

---

## LocalX Ecosystem

- [LocalStack](https://github.com/C0deGeek-dev/LocalStack) is the umbrella
  ecosystem for the LocalX tools.
- [LocalBox](https://github.com/C0deGeek-dev/LocalBox) is this model runtime
  and launcher: it runs local GGUF models through Claude Code, Codex, or
  LocalPilot via llama.cpp.
- [LocalMind](https://github.com/C0deGeek-dev/LocalMind) is the local-first
  learning engine for reviewed project memory, graph-connected knowledge,
  reusable skills, and agent context.
- [LocalBench](https://github.com/C0deGeek-dev/LocalBench) is the benchmarking
  and evaluation companion that exports recommended launcher profiles.
- [LocalPilot](https://github.com/C0deGeek-dev/LocalPilot) is the local CLI
  coding agent that LocalBox can target with `-LocalPilot`.

---

## What this is

The vendored Anthropic models (Opus, Sonnet, Haiku) are good. They're also
paid, rate-limited, hosted, and out of your control. A *local* model running
through the *same agent harness* gets you the Claude Code editing loop,
tool-calling discipline, and CLI ergonomics — but pointed at weights you
actually own.

That sounds simple. In practice it isn't:

- **Each model family wants a different chat template, sampler, and stop set.**
  Qwen3-Coder needs the `qwen3coder` parser; Qwen 3.6 wants `qwen36`; Devstral
  self-templates and you must pass `Parser: none` or it fights the GGUF.
- **Anthropic's wire format carries `thinking` / `reasoning` blocks** that
  `llama-server`'s `/v1/messages` endpoint can't ingest. The launcher routes
  traffic through a small Python proxy (`no-think-proxy.py`) that strips them
  on the way in. For strip-mode launches it also passes `--reasoning off` and
  `--reasoning-budget 0` so hidden thinking tokens are not generated in the
  first place. Thinking-trained models (`ThinkingPolicy: keep`) bypass the
  proxy.
- **VRAM math is non-trivial.** Q8 KV at 256 k tokens OOMs a 4090. Q4_K_M
  weights leave room for KV but lose precision on coding. The launcher tags
  every quant with `[fits] / [tight] / [over]` against your actual card and
  *refuses* combinations that will OOM, telling you what to drop.
- **Agent launches are single-session by default.** `llama-server` can serve
  multiple slots, but Claude/LocalPilot side requests compete with the main
  turn when auto-parallelism is left on. LocalBox launches agent sessions with
  `--parallel 1` and prompt-cache reuse by default so repeated large prompts
  stay local to one slot. Both values are configurable in `settings.json`.
- **Three harnesses, one dispatch path.** Whether you launch Claude Code,
  LocalPilot, or Codex, the same env stack and proxy are set up through the
  `-LocalPilot` / `-Codex` switches on every model function.

The end result: one PowerShell function per model, flag-based, with the
fiddly bits (process bouncing, env restoration, cache types, KV ceilings,
tool allowlists, system prompts) hidden behind it.

```powershell
qcoder -Ctx 32k -LocalPilot                Qwen3-Coder @ 32k → LocalPilot
q36p -Ctx 128k                              Qwen 3.6 Plus @ 128k → Claude Code
qcoder -Ctx 256 -Quant iq4xs                256k coder context (4090 ceiling)
q36p -Mode turboquant -KvK turbo4 -KvV turbo4   Turbo KV via the fork binary
q36p -AutoBest                              Replay the saved tuner profile
llmdefault                                  whatever the catalog / settings / .llm-default says
llm                                         interactive wizard (Spectre when available)
llmc                                        native selectable wizard
llms                                        Spectre wizard, explicit
info                                        dashboard: VRAM fit, parser freshness, defaults
info -Commands                              full LocalBox + LocalBench command list
```

---

---

## Install

From the repo root (PowerShell, Windows):

```powershell
. .\install.ps1                  # copy files to deployed locations + wire $PROFILE
. .\install.ps1 -DryRun          # preview without changing anything
```

Then open a fresh PowerShell and run `llm`. Full options (symlink mode,
companion checkouts, the Terminal.Gui TUI) are in
**[docs/install.md](docs/install.md)**.

## Day-to-day

```powershell
qcoder -Ctx 32k -LocalPilot       # code agent (Qwen3-Coder, 32k) via LocalPilot
q36p -Ctx 128k                    # big context (Qwen 3.6 Plus, 128k) via Claude Code
qcoder -Ctx 256 -Quant iq4xs      # 256k coder context (4090 ceiling)
q36p -AutoBest                    # replay the saved tuner profile
llm                               # interactive wizard
info                              # dashboard: VRAM fit, defaults, parser freshness
info -Commands                    # full LocalBox + LocalBench command list
```

The full flag reference, quant keys, and the 256k-on-24GB recipe are in
**[docs/usage.md](docs/usage.md)**.

## Documentation

| Topic | Doc |
|---|---|
| Install & TUI | [docs/install.md](docs/install.md) |
| Harness mode (Claude Code / LocalPilot / Codex / serve / strict) | [docs/harness-mode.md](docs/harness-mode.md) |
| llama.cpp modes (native / turboquant / mtpturbo) | [docs/llamacpp-modes.md](docs/llamacpp-modes.md) |
| Day-to-day usage & flags | [docs/usage.md](docs/usage.md) |
| Model management (add / VRAM fit) | [docs/model-management.md](docs/model-management.md) |
| Per-machine settings & verified downloads | [docs/settings.md](docs/settings.md) |
| MCP servers | [docs/mcp.md](docs/mcp.md) |
| Auto-tuner (`findbest`) & AutoBest profiles | [docs/auto-tuner.md](docs/auto-tuner.md) · [docs/autobest-profile.md](docs/autobest-profile.md) |
| Wizard & Terminal.Gui TUI | [docs/wizard-and-tui.md](docs/wizard-and-tui.md) |
| Repo layout & casing | [docs/architecture.md](docs/architecture.md) |
| Troubleshooting | [docs/troubleshooting.md](docs/troubleshooting.md) |

See **[docs/README.md](docs/README.md)** for the full doc-ownership map, and
`CHANGELOG.md` for what shipped when.
