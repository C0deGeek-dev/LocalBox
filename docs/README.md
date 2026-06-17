# LocalBox docs

Documentation index and doc-ownership map. Match a change to its owning doc
before editing; don't restate the same area in two places. Keep the top-level
`README.md` a lean overview — deep how-to content belongs here in `docs/`.

| Area | Owning doc |
|---|---|
| Project overview, install entry point, ecosystem links | top-level `README.md` |
| Autobest profile tuning | [`autobest-profile.md`](autobest-profile.md) |
| Install / prerequisites (planned owned doc) | `install.md` |
| Model management — catalog, adding a model, quant fit | `model-management.md` (planned) |
| No-think proxy behaviour | `proxy.md` (planned) |
| Harness mode — Claude Code / LocalPilot / Codex dispatch | `harness-mode.md` (planned) |

Pages marked *(planned)* are created as the top-level README is slimmed; until
then those areas live in `README.md`.

## Wiki

User-facing guides (Getting Started, How-Tos, Examples, Troubleshooting) are
authored as in-repo Markdown under `docs/wiki/` and one-way CI-synced to the
GitHub Wiki. The in-repo source is authoritative — never edit pages on
github.com. Wiki Reference pages link these `docs/` pages rather than
duplicating them.

## Changelog & version

Every user-facing change updates the top-level `CHANGELOG.md` in the same
checkpoint. No doc, README, or wiki page may claim behaviour beyond the current
`VERSION`.
