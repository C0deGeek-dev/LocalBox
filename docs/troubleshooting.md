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
  "the staged llama-server could not start because Windows could not find a
  DLL it needs" means the build needs a library this computer does not have —
  the working engine is still there. When the message goes on to name
  `vcruntime140.dll`/`msvcp140.dll`, that library is the Microsoft Visual C++
  Redistributable (x64), which every llama.cpp Windows build loads and a fresh
  Windows does not have: install it from
  https://aka.ms/vs/17/release/vc_redist.x64.exe and run the command again.
  Otherwise report it upstream, pin a release that carries its dependencies, or
  rerun with `--skip-load-probe` if you are certain the probe is wrong;
  `localbox launch <model> --dry-run` prints the full plan (paths, argv,
  environment) without touching the system.
- **The CUDA build fails to start or answer on this machine** → run
  `localbox probe <model> --fallback`. It tries the Vulkan build, then the CPU
  build, keeps the original until one passes, and restores it when none does.
  `localbox status` then shows the verified backend and the fallback in use.
  A driver update brings the CUDA build back on the next `localbox update`.
- **The probe says the model does not fit, or cannot tell why it failed** →
  the engine build was left alone on purpose. Try a smaller quant or context
  (`localbox info <model>` lists them) or a smaller model. If the server log
  shows the GPU backend itself failing, choose another build with
  `localbox update --variant vulkan` (or `cpu`) and probe again.
- **"an interrupted backend fallback left a state LocalBox cannot settle
  safely"** → a fallback was stopped part way and the engine folder is not one
  of the two builds it recorded, so nothing was deleted. The message names the
  engine folder, the kept original (`llama-cpp.previous`) and the journal
  (`.localbox-fallback.json`). Each build's `.build-stamp` names its release
  and variant: keep the one you want as `llama-cpp`, remove the other and the
  journal, then run the command again. An interruption LocalBox can settle is
  undone (or finished) by itself on the next `update` or `probe`.
- **"another LocalBox command is changing the engine right now"** → an
  `update` or `probe` is already running; wait for it to finish. The lock is
  released when that process ends, however it ends.
- **`serve --again` refuses** → it starts only the last successful `serve`
  on that proxy port. "LocalBox has no saved launch for port …" means no serve
was recorded there yet,
  or the last one used an option a replay cannot repeat (other raw server
  arguments, `--keep-thinking`, network exposure): run `localbox serve <model>`
  once. "LocalBox already serves … on port …" means another LocalBox model holds
  the port: `localbox stop` frees it (it stops every LocalBox model server, not
  only that one), then retry. "port … is already in use" means
  another program holds it; `serve --again` leaves it alone, so close or
  reconfigure that program, then retry (do not use `localbox stop` for it). "the saved launch
  for port … is X, not Y" means `--expect-model` named another model than the
  saved one.
- **Piping `localbox serve` output waits until the model stops (Windows)** →
  the model server LocalBox starts keeps a copy of the pipe, so a reader such
  as `| Tee-Object` or `| more` sees the end only when the server exits.
  Redirect to a file instead (`localbox serve <model> > serve.log 2>&1`), or
  use `--json` and read the file. `localx setup` and LocalPilot are not
  affected.
- **`localpilot` not on PATH** → run the LocalX installer (or `localx
  install`) and open a new terminal; `localx doctor` names an older copy that
  runs instead.
- **Start over on model files** → `localbox purge` stops servers and deletes
  downloaded GGUFs; they download again on the next launch.
- **A model starts but runs far slower than it should (Windows)** → the GPU
  driver ran out of VRAM and quietly moved part of it into system memory
  instead of failing; the server works at a fraction of its speed. Put more of
  the model on the CPU (a higher `NCpuMoe`, a lower `NGpuLayers`), use a
  smaller context or KV cache type, or raise `LlamaCppFitTargetMiB` so
  llama.cpp's own placement keeps more VRAM free. `localbench findbest` finds
  the fastest placement that fits and saves it as the model's AutoBest profile.
- **`CUDA error: shared object initialization failed` at startup** → the host
  ran out of *commit* (RAM plus page file), not VRAM. It happens most with
  `NoMmap` (`--load-mode none`) on a large MoE model, whose CPU-side experts
  are copied into RAM, and when another model is loading on the same GPU.
  Enlarge the Windows page file (a fixed size of 32 GB or more on a 64 GB
  machine), stop the other model, or drop `NoMmap` for that model.
- **Local model replies stop mid-sentence or mid-word, with no error** → the
  agent's completion hit `LocalModelMaxOutputTokens` (default 16384), a
  client-side output cap, not a crash. Raise it in `~/.local-llm/settings.json`
  (e.g. `"LocalModelMaxOutputTokens": 32768`), or set it to `0` to leave the
  client's own default (32k) untouched. A larger cap costs decode time on
  local hardware for replies that actually need it, not extra VRAM.

---
