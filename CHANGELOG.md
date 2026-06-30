# Changelog

Past-tense record of shipped changes.

## Unreleased

- **`Start-LocalPilot -UseVision` now actually loads the vision projector, and
  auto-declares it to LocalPilot.** The LocalPilot agent launch previously
  hardcoded an empty projector path, so `-UseVision` set the AutoBest profile but
  never passed `--mmproj` to `llama-server` — an image-capable model ran text-only.
  It now resolves the model's `mmproj.gguf` (mirroring the Claude Code launch),
  guarded by an availability check so a missing projector gives a clear message
  and a text-only launch rather than a broken `--mmproj`. A real launch downloads
  the projector on demand; a `-DryRun` preview resolves the expected path without
  downloading. On that vision launch LocalBox also writes `supports_vision = true`
  into the `[providers.local]` block of the generated `.localpilot.toml`, so
  LocalPilot accepts image input zero-config. The default (no `-UseVision`) path
  loads no projector and declares nothing — unchanged. See `docs/harness-mode.md`
  → "LocalPilot harness".

## v1.1.0 - 2026-06-29

Coordinated LocalX release.

- **A CPU-only embedding server (`llmembedserve`).** A small, self-contained
  sibling of `llmdefaultserve` that serves a GGUF embedding model through
  llama-server's OpenAI-compatible `POST /v1/embeddings` on a dedicated loopback
  port (`8090` by default), forced onto the CPU (`-ngl 0`) so it costs **zero GPU
  VRAM**. That CPU rule is load-bearing for fair benchmarking: a GPU-resident
  embedding model would steal VRAM from a chat model running alongside it, so a
  benchmark pairing the two would see a degraded chat model on the embeddings side
  only — keeping embeddings on the CPU leaves the chat model byte-identical. The
  server has its own port, process, and lifecycle (`llmembedstop`), independent of
  the chat server. Default model: **Qwen3-Embedding-0.6B** (GGUF `Q8_0`,
  Apache-2.0, 1024-dim, `--pooling last`), acquired on first run into the models
  dir (never committed) and overridable via `EmbedModelRepo`/`EmbedModelFile`/
  `EmbedModelRoot`/`EmbedPort`/`EmbedPooling` in `settings.json`. `-WhatIf` renders
  the exact served command without acquiring a model or launching anything;
  `Test-LocalLLMEmbedEndpoint` probes a running server and returns the vector
  dimension. See `docs/harness-mode.md` → "CPU embedding server".

- **no-think proxy v0.4.0 — normalizes system messages so strict (qwen-family)
  chat templates accept Anthropic agentic clients.** llama.cpp's qwen3 template
  hard-rejects any system message that is not the first message
  (`raise_exception('System message must be at the beginning')`), surfacing as
  `400 Unable to generate parser for this template`. Anthropic clients such as
  Claude Code put the base prompt in the top-level `system` field **and** can
  inject a second `role: system` message inside `messages` (e.g. a SessionStart
  hook); llama.cpp renders both, so the in-array one lands second and the
  template raises. The proxy now folds any in-array system message **into** the
  top-level `system` field (preserving existing content blocks and their
  `cache_control`) and removes it from `messages`; for OpenAI-form requests
  (no top-level `system`) it collapses misplaced/duplicate system messages into
  a single leading system message. Default-on; opt out with
  `NO_THINK_PROXY_MERGE_SYSTEM=0`. `defaults.json`
  `NoThinkProxyRequiredVersion` bumped to `0.4.0`. This unblocks driving the
  default local model from Claude Code (and any Anthropic-native harness)
  through `llmdefaultserve`. (The no-think proxy remains deprecated and kept one
  release for non-LocalPilot clients.)

## v1.0.0 - 2026-06-24

Coordinated LocalX 1.0 release. First stable launcher surface.

- **Dead-code cleanup: removed 7 unused PowerShell helpers.** No call sites
  remained for `Convert-ToPosixPath`, `Get-HuggingFaceModelFiles`,
  `Get-LlamaCppTemplatesDir`, `Set-LocalBoxTuiSetting`, `Invoke-LocalBoxTuiLaunch`,
  `Get-LocalBenchLauncherBestConfig`, or `Get-LocalBenchLauncherBestConfigCandidates`.
  Also dropped the stale `local-llm/bench-history.jsonl` `.gitignore` entry
  (nothing writes it). No behaviour change. (The `Ensure-LlamaBenchExe` /
  `Ensure-LlamaPerplexityExe` finders/installers were left in place: they are part
  of the LocalBench launcher contract surface, not dead.)
- **`llmdefaultserve -DryRun` now previews the recipe that actually launches.** When the
  DefaultLaunch recipe selects a non-default quant, the dry run used to show the model's
  *default* quant (e.g. `APEX-Balanced.gguf` with `q8` KV-cache args) while the live
  launch ran the selected one (e.g. `APEX-I-Quality.gguf` with `turbo3` args) — the quant
  was applied only on the live path. The selected quant is now resolved for both paths, so
  the preview renders the same GGUF + AutoBest/KV-cache recipe the live launch consumes; a
  dry run reverts the change afterwards, so previewing still commits no session state.
- **Stale no-think proxy is now diagnosable instead of a bare 502.** When the no-think
  proxy is up but the upstream model server is down, a request returned only
  `502 Bad Gateway`. A bounded, non-blocking health probe now distinguishes that stale
  state from a fully-down or healthy stack and recommends the fix (`llmstop;
  llmdefaultserve`); it is surfaced when a headless serve's smoke test fails, and never
  blocks the launch.

- **`llmdefaultserve` — headless model serve for CLI / agent / CI.** Brings up the
  DefaultLaunch model as a background llama-server + no-think proxy (loopback) with a
  visible-response smoke test and does **not** attach an interactive agent or tear the
  endpoint down on exit — unlike `llmdefault`, whose agent-attach (`Start-LocalPilot`)
  stops the server and proxy when the attached process exits. For driving the model from
  a separate `localpilot`/`claude` process. Distinct from the LAN serve gateway
  `llmserve` (binds 0.0.0.0 with auth); this one is loopback-only. Stop with `llmstop`.
- **Fixed the `-DryRun` launch-plan "Agent argv" error.** An empty extra-args list
  arrives as a nested empty array element (helpers return `,$extras`); the real launch
  splats it away, but the preview's `Format-LocalLLMArgvLine` failed to bind it as a
  string. The formatter now flattens nested elements, so the preview renders the agent
  command line cleanly.

- **Removed the legacy bench history viewer.** The `obench` command and its
  `70-bench.ps1` module are gone — LocalBench owns benchmarking. The old
  `~/.local-llm/bench-history.jsonl` (written by the retired `ospeed` helper) is
  no longer read; use LocalBench for benchmarking and tuning.

## v0.3.0-beta.3 - 2026-06-18

### 2026-06-19 - Launch safety

- **Bypass is no longer a default.** Launching LocalPilot through LocalBox no
  longer passes `--bypass` unconditionally, and Codex no longer defaults
  `--dangerously-bypass-approvals-and-sandbox` to on. Each is now a first-run,
  persisted decision (`LocalPilotBypass` / `CodexBypassApprovalsAndSandbox`) that
  **defaults off and fails closed in non-interactive sessions**, mirroring the
  existing `LocalModelSkipPermissions` prompt. The active posture is shown in
  `Show-LocalBoxSecuritySummary` and every `-DryRun` launch plan, and an env
  override (`LOCAL_LLM_LOCALPILOT_BYPASS` / `LOCAL_LLM_CODEX_BYPASS`) applies per
  launch. See [`docs/settings.md`](docs/settings.md). This restores parity with
  LocalPilot's "bypass is never the default" posture for users who enter through
  LocalBox.
- **Codex bypass default fully enforced at config load.** `Import-LocalLLMConfig`
  no longer injects `CodexBypassApprovalsAndSandbox = $true` for an unset key, which
  had quietly re-enabled the default-on posture for a fresh machine even though the
  launch path itself never prompted. The key now stays absent when unset — exactly
  like `LocalPilotBypass` — so the security summary reads "undecided" and the
  resolver reaches its first-run prompt (interactive) or fails closed
  (non-interactive). A regression test loads the real merged config (not a stubbed
  one) to keep this from regressing again.
- Fixed the LocalPilot install hint thrown by `Start-LocalPilot`: the crate is
  `cargo install localpilot` (not `localpilot-cli`), matching the troubleshooting
  doc.

- Repinned the turboquant llama.cpp build to the forked `tqp-v0.2.0` (`b9901`,
  lazy-grammar), fixing the `q3635ba3bapex` 400 `empty grammar stack after
  <think>` failure on constrained-decoding requests. Dropped the stale TheTom
  turboquant refs.

### 2026-06-18 - Versioning policy

- Documented the SemVer policy in [`docs/versioning.md`](docs/versioning.md):
  break → major, add → minor, fix → patch, plus the pre-1.0 rules (breaking
  changes bump the minor `0.MINOR`; features and fixes bump the patch) and the
  deprecation-vs-removal distinction. Linked from the docs index.

### 2026-06-18 - Deprecation

- Marked `localbox-proxy/no-think-proxy.py` **deprecated** (scheduled for
  removal). LocalPilot strips `<think>` blocks natively on both provider paths
  and suppresses the thinking request shape itself, so the out-of-band proxy is
  no longer needed for the LocalBox → LocalPilot path.

### 2026-06-17 - Documentation restructure

- Split the long top-level `README.md` into a lean overview plus owned `docs/`
  pages (install, harness mode, llama.cpp modes, usage, model management,
  settings, MCP, auto-tuner, wizard/TUI, architecture, troubleshooting), indexed
  by `docs/README.md`.
- Fixed the LocalPilot install hint in troubleshooting: the crate is
  `cargo install localpilot` (not `localpilot-cli`).
- Added an in-repo wiki source (`docs/wiki/`) that is one-way CI-synced to the
  GitHub Wiki, plus an offline link check over the docs.

## v0.3.0-beta.2 - 2026-06-15

Coordinated LocalX beta release.

- Serve gateway now discloses its safe operating posture (LAN/VPN-only, HTTPS in
  front) in the client instructions and the security banner; the guard still
  refuses open public HTTP.
- Conformed the bundled tuner best-config fixture to the current
  `tuner-best-config` schema (added the required `profile` field) so producer and
  consumer agree on the versioned contract.

- **`llm-update` now refreshes installed artifacts after source updates.**
  LocalPilot is reinstalled from its updated checkout when it fast-forwards, so
  the `localpilot` CLI on `PATH` no longer lags behind the repo. Added
  `-RefreshInstalled` to force redeploy/rebuild from already-current checkouts;
  `Update-LocalPilot -RefreshInstalled` does the same for the standalone helper.

## 2026-06-12 - v0.3.0-beta.1 LocalX release train

- Aligned LocalBox with the coordinated LocalX `v0.3.0-beta.1` beta release.
- Kept the LocalBench launcher contract binding and updated the no-think proxy
  required version marker for this release train.

## 2026-06-03 - Serve gateway rename

- **Remote launch target renamed to Serve.** The TUI action contract now uses `serve`, the command is `llmserve`, the password environment variable is `LOCAL_LLM_SERVE_PASS`, and the docs describe the gateway as serving any Anthropic-compatible agentic client, with LocalPilot as one example.

## 2026-05-24 — Terminal.Gui TUI and catalog polish

- **`llmtui` documented as the explicit Terminal.Gui preview path.** README now covers publish/install, profile resolution, core navigation keys, and the LocalBench handoff key.
- **Spectre model catalog layout fixed.** The dashboard now uses explicit column widths and a compact quant list so the context column stays readable instead of wrapping to one or two characters.
- **MTP catalog contexts expanded.** `q3535ba3bmtp` and `genesisv2` now expose an explicit `256k` context key while keeping default context at `128k`.
- **Quant naming clarified.** README notes that labels like `mtp`, `mtp-apex`, and `mtp-q8kp` are model-local quant keys that map to concrete GGUF filenames.
- **TUI packaging added.** `install.ps1 -InstallTui` publishes LocalBox.Tui and LocalBench.Tui when available; `llm-update -InstallTui` refreshes installed TUI binaries.
- **`lbtui` wrapper added.** LocalBox now exposes a LocalBench.Tui entrypoint alongside `llmtui`, and inline help documents both.

## 2026-05-24 — Dropped Ollama backend (breaking)

### Breaking changes

- **Ollama support has been removed.** LocalBox now targets llama.cpp's `llama-server` exclusively. The `-Backend ollama` parameter, the Ollama process control (`Start-OllamaApp`, `Wait-Ollama`, `Stop-OllamaModels`, …), Modelfile-based alias creation, Ollama strict siblings, the Ollama remote gateway path, and every shortcut that pulled or rebuilt Ollama aliases (`init`, `initmodel`, `ostop`, `qkill`, `ops`, `cleanorphans`, `listorphans`, `ospeed`) are gone. If you still need that path, check out the `ollama-classic` git tag.
- **`-Chat` and `-Q8` flags removed from per-model shortcuts.** `llama-server` has no chat REPL, and Q8 KV was the Ollama env var (`OLLAMA_KV_CACHE_TYPE=q8_0`). Use `-KvK q8_0 -KvV q8_0` (or the wizard's KV-cache step) for the equivalent llama.cpp setting.
- **Catalog scalars removed.** `MinOllamaVersion`, `OllamaAppPath`, `OllamaCommunityRoot`, `KeepAlive`, `RequireAdvertisedTools`, `LlamaCppCoexistOllama` are stripped from `defaults.json` and from any merged settings at load time. `SourceType: remote` model entries are no longer supported (all in-catalog entries are `gguf`).
- **`ollama-proxy/` folder renamed to `localbox-proxy/`.** The deployed location is now `~/.localbox-proxy/`. The Python no-think proxy itself is unchanged in behavior; it now targets `127.0.0.1:8080` by default instead of `127.0.0.1:11434`. The `enforcer-claude.ps1` Ollama wrapper has been deleted.
- **`Save-LLMDefaultLaunch` schema simplified.** No more `Backend`, `UseQ8`. Existing `DefaultLaunch` entries with those fields will ignore the obsolete keys; re-save via the wizard if you want the schema clean.

### Other

- `Invoke-Backend` no longer takes `-Backend`; it always dispatches to llama.cpp. `launch-chat` action removed.
- `llm-status` is now just `Invoke-LlamaCppStatus`. `Show-OllamaStatus` is replaced by `Show-LocalBackendStatus` + `Show-ConfiguredGgufQuants` in the dashboard.
- The wizard's backend step is now a mode picker (native / turboquant / mtpturbo). Default is `LlamaCppDefaultMode` (still `native`).
- Codex launches always use the OpenAI-compatible llama-server provider; the `--oss --local-provider ollama` path is gone.

## 2026-05-08 - Codex target and default launch recipes

- **`ostop` now leaves Ollama stopped.** It no longer restarts the Ollama app after teardown.
- **`llm-stop`.** Added a hyphenated all-backend stop command alongside `llmstop` / `unloadall`.
- **Codex launch target.** Model shortcuts and the wizard now support `-Codex` / `Codex` as a peer to Claude Code and LocalPilot. Ollama launches use Codex's local Ollama provider; llama.cpp launches pass a custom OpenAI-compatible provider pointed at the selected `llama-server` `/v1` endpoint.
- **Default launch recipes.** The wizard can save the selected model, target, backend, context, quant, strict/Q8 flags, llama.cpp mode, KV cache, and AutoBest profile into `DefaultLaunch` so `llmdefault` can replay the full recipe.
- **Wizard default is native selectable.** `llm`/`llmmenu` now use an in-repo arrow-key picker by default. `llms` opens the Spectre wizard explicitly, and `$env:LOCAL_LLM_USE_SPECTRE=1` opts `llm` back into Spectre.
- **Context menu noise reduced.** Removed visible `fast`, `deep`, and bare `128` context aliases from the catalog and new-model defaults. Use `32k`, `64k`, `128k`, and `256k`; legacy aliases still resolve for old commands and saved AutoBest profiles.

## 2026-05-07 - Suite updater, LocalPilot cleanup

### Added

- **`llm-update` / `Update-LocalLLMSuite`.** Checks LocalBox, LocalPilot, and LocalBench when they are installed as git checkouts, fetches upstream state, and fast-forwards only when an update is available. Missing companions are skipped, current checkouts are reported as current, and diverged/no-upstream checkouts are left untouched with a reason.
- **`LocalBoxRoot` setting.** `install.ps1` now records the source checkout used for installation so deployed copy-mode profiles can find the LocalBox repo for future self-updates.
- **LocalPilot-only command surface.** Removed the old shorthand and pre-rename aliases. Model launches now use the explicit `-LocalPilot` switch, and the default shortcut is `llmdefaultlocalpilot`.

## 2026-05-03 — Wizard back-step nav, full-quant backfill, install fix

### Added

- **`updatellm <key>` / `Update-LocalLLMModelQuants`.** Backfills missing quants on an existing GGUF entry by re-fetching its HF repo and merging any quant codes not already present. Existing `Quants`, `QuantSizesGB`, and `QuantNotes` entries are preserved verbatim — only new keys are added (auto-generated note via `New-LocalLLMQuantNoteText`). `-DryRun` previews the additions without writing. Applied to the catalog: `qcoder30` 3 → 23, `qcodernext` 3 → 23, `q27heretic` 4 → 6, `q27hauhau` 2 → 10. `q36plus` (HF gated, returns 401 on the API) and `q36heretic` (subdirectory-organized repo layout) were left untouched.

### Fixed

- **LM wizard couldn't go back one step.** Both `Start-LLMWizardClassic` and `Start-LLMWizardSpectre` were a flat `while ($true)` / `continue` loop where every `[[Back]]` returned to the top (re-pick model). The Spectre quant menu had no Back at all (only `[[Keep current: …]]`). Both wizards are now step-state machines (`'model' → 'quant' → 'context' → 'action' → 'q8' → 'launch'`); each step's `$null` return walks back exactly one step. Quant menu in Spectre now has both `[[Keep current: <q>]]` and `[[Back]]`; the classic quant menu uses a letter shortcut (`k` = keep current, `0` = back) via a new `-LetterChoices` hashtable on `Read-LLMChoiceIndex`. The q8 prompt also accepts `b` (classic) / `[[Back]]` (Spectre) to walk back to action selection.
- **`addllm` could pick up imatrix calibration files.** Top-level `*.imatrix.gguf` (mradermacher's calibration data, not a quantized model) used to pass the file filter and only got dropped because `Get-HuggingFaceQuantCode` returned `$null` for it. Now excluded explicitly in both `Add-LocalLLMModel` and `Update-LocalLLMModelQuants` so a future quant-code regex change can't accidentally include them.
- **`install.ps1` failed when `-Profile` was passed.** The `[switch]$Profile` parameter shadowed PowerShell's `$PROFILE` automatic variable inside the script, so `$PROFILE.CurrentUserAllHosts` resolved to `[switch].CurrentUserAllHosts` (no such property → `$null`), then `Test-Path $null` threw `Value cannot be null`. Renamed the parameter to `[switch]$SetupProfile` with `[Alias("Profile")]` so existing `-Profile` invocations still bind. Also collapsed the convoluted `installFiles` flag computation into one line, and fixed a cosmetic message bug where `\$PROFILE` was meant as a literal but PowerShell escapes with backtick (`` ` ``), not backslash — the message accidentally interpolated the auto-variable.

## 2026-05-03 — Spectre wizard

### Added

- **Spectre-rendered `llm` wizard.** `Start-LLMWizard` now dispatches to `Start-LLMWizardSpectre` when PwshSpectreConsole is available, falling back to `Start-LLMWizardClassic` otherwise. The Spectre flow uses `Read-SpectreSelection` for model / quant / context / action picks and `Read-SpectreConfirm` for the `-Q8` toggle, and renders the full `Show-ModelCatalogSpectre` table above the model picker so quant fit / size / built status stay visible while choosing. Same env switch as the dashboard: `$env:LOCAL_LLM_NO_SPECTRE=1` forces the classic wizard.
- **`llmc` escape hatch.** New `llmc` function calls `Start-LLMWizardClassic` directly, bypassing Spectre regardless of availability — useful when a Spectre render bug makes the rich wizard unusable.
- **Wizard error trap.** Each Spectre prompt is wrapped in `Invoke-LLMWizardStep`; on exception, `Save-LocalLLMWizardError` records the full trace (timestamp, context tag, exception type/message, `InvocationInfo.PositionMessage`, `ScriptStackTrace`, inner exception) to `~/.local-llm/wizard-errors.log` and pauses with `Press Enter to continue` so a Spectre live-display refresh can't scroll the trace off screen. Inspect with `llmlogerr [-Lines 80]`; reset with `llmlogerrclear`.

### Fixed

- **Spectre markup parse errors crashed wizard prompts.** Choice labels like `[Back]`, `[Cancel]`, `[Show all tiers]`, `[Keep current: …]`, and the fit tags `[fits]`/`[tight]`/`[over]`/`[?]` were passed straight to `SelectionPrompt`, which interprets `[…]` as Spectre markup — every render frame threw `Encountered malformed markup tag …` and the live-display refresh hid the trace. Sentinels are escaped as `[[…]]`; user-supplied text (display names, quant/context keys, notes, `$ModelKey` titles) is routed through `ConvertTo-LocalLLMSpectreSafe`; fit tags became proper colored markup (`[green]fits[/]`, `[yellow]tight[/]`, `[red]over[/]`, `[grey50]?[/]`).
- **Renderables leaked into captured pipeline output.** `Format-SpectrePanel` / `Format-SpectreTable` `return` a `Spectre.Console.Renderable`; PwshSpectreConsole's `format.ps1xml` only renders that to ANSI at `Out-Default`. Inside `Show-ModelCatalogSpectre` (called from `Select-LLMModelKeySpectre`, captured by `$modelKey = …`), the Panel and Table objects bubbled up into `$modelKey`, producing arrays like `[Panel, Table, "qcoder30"]` and tripping `Cannot convert value to type System.String` on the next `Get-ModelDef -Key $modelKey`. All six `Format-Spectre*` call sites now pipe to `| Out-Host` so the renderable renders eagerly and emits nothing to the caller's pipeline. Side effect: the catalog table that was silently swallowed during the wizard now appears as intended.

## 2026-05-02 — 256k Qwen3-Coder profile, VRAM-aware tradeoffs, per-quant/context notes

### Added

- **`qcoder30` 256k context.** Added `"256": 262144` to the Qwen3-Coder-30B-A3B Heretic model and a new `iq4xs` quant (`Qwen3-Coder-30B-A3B-Instruct-Heretic.i1-IQ4_XS.gguf`, ~16.5 GB). The 256k profile only fits a 4090 with IQ4_XS weights + q4_0 KV cache (~6 GB at 256k); use `qcoder -Ctx 256 -Quant iq4xs`.
- **`qcodernext` (experimental).** New entry pointing at `mradermacher/Huihui-Qwen3-Coder-Next-abliterated-i1-GGUF` — the 80B/3B-active hybrid DeltaNet+Attention coder. Quants: `iq1m`, `iq2s`, `iq3xxs`. Only `iq1m` (~18.1 GB) fits a single 4090 with any KV headroom; flagged as experimental and "tight on 4090" in the display name.
- **Per-model `Description`, `QuantNotes`, `ContextNotes` catalog fields.** Free-form strings keyed by quant/context name. Backfilled across the existing catalog so users can see file sizes, KV pressure, and "when to pick this" guidance without leaving the launcher.
- **`info` / `llmdocs` / `llm` wizard surfaces.** `Show-ModelCatalog`, `Show-LLMDynamicModelSummary`, `Select-LLMModelKey`, `Select-LLMQuantKey`, and `Format-LLMContextLabel` all render the new notes inline. The current default quant is marked with `*` in the per-quant list.
- **`addllm -Description`, `-QuantNotes`, `-ContextNotes`.** Optional params on `Add-LocalLLMModel` / `addllm` that round-trip into the catalog entry. Notes are hashtables (`@{key='note'}`) keyed by the same quant/context shortname.
- **`-Q8` + long-context guard.** `Invoke-ModelShortcut` refuses `-UseQ8` whenever the resolved `num_ctx` exceeds the `Q8KvMaxContext` ceiling. The error message tells the user to drop `-Q8`, lower `-Ctx`, or raise the threshold.
- **VRAM-aware recommendations.** New top-level `VRAMGB` setting plus `Get-LocalLLMVRAMInfo` helper. Auto-detects via `nvidia-smi --query-gpu=memory.total` (largest GPU on a multi-card box). Override via `Set-LocalLLMSetting VRAMGB 32`. The dashboard surfaces the resolved value + source (configured / auto / fallback).
- **`QuantSizesGB` per-quant numeric field.** Drives a `[fits]` / `[tight]` / `[over]` badge next to each quant in `info` and the wizard, computed against the host's VRAM (weight-budget heuristic: `[fits]` when the model leaves >=7 GB headroom for KV, `[tight]` when only ~2 GB headroom, `[over]` otherwise). Backfilled across `qcoder30`, `qcodernext`, `q36plus`, `q36heretic`, `q27heretic`, `q27hauhau`.
- **`Q8KvMaxContext` now scales with VRAM by default.** Removed the explicit `131072` literal from the catalog. The guard derives `(VRAMGB - 16) * 16384` (floored at 64k) when not pinned, so a 5090 (32 GB) gets ~256k while a 4090 (24 GB) gets ~128k. Override still works via `Set-LocalLLMSetting Q8KvMaxContext`.
- **Quant notes rewritten to be VRAM-agnostic** where possible. The hand-written notes describe quality/use-case (no longer "partial offload on a 4090"); the per-quant `[fits]/[tight]/[over]` badge is the live verdict for the host's actual VRAM.

### Why

Picking a quant and context blindly was costing real time — Q4_K_M is fine at 64k but cannot fit 256k KV; Q6_K is too heavy at any long context; `-Q8` looks free until it OOMs at 128k+. The notes encode the tradeoff directly next to the selector, and the guard prevents the worst foot-gun (`-Q8 -Ctx 256`) from ever launching.

VRAM auto-detection was the next cliff: every recommendation in the catalog implicitly assumed a 24 GB 4090. A 5090 user (32 GB) should see Q5_K_M as `[fits]`, not "partial offload"; a 4080 user (16 GB) should see most 35B variants flagged `[over]` and not waste time downloading them. The fit badge gives a per-host verdict without the user having to do KV-cache arithmetic.

The catalog gained one realistic 256k coder option (`qcoder30 -Ctx 256 -Quant iq4xs`) and one aspirational one (`qcodernext`) so the "uncensored 256k on a 4090" question has a documented answer instead of trial-and-error.

## 2026-04-30 — Per-machine settings + auto-install LocalPilot

### Added

- **`~/.local-llm/settings.json`** — per-machine overlay for the catalog. Top-level scalars (`LocalPilotRoot`, `OllamaAppPath`, `Default`, `KeepAlive`, `RequireAdvertisedTools`, `NoThinkProxyPort`, `LocalModelTools`, `LocalPilotRepoUrl`, etc.) load from `llm-models.json` first, then any matching keys in `settings.json` override. `Models` and `CommandAliases` are catalog-only and protected from override.
- **`Set-LocalLLMSetting <Key> <Value>`** — writes to `settings.json` and reloads. Pass `$null`/`""` to remove a key. Refuses `Models`/`CommandAliases`.
- **`LocalPilotRepoUrl`** config field, defaulting to `https://github.com/C0deGeek-dev/LocalPilot`.
- **`Ensure-LocalPilotInstalled`** — called by `Invoke-LocalPilotCli` before doing anything. If the configured `LocalPilotRoot` doesn't contain `src/entrypoints/cli.tsx`, it prompts `Clone <url>? [y/N]` and runs `git clone` on confirmation. Aborts with a clear instruction otherwise.
- `settings.json` added to `.gitignore` so per-machine config never lands in the repo.
- `install.ps1` prints a tip pointing at `Set-LocalLLMSetting` for fresh-machine setup.

### Why

Cloning the public repo onto a different machine should not require editing `llm-models.json` to fix `LocalPilotRoot` (and risking merge conflicts with future pulls). LocalPilot launches should do the obvious thing on a fresh machine instead of failing because no checkout is around.

## 2026-04-30 — LocalPilot rename

The external harness fork was renamed to [LocalPilot](https://github.com/C0deGeek-dev/LocalPilot). Propagated through this project:

- JSON config field renamed to `LocalPilotRoot`.
- Internal CLI wrapper renamed to `Invoke-LocalPilotCli`.
- Switch parameter renamed to `-LocalPilot` on `Start-ClaudeWithOllamaModel`, `Invoke-ModelShortcut`, and the per-model shortcut functions.
- User-visible labels updated: launcher banner, wizard action label, install diagnostics, README, quick reference (`llmdocs`).
- Existing local folder paths were not renamed and still work as the configured `LocalPilotRoot`.

## 2026-04-29 — second-pass refactor

Reviewed the project, then ran a single-day refactor pass guided by an explicit plan (`plan.md`, retired into this changelog).

### Bugs fixed

- **Persona pollution.** `LocalLLMProfile.ps1` had a hardcoded "You are Qwen, created by Alibaba Cloud" prepended to every model launch — wrong for Devstral and even somewhat wrong for the Qwen variants whose GGUF templates already self-identify. Removed the persona layer entirely; the system prompt now contains only universal tool-use rules, plus an opt-in deferred-tool-schema block (gated on `LimitTools`).
- **`enforcer-claude.ps1` rewritten.** The wrapper used to hardcode `qcoder30` and bypass the no-think proxy by pointing at `localhost:11434`. Now it reads `Default` from `llm-models.json` (or `$env:ENFORCER_MODEL`), routes through the proxy on `11435`, self-starts the proxy if needed, and sets the same thinking/caching/attribution env stack as the main launcher.
- **Legacy harness stub deleted.** It was a one-liner that called the old harness wrapper with no args and ignored everything. The flag-based LocalPilot launch path covers it now.
- **Tool-support detection rewritten.** `Test-OllamaModelSupportsTools` used to grep `ollama show` text for the literal word "tools" — which could match unrelated lines. Now POSTs to `/api/show` and checks the structured `capabilities` array. Falls back to the regex if the API is unreachable.
- **Devstral parser confirmed correct.** `Parser: "none"` was the right call (its GGUF self-templates with persona, `[SYSTEM_PROMPT]`/`[TOOL_CALLS]` tags, and `capabilities=[completion,vision,tools]`). Documented inline via a `ParserNote` field.
- **`init -Stale` parameter shadow bug.** `Initialize-LocalLLM` declared `[switch]$Stale`; the body did `$stale = @(Get-StaleModelAliases)`. PowerShell variables are case-insensitive, so the assignment tried to coerce an array into a `SwitchParameter` and failed silently, leaving `$stale` as the boolean `$true`. Renamed the local to `$staleEntries`.

### Added capabilities

- **Per-model `Tools` allowlist.** `Start-ClaudeWithOllamaModel` now takes `-Tools`; `Invoke-ModelShortcut` reads the optional `Tools` field from the model def, falling back to the global `LocalModelTools`. No models populated yet — capability only.
- **Auto-generated alias prefixes.** Added `ShortName` field per model. `Register-ModelShortcuts` walked `ShortName × Contexts × actions` and registered PowerShell aliases. Pruned the 30 hand-maintained `CommandAliases` entries to `{}`.
- **Parser-version stamping.** `New-OllamaModelFromSource` now writes a sha256-hash sidecar at `<profile-root>\parser-versions\<aliasname>.txt`. `Test-ModelAliasFresh`, `Get-StaleModelAliases`, `init -Stale`, and the `info` dashboard surface stale aliases (parser config drifted since build).
- **Default model.** Added `"Default"` field at the top of `llm-models.json`. `Get-DefaultModelKey` reads it (with a recommended-tier fallback). New shortcuts: `llmdefault`, an LocalPilot default shortcut, and `llmdefaultchat`. Used by the enforcer.
- **`ThinkingPolicy` per model.** Either `strip` (default) or `keep`. `keep` mode bypasses the no-think proxy, points `ANTHROPIC_BASE_URL` at Ollama directly, and skips the thinking-disable env vars. Set on `q36opus47abl`. Launcher banner shows the active mode.
- **Configurable `OLLAMA_KEEP_ALIVE`.** Top-level `KeepAlive` field; `Set-OllamaRuntimeEnv` reads it (defaults to `"-1"`).
- **`Wait-Ollama` resilience.** Deadline bumped 20s → 60s. After 5s of waiting, prints `Waiting for Ollama` and adds a `.` every 2s.
- **Bench history persistence.** `Test-OllamaSpeed` now appends to `<profile-root>\bench-history.jsonl` per run. `Show-LLMBenchHistory [-Model] [-Last N]` and the short `obench` alias display recent runs.
- **Header truth.** File header lost the "+ LM Studio" advertisement (LM Studio support was never implemented). Now states "Windows / PowerShell only — does not work in WSL/bash."

### Architectural changes

- **Flag-based shortcut scheme (Option C).** Replaced ~135 multi-suffix functions with 9 model functions: `dev`, `qcoder`, `q36`, `q36hau`, `q36p`, `q36h`, `q27`, `q27hau`, `qop`. Each takes `-Ctx`, `-LocalPilot`, `-Chat`, `-Q8`, and (where applicable) `-Quant`. Introduced `Get-ModelShortcutName` and `Unregister-AllModelShortcuts`; `Register-ModelShortcuts` is now idempotent.

### Deferred

- **Diagnostic logging on tool-call failure.** Naive stderr-tee breaks Claude Code's interactive terminal; needs better design (probably a debug-mode flag rather than a wrapper).

## Pre-2026-04-29

Project predates this changelog. The state at the start of this round:

- Single 2,506-line `LocalLLMProfile.ps1` engine with JSON catalog (`llm-models.json`).
- Per-(model, context) Ollama aliases.
- Hand-maintained `CommandAliases` map.
- HTTP proxy on `11435` stripping Anthropic thinking/reasoning fields.
- Hardcoded "You are Qwen" persona prepended to every launch.
- `enforcer-claude.ps1` hardcoded to `qcoder30` and pointing at the wrong port.
