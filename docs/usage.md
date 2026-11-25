# Day-to-day usage

Part of the [LocalBox documentation](README.md).

The guided launcher (`localbox` with no arguments) is the everyday path:
pick a model, read the plain-language summary, confirm, and the agent opens.
Everything it does is also scriptable:

```text
localbox                            open the guided launcher (pick a model)
localbox --plain                    guided launcher with plain-text menus
localbox launch <model> [options]   resolve, start, and hand off to an agent
localbox serve <model> [options]    start the model (and proxy) headless
localbox stop                       stop every model server and the proxy, and wait for them to exit
localbox status                     report serve health and the remedy
localbox info [model]               list the configured models, or one in detail
localbox download <model> [--quant <key>] [--vision] [--draft]
                                    fetch a model's files without starting it
localbox purge                      stop servers and delete downloaded model files
localbox clean --partials [--check] remove partial downloads LocalBox will not
                                    continue (--check previews, removes nothing)
localbox log [--lines <n>]          tail the most recent server log
localbox embed-serve [--port <p>]   start the CPU-only embedding server
localbox embed-stop                 stop the embedding server
localbox launch <model> [--if-running <ask|continue|stop|cancel>]
                                    warn when a model is already running
                                    (named with its port; an embedding
                                    server never counts)
localbox update [--mode <m>] [--check] [--variant <cuda|vulkan|cpu|auto>]
                                    install and pin the latest llama.cpp releases
localbox probe <model> [--fallback] [--keep-serving] [--json]
                                    prove the engine can load the model and
                                    answer; --fallback tries other builds
localbox version                    print the launcher version envelope
```

Options for `launch` / `serve`:

| Flag | Effect |
|------|--------|
| `--context <key>` | One of the model's context keys (see `localbox info <model>`). Omit for the default. |
| `--mode <m>` | `native` / `turboquant` / `prism` — which llama-server binary to use. A model's `RequiredMode` wins. |
| `--quant <key>` | Switch the GGUF quant for this launch (default per model). |
| `--auto-best` | Require a usable saved tune; refuse fallback. |
| `--no-auto-best` | Deliberately use catalog/settings defaults. |
| `--allow-untuned` | Permit non-interactive fallback after the warning was reviewed. |
| `--adopt-tune` | Carry a matching older tune's runnable settings into the current version, record provenance, and use it in this launch. |
| `--vision` | Load the model's multimodal projector when it has one; a configured missing projector downloads on demand. |
| `--keep-thinking` | Let the model's thinking reach the agent unfiltered. |
| `--agent <a>` | `claude` (default) / `localpilot` / `codex` / `none`. |
| `--server-arg <arg>` | Pass one raw argument to `llama-server` for this launch only — repeat it per token (`--server-arg --spec-type --server-arg ngram-mod`). Appended last so it wins; never saved. |
| `--dry-run` | Print the full plan (GGUF, argv, env) and change nothing. |
| `--lan` | Expose the gateway on the network (see [harness-mode.md](harness-mode.md)). |

Quant keys are model-local labels, not a universal naming scheme. Use
`localbox info <model>` to see the exact filename and size behind each quant
key, and which contexts the model declares.

### Examples

```text
localbox launch q3635ba3bapex --context 32k --agent localpilot
localbox launch q36plus --context 128k
localbox launch q36plus --mode turboquant --quant q6kp
localbox serve q36plus --context 64k
localbox launch ornith35hapex --context 16k --vision
```

### Checking the engine on this machine

`localbox probe <model>` starts the model with the installed llama.cpp build,
checks a one-word reply through a no-think proxy, the same path agents use,
and stops what it started. That proxy is the probe's own, on a free loopback
port: a probe never starts, repoints or stops the shared proxy. A
pass is recorded as the verified backend, which `localbox status` shows.
It takes the same `--quant`, `--context`, `--mode` and run-profile flags as
`serve`; `--keep-serving` leaves the model and the probe's proxy running
(the endpoint is printed), and `--json` prints the result (backend, any fallbacks with their
reasons, launch arguments) for scripts; a failure prints `ok: false` with
the stage it stopped at, a reason class, the message and, when a server was
started, its log. A probe needs the proxy and the GPU to itself, so it refuses to run
while another model is serving unless you pass `--if-running stop`.

When the CUDA build cannot run on this machine, `--fallback` tries the Vulkan
build, then the CPU build. The build that was installed stays on disk until
one of them passes, and comes back if none does. Only a failure the server
log ties to the build itself (a driver or toolkit mismatch, a missing kernel
or device, a crash while the GPU backend starts) starts the fallback. A model
that does not fit in memory, or a failure the log does not explain, changes
nothing and says what to try instead, because every model shares the one
build and a slower build would slow all of them.

A fallback build is kept only while the NVIDIA driver is the one it was
decided on. After a driver update, the next `localbox update` installs the
build the driver and GPU checks pick again and tells you to probe it.

`localbox update --variant vulkan` (or `cuda`, `cpu`) installs that build
on purpose and keeps it on later updates, whatever the driver;
`--variant auto` hands the choice back to the driver and GPU checks.

### Large contexts on a 24 GB card

A 4-KV-head model at a compact quant with a quantized KV cache is what fits
the biggest contexts on a single 4090 — for example Qwen3-Coder-30B-A3B at
IQ4_XS (weights ~16.5 GB) leaves room for ~6 GB of q4_0 KV cache at 256k.
Check the guided launcher's fit hint (`fits` / `tight` / `over`) per quant and
context, and let `localbench findbest` measure the best flags for your card.

---
