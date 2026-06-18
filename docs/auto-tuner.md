# LocalBench auto-tuner (findbest)

Part of the [LocalBox documentation](README.md).

`findbest` is a LocalBox compatibility command that delegates tuning to
[LocalBench](https://github.com/C0deGeek-dev/LocalBench). LocalBench writes a
LocalBox-compatible result to `~/.local-llm/tuner/best-<key>.json`, and
`Start-ClaudeWithLlamaCppModel -AutoBest` replays that saved profile.

The functions, parameters, shared file formats, and version gates the two
repos exchange are governed by the formal contract LocalBench owns:
[`docs/launcher-contract.md`](https://github.com/C0deGeek-dev/LocalBench/blob/main/docs/launcher-contract.md).
Both repos' CI assert conformance against the real counterpart, so a breaking
change on either side fails that side's build.

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
that registers them; LocalBox's turboquant path uses the C0deGeek-dev fork.

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
