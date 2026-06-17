# Per-machine settings (settings.json)

Part of the [LocalBox documentation](README.md).

`llm-models.json` is the model **catalog** — committed, sharable. Per-machine
paths and preferences belong in a sibling `settings.json` at
`~/.local-llm/settings.json` (gitignored). It overlays top-level scalars from
`defaults.json` at load time, so you don't have to hand-edit `llm-models.json`
to fix paths on a fresh machine.

Use the helper instead of editing JSON:

```powershell
Set-LocalLLMSetting LocalPilotRoot '<path-to-localpilot>'   # else: sibling checkout, then ~/.local-llm/tools/localpilot
Set-LocalLLMSetting LocalBenchRoot '<path-to-localbench>'   # usually auto-set by install.ps1
Set-LocalLLMSetting LocalBoxRoot '<path-to-LocalBox>'        # auto-set by install.ps1
Set-LocalLLMSetting Default q36plus
Set-LocalLLMSetting VRAMGB 32                        # override auto-detect
Set-LocalLLMSetting LlamaCppDefaultMode native       # or 'turboquant' / 'mtpturbo'
Set-LocalLLMSetting LlamaCppMtpTurboRepo EsmaeelNabil/llama.cpp      # mtpturbo upstream
Set-LocalLLMSetting LlamaCppMtpTurboBranch feat/mtp-turboquant-kv-cache   # mtpturbo branch
Set-LocalLLMSetting LlamaCppMtpTurboCommit <sha>     # pin the mtpturbo build to an exact commit (not a force-pushable branch)
Set-LocalLLMSetting LlamaCppRequireDownloadPins $true # fail any binary download that has no recorded SHA-256 pin
Set-LocalLLMSetting LlamaCppNCpuMoe 35               # MoE expert CPU offload (default 35; 0 to disable)
Set-LocalLLMSetting LlamaCppMlock $false             # disable RAM locking (default $true)
Set-LocalLLMSetting LlamaCppNoMmap $false            # disable no-mmap (default $true)
Set-LocalLLMSetting LlamaCppAgentParallel 1          # agent slots (default 1; 0 = llama.cpp auto)
Set-LocalLLMSetting LlamaCppAgentCacheReuse 256      # prompt-cache reuse chunk size (default 256; 0 = llama.cpp default)
Set-LocalLLMSetting LocalModelMaxOutputTokens 4096   # cap local Claude/LocalPilot completions (0 = tool default)
Set-LocalLLMSetting LocalModelSkipPermissions $false # require Claude Code permission prompts (unset = first launch asks once)
Set-LocalLLMSetting LocalPilotRoot $null             # remove an entry
```

The `Models` and `CommandAliases` keys are catalog-only and rejected by
`Set-LocalLLMSetting`. Everything else is fair game.

### Verified binary downloads

LocalBox downloads `llama-server` binaries from third-party GitHub releases and
builds the `mtpturbo` binary from a fork branch. Out of the box, every binary
download is pinned and verified:

- **`defaults.json` ships pinned release tags** (`LlamaCppPinnedTag` for
  llama.cpp, `LlamaCppTurboquantPinnedTag` for turboquant) **and a
  `LlamaCppDownloadPins` table** with the SHA-256 of every asset those tags can
  install. Downloads target the pinned tag, and a checksum mismatch deletes the
  file and aborts the install.
- **`LlamaCppRequireDownloadPins` defaults to `true`**: an asset with no
  recorded pin is a hard failure. To opt out of pinning (trust-on-first-use:
  the download proceeds and prints its `sha256=...`), set it to `false`:
  `Set-LocalLLMSetting LlamaCppRequireDownloadPins $false`.
- **`LlamaCppMtpTurboCommit`** pins the from-source mtpturbo build to an exact
  commit instead of a force-pushable branch HEAD (also pre-set in
  `defaults.json`).

**Updating the pins** (e.g. to move to a newer llama.cpp build) is a deliberate
loop, done in `~/.local-llm/settings.json` (overrides the shipped defaults;
`LlamaCppDownloadPins` is a nested map, so edit the file directly rather than
via `Set-LocalLLMSetting`):

1. Pick the new tag on the release page and set `LlamaCppPinnedTag` (or
   `LlamaCppTurboquantPinnedTag`) to it.
2. Take each asset's SHA-256 from the GitHub release API's `digest` field
   (`https://api.github.com/repos/ggerganov/llama.cpp/releases/tags/<tag>`)
   and record it under `LlamaCppDownloadPins`, keyed by the exact asset
   filename. Cross-check one real download with
   `(Get-FileHash -Algorithm SHA256 <file>).Hash` if you want a second source.
3. Reinstall (`Install-LlamaServerNative -Force`) — the install fails loudly
   if a hash doesn't match.

The same keys in `settings.json` always win over `defaults.json`, so machine
pins can lead or lag the shipped ones.

### Per-workspace default model

Drop a `.llm-default` file in any directory containing a single line — a
model key, `ShortName`, or `Root`. `llmdefault` walks up from `$PWD` and uses
the nearest match. Falls back to settings → catalog `Default`.

```
echo q36p > .llm-default          # this workspace prefers Qwen 3.6 Plus
```

---
