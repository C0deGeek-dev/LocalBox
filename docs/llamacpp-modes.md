# llama.cpp modes

Part of the [LocalBox documentation](README.md).

The launcher supports three flavors of `llama-server`:

- **`native`** — upstream `llama-server.exe`. Mainline KV types only
  (`q8_0`, `f16`, `q5_1`, `q5_0`, `q4_1`, `q4_0`, `iq4_nl`, `bf16`, `f32`).
  Supports `--spec-type draft-mtp` for native Multi-Token Prediction
  speculative decoding on MTP-capable GGUFs.
- **`turboquant`** — the [C0deGeek-dev llama.cpp turboquant fork](https://github.com/C0deGeek-dev/llama-cpp-turboquant), which
  ships `turbo3` and `turbo4` KV cache types (more aggressive than `q4_0` but
  with a quality cliff that's a function of context length). Only available
  through the fork binary. Auto-downloaded from GitHub releases on first use.
  Does **not** support MTP — LocalBox rejects `--spec-type draft-mtp` up front
  in this mode.
- **`mtpturbo`** — combined build: MTP spec-decode **and** turbo KV cache in
  one binary. No prebuilt Windows CUDA release exists for any fork that
  carries both features, so LocalBox builds it from source on first use.
  When you pick `mtpturbo` and the binary is absent:
  - LocalBox probes for the toolchain (git, cmake, ninja, nvcc, MSVC). If
    anything is missing it prints the exact `winget install` command for
    each.
  - If the toolchain is complete it prompts `Build it now? [Y/n]`, then
    shallow-clones [`EsmaeelNabil/llama.cpp#feat/mtp-turboquant-kv-cache`](https://github.com/EsmaeelNabil/llama.cpp/tree/feat/mtp-turboquant-kv-cache),
    auto-detects compute capability via `nvidia-smi`, single-arch CUDA build
    via Ninja (~5–30 min depending on GPU), installs into
    `~/.local-llm/llama-cpp-mtpturbo/`, writes `.build-stamp`.
  - Repo + branch are overrideable via `LlamaCppMtpTurboRepo` /
    `LlamaCppMtpTurboBranch` settings if you fork it. CUDA Toolkit + VS
    BuildTools are heavyweight system-wide deps that LocalBox never silent-
    installs; it just names the winget IDs and gets out of the way.

All three modes start a native `llama-server` process, pin to a free port from
`LlamaCppPort` (default `8080`), wait for `/v1/models` to come up, then point
Claude Code at `http://localhost:<port>`.

```powershell
# Wizard route — pick mode interactively
llm

# Direct
Invoke-Backend -Action launch-claude `
  -Key qcoder30 -ContextKey 256 `
  -LlamaCppMode turboquant -KvCacheK turbo4 -KvCacheV turbo4 -Strict

# MTP + turbo KV together — the unsloth 256K-on-24GB recipe. Catalog stores
# SpecType=draft-mtp (mainline canonical); LocalBox translates to bare 'mtp'
# at emit time for this mode automatically.
Invoke-Backend -Action launch-claude `
  -Key genesisv2 -ContextKey 128k `
  -LlamaCppMode mtpturbo -KvCacheK turbo3 -KvCacheV turbo4

lps                           # show running llama-server (port, pid, gguf path)
llm-status                    # detailed per-process status (KV, ngl, MTP, VRAM, slots, /props)
lstop                         # stop it
llm-stop                      # alias for unloadall: stop every running llama-server
```

---
