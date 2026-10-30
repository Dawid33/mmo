#!/usr/bin/env python3
"""Generates the tree block textures into assets/blocks/ (16x16 RGBA,
deterministic: fixed seed per file, so re-running changes nothing).

Per species <sp> in oak, pine, birch: <sp>_bark.png (log sides),
<sp>_rings.png (log top/bottom), <sp>_branch.png, <sp>_leaves.png (about 35 %
of texels fully transparent, every other texel fully opaque -- cutout
rendering has no blending).
"""
import os
import random
import sys

from PIL import Image

SIZE = 16
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "assets", "blocks")

# (bark, bark dark, ring light, ring dark, leaf light, leaf dark)
SPECIES = {
    "oak": ((102, 78, 48), (72, 54, 32), (176, 140, 88), (140, 106, 62), (74, 128, 44), (48, 92, 30)),
    "pine": ((74, 56, 40), (50, 36, 26), (150, 118, 80), (118, 88, 58), (36, 84, 52), (22, 58, 38)),
    "birch": ((216, 212, 200), (60, 56, 52), (214, 196, 150), (180, 160, 112), (118, 156, 60), (88, 124, 44)),
}
LEAF_HOLE_FRACTION = 0.35


def shade(c, f):
    return tuple(max(0, min(255, int(v * f))) for v in c) + (255,)


def bark(sp, rng):
    base, dark, *_ = SPECIES[sp]
    img = Image.new("RGBA", (SIZE, SIZE))
    # vertical streaks: one shade per column, jittered per texel
    cols = [rng.uniform(0.85, 1.15) for _ in range(SIZE)]
    for x in range(SIZE):
        for y in range(SIZE):
            c = shade(base, cols[x] * rng.uniform(0.92, 1.08))
            if sp == "birch" and rng.random() < 0.10:
                c = shade(dark, rng.uniform(0.9, 1.2))  # the dark birch dashes
            elif rng.random() < 0.12:
                c = shade(dark, rng.uniform(0.9, 1.1))
            img.putpixel((x, y), c)
    return img


def rings(sp, rng):
    _, _, light, dark, *_ = SPECIES[sp]
    img = Image.new("RGBA", (SIZE, SIZE))
    for x in range(SIZE):
        for y in range(SIZE):
            d = max(abs(x - 7.5), abs(y - 7.5))  # square rings
            ring = int(d) % 2
            edge = d > 6.5
            c = shade(SPECIES[sp][0], 1.0) if edge else shade(light if ring == 0 else dark, rng.uniform(0.95, 1.05))
            img.putpixel((x, y), c)
    return img


def branch(sp, rng):
    base, dark, *_ = SPECIES[sp]
    img = Image.new("RGBA", (SIZE, SIZE))
    for x in range(SIZE):
        for y in range(SIZE):
            c = shade(base, rng.uniform(0.9, 1.1))
            if (x + y) % 5 == 0 and rng.random() < 0.6:
                c = shade(dark, rng.uniform(0.95, 1.15))
            img.putpixel((x, y), c)
    return img


def leaves(sp, rng):
    *_, light, dark = SPECIES[sp]
    img = Image.new("RGBA", (SIZE, SIZE))
    cells = [(x, y) for x in range(SIZE) for y in range(SIZE)]
    holes = set(rng.sample(cells, round(SIZE * SIZE * LEAF_HOLE_FRACTION)))
    for x, y in cells:
        if (x, y) in holes:
            img.putpixel((x, y), (0, 0, 0, 0))
        else:
            c = light if rng.random() < 0.5 else dark
            img.putpixel((x, y), shade(c, rng.uniform(0.9, 1.1)))
    return img


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else OUT
    for i, sp in enumerate(SPECIES):
        for j, (name, fn) in enumerate(
            [("bark", bark), ("rings", rings), ("branch", branch), ("leaves", leaves)]
        ):
            rng = random.Random(0x7EE5 + i * 16 + j)
            fn(sp, rng).save(os.path.join(out, f"{sp}_{name}.png"), optimize=False)
    print("wrote", len(SPECIES) * 4, "textures to", os.path.normpath(out))


if __name__ == "__main__":
    main()
