#!/usr/bin/env python3
"""Generate sprites in style S1 via Codex-driven gpt-image-2, then auto-remove
backgrounds for unit/icon groups.

Primary path  : `codex exec` drives gpt-image-2 (uses your Codex subscription).
Fallback path : OpenAI Images API directly (--fallback-api; billed per image).
Dry run       : --dry-run prints the composed prompts and writes a manifest only.

    python tools/gen_art.py --pack assets/prompts/art_pack.yaml --out assets/generated
    python tools/gen_art.py --group units --dry-run

gpt-image-2 has no transparent background, so `transparent` groups are generated on a
neutral plate and cut out by tools/bg_remove.py.
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import time
from pathlib import Path

import yaml


def _codex_image_dir() -> Path:
    """Where Codex's (subscription) imagegen drops generated PNGs."""
    home = os.environ.get("CODEX_HOME") or str(Path.home() / ".codex")
    return Path(home) / "generated_images"


def compose_prompt(style: dict, asset_prompt: str, transparent: bool, tint_hex: str | None) -> str:
    parts = [style["shared_prompt"].strip(), asset_prompt.strip()]
    if tint_hex:
        parts.append(f"primary color {tint_hex}")
    if transparent:
        parts.append(f"centered single subject on a solid flat {style['palette']['background_neutral']} "
                     f"background plate (for clean cutout), no scenery")
    parts.append(f"{style['size_px']}x{style['size_px']} pixels, {style.get('notes','')}")
    return ", ".join(p for p in parts if p)


def gen_via_codex(prompt: str, out_path: Path, size_px: int = 256) -> bool:
    """Generate one sprite via Codex's subscription imagegen (no billing). Codex saves the PNG into
    `~/.codex/generated_images/<id>/ig_*.png` and cannot reliably copy it to an arbitrary path, so we
    let it generate, then HARVEST the newest image it produced, resize it to `size_px`, and write it
    to `out_path`."""
    codex = shutil.which("codex")
    if codex is None:
        return False
    gen_dir = _codex_image_dir()
    before = {p for p in gen_dir.rglob("*.png")} if gen_dir.exists() else set()
    started = time.time()
    task = (f"Generate exactly one image using your image-generation capability. "
            f"Image description: {prompt}. Just generate the image — do not write or run any code, "
            f"and do not try to copy or move the file.")
    # codex is a .CMD shim on Windows; CreateProcess can't run it directly, so go through the shell.
    cmd = ([os.environ.get("COMSPEC", "cmd.exe"), "/c", codex, "exec", "--skip-git-repo-check", task]
           if os.name == "nt" else [codex, "exec", "--skip-git-repo-check", task])
    # Run from a LOCAL cwd (not a mapped/UNC drive): Codex's sandboxed imagegen subprocess fails with
    # `CreateProcessWithLogonW failed: 267` (invalid directory) when cwd is a UNC path like Z:\… ->
    # \\host\Shared Folders\…, and then silently falls back to an inline SVG instead of a real PNG.
    # The image lands in ~/.codex/generated_images regardless of cwd, so a safe cwd is harmless.
    safe_cwd = os.environ.get("TEMP") or str(Path.home())
    try:
        subprocess.run(cmd, check=False, stdin=subprocess.DEVNULL, timeout=420, cwd=safe_cwd,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    except subprocess.TimeoutExpired:
        pass
    if not gen_dir.exists():
        return False
    fresh = [p for p in gen_dir.rglob("*.png")
             if p not in before and p.stat().st_mtime >= started - 5]
    if not fresh:
        return False
    newest = max(fresh, key=lambda p: p.stat().st_mtime)
    try:
        from PIL import Image
        img = Image.open(newest).convert("RGB").resize((size_px, size_px), Image.LANCZOS)
        img.save(out_path)
    except Exception:
        shutil.copy2(newest, out_path)
    return out_path.exists()


def gen_via_api(prompt: str, out_path: Path, size_px: int) -> bool:
    try:
        from openai import OpenAI
    except Exception:
        print("  openai SDK not installed; skipping API fallback")
        return False
    client = OpenAI()  # OPENAI_API_KEY; BILLED per image
    size = f"{size_px}x{size_px}" if size_px in (256, 512, 1024) else "1024x1024"
    result = client.images.generate(model="gpt-image-2", prompt=prompt, size=size)
    import base64
    b64 = result.data[0].b64_json
    out_path.write_bytes(base64.b64decode(b64))
    return True


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--pack", default="assets/prompts/art_pack.yaml")
    ap.add_argument("--out", default="assets/generated")
    ap.add_argument("--group", default="all")
    ap.add_argument("--fallback-api", action="store_true", help="use OpenAI Images API (billed)")
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--force", action="store_true",
                    help="regenerate even if the sprite already exists (default: resume/skip)")
    args = ap.parse_args()

    pack = yaml.safe_load(Path(args.pack).read_text(encoding="utf-8"))
    style = pack["style"]
    out_dir = Path(args.out)
    out_dir.mkdir(parents=True, exist_ok=True)

    manifest = {}
    groups = pack["groups"]
    selected = groups.keys() if args.group == "all" else [args.group]

    for gname in selected:
        group = groups[gname]
        transparent = bool(group.get("transparent"))
        variants = group.get("variants", [None])
        for asset in group["assets"]:
            for variant in variants:
                tint = style["palette"].get(variant) if variant else None
                suffix = f"_{variant}" if variant else ""
                asset_id = f"{asset['id']}{suffix}"
                out_path = out_dir / f"{asset_id}.png"
                prompt = compose_prompt(style, asset["prompt"], transparent, tint)

                if args.dry_run:
                    print(f"[{gname}] {asset_id}: {prompt}")
                elif out_path.exists() and not args.force:
                    print(f"  -- skip {asset_id} (exists)")  # resume
                else:
                    print(f"  .. generating {asset_id}")
                    ok = gen_via_codex(prompt, out_path, style["size_px"])
                    if not ok and args.fallback_api:
                        ok = gen_via_api(prompt, out_path, style["size_px"])
                    if not ok:
                        print(f"  !! could not generate {asset_id} (need codex or --fallback-api)")
                        continue
                    if transparent:
                        subprocess.run(["python", "tools/bg_remove.py",
                                        str(out_path), str(out_path)], check=False)

                # Only manifest sprites that actually exist on disk (a partial / quota-limited run
                # still yields a valid manifest; never list a missing PNG).
                if not args.dry_run and out_path.exists():
                    manifest[asset_id] = {"group": gname, "file": f"{asset_id}.png",
                                          "transparent": transparent, "variant": variant}

    if args.dry_run:
        return  # dry-run inspects prompts only; it never touches the manifest

    # Merge with any existing manifest (resume across runs), then PRUNE to files that exist so the
    # manifest is always an accurate inventory of the generated sprites.
    mf_path = out_dir / "manifest.json"
    if mf_path.exists():
        try:
            existing = json.loads(mf_path.read_text(encoding="utf-8"))
            existing.update(manifest)
            manifest = existing
        except Exception:
            pass
    manifest = {k: v for k, v in manifest.items() if (out_dir / v["file"]).exists()}
    mf_path.write_text(json.dumps(manifest, indent=2, ensure_ascii=False), encoding="utf-8")
    print(f"manifest -> {out_dir/'manifest.json'} ({len(manifest)} entries)")


if __name__ == "__main__":
    main()
