#!/usr/bin/env python3
"""Tiled (.tmx) -> openstratcore map.schema.json importer (skeleton, functional for a
basic hex map with a terrain tile layer and an optional integer elevation layer).

    python tools/tmx_import.py path/to/map.tmx > scenarios/maps/imported.map.json

Customize TILE_TERRAIN to map your tileset's tile ids/names to terrain enums.
Assumes a hexagonal Tiled map; (x,y) grid is emitted as axial (q=x, r=y) — adjust
the offset->axial conversion to match your tileset's stagger if needed.
"""
from __future__ import annotations

import json
import sys
import xml.etree.ElementTree as ET

# Map Tiled tile gid (or a custom 'terrain' property) -> our terrain enum.
TILE_TERRAIN = {
    "open": "open", "forest": "forest", "urban": "urban", "river": "river",
    "lake": "lake", "soft": "soft", "road": "road", "rail": "rail",
}


def layer_data(layer: ET.Element, width: int):
    """Return a list of (x, y, gid) for a CSV-encoded tile layer."""
    data = layer.find("data")
    if data is None or (data.get("encoding") != "csv"):
        raise SystemExit("only CSV-encoded tile layers are supported in this skeleton")
    gids = [int(g) for g in data.text.replace("\n", "").split(",") if g.strip() != ""]
    for i, gid in enumerate(gids):
        yield (i % width, i // width, gid)


def tileset_terrain_by_gid(root: ET.Element):
    """Build gid -> terrain using each tile's 'terrain' custom property, if present."""
    mapping = {}
    for ts in root.findall("tileset"):
        firstgid = int(ts.get("firstgid", "1"))
        for tile in ts.findall("tile"):
            tid = int(tile.get("id"))
            props = tile.find("properties")
            if props is None:
                continue
            for p in props.findall("property"):
                if p.get("name") == "terrain":
                    mapping[firstgid + tid] = TILE_TERRAIN.get(p.get("value"), "open")
    return mapping


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: tmx_import.py map.tmx")
    root = ET.parse(sys.argv[1]).getroot()
    width = int(root.get("width"))
    name = sys.argv[1].rsplit("/", 1)[-1].replace(".tmx", "")

    gid_terrain = tileset_terrain_by_gid(root)
    layers = {l.get("name"): l for l in root.findall("layer")}

    terrain_layer = layers.get("terrain") or next(iter(layers.values()))
    elevation = {}
    if "elevation" in layers:
        for x, y, gid in layer_data(layers["elevation"], width):
            elevation[(x, y)] = max(0, gid - 1)  # gid 1 -> elevation 0, etc.

    hexes = []
    for x, y, gid in layer_data(terrain_layer, width):
        if gid == 0:
            continue
        hexes.append({
            "q": x, "r": y,
            "elevation": int(elevation.get((x, y), 0)),
            "terrain": gid_terrain.get(gid, "open"),
        })

    out = {"format": "openstratcore.map", "version": 1, "name": name, "hexes": hexes}
    print(json.dumps(out, indent=2))


if __name__ == "__main__":
    main()
