# Per-machine settings (settings.json)

Part of the [LocalBox documentation](README.md).

`llm-models.json` is the model **catalog** — sharable, yours to edit. Per-machine
paths and preferences belong in a sibling `settings.json` at
`~/.local-llm/settings.json`. It overlays top-level scalars from
`defaults.json` at load time, so you don't have to hand-edit `llm-models.json`
to fix paths on a fresh machine. Precedence is always
`defaults.json` < `llm-models.json` < `settings.json`, and the catalog-only
keys (`Models`, `CommandAliases`) can never be overridden from settings.

`settings.json` is a flat JSON object. Common keys:

```jsonc
{
  "VRAMGB": 32,                          // override nvidia-smi auto-detect
  "LlamaCppGgufRoot": "~/.local-llm/gguf",   // where model weights live (~ and %VAR% ok)
  "LlamaCppNCpuMoe": 35,                 // MoE expert CPU offload (0 disables)
  "LlamaCppMlock": true,                 // RAM locking
  "LlamaCppNoMmap": true,
  "LlamaCppAgentParallel": 1,            // server slots, every launch incl. serve
                                          // (unset = 1; 0 or -1 = llama.cpp auto —
                                          // auto allocates the FULL context per slot,
                                          // multiplying KV-cache memory)
  "LlamaCppAgentCacheReuse": 256,        // prompt-cache reuse chunk (unset = 256,
                                          // 0 or -1 = llama.cpp default)
  "LocalModelMaxOutputTokens": 16384,    // cap agent completions; raise if replies
                                          // truncate mid-word, 0 = tool default (32k)
  "NoThinkProxyPort": 11435
}
```

The guided launcher's Customize → save-as-default flow persists its own
`DefaultLaunch` recipe through the same store, including target, engine,
Auto-tune, quant, context, KV cache, strict output, and vision. Model-specific
recipe choices, including vision, replay only for the saved model; catalog-only
keys are refused on every write path.

### Launch permission and bypass decisions

LocalBox launches other agents (Claude Code, LocalPilot, Codex) against a local
model. Those agents have a "bypass everything" mode that hands the model full
command/file authority with no per-action approval. Less-aligned local models
make that authority riskier, so **LocalBox never enables bypass by default** —
each is a conscious, persisted decision, and a **non-interactive session always
fails closed (bypass off)**:

| Setting | Agent / flag | First-run behaviour | Env override (this launch only) |
|---|---|---|---|
| `LocalModelSkipPermissions` | Claude Code `--dangerously-skip-permissions` | asks once, defaults off, persists | `LOCAL_LLM_SKIP_PERMISSIONS` |
| `LocalPilotBypass` | LocalPilot bypass profile | asks once, defaults off, persists | `LOCAL_LLM_LOCALPILOT_BYPASS` |
| `CodexBypassApprovalsAndSandbox` | Codex `--dangerously-bypass-approvals-and-sandbox` | asks once, defaults off, persists | `LOCAL_LLM_CODEX_BYPASS` |

The active posture is shown in every `--dry-run` launch plan. An env override
(`0`/`false`/`no`/`off` = off, anything else = on) wins for a single launch
without changing the persisted answer; clear a persisted choice by editing
`~/.local-llm/settings.json`.

### Verified binary downloads

LocalBox downloads `llama-server` binaries from third-party GitHub releases.
Out of the box, every binary download is pinned and verified:

- **`defaults.json` ships pinned release tags** (`LlamaCppPinnedTag` for
  llama.cpp, `LlamaCppTurboquantPinnedTag` for turboquant) **and a
  `LlamaCppDownloadPins` table** with the SHA-256 of every asset those tags can
  install. Downloads target the pinned tag, and a checksum mismatch deletes the
  file and aborts the install.
- **`LlamaCppRequireDownloadPins` defaults to `true`**: an asset with no
  recorded pin is a hard failure. To opt out of pinning (trust-on-first-use),
  set it to `false` in `settings.json`.
**Updating the pins** (e.g. to move to a newer llama.cpp build) is a deliberate
loop, done in `~/.local-llm/settings.json`:

1. Pick the new tag on the release page and set `LlamaCppPinnedTag` (or
   `LlamaCppTurboquantPinnedTag`) to it.
2. Take each asset's SHA-256 from the GitHub release API's `digest` field
   (`https://api.github.com/repos/ggerganov/llama.cpp/releases/tags/<tag>`)
   and record it under `LlamaCppDownloadPins`, keyed by the exact asset
   filename.
3. Run `localbox update` — the install fails loudly if a hash doesn't match.

The same keys in `settings.json` always win over `defaults.json`, so machine
pins can lead or lag the shipped ones.

### Per-workspace default model

Drop a `.llm-default` file in any directory containing a single line — a model
key or its on-disk folder name. The guided launcher walks up from the working
directory and preselects the nearest match in the model picker; without one it
starts at the top of the list.

```
echo q36plus > .llm-default        # this workspace prefers Qwen 3.6 Plus
```

---
