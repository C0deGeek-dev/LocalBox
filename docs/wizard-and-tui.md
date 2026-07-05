# Guided launcher

Part of the [LocalBox documentation](README.md).

`localbox` with no arguments opens the guided launcher — a plain-language,
non-developer-friendly flow built into the binary (no separate TUI install):

```text
pick a model  →  read the summary  →  confirm  →  launch  →  return
```

- **The picker** shows the catalog's `recommended` tier by default, with a
  `[Show all tiers]` row when more models exist (a model without a tier reads
  as `experimental` and stays behind that row). Each row carries the model's
  display name and a fit hint against your VRAM: green `fits`, yellow
  `tight`, red `over`.
- **The summary** is five short plain-language lines — what runs, how much
  memory it wants, the quality hint, where it serves, what happens next. No
  jargon: internals like quant spellings stay one level down.
- **The confirm menu** offers exactly five actions: launch, customize, save
  as my default, help, and back.
- **Customize** is the power level: quant variant, context window, engine
  (`native` / `turboquant` / `mtpturbo`), KV cache, vision, strict, and
  auto-tune. Rows the current selection locks out explain *why* instead of
  disappearing. Auto-tune (on by default when a measured profile exists)
  replays the best saved `localbench findbest` result and owns the
  engine/KV choices while on; manual KV settings only fill gaps the profile
  left.
- **Save as my default** persists the whole recipe; the next `localbox` run
  offers it as a one-keystroke replay. Model-specific choices such as quant,
  context, strict, and vision replay only for the model they were saved with.
  A `.llm-default` file in a workspace (a single line naming a model)
  overrides the saved default for that directory tree — the nearest file
  walking up from the working directory wins.
- **Help** opens a plain-language glossary of every term the flow uses.

After the agent session ends, the launcher returns to the picker.

## Rendering

On a real terminal the launcher renders as an inline list in a fixed-height
viewport — no alternate screen, no whole-screen clears, so everything above
the live band stays in your native scrollback. Errors render as one bounded,
plain-language line, never a stack trace.

On a non-TTY (a pipe, a harness) or with `--plain`, the same flow degrades to
numbered plain-text menus with zero escape codes.

```text
localbox            # inline interactive launcher
localbox --plain    # numbered plain-text menus
```

## Logs

Server output lands under `~/.local-llm/logs/`. Tail the most recent log:

```text
localbox log --lines 80
```

---
