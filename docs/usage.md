# Day-to-day usage

Part of the [LocalBox documentation](README.md).

One function per model. Flag-based:

```
qcoder -Ctx 32k -LocalPilot       Code agent (Qwen3-Coder, 32k, LocalPilot)
qcoder -Ctx 32k -Codex            Code agent (Qwen3-Coder, 32k, Codex)
q36p -Ctx 32k -LocalPilot         General Qwen 3.6 agent (32k, LocalPilot)
dev -Ctx 32k                      Smaller / faster (Devstral 24B, 32k)
q36p -Ctx 128k -LocalPilot        Big context (Qwen 3.6 Plus, 128k)
qcoder -Ctx 256 -Quant iq4xs      256k coder context (4090 ceiling)
q36p -Quant q6kp                  Switch the GGUF quant
q36p -Mode turboquant -KvK turbo4 -KvV turbo4   Turbo KV via fork binary
q36p -AutoBest                    Replay the saved tuner config
llmdefault                        Launch the configured default recipe/model
llmdefaultlocalpilot              Same, via LocalPilot
llmdefaultcodex                   Same, via Codex
llm                               Guided wizard (Spectre when available)
llmtui                            Terminal.Gui TUI preview
lbtui                             LocalBench Terminal.Gui TUI preview
llmc                              Native selectable wizard, explicit alias
llms                              Spectre wizard, explicit alias
info                              Dashboard
info -Commands                    Full LocalBox + LocalBench command list
llmdocs                           Quick reference
llm-update [-InstallTui] [-RefreshInstalled]
                                  Update LocalBox + companions, then refresh installed artifacts
```

| Flag | Effect |
|------|--------|
| `-Ctx <name>` | One of the model's context keys (`32k`, `64k`, `128k`, `256k`). Omit for default. |
| `-LocalPilot` | Use LocalPilot instead of Claude Code. |
| `-Codex` | Use OpenAI Codex instead of Claude Code. |
| `-Strict` | Apply the strict engineering overlay (sampler + system prompt). Requires `Strict: true` on the model. |
| `-Mode <name>` | `native` / `turboquant` / `mtpturbo` — which llama-server binary to use. |
| `-KvK / -KvV` | Override the KV cache types passed to llama-server. |
| `-AutoBest` | Replay the latest saved tuner profile for this (model, ctx, mode). |
| `-Quant <name>` | Switch the model's selected GGUF quant (no launch). |

Quant keys are model-local labels, not a universal naming scheme. For example,
`mtp-apex` means the Genesis V2 MTP-enabled APEX GGUF file, while another model
may use a simpler `mtp` label when there is only one MTP variant. Use
`info <key>` to see the exact filename behind each quant key.

### 256 k context on a 24 GB card

The combination of **Qwen3-Coder-30B-A3B Heretic** (4 KV heads, 48 layers) at
the **IQ4_XS** quant with **q4_0 KV cache** is the only setup that fits a full
256k context on a single 4090:

```powershell
qcoder -Ctx 256 -Quant iq4xs                  # Claude Code @ 256k
qcoder -Ctx 256 -Quant iq4xs -LocalPilot      # LocalPilot @ 256k
```

Weights ~16.5 GB; q4_0 KV @ 256k ~6 GB; total ~23.6 GB.

---
