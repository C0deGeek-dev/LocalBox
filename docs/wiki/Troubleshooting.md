# Troubleshooting & FAQ

Common problems, error messages, and their fixes. Entries match shipped
behaviour at the current `VERSION`.

> **Do not edit on github.com.** This wiki is generated from in-repo Markdown
> under `docs/wiki/` and synced one-way on every push to `main`. Edit the source
> in `docs/wiki/`; web edits are overwritten on the next sync.

## Stale wizard or weird errors

Run `llmlogerr` for the full trace and `llmlog` for launch/debug details (vision,
proxy, llama-server, Claude). Use `llmc` for the native picker, or set
`$env:LOCAL_LLM_NO_SPECTRE=1` to disable Spectre everywhere.

## The Spectre wizard stalls

Raise the prompt cooldown: `$env:LOCAL_LLM_SPECTRE_PROMPT_COOLDOWN_MS = '750'`.

## `localpilot` not on PATH

Install the CLI: `cargo install localpilot`.

## A quant won't fit / `llama-server` OOMs

Check `info` — quants are tagged `[fits] / [tight] / [over]` against your VRAM.
Pick a smaller quant or a smaller context, or override detection with
`Set-LocalLLMSetting VRAMGB <gb>`.

## Roll back to the Ollama era

`git checkout ollama-classic` in the repo and re-run `install.ps1`.

Full troubleshooting reference:
[troubleshooting.md](https://github.com/C0deGeek-dev/LocalBox/blob/main/docs/troubleshooting.md).
