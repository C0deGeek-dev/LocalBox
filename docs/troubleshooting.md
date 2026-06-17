# Troubleshooting

Part of the [LocalBox documentation](README.md).

- **Stale wizard / weird errors** → `llmlogerr` for the full trace; use
  `llmlog` for launch/debug details (vision, proxy, llama-server, Claude);
  `llmc` for the native picker or set `$env:LOCAL_LLM_NO_SPECTRE=1` to disable
  Spectre everywhere.
- **Spectre wizard stalls** → raise `$env:LOCAL_LLM_SPECTRE_PROMPT_COOLDOWN_MS`.
- **`localpilot` not on PATH** -> install the CLI with
  `cargo install localpilot`.
- **Need to roll back to the Ollama era** → `git checkout ollama-classic` in
  the repo and re-run `install.ps1`.

---
