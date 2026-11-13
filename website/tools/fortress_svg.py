#!/usr/bin/env python3
"""Draw website/site/img/fortress.svg: the landing page's voxel fortress.

A slab of layered terrain (grass, soil, stone, deep rock) seen from above at an isometric
angle, cut open along its two front faces: a walled keep on the surface, a stairwell dug down
the front, a room in the soil, a lit hall in the stone with a builder in it, and a pocket of
glowing ore at the bottom. Every block is a unit cube; only faces that touch air are drawn,
back to front (painter's order), each face flat-shaded: top light, left mid, right dark.

Standard library only and deterministic (no randomness: per-block tone comes from a hash of
its coordinates). Usage: fortress_svg.py [output] (default: website/site/img/fortress.svg
next to this script's website/ directory).
"""

import pathlib
import sys

# Footprint (x runs to the lower right, y to the lower left) and terrain layers (z up).
SIZE_X, SIZE_Y = 16, 10
GRASS_Z = 10  # the turf layer; below it soil, a band of sandstone, stone, then deep rock
SOIL_FROM, SAND_Z, STONE_FROM = 7, 6, 3

# Projection: a 2:1 isometric grid in whole pixels. A block's top is 4K wide and 2K tall,
# its sides H tall.
K, H = 7, 16

# Materials: (top, left face, right face). Light comes from the upper left.
MATERIALS = {
    "grass": ("#a3b878", "#7f8f63", "#66744f"),
    "soil": ("#8e6e55", "#765a45", "#5e4736"),
    "sand": ("#d2b27a", "#b89663", "#977a4f"),
    "stone": ("#8d98a6", "#737e8c", "#5b6572"),
    "deep": ("#585d7a", "#474b65", "#373a50"),
    "ore": ("#ffd98a", "#ffb547", "#e48c22"),
    "wall": ("#ddd7ca", "#bfb8a8", "#9e9787"),
    "wood": ("#bf8a50", "#9e6d39", "#7d542b"),
    "banner": ("#4fd0a4", "#21936f", "#166a51"),
    "skin": ("#f0c39a", "#d9a47a", "#b9845d"),
}
# Faces that look into a dug space are shaded; the hall's are warmed by its lanterns.
SHADE = 0.68
WARM = (1.0, 0.86, 0.62)


def terrain(x, y, z):
    """The undug ground: a flat plain with a low hill in the back corner."""
    top = GRASS_Z + (2 if x < 4 and y < 3 else 1 if x < 6 and y < 5 else 0)
    if z > top:
        return None
    if z == top:
        return "grass"
    if z >= SOIL_FROM:
        return "soil"
    if z == SAND_Z:
        return "sand"
    if z >= STONE_FROM:
        return "stone"
    return "deep"


def build():
    blocks = {}
    for x in range(SIZE_X):
        for y in range(SIZE_Y):
            for z in range(GRASS_Z + 3):
                m = terrain(x, y, z)
                if m:
                    blocks[(x, y, z)] = m

    dug = {}  # air cell -> "room" | "hall"

    def dig(xs, ys, zs, kind="room"):
        for x in xs:
            for y in ys:
                for z in zs:
                    blocks.pop((x, y, z), None)
                    dug[(x, y, z)] = kind

    # Ore veins showing on the deep rock's front faces.
    for x, y, z in [(1, 9, 1), (2, 9, 0), (5, 9, 2), (9, 9, 0), (15, 2, 1), (15, 6, 2), (15, 7, 0)]:
        blocks[(x, y, z)] = "ore"

    # A room in the soil, open on the left front face, with a table and a bed.
    dig(range(1, 6), range(7, 10), range(7, 9))
    blocks[(3, 8, 7)] = "wood"
    blocks[(1, 7, 7)] = "wood"
    blocks[(1, 8, 7)] = "wood"

    # A stairwell down the front: each step one block lower, open to the sky.
    for i in range(7):
        dig([6 + i], range(8, 10), range(GRASS_Z - i, GRASS_Z + 1))

    # The hall in the stone at the stairwell's foot, lit, with a builder in it.
    dig(range(8, 15), range(6, 10), range(3, 6), "hall")
    blocks[(9, 6, 3)] = "ore"  # lanterns
    blocks[(13, 6, 3)] = "ore"
    blocks[(11, 8, 3)] = "banner"  # the builder: body ...
    blocks[(11, 8, 4)] = "skin"  # ... and head

    # A mine tunnel along the right front face, and the glowing pocket below it.
    dig([13, 14, 15], range(1, 4), range(4, 6))
    dig([12, 13, 14, 15], range(1, 5), range(0, 2))
    for x, y in [(12, 2), (13, 1), (14, 3), (13, 3)]:
        blocks[(x, y, 0)] = "ore"
    blocks[(12, 1, 0)] = "deep"

    # The keep on the surface: a wall ring with a gate, battlements and a tower.
    z0 = GRASS_Z + 1
    x0, x1, y0, y1 = 7, 14, 1, 6
    for x in range(x0, x1 + 1):
        for y in range(y0, y1 + 1):
            if x in (x0, x1) or y in (y0, y1):
                if y == y1 and x in (10, 11):
                    continue  # the gate
                for z in (z0, z0 + 1):
                    blocks[(x, y, z)] = "wall"
                if (x + y) % 2 == 0:
                    blocks[(x, y, z0 + 2)] = "wall"
    for x in (8, 9):
        for y in (2, 3):
            for z in range(z0, z0 + 5):
                blocks[(x, y, z)] = "wall"
    for x, y in [(8, 2), (9, 3)]:
        blocks[(x, y, z0 + 5)] = "wall"
    blocks[(9, 2, z0 + 5)] = "banner"
    blocks[(9, 2, z0 + 6)] = "banner"
    return blocks, dug


def tone(hex_colour, factor):
    r, g, b = (int(hex_colour[i : i + 2], 16) for i in (1, 3, 5))
    fr, fg, fb = factor if isinstance(factor, tuple) else (factor, factor, factor)
    return "#%02x%02x%02x" % tuple(min(255, round(c * f)) for c, f in ((r, fr), (g, fg), (b, fb)))


def variant(x, y, z):
    """A small, fixed brightness step per block so neighbouring blocks read apart."""
    h = (x * 73856093) ^ (y * 19349663) ^ (z * 83492791)
    return (0.96, 1.0, 1.0, 1.03)[h % 4]


def render(blocks, dug):
    max_z = max(z for _, _, z in blocks) + 1
    ox = SIZE_Y * 2 * K + 4  # the slab's leftmost corner sits 4 px in
    oy = max_z * H + 4

    def pt(px, py, pz):
        return ox + (px - py) * 2 * K, oy + (px + py) * K - pz * H

    def face(corners):
        pts = [pt(*c) for c in corners]
        d = "M%d %d" % pts[0]
        d += "l" + " ".join("%d %d" % (b[0] - a[0], b[1] - a[1]) for a, b in zip(pts, pts[1:]))
        return d + "z"

    faces = []  # (colour, path) in painter's order
    for (x, y, z) in sorted(blocks, key=lambda p: (p[0] + p[1] + p[2], p[2])):
        top, left, right = MATERIALS[blocks[(x, y, z)]]
        v = variant(x, y, z)
        lit = blocks[(x, y, z)] in ("ore", "banner", "skin")  # lanterns, the builder: unshaded
        for colour, air, corners in (
            (top, (x, y, z + 1), [(x, y, z + 1), (x + 1, y, z + 1), (x + 1, y + 1, z + 1), (x, y + 1, z + 1)]),
            (left, (x, y + 1, z), [(x, y + 1, z + 1), (x + 1, y + 1, z + 1), (x + 1, y + 1, z), (x, y + 1, z)]),
            (right, (x + 1, y, z), [(x + 1, y, z + 1), (x + 1, y + 1, z + 1), (x + 1, y + 1, z), (x + 1, y, z)]),
        ):
            if air in blocks:
                continue
            c = tone(colour, v)
            if not lit and air in dug:
                c = tone(c, SHADE)
                if dug[air] == "hall":
                    c = tone(c, WARM)
            faces.append((c, face(corners)))

    width = (SIZE_X + SIZE_Y) * 2 * K + 8
    height = oy + (SIZE_X + SIZE_Y) * K + 8
    out = [
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 %d %d" width="%d" height="%d">'
        % (width, height, width, height),
        "  <!-- Generated by website/tools/fortress_svg.py; edit that and re-run it, not this file. -->",
        "  <defs>",
        '    <radialGradient id="glow">',
        '      <stop offset="0" stop-color="#ffc266" stop-opacity="0.55"/>',
        '      <stop offset="1" stop-color="#ffc266" stop-opacity="0"/>',
        "    </radialGradient>",
        "  </defs>",
    ]
    run_colour, run = None, []
    for colour, d in faces + [(None, "")]:
        if colour != run_colour and run:
            out.append('  <path fill="%s" d="%s"/>' % (run_colour, "".join(run)))
            run = []
        run_colour = colour
        run.append(d)
    # The ore light: over the glowing pocket and the hall's lanterns.
    for (gx, gy, gz), r in (((SIZE_X - 1, 2.5, 1), 40), ((11, SIZE_Y, 4), 56)):
        cx, cy = pt(gx, gy, gz)
        out.append('  <circle cx="%d" cy="%d" r="%d" fill="url(#glow)"/>' % (cx, cy, r))
    out.append("</svg>")
    return "\n".join(out) + "\n"


def main():
    default = pathlib.Path(__file__).resolve().parent.parent / "site" / "img" / "fortress.svg"
    target = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else default
    blocks, dug = build()
    target.write_text(render(blocks, dug), encoding="utf-8")


if __name__ == "__main__":
    main()
