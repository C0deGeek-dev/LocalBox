# Troubleshooting

Part of the [LocalBox documentation](README.md).

- **Launch fails or the agent can't reach the model** → `localbox status`
  reports the serve health (proxy + server) and the remedy; `localbox log`
  tails the most recent server log.
- **Menus render oddly in your terminal** → `localbox --plain` uses numbered
  plain-text menus with no escape sequences; a non-TTY session degrades to
  them automatically.
- **A download or install looks wrong** → `localbox update --check` reports
  each mode's latest release and selected assets, including runtime companions
  and expected download sizes, without changing anything. If an engine binary
  is missing, run the exact mode command from the error, for example
  `localbox update --mode native`. A checksum/extraction failure leaves the
  prior engine and stamp active, and so does a staged build that cannot start:
  "the staged llama-server could not start because a runtime library it links
  against is missing from this host" means the release archive is incomplete
  for this machine, not that your install broke — the working engine is still
  there. Report it upstream, pin a release that carries its dependencies, or
  rerun with `--skip-load-probe` if you are certain the probe is wrong;
  `localbox launch <model> --dry-run` prints the full plan (paths, argv,
  environment) without touching the system.
- **`localpilot` not on PATH** → install the CLI with
  `cargo install localpilot`.
- **Start over on model files** → `localbox purge` stops servers and deletes
  downloaded GGUFs; they download again on the next launch.
- **Local model replies stop mid-sentence or mid-word, with no error** → the
  agent's completion hit `LocalModelMaxOutputTokens` (default 16384), a
  client-side output cap, not a crash. Raise it in `~/.local-llm/settings.json`
  (e.g. `"LocalModelMaxOutputTokens": 32768`), or set it to `0` to leave the
  client's own default (32k) untouched. A larger cap costs decode time on
  local hardware for replies that actually need it, not extra VRAM.

---
