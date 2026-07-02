# LocalBench auto-tuner (findbest)

Part of the [LocalBox documentation](README.md).

Tuning belongs to
[LocalBench](https://github.com/C0deGeek-dev/LocalBench): its `findbest`
command measures candidate configurations live through the launcher contract
and writes a LocalBox-compatible result to `~/.local-llm/tuner/best-<key>.json`.
LocalBox's guided launcher replays that saved profile as the auto-tuned
configuration.

The trait, envelope, shared file formats, and version gates the two repos
exchange are governed by the formal contract LocalBench owns:
[`docs/launcher-contract.md`](https://github.com/C0deGeek-dev/LocalBench/blob/main/docs/launcher-contract.md).
Both repos' CI assert conformance against the real counterpart, so a breaking
change on either side fails that side's build.

Standard catalog context aliases are `32k`, `64k`, `128k`, and `256k` unless a
model explicitly lacks support. AutoBest profiles are context-aware: the saved
entry records both `contextKey` and the resolved `contextTokens`, and launcher
selection still requires the same context key.

```text
# Tune a model at the 64k context preset, native llama.cpp, default budget.
# Default goal is coding-agent: long-prefill end-to-end latency.
localbench findbest --model q36plus --context 64k

# Optimize for prompt-eval (prefill) or generation explicitly
localbench findbest --model q36plus --context 64k --optimize prompt
localbench findbest --model q36plus --context 64k --optimize gen

# Save both the fastest raw profile and a workstation-friendly balanced one
localbench findbest --model q36plus --context 64k --profile both

# Default sampling is three runs per candidate; override when needed
localbench findbest --model q36plus --context 64k --runs 5

# Bound the search (trial budget, clamped to [1, 100])
localbench findbest --model q36plus --context 64k --budget 20

# Measure without saving a profile
localbench findbest --model q36plus --context 64k --no-save

# Cache control. Decisive measurements persist across runs in a fingerprinted
# trial cache (tuner/trial-cache-<key>[-<context>].json); a repeated or
# interrupted tune reuses them. The cache invalidates itself — naming the
# differing fields — when anything shaping a measurement changes (model file,
# runs, optimize goal, tuner version, ...). Skip it for one run with:
localbench findbest --model q36plus --context 64k --no-cache
```

Candidates are measured through `llama-server` — the same binary LocalBox will
actually launch — so turboquant KV encodings (`turbo3`/`turbo4`) are measured
through the fork that registers them, never approximated with mainline
`llama-bench` numbers.

`--quant` selects the GGUF model file and stays fixed during a tuner run; KV
cache types are only runtime encodings.

**Replaying the saved best:**

The guided launcher (`localbox`) replays a saved profile automatically when
auto-tune is on: the entry is matched on quant, context key, and mode, with
the wanted profile first, then the nearest measured VRAM, then score. An
unsupported store schema fails closed to the recommended defaults, and manual
KV choices only fill gaps the profile left. The Customize menu's auto-tune
item points at `localbench findbest` when no profile matches.

Before handing a session to an agent, LocalBox sends a tiny `/v1/messages`
smoke request through the same route the agent will use. The smoke must
produce the requested visible answer; text hidden inside `<think>...</think>`
does not count. If the reply is degenerate, the launch stops with a
plain-language explanation instead of starting an unusable session.

---
