#!/usr/bin/env python3
"""Auto background removal (gpt-image-2 has no transparent output).
Uses `rembg` (U^2-Net). Install: pip install rembg onnxruntime.

    python tools/bg_remove.py in.png out.png
    python tools/bg_remove.py assets/generated/   # in place, all *.png
"""
from __future__ import annotations

import sys
from pathlib import Path


def remove_file(src: Path, dst: Path) -> None:
    try:
        from rembg import remove
    except Exception:
        print("rembg not installed; run: pip install rembg onnxruntime", file=sys.stderr)
        sys.exit(1)
    data = src.read_bytes()
    dst.write_bytes(remove(data))
    print(f"cut out: {src.name} -> {dst.name}")


def main():
    args = sys.argv[1:]
    if len(args) == 1 and Path(args[0]).is_dir():
        d = Path(args[0])
        for p in sorted(d.glob("*.png")):
            remove_file(p, p)
    elif len(args) == 2:
        remove_file(Path(args[0]), Path(args[1]))
    else:
        print("usage: bg_remove.py <in.png> <out.png> | <dir>", file=sys.stderr)
        sys.exit(2)


if __name__ == "__main__":
    main()
