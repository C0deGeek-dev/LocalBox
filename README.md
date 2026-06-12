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

[![install](https://img.shields.io/badge/install-one--liner-555?style=flat-square)](#install)
[![stars](https://img.shields.io/github/stars/C0deGeek-dev/LocalBox?style=flat-square&label=stars&color=007ec6)](https://github.com/C0deGeek-dev/LocalBox/stargazers)
[![issues](https://img.shields.io/github/issues/C0deGeek-dev/LocalBox?style=flat-square&label=issues&color=4c1)](https://github.com/C0deGeek-dev/LocalBox/issues)
[![runtime](https://img.shields.io/badge/runtime-llama.cpp-555?style=flat-square)](#llamacpp-modes)
[![models](https://img.shields.io/badge/models-GGUF%20catalog-orange?style=flat-square)](#adding-a-model)
[![harnesses](https://img.shields.io/badge/harnesses-3%20targets-4c1?style=flat-square)](#harness-mode)
[![platform](https://img.shields.io/badge/platform-Windows%20PowerShell-007ec6?style=flat-square)](#install)

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

## Harness mode

A **harness** is the agent loop wrapping the model — the thing that turns raw
generation into "read this file, run that command, edit this code, then ask the
user". Claude Code is one such harness. LocalPilot is an independent,
clean-room harness with a similar operating model.

### Claude Code harness (default)

```powershell
qcoder -Ctx 32k               # qcoder is the per-model function name
```

What happens:

1. The launcher snapshots and clears any `ANTHROPIC_*` env vars in the current shell.
2. Resolves the GGUF (downloads from HuggingFace on first use).
3. Starts `llama-server` on a free port from `LlamaCppPort` (default 8080) with
   the per-model parser, KV-cache, MoE-offload, and reasoning flags.
4. Starts the no-think proxy on `127.0.0.1:11435` (Python; ~300 ms cold) in
   front of `llama-server`.
5. Sets `ANTHROPIC_BASE_URL=http://localhost:11435`, points
   `ANTHROPIC_DEFAULT_*_MODEL` at the model's `Root`, disables thinking +
   prompt caching, bumps `API_TIMEOUT_MS` to 30 min (local prefill is slow on
   big prompts).
6. Launches `claude --model <root> [--dangerously-skip-permissions]
   [--tools <allowlist>] --append-system-prompt <local-tool-rules>`.
   Whether the permission skip is passed is a first-run decision: the first
   agent launch asks "skip permission prompts for agent launches? [y/N]" and
   persists the answer to `settings.json`. The default answer keeps Claude
   Code's per-action permission prompts — the human-in-the-loop that catches a
   runaway or injected tool call from a less-aligned local model. Change it
   any time with `Set-LocalLLMSetting LocalModelSkipPermissions $true|$false`
   or per-shell with `LOCAL_LLM_SKIP_PERMISSIONS=1|0`.
7. On exit, restores the original env, stops the proxy, and stops `llama-server`.

The model believes it's Claude. Claude Code believes it's talking to Anthropic.
The proxy quietly strips Anthropic-only fields the local backend can't parse.

### LocalPilot harness

Same flow, except the launch shells into `localpilot chat --model <model>`
instead of `claude`. `LocalPilotRoot` points at the Rust checkout used by
LocalBox update/install flows; when unset, LocalBox discovers a sibling
`LocalPilot` checkout next to the LocalBox repo and otherwise falls back to
`~/.local-llm/tools/localpilot`. `LocalPilotRepoUrl` defaults to
`https://github.com/C0deGeek-dev/LocalPilot`.

```powershell
qcoder -Ctx 32k -LocalPilot
```

### Codex harness

Same flow, except the launch shells into `codex` with an OpenAI-compatible
provider pointed at the running `llama-server`'s `/v1` endpoint.

```powershell
qcoder -Ctx 32k -Codex
```

> **Note.** The `codex` CLI itself, when pointed at OpenAI rather than a local
> endpoint, drives OpenAI's hosted backend. If you use it against the LocalPilot
> Codex adapter (a reverse-engineered private endpoint), be aware that path may
> violate OpenAI's Terms of Use. Against a local `llama-server` `/v1` endpoint
> as shown here, this concern does not apply.

### Serve gateway

Choose `Serve` in `llm`, or use `llmserve`, to serve a model from this
machine to any agentic client that can use an Anthropic-compatible endpoint.
The server starts `llama-server` and exposes only the LocalBox no-think
gateway; `llama-server` itself stays bound to localhost.

```powershell
$env:LOCAL_LLM_SERVE_PASS = "chosenpass"
llmserve -Key qcoder30 -ContextKey 32k -LlamaCppMode native
```

After startup, LocalBox opens a serve monitor with the gateway status and live
request log. Press `Q` to return to the menu while leaving the server running,
or `S` to stop the gateway and backend. Use `llmserve -NoMonitor` for scripted
or detached starts.

On the client, no LocalBox helper is required. Set the Anthropic-compatible
environment variables for your agentic client. For LocalPilot:

```bash
export ANTHROPIC_BASE_URL="http://192.168.178.61:11435"
export ANTHROPIC_AUTH_TOKEN="chosenpass"
export ANTHROPIC_API_KEY="chosenpass"
localpilot
```

Password-only HTTP is convenient for LAN testing. Over a public IP it is not
encrypted: the password and prompts can be observed in transit unless you put a
VPN or HTTPS reverse proxy in front of it.

### Strict overlay (engineering mode)

Some models in the catalog have `Strict: true`. Pass `-Strict` and the
launcher injects a tighter sampler (`temperature 0.2`, `top_p 0.8`, `top_k 20`,
`min_p 0.05`, `repeat_penalty 1.15`, `repeat_last_n 4096`) plus a
non-negotiable engineering system prompt:

> Do not create mocks, stubs, fake data, dummy implementations, placeholder
> services, TODO implementations, temporary bypasses, hardcoded sample
> responses, or `NotImplementedException`.
> Do not invent new architecture, schema fields, configuration properties,
> or abstractions unless they fit existing patterns.
> Do not make tests pass by weakening, bypassing, deleting, or faking real
> behavior.
> Reuse existing architecture and production code paths. If the real
> implementation is missing, blocked, or ambiguous: stop and explain what
> is missing instead of inventing a substitute.

The sampler flags are injected directly into the llama-server argv; the strict
system prompt is appended on the harness side.

> **When to use it.** Strict overlay is for actual engineering work where the
> model's lazy paths (mock, stub, "// TODO", placeholder JSON) cost real time.
> Skip it for chat, brainstorming, RAG-style Q&A.

---

## llama.cpp modes

The launcher supports three flavors of `llama-server`:

- **`native`** — upstream `llama-server.exe`. Mainline KV types only
  (`q8_0`, `f16`, `q5_1`, `q5_0`, `q4_1`, `q4_0`, `iq4_nl`, `bf16`, `f32`).
  Supports `--spec-type draft-mtp` for native Multi-Token Prediction
  speculative decoding on MTP-capable GGUFs.
- **`turboquant`** — TheTom's [llama.cpp turboquant fork](https://github.com/TheTom/llama-cpp-turboquant), which
  ships `turbo3` and `turbo4` KV cache types (more aggressive than `q4_0` but
  with a quality cliff that's a function of context length). Only available
  through the fork binary. Auto-downloaded from GitHub releases on first use.
  Does **not** support MTP — LocalBox rejects `--spec-type draft-mtp` up front
  in this mode.
- **`mtpturbo`** — combined build: MTP spec-decode **and** turbo KV cache in
  one binary. No prebuilt Windows CUDA release exists for any fork that
  carries both features, so LocalBox builds it from source on first use.
  When you pick `mtpturbo` and the binary is absent:
  - LocalBox probes for the toolchain (git, cmake, ninja, nvcc, MSVC). If
    anything is missing it prints the exact `winget install` command for
    each.
  - If the toolchain is complete it prompts `Build it now? [Y/n]`, then
    shallow-clones [`EsmaeelNabil/llama.cpp#feat/mtp-turboquant-kv-cache`](https://github.com/EsmaeelNabil/llama.cpp/tree/feat/mtp-turboquant-kv-cache),
    auto-detects compute capability via `nvidia-smi`, single-arch CUDA build
    via Ninja (~5–30 min depending on GPU), installs into
    `~/.local-llm/llama-cpp-mtpturbo/`, writes `.build-stamp`.
  - Repo + branch are overrideable via `LlamaCppMtpTurboRepo` /
    `LlamaCppMtpTurboBranch` settings if you fork it. CUDA Toolkit + VS
    BuildTools are heavyweight system-wide deps that LocalBox never silent-
    installs; it just names the winget IDs and gets out of the way.

All three modes start a native `llama-server` process, pin to a free port from
`LlamaCppPort` (default `8080`), wait for `/v1/models` to come up, then point
Claude Code at `http://localhost:<port>`.

```powershell
# Wizard route — pick mode interactively
llm

# Direct
Invoke-Backend -Action launch-claude `
  -Key qcoder30 -ContextKey 256 `
  -LlamaCppMode turboquant -KvCacheK turbo4 -KvCacheV turbo4 -Strict

# MTP + turbo KV together — the unsloth 256K-on-24GB recipe. Catalog stores
# SpecType=draft-mtp (mainline canonical); LocalBox translates to bare 'mtp'
# at emit time for this mode automatically.
Invoke-Backend -Action launch-claude `
  -Key genesisv2 -ContextKey 128k `
  -LlamaCppMode mtpturbo -KvCacheK turbo3 -KvCacheV turbo4

lps                           # show running llama-server (port, pid, gguf path)
llm-status                    # detailed per-process status (KV, ngl, MTP, VRAM, slots, /props)
lstop                         # stop it
llm-stop                      # alias for unloadall: stop every running llama-server
```

---

## Architecture

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

## Install

From the repo root:

```powershell
. .\install.ps1                  # copy files to deployed locations + add to $PROFILE
. .\install.ps1 -Symlink         # symlink instead of copy (admin / dev mode)
. .\install.ps1 -SetupProfile    # only ensure $PROFILE dot-sources the deployed file
. .\install.ps1 -InstallLocalBench   # also clone LocalBench if missing
. .\install.ps1 -InstallLocalPilot   # also clone LocalPilot if missing
. .\install.ps1 -DryRun          # preview without changing anything
```

After install, open a fresh PowerShell:

```powershell
llm                              # interactive wizard — pick model, mode, action
llmtui                           # Terminal.Gui TUI, explicit preview path
info                             # verify: VRAM, default model, configured quants
```

The install step offers to clone missing LocalBench and LocalPilot into
`~/.local-llm/tools/` (a sibling `LocalPilot` checkout next to the LocalBox
repo is detected and used first). Use `-SkipToolPrompts` for
unattended installs. `Show-Diagnostics` also reports on `python`, the
`localpilot` CLI, `PwshSpectreConsole`, LocalBench, and LocalPilot.
Installs also record `LocalBoxRoot` in `settings.json`, which lets `llm-update`
pull this repo and redeploy the profile files later.

---

## Day-to-day usage

One function per model. Flag-based:

```
qcoder -Ctx 32k -LocalPilot       Code agent (Qwen3-Coder, 32k, LocalPilot)
qcoder -Ctx 32k -Codex            Code agent (Qwen3-Coder, 32k, Codex)
q36p -Ctx 32k -LocalPilot         General Qwen 3.6 agent (32k, LocalPilot)
dev -Ctx 32k                      Smaller / faster (Devstral 24B, 32k)
q36p -Ctx 128k -LocalPilot        Big context (Qwen 3.6 Plus, 128k)
qcoder -Ctx 256 -Quant iq4xs      256k coder context (4090 ceiling)
q36p -Quant q6kp                  Switch the GGUF quant
q36p -Mode turboquant -KvK turbo4 -KvV turbo4   Turbo KV via fork binary
q36p -AutoBest                    Replay the saved tuner config
llmdefault                        Launch the configured default recipe/model
llmdefaultlocalpilot              Same, via LocalPilot
llmdefaultcodex                   Same, via Codex
llm                               Guided wizard (Spectre when available)
llmtui                            Terminal.Gui TUI preview
lbtui                             LocalBench Terminal.Gui TUI preview
llmc                              Native selectable wizard, explicit alias
llms                              Spectre wizard, explicit alias
info                              Dashboard
info -Commands                    Full LocalBox + LocalBench command list
llmdocs                           Quick reference
llm-update [-InstallTui]           Update LocalBox + companions; optionally refresh TUI binaries
```

| Flag | Effect |
|------|--------|
| `-Ctx <name>` | One of the model's context keys (`32k`, `64k`, `128k`, `256k`). Omit for default. |
| `-LocalPilot` | Use LocalPilot instead of Claude Code. |
| `-Codex` | Use OpenAI Codex instead of Claude Code. |
| `-Strict` | Apply the strict engineering overlay (sampler + system prompt). Requires `Strict: true` on the model. |
| `-Mode <name>` | `native` / `turboquant` / `mtpturbo` — which llama-server binary to use. |
| `-KvK / -KvV` | Override the KV cache types passed to llama-server. |
| `-AutoBest` | Replay the latest saved tuner profile for this (model, ctx, mode). |
| `-Quant <name>` | Switch the model's selected GGUF quant (no launch). |

Quant keys are model-local labels, not a universal naming scheme. For example,
`mtp-apex` means the Genesis V2 MTP-enabled APEX GGUF file, while another model
may use a simpler `mtp` label when there is only one MTP variant. Use
`info <key>` to see the exact filename behind each quant key.

### 256 k context on a 24 GB card

The combination of **Qwen3-Coder-30B-A3B Heretic** (4 KV heads, 48 layers) at
the **IQ4_XS** quant with **q4_0 KV cache** is the only setup that fits a full
256k context on a single 4090:

```powershell
qcoder -Ctx 256 -Quant iq4xs                  # Claude Code @ 256k
qcoder -Ctx 256 -Quant iq4xs -LocalPilot      # LocalPilot @ 256k
```

Weights ~16.5 GB; q4_0 KV @ 256k ~6 GB; total ~23.6 GB.

---

## Adding a model

```powershell
addllm <hf-url-or-repo> -Key <key> [-Quants Q4_K_P,IQ4_XS] [-DefaultQuant Q4_K_P] [-Tier recommended]
```

`addllm` registers **every recognized GGUF quant** the HF repo publishes by
default (the `imatrix.gguf` calibration file is excluded). Pass `-Quants` only
when you want to filter the catalog entry to a subset. The GGUF itself is
downloaded on first launch.

Backfilling missing quants on an existing entry (rerunning HF discovery
without overwriting your manual `QuantNotes` / `ContextNotes`):

```powershell
updatellm <key>            # adds any HF quants missing from the entry
updatellm <key> -DryRun    # preview without writing
```

Removing a model:

```powershell
removellm <key>            # confirms first; deletes GGUF folder by default
removellm <key> -Force     # skip confirmation
removellm <key> -KeepFiles # keep the GGUF blobs on disk
```

---

## VRAM-aware tradeoffs

The launcher reads your GPU's VRAM and uses it to **tag every quant** with
`[fits]` / `[tight]` / `[over]` in `info` and the `llm` wizard, so you can see
at a glance which builds will load fully on your card.

VRAM resolves in this order:

1. `VRAMGB` set in `settings.json` or `llm-models.json` (top-level).
2. `nvidia-smi --query-gpu=memory.total` auto-detect (largest GPU on a multi-GPU box).
3. Fallback to 24.

The `info` dashboard shows the resolved value and source
(`auto` / `configured` / `fallback`).

```powershell
Set-LocalLLMSetting VRAMGB 32          # 5090
Set-LocalLLMSetting VRAMGB 48          # RTX 6000 Ada / dual-card aggregate
Set-LocalLLMSetting VRAMGB $null       # remove override, fall back to auto-detect
```

Per-quant tradeoffs come from two optional catalog fields:

- `QuantSizesGB` — file size per quant in GB (drives the fit badge).
- `QuantNotes` — human-readable note per quant (quality/use-case context). Shown verbatim.

Per-context guidance comes from `ContextNotes` in the same shape. Backfill
these on any model you add — they show up inline in `info` and the wizard.

---

## Per-machine settings (`settings.json`)

`llm-models.json` is the model **catalog** — committed, sharable. Per-machine
paths and preferences belong in a sibling `settings.json` at
`~/.local-llm/settings.json` (gitignored). It overlays top-level scalars from
`defaults.json` at load time, so you don't have to hand-edit `llm-models.json`
to fix paths on a fresh machine.

Use the helper instead of editing JSON:

```powershell
Set-LocalLLMSetting LocalPilotRoot '<path-to-localpilot>'   # else: sibling checkout, then ~/.local-llm/tools/localpilot
Set-LocalLLMSetting LocalBenchRoot '<path-to-localbench>'   # usually auto-set by install.ps1
Set-LocalLLMSetting LocalBoxRoot '<path-to-LocalBox>'        # auto-set by install.ps1
Set-LocalLLMSetting Default q36plus
Set-LocalLLMSetting VRAMGB 32                        # override auto-detect
Set-LocalLLMSetting LlamaCppDefaultMode native       # or 'turboquant' / 'mtpturbo'
Set-LocalLLMSetting LlamaCppMtpTurboRepo EsmaeelNabil/llama.cpp      # mtpturbo upstream
Set-LocalLLMSetting LlamaCppMtpTurboBranch feat/mtp-turboquant-kv-cache   # mtpturbo branch
Set-LocalLLMSetting LlamaCppMtpTurboCommit <sha>     # pin the mtpturbo build to an exact commit (not a force-pushable branch)
Set-LocalLLMSetting LlamaCppRequireDownloadPins $true # fail any binary download that has no recorded SHA-256 pin
Set-LocalLLMSetting LlamaCppNCpuMoe 35               # MoE expert CPU offload (default 35; 0 to disable)
Set-LocalLLMSetting LlamaCppMlock $false             # disable RAM locking (default $true)
Set-LocalLLMSetting LlamaCppNoMmap $false            # disable no-mmap (default $true)
Set-LocalLLMSetting LlamaCppAgentParallel 1          # agent slots (default 1; 0 = llama.cpp auto)
Set-LocalLLMSetting LlamaCppAgentCacheReuse 256      # prompt-cache reuse chunk size (default 256; 0 = llama.cpp default)
Set-LocalLLMSetting LocalModelMaxOutputTokens 4096   # cap local Claude/LocalPilot completions (0 = tool default)
Set-LocalLLMSetting LocalModelSkipPermissions $false # require Claude Code permission prompts (unset = first launch asks once)
Set-LocalLLMSetting LocalPilotRoot $null             # remove an entry
```

The `Models` and `CommandAliases` keys are catalog-only and rejected by
`Set-LocalLLMSetting`. Everything else is fair game.

### Verified binary downloads

LocalBox downloads `llama-server` binaries from third-party GitHub releases and
builds the `mtpturbo` binary from a fork branch. Out of the box, every binary
download is pinned and verified:

- **`defaults.json` ships pinned release tags** (`LlamaCppPinnedTag` for
  llama.cpp, `LlamaCppTurboquantPinnedTag` for turboquant) **and a
  `LlamaCppDownloadPins` table** with the SHA-256 of every asset those tags can
  install. Downloads target the pinned tag, and a checksum mismatch deletes the
  file and aborts the install.
- **`LlamaCppRequireDownloadPins` defaults to `true`**: an asset with no
  recorded pin is a hard failure. To opt out of pinning (trust-on-first-use:
  the download proceeds and prints its `sha256=...`), set it to `false`:
  `Set-LocalLLMSetting LlamaCppRequireDownloadPins $false`.
- **`LlamaCppMtpTurboCommit`** pins the from-source mtpturbo build to an exact
  commit instead of a force-pushable branch HEAD (also pre-set in
  `defaults.json`).

**Updating the pins** (e.g. to move to a newer llama.cpp build) is a deliberate
loop, done in `~/.local-llm/settings.json` (overrides the shipped defaults;
`LlamaCppDownloadPins` is a nested map, so edit the file directly rather than
via `Set-LocalLLMSetting`):

1. Pick the new tag on the release page and set `LlamaCppPinnedTag` (or
   `LlamaCppTurboquantPinnedTag`) to it.
2. Take each asset's SHA-256 from the GitHub release API's `digest` field
   (`https://api.github.com/repos/ggerganov/llama.cpp/releases/tags/<tag>`)
   and record it under `LlamaCppDownloadPins`, keyed by the exact asset
   filename. Cross-check one real download with
   `(Get-FileHash -Algorithm SHA256 <file>).Hash` if you want a second source.
3. Reinstall (`Install-LlamaServerNative -Force`) — the install fails loudly
   if a hash doesn't match.

The same keys in `settings.json` always win over `defaults.json`, so machine
pins can lead or lag the shipped ones.

### Per-workspace default model

Drop a `.llm-default` file in any directory containing a single line — a
model key, `ShortName`, or `Root`. `llmdefault` walks up from `$PWD` and uses
the nearest match. Falls back to settings → catalog `Default`.

```
echo q36p > .llm-default          # this workspace prefers Qwen 3.6 Plus
```

---

## MCP servers

Claude Code's MCP servers expose tools with names like `mcp__<server>__<tool>`.
They reach the local model through the same launch path:

- Models with `"LimitTools": false` (e.g. `dev`) get every MCP tool
  automatically — the `--tools` flag isn't passed.
- Models with `"LimitTools": true` (default) only see tools in the allowlist.
  Add the MCP tool names you want to either the global `LocalModelTools` field
  in `defaults.json` / `settings.json` or a per-model `Tools` override.

Example per-model override:

```json
"q36plus": {
  ...,
  "Tools": "Bash,Read,Write,Edit,Glob,Grep,mcp__filesystem__read_file,mcp__filesystem__write_file"
}
```

`info` shows a `Tools  : ...` line for any model that overrides the global list.

---

## LocalBench auto-tuner (`findbest`)

`findbest` is a LocalBox compatibility command that delegates tuning to
[LocalBench](https://github.com/C0deGeek-dev/LocalBench). LocalBench writes a
LocalBox-compatible result to `~/.local-llm/tuner/best-<key>.json`, and
`Start-ClaudeWithLlamaCppModel -AutoBest` replays that saved profile.

Standard catalog context aliases are `32k`, `64k`, `128k`, and `256k` unless a
model explicitly lacks support. AutoBest profiles are context-aware: the saved
entry records both `contextKey` and the resolved `contextTokens`, and launcher
selection still requires the same context key.

```powershell
# Tune q36plus at the 256k context preset, native llama.cpp, default budget.
# Default goal is coding-agent: long-prefill end-to-end latency.
findbest q36plus -ContextKey 256k

# Quick mode — only baseline + n-cpu-moe + batching (~10 trials)
findbest q36plus -ContextKey 256k -Quick

# Deep mode — normal phases, then finer local offload/batch/thread refinement
findbest q36plus -ContextKey 256k -Deep

# Default sampling is three runs per candidate; override when needed
findbest q36plus -ContextKey 256k -Runs 5

# Save both the fastest raw profile and a workstation-friendly balanced profile
findbest q36plus -ContextKey 256k -Profile both

# Force the expanded beam search and keep three survivors after each phase
findbest q36plus -ContextKey 256k -SearchStrategy beam -BeamWidth 3

# Optimize for prompt-eval (prefill) or generation explicitly
findbest q36plus -ContextKey 256k -Optimize prompt
findbest q36plus -ContextKey 256k -Optimize gen

# Allow KV cache variation. Native mode defaults to the model's current type;
# turboquant mode always also tests turbo3/turbo4 KV cache encodings.
findbest q36plus -ContextKey 256k -AllowedKvTypes q8_0,f16

# Try mismatched K/V pairs too, and allow an explicit quality trade if wanted
findbest q36plus -ContextKey 256k -AllowedKvTypes q8_0,q4_0 -AggressiveKv

# Power-user: tune separate short- and long-prefill profiles
findbest q36plus -ContextKey 256k -PromptLengths short,long

# Cache control. LocalBench reuses prior measurements across runs (keyed by a
# fingerprint of the build, prompt, and tuner version). After changing
# measurement/scoring code that doesn't move that fingerprint, force fresh
# numbers: -ClearTrialCache deletes the cache then repopulates it; -NoTrialCache
# ignores it for this run (no read, no write). The wizard ('findbest' menu item)
# also asks whether to clear the cache before tuning.
findbest q36plus -ContextKey 256k -ClearTrialCache
findbest q36plus -ContextKey 256k -NoTrialCache

# Inspect every trial run for a model
Show-LlamaCppTunerHistory -Key q36plus -Last 50
```

LocalBench may use fast `llama-bench` probes where supported, but turboquant
mode uses `llama-server` probes so `turbo3` / `turbo4` are measured through the
same binary LocalBox will actually launch. Upstream `llama-bench` has KV-cache
flags (`-ctk` / `-ctv`), but TurboQuant cache types only work in a fork/build
that registers them; LocalBox's turboquant path uses TheTom's fork.

`-Quant` selects the GGUF model file and stays fixed during a tuner run. `KvK`
and `KvV` are only runtime KV-cache encodings.

**Replaying the saved best:**

```powershell
Start-ClaudeWithLlamaCppModel -Key q36plus -ContextKey 256k -Mode native -AutoBest
Start-ClaudeWithLlamaCppModel -Key q36plus -ContextKey 256k -Mode native -AutoBest -AutoBestProfile balanced
```

The launcher matches the saved entry on `(key, contextKey, mode, profile,
prompt_length, quant, vramGB ± 1)` and a tuner-version stamp; `contextTokens`
is recorded as provenance for the actual `num_ctx` used by the run. On a miss
it warns and falls through to defaults. Caller-supplied `-KvCacheK` /
`-KvCacheV` / `-ExtraArgs` always win over the saved values.

Before handing an AutoBest llama.cpp session to Claude or LocalPilot, LocalBox
sends a tiny `/v1/messages` smoke request, including the same system prompt
used for the real launch, through the same Anthropic-compatible route. The
smoke must produce the requested visible answer; text hidden inside
`<think>...</think>` does not count. If the no-think proxy route fails,
LocalBox tries a direct llama-server route for that session. If both routes
fail, launch stops immediately instead of starting an unusable spinner-only
session.

In the wizard, choose **Find best settings** to run the same tuner
interactively, with prompts for normal vs deep tuning, pure vs balanced vs
both selection profiles, KV variation, saving the winner, and launching
immediately with `-AutoBest`.
When both pure and balanced profiles are saved, the launch-settings step shows
separate **Use balanced** and **Use pure** choices, plus **Use AutoBest** for
the default balanced-then-pure preference. Choose **Delete best settings**
from the same action menu to remove saved AutoBest entries before re-tuning.

After a matching best config has been saved, normal wizard launches for the
same `(model, quant, context, backend mode, VRAM)` automatically replay it and
skip the manual KV-cache picker.

---

## Wizard

`llm` launches the Spectre picker when `PwshSpectreConsole` is available. Use
`llmc` for the native selectable picker; it uses arrow keys + Enter, while
keeping number/letter shortcuts for fast selection.
It walks: model → quant → mode → vision → strict → context → action →
kvcache/AutoBest → launch.
Each step has a Back option (`0`/Escape in native, `[[Back]]` in Spectre); the
Spectre wizard wraps each prompt in `Invoke-LLMWizardStep` and logs the
full exception trace to `~/.local-llm/wizard-errors.log` if anything throws,
so a Spectre live-display refresh can't scroll the trace off screen. Inspect
with `llmlogerr [-Lines 80]`; reset with `llmlogerrclear`. The launch debug
trace (vision, proxy, llama-server, Claude launches) is recorded in
`~/.local-llm/launch.log` and tailable with `llmlog [-Lines 80]`.

After a model is selected, the Spectre wizard waits briefly before drawing the
next prompt and retries one fast-empty transition. Tune that guard with
`LOCAL_LLM_SPECTRE_PROMPT_COOLDOWN_MS` (default `500`, max `5000`).

`llms` launches the Spectre wizard explicitly. `llmc` remains an explicit
native-picker alias.

```powershell
$env:LOCAL_LLM_SPECTRE_PROMPT_COOLDOWN_MS = '750'
$env:LOCAL_LLM_NO_SPECTRE = '1'   # disable Spectre everywhere / make llm use native
```

---

## Terminal.Gui TUI

`llmtui` launches the C# Terminal.Gui frontend. It is currently an explicit
preview path; `llm` still opens the existing PowerShell wizard flow.

Build and install it from the repo:

```powershell
pwsh .\tui\publish-tui.ps1 -Install
reloadllm
llmtui
```

The main installer can publish TUI binaries too:

```powershell
.\install.ps1 -InstallTui
llm-update -InstallTui
```

Without `-InstallTui`, `install.ps1` offers to publish the TUIs interactively
unless `-SkipToolPrompts` is set. `llm-update` refreshes already-installed TUI
binaries after an update, and `-InstallTui` forces a refresh even when the
checkouts are already current.

When installed, the launcher runs `~/.local-llm/bin/LocalBox.Tui.exe` and passes
the active `LocalLLMProfile.ps1` path with `--profile`. From a repo checkout, it
can also run the TUI project directly with `dotnet run`, so the command works on
fresh developer machines before publishing.

Useful controls:

| Key | Action |
|-----|--------|
| `Up` / `Down` | Move in the active list. |
| `Enter` / `Right` | Advance through model -> context -> quant -> action -> mode -> AutoBest -> confirm. |
| `Left` | Go back one wizard step. |
| `Space` | Cycle the current step. |
| `Tab` | Move focus to details so long text can scroll. |
| `Ctrl+B` | Open LocalBench.Tui when LocalBench is installed and has a TUI build. |
| `F5` | Refresh backend data. |
| `F9` | Show dry-run launch command. |
| `F10` | Quit. |

`lbtui` opens LocalBench.Tui directly. It runs the LocalBench TUI project from a
checkout when available, otherwise it falls back to the published
`~/.local-llm/tools/localbench/bin/LocalBench.Tui.exe`.

---

## Casing convention

The repo mixes three styles intentionally:

- `kebab-case` for folders (`local-llm/`, `localbox-proxy/`) — matches their deployed path.
- `PascalCase` for the entry-point script (`LocalLLMProfile.ps1`) — PowerShell convention.
- `kebab-case` for data files (`llm-models.json`).

These names are user-visible (the deployed paths). Renaming them would break
setups, so they stay.

---

## Troubleshooting

- **Stale wizard / weird errors** → `llmlogerr` for the full trace; use
  `llmlog` for launch/debug details (vision, proxy, llama-server, Claude);
  `llmc` for the native picker or set `$env:LOCAL_LLM_NO_SPECTRE=1` to disable
  Spectre everywhere.
- **Spectre wizard stalls** → raise `$env:LOCAL_LLM_SPECTRE_PROMPT_COOLDOWN_MS`.
- **`localpilot` not on PATH** -> install the CLI with
  `cargo install localpilot-cli`.
- **Need to roll back to the Ollama era** → `git checkout ollama-classic` in
  the repo and re-run `install.ps1`.

---

## More

- `CHANGELOG.md` — what shipped, when.
