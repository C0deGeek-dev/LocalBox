# Examples

Copy-pasteable launch recipes that match shipped behaviour at the current
`VERSION`. Each states what it does and what to expect.

> **Do not edit on github.com.** This wiki is generated from in-repo Markdown
> under `docs/wiki/` and synced one-way on every push to `main`. Edit the source
> in `docs/wiki/`; web edits are overwritten on the next sync.

## Code agent at 32k, via LocalPilot

```powershell
qcoder -Ctx 32k -LocalPilot
```

Starts `llama-server` with the Qwen3-Coder parser, brings up the no-think proxy,
points LocalPilot at it, and drops you into an agent session. On exit the
original environment is restored and both processes stop.

## 256k context on a 24 GB card

```powershell
qcoder -Ctx 256 -Quant iq4xs                  # Claude Code @ 256k
qcoder -Ctx 256 -Quant iq4xs -LocalPilot      # LocalPilot @ 256k
```

Qwen3-Coder-30B-A3B at IQ4_XS with q4_0 KV cache is the combination that fits a
full 256k context on a single 4090 (weights ~16.5 GB; q4_0 KV @ 256k ~6 GB).

## Turbo KV via the fork binary

```powershell
q36p -Mode turboquant -KvK turbo4 -KvV turbo4
```

Uses TheTom's turboquant `llama-server` fork (auto-downloaded on first use) so
the `turbo4` KV-cache encodings are available.

## Serve a model to another machine

```powershell
# host
$env:LOCAL_LLM_SERVE_PASS = "chosenpass"
llmserve -Key qcoder30 -ContextKey 32k -LlamaCppMode native

# client (e.g. LocalPilot)
$env:ANTHROPIC_BASE_URL = "http://<host-ip>:11435"
$env:ANTHROPIC_AUTH_TOKEN = "chosenpass"
localpilot
```

LAN/VPN only unless you terminate HTTPS in front of the gateway.

More launch detail: [usage.md](https://github.com/C0deGeek-dev/LocalBox/blob/main/docs/usage.md).
