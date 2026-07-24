# llama.cpp modes

Part of the [LocalBox documentation](README.md).

The launcher supports four flavors of `llama-server`:

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
  one binary. No prebuilt release exists for any fork that carries both
  features, so the binary is built from source (pinned to an exact commit via
  `LlamaCppMtpTurboCommit`; repo/branch overrideable via
  `LlamaCppMtpTurboRepo` / `LlamaCppMtpTurboBranch`) and installed into
  `~/.local-llm/llama-cpp-mtpturbo/` with a `.build-stamp`.
  `localbox update --mode mtpturbo --check` reports whether the installed
  stamp still matches the pinned source; when no build is present, LocalBox
  explains honestly that this mode has no prebuilt download and how to
  provide the binary yourself.
- **`prism`** — the pinned [PrismML llama.cpp fork](https://github.com/PrismML-Eng/llama.cpp)
  with the group-128 low-bit kernels required by Bonsai. LocalBox installs the
  Windows x64 CUDA 12.4 binary plus its CUDA runtime DLL bundle, the regular
  Apple Silicon Metal archive, or a Linux archive (CUDA matched to the
  driver's major with the newest toolkit build within it, Vulkan on an AMD
  GPU, CPU otherwise; x64 and arm64). The KleidiAI archive is deliberately
  not used: KleidiAI accelerates the Arm CPU backend, while normal Apple
  launches use Metal; the rocm archive is likewise not selected — AMD routes
  to Vulkan, matching native mode. When the only available CUDA build's major
  does not match the driver's, the installer warns that the pairing can emit
  garbage output; the launch smoke test is the backstop. Not installed on
  Windows CPU/Vulkan/HIP or Intel macOS.

All four modes start a native `llama-server` process on a free port (default
search starts at `8080`), wait for readiness, then point the agent at
`http://127.0.0.1:<port>` (through the no-think proxy when thinking is
stripped).

```text
# Guided route — pick mode one level down in Customize
localbox

# Direct
localbox launch q3635ba3bapex --context 256k --mode turboquant

# Classic draft-model speculation (any mode): the catalog's DraftModule
# downloads on demand and runs as --spec-type draft-simple. Distinct from
# MTP — one speculation engine per launch; combining them is refused.
# The drafter must share the main model's tokenizer: on a mismatch the
# server logs the incompatibility and keeps running WITHOUT speculation,
# and the launcher warns so a --draft launch never silently costs the
# drafter's memory for nothing.
localbox launch tbonsai27b --context 64k --draft

# MTP + turbo KV together — the 256K-on-24GB recipe. The catalog stores
# SpecType=draft-mtp (mainline canonical); LocalBox translates it to the
# fork's spelling at emit time for this mode automatically.
localbox launch genesisv2 --context 128k --mode mtpturbo

# The catalog requires Prism automatically for this model; --mode is optional.
localbox launch tbonsai27b --context 64k --vision

localbox status               # serve health (proxy + server) and the remedy
localbox log                  # tail the most recent server log
localbox stop                 # stop every llama-server and the proxy
```

---
