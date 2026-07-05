# Troubleshooting

Part of the [LocalBox documentation](README.md).

- **Launch fails or the agent can't reach the model** → `localbox status`
  reports the serve health (proxy + server) and the remedy; `localbox log`
  tails the most recent server log.
- **Menus render oddly in your terminal** → `localbox --plain` uses numbered
  plain-text menus with no escape sequences; a non-TTY session degrades to
  them automatically.
- **A download or install looks wrong** → `localbox update --check` reports
  what each mode's binary resolves to without changing anything;
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
