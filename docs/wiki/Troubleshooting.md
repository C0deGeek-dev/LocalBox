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
with the `VRAMGB` key in `~/.local-llm/settings.json`.

## Roll back to the Ollama era

`git checkout ollama-classic` in the repo — that era is PowerShell-based and
installs via its own `install.ps1`.

Full troubleshooting reference:
[troubleshooting.md](https://github.com/C0deGeek-dev/LocalBox/blob/main/docs/troubleshooting.md).
