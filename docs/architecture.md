# Architecture

Part of the [LocalBox documentation](README.md).

LocalBox is a Rust workspace producing one binary, `localbox`, built on the
shared [localx-llama](https://github.com/C0deGeek-dev/localx-llama) crate tier
(consumed as a git dependency pinned by revision in `Cargo.lock`).

```
crates/
  localbox-launcher/    the launcher library
    catalog.rs          three-layer config load (defaults < catalog < settings)
    launcher.rs         the launcher-contract implementation (LlamaLauncher),
                        GGUF path resolution, path expansion (~ and %VAR%)
    orchestrate.rs      read-only launch planning: request → LaunchPlan
                        (argv, ports, proxy config, env plan, provider TOML)
    proxy.rs            no-think proxy lifecycle (tri-state target check,
                        repoint-not-restart, reap-before-probe)
    posture.rs          serve-gateway guard (public-looking no-auth refused),
                        default GGUF root
    permissions.rs      settings persistence (catalog-only keys refused),
                        permission fail-closed rules
    smoke.rs            reply-path smoke evaluation (degenerate-output detectors)
    env.rs              agent environment plan (model aliases, endpoint vars)
    localpilot_config.rs  generated .localpilot.toml provider block
  localbox-tui/         the guided-launcher flow as pure functions
    vocab.rs            plain-language vocabulary, plan summary, glossary
    plan.rs             model → GuidedPlan resolution, DefaultLaunch replay
    customize.rs        the Customize menu state machine, save gates
    ui.rs               picker/confirm row model (TestBackend snapshots)
    driver.rs           terminal policy: inline viewport, plain fallback
  localbox/             the application binary
    main.rs             hand-rolled CLI (worker thread, 16 MiB stack)
    guided.rs           the persistent pick → confirm → launch → return loop
    live.rs             launch execution: download → spawn → readiness →
                        proxy → smoke → agent handoff; stop; status
    exec.rs             process/socket effects (spawn, EnvGuard, socket-table
                        PIDs, interactive agent launch)
    fetch.rs            resumable HTTP downloads (HF GGUF pulls); integrity is
                        checked at the reply-path smoke test, not by a per-file
                        checksum. sha256 pins cover the llama.cpp binary zips
                        (update.rs), not the GGUF weights.
    embed.rs            CPU-only embedding server lifecycle
    update.rs           llama.cpp binary install/update per mode
    manage.rs           info / purge / log conveniences

local-llm/
  defaults.json             shipped launcher defaults (embedded + seeded)
  llm-models.example.json   example model catalog (embedded + seeded)
```

The shared tier supplies the pure domain (`localx-llama-core`: model catalog
types, argv construction, VRAM/fit, config precedence, tuner store schema, the
launcher trait) and the runtime effects (`localx-llama-runtime`: downloads and
pin verification, health classification, the in-process no-think filter, port
and spawn utilities).

## Boundaries that hold the design

- **Planning is read-only; execution is one module.** `orchestrate` resolves a
  complete `LaunchPlan` without touching the system; `live` is the only place
  the plan's effects happen. A `--dry-run` prints the same plan the launch
  executes.
- **The guided flow is pure; frontends pick indexes.** Every TUI decision
  lives in `localbox-tui` as tested functions; the ratatui and plain-text
  frontends only select rows, so behaviour cannot fork between them.
- **The no-think proxy is in-process.** The filter is a library in the shared
  runtime tier; the binary hosts it by re-invoking itself
  (`localbox nothink-proxy`). There is no sidecar to install or version.
- **The launcher contract is a trait.** LocalBench consumes `LlamaLauncher`
  through the shared trait and its versioned envelope; conformance is tested
  cross-repo in CI in both directions.
- **Tier-1 platforms are equal.** OS-specific behaviour (socket-table PID
  lookup, executable suffixes, path expansion) sits behind small seams with
  per-OS implementations; CI runs the full gate on Windows, Linux, and macOS.

## On-disk layout (`~/.local-llm`)

Seeded on first run, never overwritten: `defaults.json`,
`llm-models.example.json`, and the user's editable `llm-models.json`.
Everything else appears as it is used — `settings.json` (per-machine
overrides), `gguf/` (model weights), `llama-cpp*/` (per-mode server
binaries + `.build-stamp`), `tuner/` (AutoBest profiles and the trial
cache), and `logs/`.
