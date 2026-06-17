# Install

Part of the [LocalBox documentation](README.md).

From the repo root:

```powershell
. .\install.ps1                  # copy files to deployed locations + add to $PROFILE
. .\install.ps1 -Symlink         # symlink instead of copy (admin / dev mode)
. .\install.ps1 -SetupProfile    # only ensure $PROFILE dot-sources the deployed file
. .\install.ps1 -InstallLocalBench   # also clone LocalBench if missing
. .\install.ps1 -InstallLocalPilot   # also clone LocalPilot if missing
. .\install.ps1 -DryRun          # preview without changing anything
```

After install, open a fresh PowerShell:

```powershell
llm                              # interactive wizard — pick model, mode, action
llmtui                           # Terminal.Gui TUI, explicit preview path
info                             # verify: VRAM, default model, configured quants
```

The install step offers to clone missing LocalBench and LocalPilot into
`~/.local-llm/tools/` (a sibling `LocalPilot` checkout next to the LocalBox
repo is detected and used first). Use `-SkipToolPrompts` for
unattended installs. `Show-Diagnostics` also reports on `python`, the
`localpilot` CLI, `PwshSpectreConsole`, LocalBench, and LocalPilot.
Installs also record `LocalBoxRoot` in `settings.json`, which lets `llm-update`
pull this repo and refresh the installed LocalX artifacts later.

---
