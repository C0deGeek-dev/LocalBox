# Install

Part of the [LocalBox documentation](README.md).

LocalBox is a single native binary. It needs no PowerShell, .NET, or Python
at runtime, on any platform.

## From source

From the repo root (Rust toolchain pinned by `rust-toolchain.toml`):

```text
cargo install --path crates/localbox --locked
```

Or run straight from the checkout without installing:

```text
cargo run -p localbox
```

## First run

```text
localbox                         # guided launcher: pick a model, confirm, go
localbox info                    # list the configured models
localbox status                  # serve health and the remedy when down
```

The first run seeds `~/.local-llm` with the shipped defaults and an editable
model catalog (`llm-models.json`). Your catalog and `settings.json` are never
overwritten; the two *shipped* layers (`defaults.json`,
`llm-models.example.json`) refresh to match the installed binary so release
pins and the shipped model list never go stale — put your own overrides in
`settings.json`, which always wins. When a new LocalBox version ships models
your catalog predates, `localbox update` says so, and
`localbox update --merge-models` adds them without touching your entries.
`llama-server` binaries download pinned and checksum-verified on first use
(`localbox update`); GGUF weights download from Hugging Face on first launch.

## Per platform

- **Windows** — CUDA or CPU `llama-server` builds download automatically,
  matched to your driver's CUDA major version.
- **Linux** — prebuilt llama.cpp release assets are downloaded when available
  (CUDA best-effort, CPU otherwise). If no asset fits your system, install
  your own `llama-server` into `~/.local-llm/llama-cpp/`.
- **macOS** — prebuilt Metal assets are downloaded when available; otherwise
  bring your own `llama-server` the same way.

The optional `prism` engine for Ternary Bonsai installs on Windows x64 with
an NVIDIA CUDA 12-compatible driver, Apple Silicon with Metal, and Linux
(x64/arm64: CUDA matched to your driver's major, Vulkan on AMD, CPU
otherwise; the Linux path is not yet live-validated). The Windows build
targets CUDA 12.4 — on a newer driver the installer warns that a mismatched
build can emit garbage output; the launch smoke test catches that before an
agent sees it. Install or verify explicitly with
`localbox update --mode prism`.

An NVIDIA GPU is recommended; VRAM is auto-detected via `nvidia-smi` and
drives the guided launcher's fit hints.

## Upgrading from 1.x

LocalBox 1.x was a PowerShell launcher (`install.ps1`, `LocalLLMProfile.ps1`,
the `llm*` command family). 2.0.0 replaced all of it with the `localbox`
binary, and the old cleanup path went with it — so a 1.x install needs three
manual steps after the upgrade:

1. **Remove the profile hook.** `install.ps1` added a dot-source line to your
   PowerShell profile (`$PROFILE.CurrentUserAllHosts`, typically
   `Documents\PowerShell\profile.ps1`). Delete the line that loads
   `LocalLLMProfile.ps1` — with the file gone it errors at every shell start.
2. **Delete the deployed 1.x files.** Remove
   `~/.local-llm/LocalLLMProfile.ps1` and `~/.local-llm/lib/` (symlinks in a
   symlink-mode install, copies in a copy-mode install). Keep everything
   else — `llm-models.json`, `settings.json`, downloaded models, and tuner
   state all carry over unchanged.
3. **Install the binary** as above, then run `localbox status` — it detects
   any remaining 1.x leftovers and repeats this remedy.

The commands map directly:

| 1.x | 2.x |
|---|---|
| `llm` | `localbox` |
| `llmdefaultserve` / `Start-LocalLLMHeadlessServe` | `localbox serve <model>` |
| `llmstop` | `localbox stop` |
| `llm-update` (llama.cpp binaries) | `localbox update` |
| `llm-update` (launcher itself) | `git pull` + `cargo install --path crates/localbox --locked` |
| `llmembedserve` | `localbox embed-serve` |
| `llmdocs` / `info -Commands` | `localbox help` |

## Companions

- [LocalPilot](https://github.com/C0deGeek-dev/LocalPilot) — install its CLI
  so `localbox launch <model> --agent localpilot` can hand off to it.
- [LocalBench](https://github.com/C0deGeek-dev/LocalBench) — provides
  `localbench findbest`, whose saved profiles the guided launcher replays.

---
