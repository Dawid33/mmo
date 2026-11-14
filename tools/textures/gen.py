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


# --- building set (prefabs plan 1): our own procedural patterns -----------------
DYES = [
    ("white", "E9ECEC"), ("light_gray", "8E8E86"), ("gray", "3E4447"), ("black", "15151A"),
    ("brown", "724728"), ("red", "A12722"), ("orange", "F07613"), ("yellow", "F8C627"),
    ("lime", "70B919"), ("green", "546D1B"), ("cyan", "158991"), ("light_blue", "3AAFD9"),
    ("blue", "35399D"), ("purple", "792AAC"), ("magenta", "BD44B3"), ("pink", "ED8DAC"),
]
TERRACOTTA_BASE = "985E43"


def rgb(hexstr):
    return tuple(int(hexstr[i:i + 2], 16) for i in (0, 2, 4))


def shade(c, f):
    return tuple(min(255, max(0, round(v * f))) for v in c[:3])


def mix(a, b, t):
    return lerp(a, b, t)


def canvas(fn):
    """Build a 16x16 opaque image from fn(x, y) -> rgb."""
    im = Image.new("RGBA", (16, 16))
    for y in range(16):
        for x in range(16):
            im.putpixel((x, y), (*fn(x, y), 255))
    return im


def speckle(base, spread, salt, weave=False):
    """Flat colour with per-texel lightness jitter; weave adds a faint 2-texel cloth check."""
    def px(x, y):
        f = 1 + (h(x, y, salt) - 0.5) * 2 * spread
        if weave:
            f *= 0.95 if ((x // 2) + (y // 2)) % 2 else 1.02
        return shade(base, f)
    return canvas(px)


def smooth(base, salt):
    """Near-flat, darker 1-texel border on the bottom and right edges."""
    def px(x, y):
        f = 1 + (h(x, y, salt) - 0.5) * 0.05
        if x == 15 or y == 15:
            f *= 0.86
        elif x == 0 or y == 0:
            f *= 1.04
        return shade(base, f)
    return canvas(px)


def bricks(base, mortar, bw, bh, salt, cracked=False, mossy=False):
    """Running bond: a mortar texel under each row and right of each brick, odd rows offset."""
    def px(x, y):
        row = y // bh
        xo = (x + (bw // 2 if row % 2 else 0)) % 16
        col = (x + (bw // 2 if row % 2 else 0)) // bw
        if y % bh == bh - 1 or xo % bw == bw - 1:
            return shade(mortar, 1 + (h(x, y, salt + 1) - 0.5) * 0.08)
        tint = 1 + (h(col, row, salt) - 0.5) * 0.22
        c = shade(base, tint * (1 + (h(x, y, salt + 2) - 0.5) * 0.1))
        if y % bh == 0:
            c = shade(c, 1.07)
        if cracked and h(x // 2, y // 2, salt + 3) > 0.84 and h(x, y, salt + 4) > 0.3:
            c = shade(c, 0.55)
        if mossy:
            m = h(x // 2, y // 2, salt + 5)
            if m > 0.72 or (y < 3 and m > 0.5):
                c = mix(c, (74, 112, 48), 0.65)
        return c
    return canvas(px)


def cobble(base, salt, cells=4):
    """Irregular rounded stones: nearest/second-nearest seed distance, thin gap darkened."""
    step = 16 / cells
    seeds = []
    for cy in range(cells):
        for cx in range(cells):
            seeds.append(((cx + 0.2 + 0.6 * h(cx, cy, salt)) * step, (cy + 0.2 + 0.6 * h(cx, cy, salt + 1)) * step, cx + cy * cells))

    def px(x, y):
        ds = []
        for sx, sy, i in seeds:
            dx = min(abs(x + 0.5 - sx), 16 - abs(x + 0.5 - sx))
            dy = min(abs(y + 0.5 - sy), 16 - abs(y + 0.5 - sy))
            ds.append((dx * dx + dy * dy, i))
        ds.sort()
        d1, i1 = ds[0]
        d2 = ds[1][0]
        if d2 ** 0.5 - d1 ** 0.5 < 1.0:
            return shade(base, 0.45)
        tint = 0.85 + h(i1, 0, salt + 2) * 0.3
        edge = 0.93 if d2 ** 0.5 - d1 ** 0.5 < 1.8 else 1.0
        return shade(base, tint * edge * (1 + (h(x, y, salt + 3) - 0.5) * 0.08))
    return canvas(px)


def planks(base, salt):
    """Four horizontal boards of 4 texels, staggered end joints, grain."""
    def px(x, y):
        board = y // 4
        tint = 1 + (h(board, 0, salt) - 0.5) * 0.14
        joint = int(h(board, 1, salt) * 14) + 1
        if y % 4 == 3:
            return shade(base, 0.7)
        if x == joint:
            return shade(base, 0.72)
        grain = 1 + (h(x // 3, y, salt + 2) - 0.5) * 0.14
        return shade(base, tint * grain * (1 + (h(x, y, salt + 3) - 0.5) * 0.05))
    return canvas(px)


def stripped(base, salt):
    """Vertical grain, a few dark streaks."""
    def px(x, y):
        col = 1 + (h(x, 0, salt) - 0.5) * 0.2
        if h(x, 1, salt) > 0.8:
            col *= 0.82
        return shade(base, col * (1 + (h(x, y // 3, salt + 1) - 0.5) * 0.08))
    return canvas(px)


def pillar(base, salt):
    """Vertical flutes: light, mid, dark, mid."""
    ramp4 = [1.05, 0.98, 0.9, 0.98]
    return canvas(lambda x, y: shade(base, ramp4[x % 4] * (1 + (h(x, y, salt) - 0.5) * 0.03)))


def pillar_top(base, salt):
    """Concentric squares: dark rim, light band, mid fill."""
    def px(x, y):
        r = min(x, y, 15 - x, 15 - y)
        f = {0: 0.86, 1: 1.0, 2: 1.07, 3: 0.95}.get(r, 1.0)
        return shade(base, f * (1 + (h(x, y, salt) - 0.5) * 0.03))
    return canvas(px)


def band(img, rows, f):
    for y in rows:
        for x in range(16):
            r, g, b, a = img.getpixel((x, y))
            img.putpixel((x, y), (*shade((r, g, b), f), a))
    return img


def cut_sandstone(base, salt):
    return band(smooth(base, salt), (6, 7, 8, 9), 0.9)


def chiseled_sandstone(base, salt):
    """Smooth with an inset square frame and a diamond motif."""
    im = smooth(base, salt)
    for i in range(3, 13):
        for p in ((i, 3), (i, 12), (3, i), (12, i)):
            im.putpixel(p, (*shade(base, 0.78), 255))
    for d in range(-2, 3):
        for p in ((8 + d, 8 - (2 - abs(d))), (8 + d, 8 + (2 - abs(d)))):
            im.putpixel(p, (*shade(base, 0.82), 255))
    return im


def strata(base, salt, spread):
    """Speckle plus per-row banding (red sandstone)."""
    return canvas(lambda x, y: shade(base, (1 + (h(x, y, salt) - 0.5) * 2 * spread) * (1 + (h(0, y, salt + 1) - 0.5) * 0.12)))


def glass(tint, glint=None):
    """1-texel frame at full alpha, two short diagonal glints, everything else alpha 0."""
    im = Image.new("RGBA", (16, 16), (0, 0, 0, 0))
    frame = shade(tint, 0.85)
    for i in range(16):
        for p in ((i, 0), (i, 15), (0, i), (15, i)):
            im.putpixel(p, (*frame, 255))
    gl = glint or mix(tint, (255, 255, 255), 0.6)
    for p in ((3, 5), (4, 4), (5, 3), (3, 8), (4, 7)):
        im.putpixel(p, (*gl, 255))
    return im


def metal(base, salt):
    """Bevelled plate: light top/left, dark bottom/right, inset panel line, faint noise."""
    def px(x, y):
        f = 1 + (h(x, y, salt) - 0.5) * 0.05
        if y == 0 or x == 0:
            f *= 1.15
        elif y == 15 or x == 15:
            f *= 0.8
        elif y == 1 or x == 1:
            f *= 1.06
        elif y == 14 or x == 14:
            f *= 0.9
        elif y in (3, 12) and 3 <= x <= 12 or x in (3, 12) and 3 <= y <= 12:
            f *= 0.93
        return shade(base, f)
    return canvas(px)


def hay_side(base, salt):
    """Horizontal straw strands with a darker tie band."""
    def px(x, y):
        f = 1 + (h(x // 4, y, salt) - 0.5) * 0.3 + (h(x, y, salt + 1) - 0.5) * 0.1
        if x in (5, 10):
            f *= 0.72
        return shade(base, f)
    return canvas(px)


def hay_top(base, salt):
    """Radial-ish straw ends: concentric squares with noise."""
    def px(x, y):
        r = min(x, y, 15 - x, 15 - y)
        f = (0.82, 1.0, 0.92, 1.06, 0.94, 1.02, 0.9, 1.0)[r]
        return shade(base, f * (1 + (h(x, y, salt) - 0.5) * 0.14))
    return canvas(px)


BOOKS = [(120, 38, 34), (44, 84, 120), (52, 100, 56), (122, 92, 40), (84, 52, 100), (170, 150, 110), (60, 40, 30)]


def bookshelf(plank, salt):
    """Plank rows at 0, 7, 15; two shelves of vertical books between."""
    def px(x, y):
        if y in (0, 7, 15):
            return shade(plank, 0.85 + (h(x, y, salt) - 0.5) * 0.1)
        shelf = 0 if y < 7 else 1
        # book columns: widths 1-3 from a hash walk
        pos, i = 0, 0
        while True:
            w = 1 + int(h(i, shelf, salt) * 3)
            if x < pos + w:
                break
            pos += w
            i += 1
        base = BOOKS[int(h(i, shelf, salt + 1) * len(BOOKS))]
        top = 2 + int(h(i, shelf, salt + 2) * 2)
        yy = y - (1 if shelf == 0 else 8)
        if yy < top - 2:
            return shade(plank, 0.45)  # dark gap above a short book
        f = 1.0 if x == pos else 0.85 if x == pos + w - 1 else 0.95
        return shade(base, f)
    return canvas(px)


def thatch(base, salt):
    """Diagonal straw strokes with dark lines every 4 rows."""
    def px(x, y):
        f = 1 + (h((x + y) // 2, y // 4, salt) - 0.5) * 0.3 + (h(x, y, salt + 1) - 0.5) * 0.1
        if y % 4 == 3:
            f *= 0.72
        return shade(base, f)
    return canvas(px)


def building_set():
    """The 121-block building set's textures, from the plan's colour table (never Mojang art)."""
    t = {}
    sand, rsand = rgb("D8CB9B"), rgb("B5622A")
    t["smooth_sandstone"] = smooth(sand, 101)
    t["cut_sandstone"] = cut_sandstone(sand, 102)
    t["chiseled_sandstone"] = chiseled_sandstone(sand, 103)
    t["red_sandstone"] = strata(rsand, 104, 0.07)
    t["smooth_red_sandstone"] = smooth(rsand, 105)
    q = rgb("ECE6DF")
    t["quartz"] = canvas(lambda x, y: shade(q, (1 + (h(x, y, 106) - 0.5) * 0.04) * (0.96 if (x + y) % 7 == 0 else 1.0)))
    t["smooth_quartz"] = smooth(q, 107)
    t["quartz_bricks"] = bricks(q, shade(q, 0.8), 8, 8, 108)
    t["quartz_pillar"] = pillar(q, 109)
    t["quartz_pillar_top"] = pillar_top(q, 110)
    stone = rgb("7D7D7D")
    t["cobblestone"] = cobble(stone, 111)
    t["stone_bricks"] = bricks(stone, shade(stone, 0.7), 8, 8, 112)
    t["cracked_stone_bricks"] = bricks(stone, shade(stone, 0.7), 8, 8, 112, cracked=True)
    t["mossy_stone_bricks"] = bricks(stone, shade(stone, 0.7), 8, 8, 112, mossy=True)
    t["smooth_stone"] = smooth(shade(stone, 1.08), 113)
    t["andesite"] = speckle(rgb("888A88"), 0.11, 114)
    t["polished_andesite"] = smooth(rgb("888A88"), 115)
    t["diorite"] = canvas(lambda x, y: shade(rgb("C2C2C4"), 1 + (h(x, y, 116) - 0.5) * 0.18 - (0.22 if h(x, y, 117) > 0.9 else 0)))
    t["polished_diorite"] = smooth(rgb("C2C2C4"), 118)
    t["polished_granite"] = smooth(rgb("9A6B58"), 119)
    t["tuff"] = canvas(lambda x, y: shade(rgb("6C6D66"), (1 + (h(x, y, 120) - 0.5) * 0.14) * (0.88 if h(x // 3, y // 2, 121) > 0.7 else 1.0)))
    t["calcite"] = speckle(rgb("DFE0DC"), 0.04, 122)
    ds = rgb("4A4A4F")
    t["deepslate"] = canvas(lambda x, y: shade(ds, (1 + (h(x, y, 123) - 0.5) * 0.12) * (1 + (h(0, y, 124) - 0.5) * 0.1)))
    t["cobbled_deepslate"] = cobble(ds, 125)
    t["deepslate_bricks"] = bricks(ds, shade(ds, 0.6), 8, 8, 126)
    t["polished_deepslate"] = smooth(ds, 127)
    brick = rgb("96604E")
    t["bricks"] = bricks(brick, rgb("B8AFA5"), 8, 4, 128)
    mud = rgb("8A6A4F")
    t["mud_bricks"] = bricks(mud, shade(mud, 0.72), 8, 8, 129)
    t["clay"] = speckle(rgb("A0A6B3"), 0.03, 130)
    t["packed_mud"] = canvas(lambda x, y: shade(rgb("8E6B50"), (1 + (h(x, y, 131) - 0.5) * 0.12) * (0.92 if h(x // 4, y, 132) > 0.75 else 1.0)))
    t["prismarine"] = cobble(rgb("63A396"), 133)
    t["dark_prismarine"] = speckle(rgb("335B4B"), 0.12, 134)
    bone = rgb("E3DFD0")
    t["bone_block"] = canvas(lambda x, y: shade(bone, (1.0, 0.9, 1.05, 0.95)[(x + (y // 5) % 2) % 4] * (1 + (h(x, y, 135) - 0.5) * 0.04)))
    t["bone_block_top"] = pillar_top(bone, 136)
    woods = [("oak", "A2824E"), ("spruce", "72542F"), ("birch", "C4B37B"), ("acacia", "A85A32"),
             ("jungle", "A07350"), ("dark_oak", "42290F"), ("mangrove", "75362F"), ("warped", "2B6963")]
    for i, (w, c) in enumerate(woods):
        t[f"{w}_planks"] = planks(rgb(c), 140 + i)
    for i, (w, c) in enumerate(woods):
        t[f"stripped_{w}"] = stripped(shade(rgb(c), 1.08), 150 + i)
    for i, (d, c) in enumerate(DYES):
        t[f"{d}_concrete"] = speckle(rgb(c), 0.025, 160 + i)
    t["terracotta"] = speckle(rgb(TERRACOTTA_BASE), 0.05, 180)
    tb = rgb(TERRACOTTA_BASE)
    for i, (d, c) in enumerate(DYES):
        dc = rgb(c)
        tc = tuple(round(dc[k] * 0.45 + tb[k] * 0.55) for k in range(3))
        t[f"{d}_terracotta"] = speckle(tc, 0.05, 181 + i)
        t[f"{d}_wool"] = speckle(dc, 0.06, 200 + i, weave=True)
    t["glass"] = glass(rgb("D9F1F5"))
    for d, c in DYES:
        t[f"{d}_stained_glass"] = glass(rgb(c))
    t["gold_block"] = metal(rgb("F6D03D"), 230)
    t["iron_block"] = metal(rgb("D8D8D8"), 231)
    t["copper_block"] = metal(rgb("C06B4E"), 232)
    t["hay_bale"] = hay_side(rgb("B59A1E"), 233)
    t["hay_bale_top"] = hay_top(rgb("B59A1E"), 234)
    t["bookshelf"] = bookshelf(rgb("A2824E"), 235)
    t["thatch"] = thatch(rgb("A8904A"), 236)
    t["cobbled_road"] = cobble(rgb("77736B"), 237, cells=5)
    return t


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
    t.update(building_set())
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
