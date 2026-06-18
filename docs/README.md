# LocalBox docs

Documentation index and doc-ownership map. Match a change to its owning doc
before editing; don't restate the same area in two places. The top-level
`README.md` is a lean overview — deep content lives here in `docs/`.

| Area | Owning doc |
|---|---|
| Project overview, install quickstart, day-to-day cheatsheet | top-level `README.md` |
| Install, symlink/dev mode, companion checkouts | [`install.md`](install.md) |
| Harness mode — Claude Code / LocalPilot / Codex / serve / strict | [`harness-mode.md`](harness-mode.md) |
| llama.cpp modes — native / turboquant / mtpturbo | [`llamacpp-modes.md`](llamacpp-modes.md) |
| Day-to-day usage, flags, quant keys, 256k recipe | [`usage.md`](usage.md) |
| Model management — adding a model, VRAM-aware fit | [`model-management.md`](model-management.md) |
| Per-machine settings (`settings.json`), verified downloads, per-workspace default | [`settings.md`](settings.md) |
| MCP servers | [`mcp.md`](mcp.md) |
| Auto-tuner (`findbest`) | [`auto-tuner.md`](auto-tuner.md) |
| AutoBest profile format | [`autobest-profile.md`](autobest-profile.md) |
| Wizard & Terminal.Gui TUI | [`wizard-and-tui.md`](wizard-and-tui.md) |
| Repo layout, architecture, casing convention | [`architecture.md`](architecture.md) |
| Versioning policy — when minor/major bumps, pre-1.0 rules | [`versioning.md`](versioning.md) |
| Troubleshooting | [`troubleshooting.md`](troubleshooting.md) |

## Wiki

User-facing guides (Getting Started, How-Tos, Examples, Troubleshooting) are
authored as in-repo Markdown under `docs/wiki/` and one-way CI-synced to the
GitHub Wiki. The in-repo source is authoritative — never edit pages on
github.com. Wiki Reference pages link these `docs/` pages rather than
duplicating them.

## Changelog & version

Every user-facing change updates the top-level `CHANGELOG.md` in the same
checkpoint. No doc, README, or wiki page may claim behaviour beyond the current
`VERSION`. Which number moves — major, minor, or patch — is decided by
[`versioning.md`](versioning.md) (SemVer, with pre-1.0 rules).
