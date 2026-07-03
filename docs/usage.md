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
localbox stop                       stop every model server and the proxy
localbox status                     report serve health and the remedy
localbox info [model]               list the configured models, or one in detail
localbox purge                      stop servers and delete downloaded model files
localbox log [--lines <n>]          tail the most recent server log
localbox embed-serve [--port <p>]   start the CPU-only embedding server
localbox embed-stop                 stop the embedding server
localbox update [--mode <m>] [--check]
                                    install or update the llama.cpp binaries
localbox version                    print the launcher version envelope
```

Options for `launch` / `serve`:

| Flag | Effect |
|------|--------|
| `--context <key>` | One of the model's context keys (see `localbox info <model>`). Omit for the default. |
| `--mode <m>` | `native` / `turboquant` / `mtpturbo` — which llama-server binary to use. |
| `--quant <key>` | Switch the GGUF quant for this launch (default per model). |
| `--vision` | Load the model's multimodal projector when it has one. |
| `--keep-thinking` | Let the model's thinking reach the agent unfiltered. |
| `--agent <a>` | `claude` (default) / `localpilot` / `codex` / `none`. |
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

### Large contexts on a 24 GB card

A 4-KV-head model at a compact quant with a quantized KV cache is what fits
the biggest contexts on a single 4090 — for example Qwen3-Coder-30B-A3B at
IQ4_XS (weights ~16.5 GB) leaves room for ~6 GB of q4_0 KV cache at 256k.
Check the guided launcher's fit hint (`fits` / `tight` / `over`) per quant and
context, and let `localbench findbest` measure the best flags for your card.

---
