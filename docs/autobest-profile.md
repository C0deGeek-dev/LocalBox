# LocalBox AutoBest Profile Contract

The guided launcher's auto-tune replay loads saved profiles from:

```text
~/.local-llm/tuner/best-<key>.json
```

Every launch path—including headless `localbox serve <model>`, `launch`, the
guided launcher, and a LocalPilot-triggered serve—consults the same resolver
and applies a compatible saved entry automatically. The entry supplies its
quant/context/mode and tuned launch params; explicit
`--quant`/`--context`/`--mode` remain filters and override those fields.

If no compatible profile is usable, LocalBox shows the exact reason and source
path before it can use catalog/settings defaults. Interactive CLI and guided
launches ask with a default-no choice. A non-interactive caller must pass
`--allow-untuned` after presenting that warning to its user. Use
`--no-auto-best` when defaults are the deliberate policy; the older
`--auto-best` option remains accepted as a strict compatibility spelling and
refuses fallback. When the best match is an older measurement generation, the
guided launcher also offers to adopt its still-runnable settings, and
`--adopt-tune` makes that same explicit choice for `launch` or `serve`. Run
`localbox models` for the concise human catalog or `localbox models --json` for
the versioned schema-1 contract consumed by LocalPilot.

The current compatibility schema is `localbox-autobest-v1`. The top
level object keeps launcher-owned routing fields and an `entries` array. Each
entry is matched at launch time using:

- `contextKey`
- `mode`
- `profile` (`pure` when omitted)
- `prompt_length` (`short` when omitted)
- `quant`
- `vramGB` within +/- 1 GB
- `tuner_version` (must match the shared current measurement version)
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

Tuner version 5 is the current launch-time profile generation. Version-5
measurements use templated chat and the same single-session defaults LocalBox
serves. Older and newer entries remain readable and stay on disk, but are not
replayed without a current entry. For an older match only, LocalBox can adopt
the runnable settings into version 5 after explaining that the old score is a
different quantity. It updates only that entry, writes
`localbox_adopted_from_tuner_version` and `localbox_adopted_at`, and preserves
the previous document as `best-<key>.json.bak`. A future-version entry is never
adoptable. The store schema remains version 1 because these are tolerated
provenance fields, not a shared contract change. AutoBest uses
`coding_agent_e2e_tps` by default, so it prefers long-prefill, end-to-end
latency over decode-only generation TPS. Expanded LocalBench entries can be
saved as `pure` or `balanced`; entries without a `profile` field are treated as
`pure` for backwards compatibility.

The `models --json` run-profile object keeps `source: "tuned"` for both kinds
of usable entry and distinguishes them with `origin: "measured"` or
`origin: "adopted"`. Adopted entries also report
`adopted_from_tuner_version` and `adopted_at`.

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

Every llama.cpp launch path — CLI `launch` and `serve`, the guided launcher,
and the native retry — applies `--parallel 1` and `--cache-reuse 256` by
default, outside the saved tuner override set, through one shared finalizer.
This keeps title/smoke/sidebar requests from competing with the main session
across multiple slots and gives repeated large prompts a stable cache path.
Override these with the `LlamaCppAgentParallel` and `LlamaCppAgentCacheReuse`
keys in `settings.json`; a non-positive value (`0` or `-1`) opts into
llama.cpp's own default for that flag. Beware that llama-server's own parallel
default is multi-slot auto, which allocates the full configured context per
slot — roughly 4× the KV-cache memory the launch was sized for.

Local Claude/LocalPilot launches also set
`CLAUDE_CODE_MAX_OUTPUT_TOKENS` from `LocalModelMaxOutputTokens` (default
`16384`) before starting the client. This prevents local models from silently
accepting the hosted Claude default of 32k output tokens for ordinary turns,
while leaving headroom for long code-heavy replies. A larger cap only makes a
reply that actually needs it take longer to decode locally — it does not
reserve extra VRAM/KV-cache (that is sized by the model's context window, a
separate setting). If replies still stop mid-sentence, raise
`LocalModelMaxOutputTokens` further in `settings.json`, or set it to `0` to
leave the client's own default (32k) untouched.

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
