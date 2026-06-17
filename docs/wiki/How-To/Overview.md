# How-To guides

Task-oriented recipes — each answers a single "how do I…?" against shipped
behaviour at the current `VERSION`. See **[[Getting-Started]]** first.

> **Do not edit on github.com.** This wiki is generated from in-repo Markdown
> under `docs/wiki/` and synced one-way on every push to `main`. Edit the source
> in `docs/wiki/`; web edits are overwritten on the next sync.

## Install LocalBox

```powershell
. .\install.ps1                  # copy + wire $PROFILE
. .\install.ps1 -Symlink         # symlink instead of copy (dev mode)
. .\install.ps1 -InstallLocalBench -InstallLocalPilot   # also clone companions
```

Full reference: [install.md](https://github.com/C0deGeek-dev/LocalBox/blob/main/docs/install.md).

## Manage local models

Add a model from a Hugging Face repo (registers every recognized GGUF quant by
default):

```powershell
addllm <hf-url-or-repo> -Key <key> [-Quants Q4_K_P,IQ4_XS] [-DefaultQuant Q4_K_P]
updatellm <key>          # backfill quants missing from an existing entry
removellm <key>          # remove (confirms first; -KeepFiles to keep GGUF blobs)
info <key>               # see the exact filename behind each quant key
```

VRAM-aware: `info` and the `llm` wizard tag each quant `[fits] / [tight] /
[over]` against your card. Full detail:
[model-management.md](https://github.com/C0deGeek-dev/LocalBox/blob/main/docs/model-management.md).

## Run the no-think proxy / serve a model

For Claude Code launches LocalBox automatically starts the Python no-think proxy
(`127.0.0.1:11435`) in front of `llama-server` and strips Anthropic-only
`thinking`/`reasoning` blocks the local backend can't parse. You don't start it
manually — it comes up with the launch.

To serve a model to other machines over an Anthropic-compatible endpoint:

```powershell
$env:LOCAL_LLM_SERVE_PASS = "chosenpass"
llmserve -Key qcoder30 -ContextKey 32k -LlamaCppMode native
```

Password-only HTTP is for LAN/VPN use; put HTTPS in front for anything public.
See [harness-mode.md](https://github.com/C0deGeek-dev/LocalBox/blob/main/docs/harness-mode.md).

## Save and replay an AutoBest profile

Tune with LocalBench, then replay the saved profile with `-AutoBest`:

```powershell
findbest q36plus -ContextKey 256k          # tune (delegates to LocalBench)
q36p -Ctx 256 -AutoBest                     # replay the saved profile
```

See [auto-tuner.md](https://github.com/C0deGeek-dev/LocalBox/blob/main/docs/auto-tuner.md)
and [autobest-profile.md](https://github.com/C0deGeek-dev/LocalBox/blob/main/docs/autobest-profile.md).
