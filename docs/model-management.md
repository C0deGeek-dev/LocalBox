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
the same way before the server starts; `--draft` does the same for
`DraftModule`. `localbox download <model> [--quant <key>] [--vision] [--draft]`
performs exactly those fetches without starting anything — a way to pre-fetch a
model, and the same downloader every consumer of the launcher library uses (the
LocalBench tuner fetches a missing GGUF through it before its first trial, so
tuning no longer needs a prior launch). `localbox info <model>` shows the entry
as LocalBox resolved it; unknown names list the known keys.

## Installing straight from a Hugging Face repo

You do not have to hand-write a catalog entry for a GGUF repo. Give `localbox
download` a Hugging Face repo id (or a repo URL) instead of a catalog name and
it will do the wiring for you:

```
localbox download mradermacher/Some-Model-i1-GGUF
localbox download https://huggingface.co/bartowski/Some-Model-GGUF --quant q4km
```

LocalBox reads the repo's file list from the Hugging Face API, groups the GGUF
files into quant variants (multi-part files are grouped and all their shards are
fetched), picks the variant — your `--quant` if given, otherwise a sensible
default (`q4km`, else the middle by size) — and:

1. writes a catalog entry into your `~/.local-llm/llm-models.json` (additively —
   nothing you already have is touched), with a key derived from the repo name,
   and
2. downloads the chosen file(s) into that model's folder under the GGUF root,
   resumably, the same way a launch does.

List the quant keys a repo offers by running the command with a `--quant` that
does not exist — the error names the available keys. After the install the model
is an ordinary catalog entry: launch it by its new key (shown in the output), and
it appears in `localbox info` / `localbox models` like any other. Re-running the
same repo is a no-op: an already-catalogued repo is reported and its files
resume rather than re-download.

Two limits today: a **gated or private** repo (one that needs a Hugging Face
access token) is detected and reported, not downloaded — add such a model by
other means; and only `.gguf` files are installed (no `safetensors`/GPTQ). A
repo id is only tried as a Hugging Face repo when it is *not* already a catalog
name, so a catalog key always wins.

> Design note: `localbox download` accepts a repo id because LocalBox has no
> separate "add model" command — the catalog is a plain JSON file you can also
> edit by hand (above). The synthesised entry is written additively and never
> overwrites an existing model; a repo already present is reused, and a name
> collision with an unrelated model gets a suffixed key rather than clobbering
> it. LocalBox records decisions in this doc and the `CHANGELOG`, not a separate
> ADR file.

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
