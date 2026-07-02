# Harness mode

Part of the [LocalBox documentation](README.md).

A **harness** is the agent loop wrapping the model — the thing that turns raw
generation into "read this file, run that command, edit this code, then ask the
user". Claude Code is one such harness. LocalPilot is an independent,
clean-room harness with a similar operating model.

### Claude Code harness (default)

```text
localbox launch qcoder30 --context 32k
```

What happens:

1. LocalBox resolves the GGUF (downloads from Hugging Face on first use).
2. Starts `llama-server` on a free port (default search from 8080) with the
   per-model parser, KV-cache, MoE-offload, and reasoning flags.
3. Brings up the no-think proxy on `127.0.0.1:11435` in front of
   `llama-server`. The proxy is **in-process Rust** — the binary hosts it by
   re-invoking itself (`localbox nothink-proxy`); there is no Python sidecar.
   A proxy already serving the right target is reused; a stale one is
   repointed or reaped.
4. Smoke-tests the reply path with a tiny `/v1/messages` request that must
   produce visible text (output hidden in `<think>…</think>` does not count).
   A failed smoke aborts the launch instead of starting an unusable session.
5. Applies the agent environment (`ANTHROPIC_BASE_URL` at the proxy,
   model routing, output-token cap, a long API timeout for slow local
   prefill), launches `claude`, and **restores the original environment when
   the agent exits** — including variables that were previously unset.

The model believes it's Claude. Claude Code believes it's talking to
Anthropic. The proxy strips thinking blocks the local backend can't parse;
`--keep-thinking` lets them through instead. Strip-mode models also disable
reasoning generation at the server (`--reasoning off --reasoning-budget 0`);
the proxy stays as a defensive cleaner for leaked tags.

Whether Claude Code's per-action permission prompts are skipped is a
first-run decision that defaults to **off** — see
[settings.md](settings.md#launch-permission-and-bypass-decisions).

### LocalPilot harness

```text
localbox launch qcoder30 --context 32k --agent localpilot
```

Same flow, except LocalBox writes a `.localpilot.toml` provider block into
the working directory (endpoint, model, context window) and launches
`localpilot`. Add `--vision` to load the model's multimodal projector
(`--mmproj`); that launch also declares vision support in the generated
config so LocalPilot accepts image input with no hand edit.

### Codex harness

```text
localbox launch qcoder30 --context 32k --agent codex
```

Same flow, launching `codex` against the running server's OpenAI-compatible
`/v1` endpoint.

### Serve headless

```text
localbox serve q36plus --context 64k
```

Starts the model (and the no-think proxy) and returns without attaching an
agent — for a separate `localpilot` run, a script, CI. The endpoint stays up
until `localbox stop`. Loopback only by default.

`localbox status` reports the health tri-state (proxy and server up /
proxy up but its upstream down / fully down) with the remedy; a bare `502`
from the proxy means its upstream model server is down — run
`localbox stop`, then serve again.

### Serve to other machines (`--lan`)

```text
localbox serve q36plus --context 64k --lan --password chosenpass
```

`--lan` binds the gateway on `0.0.0.0`. A password is required: every
forwarding request must present it (`Authorization: Bearer` or `x-api-key`);
`/health` stays open for target checks. Serving without a password is
refused unless you explicitly opt in with `--allow-public-no-auth`.

On the client, no LocalBox helper is required — set the Anthropic-compatible
environment for your agent:

```bash
export ANTHROPIC_BASE_URL="http://<host>:11435"
export ANTHROPIC_AUTH_TOKEN="chosenpass"
export ANTHROPIC_API_KEY="chosenpass"
localpilot
```

Password-only HTTP is convenient for LAN testing. Over a public IP it is not
encrypted: the password and prompts can be observed in transit unless you put
a VPN or HTTPS reverse proxy in front of it.

### CPU embedding server

A separate, small server for **embeddings** — distinct from the chat model.
Some consumers (e.g. LocalMind's semantic memory dedup and retrieval rerank)
need an OpenAI-compatible `POST /v1/embeddings` endpoint:

```text
localbox embed-serve             # serve the embedding model on 127.0.0.1:8090 (CPU)
localbox embed-stop              # stop it (leaves the chat server alone)
```

It is deliberately independent of the chat server: its own port (8090 by
default), its own process, its own lifecycle state — `localbox stop` and
`localbox embed-stop` never touch each other's server. Run both and point a
consumer's **chat** endpoint at the model server and its **embedding**
endpoint at 8090.

**Why CPU-only (`-ngl 0`).** The chat model already fills most of a 24 GB
card. A GPU-resident embedding model would steal VRAM from the chat model
only, so any benchmark pairing the two would silently compare a degraded
chat model. Forcing embeddings onto the CPU keeps the chat model
byte-identical whether or not embeddings run; embeddings here are not
latency-critical.

The default model is **Qwen3-Embedding-0.6B** (GGUF `Q8_0`, ~639 MB,
Apache-2.0, served with `--pooling last`), downloaded on first use. Swap it
via `EmbedModelRepo` / `EmbedModelFile` / `EmbedModelRoot` (and optionally
`EmbedPort` / `EmbedPooling`) in `~/.local-llm/settings.json`.

### Strict overlay (engineering mode)

Models in the catalog can declare `Strict: true` as their default, and the
guided launcher's Customize menu offers a strict toggle per launch. Strict
injects a tighter sampler and a non-negotiable engineering system prompt
(no mocks, no stubs, no placeholder implementations, reuse existing
architecture — stop and explain rather than invent a substitute).

> **When to use it.** Strict is for actual engineering work where the model's
> lazy paths (mock, stub, "// TODO", placeholder JSON) cost real time. Skip it
> for chat, brainstorming, RAG-style Q&A.

---
