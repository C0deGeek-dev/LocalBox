# Harness mode

Part of the [LocalBox documentation](README.md).

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

### Headless local serve

`llmserve` (above) is for serving *off-box* over the LAN. To run the model on
**this** machine for a separate local agent process — a `localpilot` CLI run, a
script, CI — use `llmdefaultserve`. It launches the configured default model
(the same `llmdefault` recipe) as a background `llama-server` plus the loopback
no-think proxy, runs a visible-response smoke test, prints the endpoint/PID, and
returns **without attaching an interactive agent**:

```powershell
llmdefaultserve            # serve the default-recipe model, headless
llmdefaultserve -WhatIf    # dry-run: print the plan, launch nothing
llmstop                    # stop the server (and proxy) when done
```

This exists because the agent-attaching launches (`llmdefault`, `llm` →
`LocalPilot`/`Claude`) start the server and proxy and then **tear them down when
the attached agent exits** — fine for an interactive session, but it pulls the
endpoint out from under a separate CLI you wanted to drive. `llmdefaultserve`
leaves the endpoint up until `llmstop`. It binds loopback only (no `0.0.0.0`, no
password); for off-box access use the serve gateway instead.

The `-WhatIf` / `-DryRun` preview renders the **same** recipe the live launch
runs, including the selected quant: when the default recipe pins a non-default
quant, the preview shows that GGUF and its KV-cache/AutoBest args (not the model's
default quant). The preview commits no session state.

**Troubleshooting a `502 Bad Gateway`.** If a request to the proxy
(`127.0.0.1:11435`) returns a bare `502`, the proxy is up but its upstream model
server (`127.0.0.1:8080`) is down — typically a stale proxy left over from a prior
session. Restart the stack:

```powershell
llmstop; llmdefaultserve
```

`llmdefaultserve` surfaces this state automatically if its post-launch smoke test
fails (a bounded, non-blocking check that never delays the launch).

### CPU embedding server

A separate, small server for **embeddings** — distinct from the chat model
above. Some consumers (e.g. LocalMind's semantic memory dedup and retrieval
rerank) need an OpenAI-compatible `POST /v1/embeddings` endpoint. `llmembedserve`
serves a GGUF embedding model for that, **on the CPU** so it costs **zero GPU
VRAM**:

```powershell
llmembedserve              # serve the embedding model on 127.0.0.1:8090 (CPU)
llmembedserve -WhatIf      # dry-run: print the exact llama-server command, launch nothing
llmembedstop               # stop the embedding server (leaves the chat server alone)
```

It is deliberately independent of `llmdefaultserve`: its own port (`8090` by
default), its own process, its own lifecycle state — so `llmstop` /
`llmembedstop` never touch each other's server. The two pair up: run both, and a
consumer points its **chat** endpoint at `8080` and its **embedding** endpoint at
`8090`.

**Why CPU-only (`-ngl 0`).** The chat model already fills most of a 24 GB card.
A GPU-resident embedding model would steal VRAM **from the chat model only**, so
any benchmark pairing the two (e.g. the LocalBench warm arm) would silently run a
degraded chat model on the embeddings side and the comparison would no longer be
fair. Forcing embeddings onto the CPU keeps the chat model byte-identical whether
or not embeddings run. Embeddings here are not latency-critical (memory dedup and
retrieval, not the solve loop), so the CPU cost is irrelevant.

The default model is **Qwen3-Embedding-0.6B** (GGUF `Q8_0`, ~639 MB, Apache-2.0,
1024-dim, served with `--pooling last`). It is acquired on first run into the
models dir (`acquire-don't-vendor` — never committed) and reused thereafter. To
swap in a different embedding model (e.g. the `nomic-embed-text-v1.5` fallback),
set `EmbedModelRepo` / `EmbedModelFile` / `EmbedModelRoot` (and optionally
`EmbedPort` / `EmbedPooling`) in `settings.json` — no code change.

The launcher binds **loopback only**. Verify it is up and serving vectors with:

```powershell
Test-LocalLLMEmbedEndpoint     # POSTs a probe input; returns the vector dimension (0 = down)
```

**Per-OS note (tier-1 parity).** The served command is the same on every
platform — only the binary name differs:

```text
# Windows
llama-server.exe -m <gguf> --embeddings -ngl 0 --host 127.0.0.1 --port 8090 --pooling last
# Linux / macOS
llama-server     -m <gguf> --embeddings -ngl 0 --host 127.0.0.1 --port 8090 --pooling last
```

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
