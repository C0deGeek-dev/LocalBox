# Wizard and Terminal.Gui TUI

Part of the [LocalBox documentation](README.md).

`llm` launches the Spectre picker when `PwshSpectreConsole` is available. Use
`llmc` for the native selectable picker; it uses arrow keys + Enter, while
keeping number/letter shortcuts for fast selection.
It walks: model → quant → mode → vision → strict → context → action →
kvcache/AutoBest → launch.
Each step has a Back option (`0`/Escape in native, `[[Back]]` in Spectre); the
Spectre wizard wraps each prompt in `Invoke-LLMWizardStep` and logs the
full exception trace to `~/.local-llm/wizard-errors.log` if anything throws,
so a Spectre live-display refresh can't scroll the trace off screen. Inspect
with `llmlogerr [-Lines 80]`; reset with `llmlogerrclear`. The launch debug
trace (vision, proxy, llama-server, Claude launches) is recorded in
`~/.local-llm/launch.log` and tailable with `llmlog [-Lines 80]`.

After a model is selected, the Spectre wizard waits briefly before drawing the
next prompt and retries one fast-empty transition. Tune that guard with
`LOCAL_LLM_SPECTRE_PROMPT_COOLDOWN_MS` (default `500`, max `5000`).

`llms` launches the Spectre wizard explicitly. `llmc` remains an explicit
native-picker alias.

```powershell
$env:LOCAL_LLM_SPECTRE_PROMPT_COOLDOWN_MS = '750'
$env:LOCAL_LLM_NO_SPECTRE = '1'   # disable Spectre everywhere / make llm use native
```

---

## Terminal.Gui TUI

`llmtui` launches the C# Terminal.Gui frontend. It is currently an explicit
preview path; `llm` still opens the existing PowerShell wizard flow.

Build and install it from the repo:

```powershell
pwsh .\tui\publish-tui.ps1 -Install
reloadllm
llmtui
```

The main installer can publish TUI binaries too:

```powershell
.\install.ps1 -InstallTui
llm-update -InstallTui
llm-update -RefreshInstalled
```

Without `-InstallTui`, `install.ps1` offers to publish the TUIs interactively
unless `-SkipToolPrompts` is set. `llm-update` refreshes already-installed TUI
binaries after an update, and `-InstallTui` forces a refresh even when the
checkouts are already current. When the LocalPilot checkout fast-forwards,
`llm-update` also reruns LocalPilot's installer so the `localpilot` CLI on
`PATH` matches the updated source. Use `-RefreshInstalled` to redeploy/rebuild
installed artifacts from already-current checkouts.

When installed, the launcher runs `~/.local-llm/bin/LocalBox.Tui.exe` and passes
the active `LocalLLMProfile.ps1` path with `--profile`. From a repo checkout, it
can also run the TUI project directly with `dotnet run`, so the command works on
fresh developer machines before publishing.

Useful controls:

| Key | Action |
|-----|--------|
| `Up` / `Down` | Move in the active list. |
| `Enter` / `Right` | Advance through model -> context -> quant -> action -> mode -> AutoBest -> confirm. |
| `Left` | Go back one wizard step. |
| `Space` | Cycle the current step. |
| `Tab` | Move focus to details so long text can scroll. |
| `Ctrl+B` | Open LocalBench.Tui when LocalBench is installed and has a TUI build. |
| `F5` | Refresh backend data. |
| `F9` | Show dry-run launch command. |
| `F10` | Quit. |

`lbtui` opens LocalBench.Tui directly. It runs the LocalBench TUI project from a
checkout when available, otherwise it falls back to the published
`~/.local-llm/tools/localbench/bin/LocalBench.Tui.exe`.

---
