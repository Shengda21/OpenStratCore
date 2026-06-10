---
name: gen-art
description: >
  Generate game sprites in the flat military-electronic-map style (S1) via Codex-driven
  gpt-image-2, then auto-remove backgrounds for unit/icon assets. Use for "出贴图", "generate
  unit icons", "make terrain tiles".
---

# /gen-art — generate sprites (art-director subagent)

Operationalizes docs/WORKFLOWS.md §3. Style + per-asset prompts live in
`assets/prompts/art_pack.yaml`. Image gen runs through Codex (your subscription).

1. Pick the group(s): `terrain | units | facilities | ui | all`.
2. Generate: `python tools/gen_art.py --pack assets/prompts/art_pack.yaml --out assets/generated --group <g>`.
   - Primary: Codex drives gpt-image-2. Fallback: `--fallback-api` (OpenAI Images, BILLED).
   - Inspect prompts first with `--dry-run`.
3. `transparent` groups are auto-cut by `tools/bg_remove.py` (gpt-image-2 has no transparency).
4. Check style consistency on thumbnails; if off, tweak the prompt in the pack and regenerate the
   small affected subset (saves credits). Do NOT hand-edit `assets/generated/` (it's protected).

Output: sprites + updated `assets/generated/manifest.json`.
