#!/usr/bin/env python3
"""Generates the biome / ice / tree-species block textures (ids 22+ in
assets/blocks/blocks.ron) into assets/blocks/. Deterministic: same bytes every run.

Most textures are derived from existing ones by mapping the source's luminance
onto a new dark->light colour ramp (keeps the pixel pattern and alpha cutout);
the rest are drawn from a hash noise in the same 3-5 colour style.
Run from anywhere (paths resolve from this file):  just block-textures [--sheet]
(or python3 tools/textures/gen.py [--sheet]); --sheet also writes a contact
sheet to target/textures-sheet.png.
"""
import sys
from pathlib import Path
from PIL import Image

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "assets" / "blocks"


def h(x, y, salt):
    v = (x * 374761393 + y * 668265263 + salt * 2246822519) & 0xFFFFFFFF
    v = ((v ^ (v >> 13)) * 1274126177) & 0xFFFFFFFF
    return (v ^ (v >> 16)) / 0xFFFFFFFF


def load(name):
    return Image.open(OUT / f"{name}.png").convert("RGBA")


def lerp(a, b, t):
    return tuple(round(a[i] + (b[i] - a[i]) * t) for i in range(3))


def ramp(colors, t):
    t = min(max(t, 0.0), 1.0) * (len(colors) - 1)
    i = min(int(t), len(colors) - 2)
    return lerp(colors[i], colors[i + 1], t - i)


def lum(p):
    return 0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2]


def recolour(src, colors):
    """Map src luminance (normalised to its own range) onto the ramp; keep alpha."""
    im = load(src)
    px = list(im.getdata())
    ls = [lum(p) for p in px if p[3]]
    lo, hi = min(ls), max(ls)
    span = max(hi - lo, 1e-6)
    out = Image.new("RGBA", im.size)
    out.putdata([(*ramp(colors, (lum(p) - lo) / span), p[3]) if p[3] else p for p in px])
    return out


def noise(colors, salt, weights=None, size=16):
    """Speckled flat texture: each texel picks a colour by hash."""
    n = len(colors)
    weights = weights or [1] * n
    tot = sum(weights)
    im = Image.new("RGBA", (size, size))
    for y in range(size):
        for x in range(size):
            r = h(x, y, salt) * tot
            for c, w in zip(colors, weights):
                if r < w:
                    im.putpixel((x, y), (*c, 255))
                    break
                r -= w
    return im


def banded(colors, salt, period=5):
    """Bluish diagonal bands with noise (glacier ice)."""
    im = Image.new("RGBA", (16, 16))
    for y in range(16):
        for x in range(16):
            t = ((x + y * 0.7) % period) / period
            t = abs(t - 0.5) * 2 + (h(x, y, salt) - 0.5) * 0.5
            im.putpixel((x, y), (*ramp(colors, t), 255))
    return im


def side_from(top_img, dirt_name, edge_rows, salt):
    """Grass-style side: the top texture's top rows over the dirt, ragged edge."""
    dirt = load(dirt_name)
    out = dirt.copy()
    for x in range(16):
        depth = edge_rows + (1 if h(x, 0, salt) > 0.55 else 0)
        for y in range(depth):
            out.putpixel((x, y), top_img.getpixel((x, y)))
    return out


def build():
    t = {}
    # --- ground ---
    t["snow"] = noise([(240, 246, 250), (226, 236, 244), (206, 220, 234)], 1, [6, 3, 1])
    t["ice"] = banded([(150, 200, 235), (182, 220, 245), (212, 238, 252)], 2, 7)
    t["glacier_ice"] = banded([(70, 130, 190), (104, 165, 214), (160, 208, 238), (196, 228, 246)], 3, 5)
    t["mud"] = noise([(72, 54, 40), (86, 64, 46), (58, 43, 33), (98, 76, 56)], 4, [4, 4, 2, 1])
    t["peat"] = noise([(52, 38, 28), (64, 46, 32), (40, 30, 24), (76, 56, 38)], 5, [4, 3, 3, 1])
    t["gravel"] = noise([(128, 126, 124), (150, 148, 144), (104, 102, 102), (170, 166, 160), (88, 86, 88)], 6, [4, 3, 3, 1, 1])
    t["red_sand"] = recolour("sand", [(150, 70, 38), (190, 98, 56), (214, 128, 82)])
    t["dry_grass_top"] = recolour("grass_top", [(112, 96, 50), (160, 140, 70), (200, 182, 104)])
    t["dry_grass_side"] = side_from(t["dry_grass_top"], "dirt", 3, 7)
    t["podzol_top"] = noise([(92, 62, 36), (112, 76, 44), (74, 50, 32), (138, 98, 58), (60, 70, 40)], 8, [4, 4, 3, 1, 1])
    t["podzol_side"] = side_from(t["podzol_top"], "dirt", 3, 9)
    t["frozen_dirt"] = recolour("dirt", [(78, 70, 74), (118, 112, 118), (182, 196, 208)])
    t["salt"] = noise([(238, 238, 232), (222, 224, 222), (205, 208, 210), (248, 248, 246)], 10, [4, 4, 2, 2])
    # --- trees: (bark ramp, rings ramp, branch ramp, leaves ramp), sources are oak / pine
    species = {
        "spruce": ("pine", [(34, 24, 20), (62, 46, 38), (92, 70, 58)], [(120, 96, 66), (162, 132, 90), (190, 162, 116)],
                   [(14, 44, 36), (26, 72, 54), (44, 104, 72)]),
        "acacia": ("oak", [(70, 62, 56), (112, 100, 90), (146, 130, 118)], [(150, 96, 60), (186, 124, 80), (214, 156, 104)],
                   [(74, 96, 28), (110, 132, 40), (152, 168, 62)]),
        "palm": ("oak", [(112, 92, 66), (154, 130, 96), (188, 164, 124)], [(166, 140, 96), (200, 176, 124), (224, 204, 150)],
                 [(34, 100, 34), (60, 140, 44), (110, 180, 66)]),
        "jungle": ("oak", [(62, 46, 30), (92, 70, 44), (122, 96, 62)], [(124, 92, 54), (160, 122, 76), (192, 152, 98)],
                   [(14, 78, 28), (24, 112, 38), (50, 150, 56)]),
    }
    for sp, (src, bark, rings, leaves) in species.items():
        t[f"{sp}_bark"] = recolour(f"{src}_bark", bark)
        t[f"{sp}_rings"] = recolour(f"{src}_rings", rings)
        t[f"{sp}_branch"] = recolour(f"{src}_branch", bark)
        t[f"{sp}_leaves"] = recolour(f"{src}_leaves", leaves)
    # palm bark: horizontal ring scars
    pb = t["palm_bark"]
    for y in range(0, 16, 4):
        for x in range(16):
            r, g, b, a = pb.getpixel((x, y))
            pb.putpixel((x, y), (r * 2 // 3, g * 2 // 3, b * 2 // 3, a))
    # --- cactus: ribbed green with spines
    cac = Image.new("RGBA", (16, 16))
    for y in range(16):
        for x in range(16):
            rib = x % 4
            c = [(40, 110, 48), (58, 142, 62), (70, 160, 72), (50, 124, 54)][rib]
            if rib == 0:
                c = (28, 84, 36)
            if rib == 0 and y % 3 == 1:
                c = (226, 214, 150)
            cac.putpixel((x, y), (*c, 255))
    t["cactus"] = cac
    # --- chest: oak planks (from the oak textures) with a dark rim, the
    # side with a lid seam and an iron latch
    plank = [(78, 52, 28), (122, 84, 46), (164, 118, 68)]
    rim = (52, 34, 18)
    top = recolour("oak_rings", plank)
    side = recolour("oak_bark", plank)
    for im in (top, side):
        for i in range(16):
            for p in ((i, 0), (i, 15), (0, i), (15, i)):
                im.putpixel(p, (*rim, 255))
    for x in range(16):
        side.putpixel((x, 5), (*rim, 255))
    for y in range(4, 8):
        for x in range(7, 9):
            side.putpixel((x, y), (150, 150, 156, 255) if y != 7 else (96, 96, 104, 255))
    t["chest_top"] = top
    t["chest_side"] = side
    return t


def main():
    t = build()
    for name, im in t.items():
        assert im.size == (16, 16), name
        im.save(OUT / f"{name}.png")
    if "--sheet" in sys.argv:
        names = sorted(t)
        cols = 8
        rows = (len(names) + cols - 1) // cols
        S, P = 8, 4
        sheet = Image.new("RGBA", (cols * (16 * S + P), rows * (16 * S + P)), (60, 60, 70, 255))
        for i, n in enumerate(names):
            sheet.alpha_composite(t[n].resize((16 * S, 16 * S), Image.NEAREST), ((i % cols) * (16 * S + P), (i // cols) * (16 * S + P)))
        (ROOT / "target").mkdir(exist_ok=True)
        sheet.save(ROOT / "target" / "textures-sheet.png")
        print("\n".join(f"{i}: {n}" for i, n in enumerate(names)))


if __name__ == "__main__":
    main()
