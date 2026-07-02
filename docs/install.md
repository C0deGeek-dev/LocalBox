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
model catalog (`llm-models.json`). Existing files are never overwritten.
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

An NVIDIA GPU is recommended; VRAM is auto-detected via `nvidia-smi` and
drives the guided launcher's fit hints.

## Companions

- [LocalPilot](https://github.com/C0deGeek-dev/LocalPilot) — install its CLI
  so `localbox launch <model> --agent localpilot` can hand off to it.
- [LocalBench](https://github.com/C0deGeek-dev/LocalBench) — provides
  `localbench findbest`, whose saved profiles the guided launcher replays.

---
