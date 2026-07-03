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

> LocalBox is a native **Rust** workspace since the v2.0.0 native-stack rewrite
> (the PowerShell module and .NET TUI are retired). Confirm/correct in subject 00.

| Purpose | Command | Notes |
|---|---|---|
| Build | `cargo check --workspace` | from `.github/workflows/ci.yml` |
| Test | `cargo test --workspace` | hermetic seams (proxy ops, env, TUI TestBackend) |
| Lint/format | `cargo fmt --check` then `cargo clippy --workspace --all-targets -- -D warnings` | both clean |
| Docs link-check | `lychee --no-progress --offline docs docs/wiki README.md` | org-pinned |

## §6 plan-specific principles (slot for §6.18+)

- **Tier-1 parity (Windows / Linux / macOS).** LocalBox is native Rust behind
  cross-platform traits (ADR-0007); a box that only works on one OS is not done.
  Reach the OS via the traits/`dunce`/platform-cfg seams, not a POSIX-shell or
  PowerShell assumption.
- **Rust engineering rules hold.** MSRV 1.82, exact-pinned workspace deps,
  `#![forbid(unsafe_code)]`, `unwrap`/`expect`/`todo`/`dbg` denied outside
  `#[cfg(test)]`, typed errors.
- **Shared crate tier is rev-pinned.** Shared primitives come from `localx-llama`
  (`localx-llama-core`/`-runtime`) as a rev-pinned git dep; advance the rev at a
  checkpoint and re-run the suite + the LocalBench launcher-envelope contract.
- **Plan/effect split is load-bearing.** `plan_launch` is read-only (no I/O
  commit); `execute_launch` is the single effect site; DryRun == live plan by
  construction. Keep it that way.
- **Doc-ownership map (which doc owns which area).** Match a change to its owning
  doc; do not restate an area in two places.
  - `README.md` — lean overview, install entry point, ecosystem links.
  - `docs/autobest-profile.md` — AutoBest replay + profile matching.
  - `docs/` owned topics: install, model-management (catalog / quant fit),
    the no-think proxy, harness mode (Claude Code / LocalPilot / Codex dispatch).
  - `docs/wiki/` — wiki source (see below).
  - `CHANGELOG.md` — every user-facing change, under an Unreleased/next heading.
- **Wiki source of truth is in-repo.** `docs/wiki/` is authoritative and
  PR-reviewed; the published GitHub Wiki is a one-way generated mirror — never
  hand-edited on github.com. Wiki Reference pages link the owned `docs/`.
- **VERSION discipline, both directions.** No README/doc/wiki claim may exceed
  the current `VERSION` (read the `VERSION` file, never hardcode a literal),
  **and** no doc may describe the retired PowerShell/.NET stack as current —
  removed paths stay described only as history with their migration tag.

## §7 plan-specific gates

- [ ] `cargo fmt --check`, clippy `-D warnings`, and `cargo test --workspace`
      pass or blockers recorded.
- [ ] No README/doc/wiki claim exceeds the current `VERSION`, and none describes
      the retired PowerShell/.NET stack as current.

## Captain Hindsight prompt — extra "Check specifically for" lines

- Any `README.md`/`docs/`/`docs/wiki/` claim that does not match shipped
  behaviour at the current `VERSION` (in either direction — ahead of VERSION, or
  describing the retired PowerShell/.NET stack), or a wiki page hand-edited on
  github.com instead of the in-repo `docs/wiki/` source.
- OS-specific assumptions that break tier-1 parity (Windows/Linux/macOS).
- A dormant module left built-but-unwired while a doc claims it works.
