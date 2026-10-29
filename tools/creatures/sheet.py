#!/usr/bin/env python3
"""A creature's review contact sheet: the asset viewer (`--shot` mode) shoots
every clip in the manifest at a telling moment, plus the commander's top-down
view and a side view of the walk, and pastes them into one labelled PNG at
target/creature-sheets/<df_id>.png. Step 5 (self-review) of the per-creature
process, docs/superpowers/specs/2026-10-29-creature-asset-pipeline.md.

python3 tools/creatures/sheet.py <df_id>     (lowercase, as in the manifest)

Needs the viewer built (`just creature-sheet` builds it). GPU: WGPU_ADAPTER_NAME,
else VOXEL_ADAPTER, else "Intel" (the functional-run default on this box).
"""
import json
import os
import subprocess
import sys

from PIL import Image, ImageDraw

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
VIEWER = os.path.join(REPO, "target", "debug", "examples", "asset_viewer")
MANIFEST = os.path.join(REPO, "assets", "models", "creatures", "creatures.json")
OUT = os.path.join(REPO, "target", "creature-sheets")
# The 3D view between the viewer's panels at its 1600×900 window (tall bipeds fill it).
CROP = (340, 20, 1300, 860)
TILE = (400, 350)
COLUMNS = 6
# Where in a clip to look: a loop or one-shot at 30% (mid-motion), a hold at its end.
AT = {"loop": 0.3, "once": 0.3, "hold": 1.0}


def shoot(df_id, path, anim, time, extra=()):
    env = dict(os.environ)
    env.setdefault("WGPU_ADAPTER_NAME", os.environ.get("VOXEL_ADAPTER") or "Intel")
    cmd = [VIEWER, "--model", f"creatures/{df_id}.bbmodel", "--anim", anim, "--time", f"{time:.3f}",
           "--shot", path, *extra]
    result = subprocess.run(cmd, env=env, capture_output=True, text=True, timeout=180)
    if result.returncode != 0 or not os.path.exists(path):
        sys.exit(f"shot failed: {' '.join(cmd)}\n{result.stderr[-2000:]}")


def main(df_id):
    if not os.path.exists(VIEWER):
        sys.exit(f"{VIEWER} missing: run `just creature-sheet` (it builds the viewer)")
    entry = json.load(open(MANIFEST))[df_id]
    os.makedirs(os.path.join(OUT, df_id), exist_ok=True)
    shots = [(name, clip["loop"], clip["length"]) for name, clip in entry["clips"].items()]
    first = "idle" if "idle" in entry["clips"] else shots[0][0]
    gait = "walk" if "walk" in entry["clips"] else first
    tiles = []
    for name, mode, length in shots:
        path = os.path.join(OUT, df_id, f"{name}.png")
        shoot(df_id, path, name, length * AT[mode])
        tiles.append((name, path))
    for label, anim, extra in (("top", first, ("--pitch", "70")), ("side", gait, ("--yaw", "90", "--pitch", "5"))):
        path = os.path.join(OUT, df_id, f"{label}.png")
        length = entry["clips"][anim]["length"]
        shoot(df_id, path, anim, length * AT[entry["clips"][anim]["loop"]], extra)
        tiles.append((f"{label} ({anim})", path))

    rows = (len(tiles) + COLUMNS - 1) // COLUMNS
    sheet = Image.new("RGB", (TILE[0] * COLUMNS, TILE[1] * rows), (40, 40, 40))
    for i, (label, path) in enumerate(tiles):
        im = Image.open(path).convert("RGB").crop(CROP).resize(TILE)
        d = ImageDraw.Draw(im)
        d.rectangle((0, 0, 8 + 7 * len(label), 22), fill=(0, 0, 0))
        d.text((6, 5), label, fill=(255, 255, 255))
        sheet.paste(im, ((i % COLUMNS) * TILE[0], (i // COLUMNS) * TILE[1]))
    out = os.path.join(OUT, f"{df_id}.png")
    sheet.save(out)
    print(os.path.relpath(out, REPO))


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    main(sys.argv[1].lower())
