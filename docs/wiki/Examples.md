# Examples

Copy-pasteable launch recipes that match shipped behaviour at the current
`VERSION`. Each states what it does and what to expect.

> **Do not edit on github.com.** This wiki is generated from in-repo Markdown
> under `docs/wiki/` and synced one-way on every push to `main`. Edit the source
> in `docs/wiki/`; web edits are overwritten on the next sync.

## Code agent at 32k, via LocalPilot

```text
localbox launch qcoder30 --context 32k --agent localpilot
```

Starts `llama-server` with the Qwen3-Coder parser, brings up the no-think
proxy, points LocalPilot at it, and drops you into an agent session. On exit
the original environment is restored; the model keeps serving until
`localbox stop`.

## 256k context on a 24 GB card

```text
localbox launch qcoder30 --context 256k --quant iq4xs
localbox launch qcoder30 --context 256k --quant iq4xs --agent localpilot
```

Qwen3-Coder-30B-A3B at IQ4_XS with q4_0 KV cache is the combination that fits a
full 256k context on a single 4090 (weights ~16.5 GB; q4_0 KV @ 256k ~6 GB).

## Turbo KV via the fork binary

```text
localbox launch q36plus --mode turboquant
```

Uses the C0deGeek-dev turboquant `llama-server` fork (auto-downloaded on first
use) so the `turbo3`/`turbo4` KV-cache encodings are available; pick the KV
types one level down in the guided launcher's Customize menu.

## Serve a model to another machine

```text
# host
localbox serve qcoder30 --context 32k --lan --password chosenpass

# client (e.g. LocalPilot)
set ANTHROPIC_BASE_URL=http://<host-name>:11435
set ANTHROPIC_AUTH_TOKEN=chosenpass
localpilot
```

LAN/VPN only unless you terminate HTTPS in front of the gateway; an open
public gateway requires an explicit opt-in flag.

More launch detail: [usage.md](https://github.com/C0deGeek-dev/LocalBox/blob/main/docs/usage.md).
