# Versioning policy

LocalBox follows [Semantic Versioning](https://semver.org): `MAJOR.MINOR.PATCH`.
This page is the single decision record for which number moves and when, so the
call is not re-litigated per release. The authoritative version is the
top-level `VERSION` file; every user-facing change also lands a `CHANGELOG.md`
entry in the same checkpoint.

## The rule

| Change | Bump |
|---|---|
| Breaking change — removed/renamed flag, changed config or profile schema, dropped behaviour a user relied on | MAJOR |
| New feature, backward compatible — existing setups keep working | MINOR |
| Bugfix, no surface change | PATCH |

Mnemonic: **break → major, add → minor, fix → patch.**

A "user-facing surface" for LocalBox means anything a user or a downstream tool
depends on: CLI flags and subcommands, `settings.json` keys, AutoBest profile
format, harness-mode names, MCP server contracts, and the documented quant keys.
Changing the wire shape of any of these is a break.

## Pre-1.0 caveat (we are here)

While the major is `0`, there is no stability promise yet, and SemVer shifts
every category down one slot:

| Change | Pre-1.0 bump |
|---|---|
| Breaking change | `0.MINOR` — e.g. `0.3.x` → `0.4.0` |
| Feature **or** fix | `0.x.PATCH` — e.g. `0.3.0` → `0.3.1` |

So today a breaking change bumps the **minor** (`0.3` → `0.4`), and features and
fixes alike bump the **patch**. The pre-release suffix (`-beta.N`) iterates a
target version that has not shipped final yet: `0.3.0-beta.2` → `0.3.0-beta.3`
is the next iteration of the unreleased `0.3.0`.

## Deprecation vs removal

Deprecating a feature (marking it for removal while it still works) is **not** a
break — it is a backward-compatible signal, so it does not force a major (or,
pre-1.0, a minor). The break happens when the deprecated thing is **removed**;
that removal is what bumps the minor pre-1.0 (major post-1.0).

Example: the standalone no-think proxy script was deprecated while its
in-process replacement shipped alongside it — a patch-class change. Deleting
it (with the rest of the PowerShell launcher surface) is the break that
triggers the major bump.

## When `1.0.0`?

Not time-driven. Ship `1.0.0` when all of these hold:

- The CLI and `settings.json`/profile surface is stable and not expected to churn.
- We commit to not breaking existing user setups without a major bump.
- Docs match shipped reality and the install path is solid.

Until then, stay on `0.x` and treat breaking changes as minor bumps.
