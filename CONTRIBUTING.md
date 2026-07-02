# Contributing

Thanks for your interest in LocalBox, part of the [LocalX](https://c0degeek-dev.github.io/LocalStack/) stack.

## Ground rules

- Keep changes scoped and focused; one concern per pull request.
- Match the surrounding code style and naming.
- Update the relevant docs when you change behavior or configuration.

## Building and testing

Rust workspace. Before opening a pull request:

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Pull requests

- Describe what changed and why, and link any related issue.
- Note how you tested the change.
- Expect review before merge.

## Reporting security issues

Do not open a public issue for a vulnerability. See [SECURITY.md](SECURITY.md).
