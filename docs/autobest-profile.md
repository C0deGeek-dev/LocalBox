# LocalBox AutoBest Profile Contract

The guided launcher's auto-tune replay loads saved profiles from:

```text
~/.local-llm/tuner/best-<key>.json
```

The current compatibility schema is `localbox-autobest-v1`. The top
level object keeps launcher-owned routing fields and an `entries` array. Each
entry is matched at launch time using:

- `contextKey`
- `mode`
- `profile` (`pure` when omitted)
- `prompt_length` (`short` when omitted)
- `quant`
- `vramGB` within +/- 1 GB
- `tuner_version` when present
- `vision` (text-only when omitted)

Entries also record `contextTokens` as provenance for the resolved `num_ctx`;
`contextKey` remains the launch-time match key.

Vision and text-only profiles are not interchangeable (the mmproj module shifts
VRAM use and behaviour), so an exact `vision` match is always preferred. Because
no current tuning path records a vision-tuned entry, a vision launch
(--vision) would otherwise never match. To keep AutoBest usable, a vision
launch falls back to the matching text-only tune and prints a warning that the
tune was measured without the mmproj (so VRAM headroom is tighter — raise
`--n-cpu-moe` or launch without vision if you hit OOM). A non-vision launch is
unaffected and only matches text-only entries.

Entries must include an `overrides` object whose keys map onto the
server-argument builder's parameters. The currently accepted tuning override keys are:

- `KvK`
- `KvV`
- `NGpuLayers`
- `NCpuMoe`
- `Mlock`
- `NoMmap`
- `UbatchSize`
- `BatchSize`
- `Threads`
- `ThreadsBatch`
- `FlashAttn`
- `SplitMode`

Tuner version 4 is the current launch-time profile generation. It invalidates
older saved profiles and uses `coding_agent_e2e_tps` by default, so AutoBest
prefers long-prefill, end-to-end latency over decode-only generation TPS.
Expanded LocalBench entries can be saved as `pure` or `balanced`; entries
without a `profile` field are treated as `pure` for backwards compatibility.

Replay defaults to the auto preference, which prefers `balanced` entries when available and
falls back to `pure`. The guided launcher's Customize menu forces the selection profile explicitly.

After a saved profile is applied and llama-server is healthy, LocalBox performs
a small Anthropic-compatible `/v1/messages` launch smoke request before handing the session to the agent. The smoke includes the real launch system
prompt and must produce visible response text; output inside `<think>...</think>`
is ignored for this check. For strip-mode models this first
uses the no-think proxy, matching the normal launch route. llama.cpp strip-mode
launches also disable reasoning generation with `--reasoning off` and
`--reasoning-budget 0`; the proxy remains as a defensive cleaner for any leaked
tags. If that proxy route does not produce visible text, LocalBox tries the
direct llama-server route for the same session. If neither route succeeds,
AutoBest launch aborts so a high-throughput profile cannot silently become an
unusable interactive session. The smoke request timeout defaults to 300 seconds
and can be overridden with `LlamaCppSmokeTestTimeoutSec` in `settings.json`.

Claude/LocalPilot llama.cpp launches are single-session agent workloads, so the
launcher also applies `--parallel 1` and `--cache-reuse 256` by default outside
the saved tuner override set. This keeps title/smoke/sidebar requests from
competing with the main agent turn across multiple slots and gives repeated
large prompts a stable cache path. Override these with the `LlamaCppAgentParallel` and `LlamaCppAgentCacheReuse` keys in `settings.json`; set either value to `0` to
fall back to llama.cpp defaults for that flag.

Local Claude/LocalPilot launches also set
`CLAUDE_CODE_MAX_OUTPUT_TOKENS` from `LocalModelMaxOutputTokens` (default
`4096`) before starting the client. This prevents local models from accepting
the hosted Claude default of 32k output tokens for ordinary turns. Set the `LocalModelMaxOutputTokens` key in `settings.json` to change it, or `0` to
leave the client default untouched.

The guided launcher exposes saved selection profiles directly: when both `balanced` and `pure` entries exist, Customize offers explicit profile choices in addition to the `auto` preference (`balanced`, then `pure`).

LocalBench-compatible exports add provenance without changing the launch-time
reader:

- `source = "localbench"`
- `localbench_version`
- `localbench_profile_path`
- `report_path`
- `launcher_export_version`
- `contextTokens`

Expanded LocalBench exports also store selection metadata and optional
diagnostics:

- `profile`
- `searchStrategy`
- `beamWidth`
- `pureScore`
- `telemetry`
- `scoreBreakdown`

Staleness checks continue to read `gpu_names` and `llamacpp_build` from each
entry.
