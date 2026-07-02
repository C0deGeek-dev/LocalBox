# How-To guides

Task-oriented recipes — each answers a single "how do I…?" against shipped
behaviour at the current `VERSION`. See **[[Getting-Started]]** first.

> **Do not edit on github.com.** This wiki is generated from in-repo Markdown
> under `docs/wiki/` and synced one-way on every push to `main`. Edit the source
> in `docs/wiki/`; web edits are overwritten on the next sync.

## Install LocalBox

```text
cargo install --path crates/localbox --locked
```

No PowerShell, .NET, or Python needed at runtime. Full reference:
[install.md](https://github.com/C0deGeek-dev/LocalBox/blob/main/docs/install.md).

## Manage local models

The catalog is `~/.local-llm/llm-models.json` — an ordinary JSON file, seeded
on first run and yours to edit (add a model by copying an entry and pointing
`Repo`/`Quants` at the Hugging Face repo):

```text
localbox info                    # list the configured models by tier
localbox info <model>            # one model in detail (any of its names works)
localbox purge                   # stop servers, delete downloaded model files
```

VRAM-aware: the guided launcher tags each quant fits / tight / over against
your card. Full detail:
[model-management.md](https://github.com/C0deGeek-dev/LocalBox/blob/main/docs/model-management.md).

## Run the no-think proxy / serve a model

For Claude Code launches LocalBox automatically brings up its in-process
no-think proxy (`127.0.0.1:11435`) in front of `llama-server` and strips
`thinking`/`reasoning` blocks the local backend can't parse. You don't start
it manually — it comes up with the launch.

To serve a model to other machines over an Anthropic-compatible endpoint:

```text
localbox serve qcoder30 --context 32k --lan --password chosenpass
```

Password-only HTTP is for LAN/VPN use; put HTTPS in front for anything public.
A public-looking bind with no password is refused unless you explicitly opt in.
See [harness-mode.md](https://github.com/C0deGeek-dev/LocalBox/blob/main/docs/harness-mode.md).

## Save and replay an AutoBest profile

Tune with LocalBench, then let the guided launcher replay the saved profile:

```text
localbench findbest --model q36plus --context 64k    # tune
localbox                                             # guided launch replays it
```

See [auto-tuner.md](https://github.com/C0deGeek-dev/LocalBox/blob/main/docs/auto-tuner.md)
and [autobest-profile.md](https://github.com/C0deGeek-dev/LocalBox/blob/main/docs/autobest-profile.md).
