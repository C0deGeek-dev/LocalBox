# Plan-Template Overrides — LocalBox

Project-specific content spliced into a copy of the canonical plan template
(the `plan-from-template` skill in the c0degeek-ai plugin). The canonical
template is generic; everything LocalBox-specific lives here. Never fork the
template — generic improvements go upstream to c0degeek-ai instead.

LocalBox has no dedicated planning skill, so the c0degeek `plan-from-template`
skill auto-splices this file from its conventional path
(`.claude/plan-template-overrides.md`). Each section below names the extension
point in the copied plan where its content lands.

> **LocalX workspace note.** Plans, tasks, and work tracking live in the private
> LocalHub repo (`LocalHub/plans/localbox/`), never in this repo. This repo keeps
> only its `docs/`, README, and CHANGELOG. See `LocalX/CLAUDE.md`.

## §2 Verification-commands rows (repo defaults, mirror CI)

| Purpose | Command | Notes |
|---|---|---|
| Lint (launcher) | `Invoke-ScriptAnalyzer -Path local-llm -Recurse -Settings ./PSScriptAnalyzerSettings.psd1 -EnableExit` | PowerShell; from `.github/workflows/pester.yml` |
| Test (unit) | `Invoke-Pester -Path tests/unit` | Pester 5 |
| Proxy lint/test | `ruff check localbox-proxy tests/test_no_think_proxy.py` then `python -m pytest tests/test_no_think_proxy.py -q` | Python no-think proxy |
| TUI build | `dotnet build tui/LocalBox.Tui/LocalBox.Tui.csproj -nologo` | .NET TUI |
| Docs link-check | `lychee --no-progress --offline docs docs/wiki README.md` | adopt lychee (org-pinned) |

## §6 plan-specific principles (slot 16)

- **Windows / PowerShell only.** The launcher manages `llama-server`, drives
  `Start-Process`, reads `nvidia-smi`, and touches `$PROFILE` — it does not work
  in WSL/bash. A box that assumes a POSIX shell is not done.
- **Doc-ownership map (which doc owns which area).** Match a change to its owning
  doc; do not restate an area in two places.
  - `README.md` — lean overview, install entry point, ecosystem links. Deep
    how-to content moves into `docs/`.
  - `docs/autobest-profile.md` — autobest profile tuning.
  - `docs/` owned topics (created as the README is slimmed): install,
    model-management (catalog / adding a model / quant fit), the no-think proxy,
    harness mode (Claude Code / LocalPilot / Codex dispatch).
  - `docs/wiki/` — wiki source (see below).
  - `CHANGELOG.md` — every user-facing change, under an Unreleased/next heading.
- **Wiki source of truth is in-repo.** `docs/wiki/` is authoritative and
  PR-reviewed; the published GitHub Wiki is a one-way generated mirror — never
  hand-edited on github.com. Wiki Reference pages link the owned `docs/`, never
  duplicate them.
- **VERSION discipline.** No README/doc/wiki claim may exceed the current
  `VERSION` (`0.3.0-beta.2`). Removed paths (e.g. Ollama) stay described only as
  history with their migration tag.

## §7 plan-specific gates

- [ ] PSScriptAnalyzer, Pester, proxy (ruff + pytest), and TUI build all pass or
      blockers recorded.
- [ ] No README/doc/wiki claim exceeds the current `VERSION`.

## Captain Hindsight prompt — extra "Check specifically for" lines

- Any `README.md`/`docs/`/`docs/wiki/` claim that does not match shipped
  behaviour at the current `VERSION`, or a wiki page hand-edited on github.com
  instead of the in-repo `docs/wiki/` source.
- PowerShell-only assumptions that silently break under WSL/bash.
