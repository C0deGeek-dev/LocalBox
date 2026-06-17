# MCP servers

Part of the [LocalBox documentation](README.md).

Claude Code's MCP servers expose tools with names like `mcp__<server>__<tool>`.
They reach the local model through the same launch path:

- Models with `"LimitTools": false` (e.g. `dev`) get every MCP tool
  automatically — the `--tools` flag isn't passed.
- Models with `"LimitTools": true` (default) only see tools in the allowlist.
  Add the MCP tool names you want to either the global `LocalModelTools` field
  in `defaults.json` / `settings.json` or a per-model `Tools` override.

Example per-model override:

```json
"q36plus": {
  ...,
  "Tools": "Bash,Read,Write,Edit,Glob,Grep,mcp__filesystem__read_file,mcp__filesystem__write_file"
}
```

`info` shows a `Tools  : ...` line for any model that overrides the global list.

---
