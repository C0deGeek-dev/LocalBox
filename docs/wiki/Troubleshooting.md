# Troubleshooting & FAQ

Common problems, error messages, and their fixes. Entries match shipped
behaviour at the current `VERSION`.

> **Do not edit on github.com.** This wiki is generated from in-repo Markdown
> under `docs/wiki/` and synced one-way on every push to `main`. Edit the source
> in `docs/wiki/`; web edits are overwritten on the next sync.

## A launch fails or the agent can't reach the model

`localbox status` reports the serve health (proxy + server) and the remedy;
`localbox log` tails the most recent server log.

## Menus render oddly in this terminal

`localbox --plain` uses numbered plain-text menus with no escape sequences;
non-TTY sessions degrade to them automatically.

## `localpilot` not on PATH

Install the CLI: `cargo install localpilot`.

## A quant won't fit / `llama-server` OOMs

Check the guided launcher — quants are tagged fits / tight / over against
your VRAM. Pick a smaller quant or a smaller context, or override detection
with the `VRAMGB` key in `~/.local-llm/settings.json`. On an engine build with
`--fit`, a model without its own placement is placed by llama.cpp itself;
`LlamaCppFitTargetMiB` sets how much VRAM it leaves free. `localbench findbest`
finds the fastest placement that fits and saves it for the next launch.

## A model starts but runs far slower than it should (Windows)

The GPU driver ran out of VRAM and moved part of it into system memory instead
of failing. Put more of the model on the CPU (a higher `NCpuMoe`, a lower
`NGpuLayers`), use a smaller context, or raise `LlamaCppFitTargetMiB`.

## `CUDA error: shared object initialization failed` at startup

The host ran out of commit (RAM plus page file), usually with `NoMmap` on a
large MoE model or while another model loads on the same GPU. Enlarge the
Windows page file, stop the other model, or drop `NoMmap` for that model.

## Roll back to the Ollama era

`git checkout ollama-classic` in the repo — that era is PowerShell-based and
installs via its own `install.ps1`.

Full troubleshooting reference:
[troubleshooting.md](https://github.com/C0deGeek-dev/LocalBox/blob/main/docs/troubleshooting.md).
