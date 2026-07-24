# Model management

Part of the [LocalBox documentation](README.md).

The model catalog is `~/.local-llm/llm-models.json` — an ordinary JSON file
that is yours to edit. The first run seeds it from the shipped example, so you
always have a working template with real entries next to it
(`llm-models.example.json`; that example copy refreshes to match the
installed binary, your catalog never does).

When a newer LocalBox ships models your catalog predates, `localbox update`
lists them, and:

```
localbox update --merge-models --check   # preview: which keys would be added
localbox update --merge-models           # add them (additive only)
```

The merge only *adds* missing model keys from the shipped set — an entry you
already have is never rewritten, and everything else in the file
(`CommandAliases`, your edits) stays as it was. Note for source checkouts: run
from a directory outside the repo — inside it, `local-llm/` in the checkout is
the live catalog by design.

Beside `VisionModule` (a multimodal projector loaded with `--vision`), an
entry may name a `DraftModule` — a small drafter GGUF in the same repo for
classic speculative decoding, loaded with `--draft` and downloaded on demand.
The drafter must share the main model's tokenizer — on a mismatch the server
logs the incompatibility and runs without speculation, and the launcher warns
about it — and it cannot combine with an MTP `SpecType`: one speculation
engine per launch.

A catalog entry:

```jsonc
"q36plus": {
  "DisplayName": "Qwen 3.6 Plus",
  "Description": "General coding model.",       // shown in the picker
  "Tier": "recommended",                        // picker shows this tier first
  "Repo": "owner/name",                         // Hugging Face repo id
  "Root": "q36plus",                            // folder under the GGUF root
  "Quants": {
    "q4kp":  { "File": "model-Q4_K_P.gguf", "SizeGB": 18.1 },
    "iq4xs": "model-IQ4_XS.gguf"                // compact spelling works too
  },
  "Quant": "q4kp",                              // default quant
  "Contexts": { "": 32768, "64k": 65536, "128k": 131072 }
}
```

Models whose weight format needs a specific engine can add
`"RequiredMode": "prism"`; LocalBox then selects and locks that engine.

The GGUF itself downloads from Hugging Face on first launch (resumable,
verified against the expected destination). When `--vision` is requested and
`VisionModule` names a missing file in the same repo, that projector downloads
the same way before the server starts. `localbox info <model>` shows the entry
as LocalBox resolved it; unknown names list the known keys.

Removing a model is editing it out of the catalog; `localbox purge` stops
servers and deletes every downloaded model folder under the GGUF root (models
download again on the next launch).

---

## VRAM-aware tradeoffs

The launcher reads your GPU's VRAM and uses it to tag every quant as
fits / tight / over in the guided launcher, so you can see at a glance which
builds will load fully on your card.

VRAM resolves in this order:

1. `VRAMGB` set in `settings.json` (top-level).
2. `nvidia-smi --query-gpu=memory.total` auto-detect.
3. Fallback to 24.

Per-quant tradeoffs come from the optional `SizeGB` (drives the fit badge)
and `Note` (human-readable quality/use-case context, shown verbatim) fields
on each `Quants` entry. Backfill these on any model you add — they show up
inline in the guided launcher.

---
