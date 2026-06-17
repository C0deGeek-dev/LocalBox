# Model management

Part of the [LocalBox documentation](README.md).

```powershell
addllm <hf-url-or-repo> -Key <key> [-Quants Q4_K_P,IQ4_XS] [-DefaultQuant Q4_K_P] [-Tier recommended]
```

`addllm` registers **every recognized GGUF quant** the HF repo publishes by
default (the `imatrix.gguf` calibration file is excluded). Pass `-Quants` only
when you want to filter the catalog entry to a subset. The GGUF itself is
downloaded on first launch.

Backfilling missing quants on an existing entry (rerunning HF discovery
without overwriting your manual `QuantNotes` / `ContextNotes`):

```powershell
updatellm <key>            # adds any HF quants missing from the entry
updatellm <key> -DryRun    # preview without writing
```

Removing a model:

```powershell
removellm <key>            # confirms first; deletes GGUF folder by default
removellm <key> -Force     # skip confirmation
removellm <key> -KeepFiles # keep the GGUF blobs on disk
```

---

## VRAM-aware tradeoffs

The launcher reads your GPU's VRAM and uses it to **tag every quant** with
`[fits]` / `[tight]` / `[over]` in `info` and the `llm` wizard, so you can see
at a glance which builds will load fully on your card.

VRAM resolves in this order:

1. `VRAMGB` set in `settings.json` or `llm-models.json` (top-level).
2. `nvidia-smi --query-gpu=memory.total` auto-detect (largest GPU on a multi-GPU box).
3. Fallback to 24.

The `info` dashboard shows the resolved value and source
(`auto` / `configured` / `fallback`).

```powershell
Set-LocalLLMSetting VRAMGB 32          # 5090
Set-LocalLLMSetting VRAMGB 48          # RTX 6000 Ada / dual-card aggregate
Set-LocalLLMSetting VRAMGB $null       # remove override, fall back to auto-detect
```

Per-quant tradeoffs come from two optional catalog fields:

- `QuantSizesGB` — file size per quant in GB (drives the fit badge).
- `QuantNotes` — human-readable note per quant (quality/use-case context). Shown verbatim.

Per-context guidance comes from `ContextNotes` in the same shape. Backfill
these on any model you add — they show up inline in `info` and the wizard.

---
