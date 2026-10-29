#!/usr/bin/env python3
"""Creature models from small sources: tools/creatures/src/<df_id>.toml → a body
template (proportions → bones, cubes, clips) → assets/models/creatures/<df_id>.bbmodel,
with a texture painted from the source's palette, plus the manifest
assets/models/creatures/creatures.json. Spec and style guide:
docs/superpowers/specs/2026-10-29-creature-asset-pipeline.md.

python3 tools/creatures/build.py [df_id ...]     (no ids: every source)

Output is deterministic: UUIDs derive from names, the texture layout and its
speckle from the cube list, so a rebuild without a source change rewrites the
same bytes. Never hand-edit the .bbmodel: the next build overwrites it; carry a
Blockbench touch-up back into the source instead.
"""
import base64
import io
import json
import math
import os
import sys
import tomllib
import uuid
import zlib

from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
SRC = os.path.join(HERE, "src")
OUT = os.path.join(REPO, "assets", "models", "creatures")
MANIFEST = os.path.join(OUT, "creatures.json")

# Bones of one template keep one UUID in every model (animations bind to bones
# by UUID), so they hash the template and bone name only.
NAMESPACE = uuid.UUID("5d0c7a52-3f3e-4f6b-9a1e-6c7265617475")


def uid(*parts):
    return str(uuid.uuid5(NAMESPACE, "/".join(parts)))


# --- Templates ----------------------------------------------------------------
#
# A template turns proportions into a skeleton: bones (name, parent, pivot, rest
# rotation), cubes (bone, part, from, to, all in model units, absolute) and clips
# (name → mode, length, {bone: {channel: [(time, [x, y, z])]}}). Creatures facing
# -Z, feet on y = 0. Signs: +X rotation swings a hanging leg's foot forward (-Z)
# and lifts a forward-pointing head's nose; a +Z-pointing tail goes down.

QUADRUPED_BUDGET = {"cubes": 24, "bones": 12}
# Every quadruped has these (the lint checks); a creature may add more.
QUADRUPED_CLIPS = ["idle", "walk", "run", "attack", "hurt", "death", "lie", "sleep", "eat"]

QUADRUPED_DEFAULTS = {
    "body": [6, 6, 13],          # width (x), height (y), length (z)
    "leg": [2, 6],               # thickness, length
    "leg_inset": 0,              # legs moved in from the body's sides
    "leg_ends": 1,               # legs moved in from the body's ends
    "head": [5, 5, 4],           # width, height, length
    "head_up": 2,                # head top above the body top
    "head_overlap": 1,           # head pushed back into the body
    "tail": [2, 7],              # thickness, length
    "tail_raise": 40,            # degrees above horizontal at rest
    # Gaits: cycle seconds and leg swing degrees.
    "walk": {"cycle": 0.8, "swing": 30, "bob": 0.3},
    "run": {"cycle": 0.45, "swing": 50, "bob": 0.8, "pitch": 6},
    "idle": {"length": 4.0, "look": 18, "breath": 0.15, "wag": 10},
    "attack": {"length": 0.6, "lunge": 3},
    "eat": {"length": 1.6, "reach": 45, "forward": 2.5},
}


def quadruped(p):
    bw, bh, bl = p["body"]
    lw, ll = p["leg"]
    hw, hh, hlen = p["head"]
    tw, tl = p["tail"]
    top = ll + bh
    bones = [
        ("root", None, [0, 0, 0], [0, 0, 0]),
        ("body", "root", [0, ll + bh / 2, 0], [0, 0, 0]),
        ("head", "body", [0, top - 1, -bl / 2], [0, 0, 0]),
        ("tail", "body", [0, top - 1, bl / 2], [-p["tail_raise"], 0, 0]),
    ]
    cubes = [("body", "body", "fur", [-bw / 2, ll, -bl / 2], [bw / 2, top, bl / 2])]
    lx = bw / 2 - lw / 2 - p["leg_inset"]
    fz = -bl / 2 + lw / 2 + p["leg_ends"]
    bz = bl / 2 - lw / 2 - p["leg_ends"]
    for name, x, z in (("leg_fl", -lx, fz), ("leg_fr", lx, fz), ("leg_bl", -lx, bz), ("leg_br", lx, bz)):
        bones.append((name, "body", [x, ll, z], [0, 0, 0]))
        # One unit up into the body, so a swinging leg never shows a gap.
        cubes.append((name, name, "legs", [x - lw / 2, 0, z - lw / 2], [x + lw / 2, ll + 1, z + lw / 2]))
    head_top = top + p["head_up"]
    hz = -bl / 2 + p["head_overlap"]
    cubes.append(("head", "head", "head", [-hw / 2, head_top - hh, hz - hlen], [hw / 2, head_top, hz]))
    ty = top - 1
    cubes.append(("tail", "tail", "tail", [-tw / 2, ty - tw / 2, bl / 2 - 1], [tw / 2, ty + tw / 2, bl / 2 - 1 + tl]))
    return {"bones": bones, "cubes": cubes, "clips": quadruped_clips(p), "budget": QUADRUPED_BUDGET,
            "required": QUADRUPED_CLIPS}


def quadruped_clips(p):
    bw, bh, bl = p["body"]
    lw, ll = p["leg"]
    clips = {}

    w = p["walk"]
    T, A, bob = w["cycle"], w["swing"], w["bob"]
    a = [(0, [A, 0, 0]), (T / 2, [-A, 0, 0]), (T, [A, 0, 0])]
    b = [(0, [-A, 0, 0]), (T / 2, [A, 0, 0]), (T, [-A, 0, 0])]
    clips["walk"] = clip("loop", T, {
        "leg_fl": {"rotation": a}, "leg_br": {"rotation": a},
        "leg_fr": {"rotation": b}, "leg_bl": {"rotation": b},
        "body": {"position": [(0, [0, 0, 0]), (T / 4, [0, bob, 0]), (T / 2, [0, 0, 0]),
                              (3 * T / 4, [0, bob, 0]), (T, [0, 0, 0])]},
        "head": {"rotation": [(0, [0, 0, 0]), (T / 4, [-3, 0, 0]), (T / 2, [0, 0, 0]),
                              (3 * T / 4, [-3, 0, 0]), (T, [0, 0, 0])]},
        "tail": {"rotation": [(0, [0, 8, 0]), (T / 2, [0, -8, 0]), (T, [0, 8, 0])]},
    }, stride=2 * ll * math.sin(math.radians(A)))

    r = p["run"]
    T, A = r["cycle"], r["swing"]
    front = [(0, [A, 0, 0]), (T / 2, [-A, 0, 0]), (T, [A, 0, 0])]
    back = [(0, [-A, 0, 0]), (T / 2, [A, 0, 0]), (T, [-A, 0, 0])]
    clips["run"] = clip("loop", T, {
        "leg_fl": {"rotation": front}, "leg_fr": {"rotation": shift(front, T * 0.08, T)},
        "leg_bl": {"rotation": back}, "leg_br": {"rotation": shift(back, T * 0.08, T)},
        "body": {
            "rotation": [(0, [r["pitch"], 0, 0]), (T / 2, [-r["pitch"], 0, 0]), (T, [r["pitch"], 0, 0])],
            "position": [(0, [0, 0, 0]), (T / 4, [0, r["bob"], 0]), (T / 2, [0, 0, 0]),
                         (3 * T / 4, [0, r["bob"], 0]), (T, [0, 0, 0])],
        },
        "head": {"rotation": [(0, [-r["pitch"], 0, 0]), (T / 2, [r["pitch"], 0, 0]), (T, [-r["pitch"], 0, 0])]},
        "tail": {"rotation": [(0, [15, 0, 0]), (T / 2, [5, 0, 0]), (T, [15, 0, 0])]},
    }, stride=2 * ll * math.sin(math.radians(A)) * 1.6)

    i = p["idle"]
    L, look, breath, wag = i["length"], i["look"], i["breath"], i["wag"]
    clips["idle"] = clip("loop", L, {
        "body": {"position": [(t, [0, breath * math.sin(2 * math.pi * t / (L / 2)), 0])
                              for t in frange(0, L, L / 8)]},
        "head": {"rotation": [(0, [0, 0, 0]), (L * 0.2, [0, look, 0]), (L * 0.35, [0, look, 0]),
                              (L * 0.5, [0, 0, 0]), (L * 0.7, [-6, -look, 0]), (L * 0.85, [-6, -look, 0]),
                              (L, [0, 0, 0])]},
        "tail": {"rotation": [(t, [0, wag * math.sin(2 * math.pi * t / (L / 5)), 0])
                              for t in frange(0, L, L / 20)]},
    })

    at = p["attack"]
    L, lunge = at["length"], at["lunge"]
    clips["attack"] = clip("once", L, {
        "body": {"position": [(0, [0, 0, 0]), (L * 0.25, [0, 0.5, -lunge]), (L * 0.45, [0, -0.5, -lunge]),
                              (L, [0, 0, 0])],
                 "rotation": [(0, [0, 0, 0]), (L * 0.25, [6, 0, 0]), (L * 0.45, [-4, 0, 0]), (L, [0, 0, 0])]},
        "head": {"rotation": [(0, [0, 0, 0]), (L * 0.2, [20, 0, 0]), (L * 0.42, [-25, 0, 0]), (L, [0, 0, 0])]},
        "leg_fl": {"rotation": [(0, [0, 0, 0]), (L * 0.3, [-25, 0, 0]), (L, [0, 0, 0])]},
        "leg_fr": {"rotation": [(0, [0, 0, 0]), (L * 0.3, [-25, 0, 0]), (L, [0, 0, 0])]},
        "leg_bl": {"rotation": [(0, [0, 0, 0]), (L * 0.3, [20, 0, 0]), (L, [0, 0, 0])]},
        "leg_br": {"rotation": [(0, [0, 0, 0]), (L * 0.3, [20, 0, 0]), (L, [0, 0, 0])]},
    })

    L = 0.35
    clips["hurt"] = clip("once", L, {
        "body": {"position": [(0, [0, 0, 0]), (L * 0.3, [0, 0.5, 1.5]), (L, [0, 0, 0])],
                 "rotation": [(0, [0, 0, 0]), (L * 0.3, [-6, 0, 8]), (L, [0, 0, 0])]},
        "head": {"rotation": [(0, [0, 0, 0]), (L * 0.3, [15, 0, 0]), (L, [0, 0, 0])]},
    })

    # Onto its side: a quarter turn about the ground under the body, lifted by
    # half the body's width so it lies on the ground, not through it.
    L = 1.0
    clips["death"] = clip("hold", L, {
        "root": {"rotation": [(0, [0, 0, 0]), (L * 0.6, [0, 0, 92]), (L * 0.75, [0, 0, 88]), (L, [0, 0, 90])],
                 "position": [(0, [0, 0, 0]), (L * 0.6, [0, bw / 2, 0]), (L, [0, bw / 2, 0])]},
        "head": {"rotation": [(0, [0, 0, 0]), (L * 0.6, [10, 0, 0]), (L, [-10, 0, 0])]},
        "leg_fl": {"rotation": [(0, [0, 0, 0]), (L * 0.6, [-20, 0, 0]), (L, [-15, 0, 0])]},
        "leg_fr": {"rotation": [(0, [0, 0, 0]), (L * 0.6, [-20, 0, 0]), (L, [-15, 0, 0])]},
        "leg_bl": {"rotation": [(0, [0, 0, 0]), (L * 0.6, [20, 0, 0]), (L, [15, 0, 0])]},
        "leg_br": {"rotation": [(0, [0, 0, 0]), (L * 0.6, [20, 0, 0]), (L, [15, 0, 0])]},
        "tail": {"rotation": [(0, [0, 0, 0]), (L, [p["tail_raise"], 0, 0])]},
    })

    # Folded: all four legs swing forward to lie flat under the body (the hind
    # ones tucked beneath it), which drops by the leg length less the folded
    # leg's thickness.
    drop = -(ll - lw)
    for name, head_down, period in (("lie", 0, 4.0), ("sleep", -28, 6.0)):
        fold = {"rotation": [(0, [80, 0, 0]), (period, [80, 0, 0])]}
        clips[name] = clip("loop", period, {
            "body": {"position": [(0, [0, drop, 0]), (period / 2, [0, drop + 0.25, 0]), (period, [0, drop, 0])]},
            "head": {"rotation": [(0, [head_down, 0, 0]), (period / 2, [head_down + 3, 0, 0]),
                                  (period, [head_down, 0, 0])],
                     "position": [(0, [0, -1 if head_down else 0, 0]), (period, [0, -1 if head_down else 0, 0])]},
            "leg_fl": fold, "leg_fr": fold, "leg_bl": fold, "leg_br": fold,
            "tail": {"rotation": [(0, [p["tail_raise"], 0, 0]), (period, [p["tail_raise"], 0, 0])]},
        })

    e = p["eat"]
    L, reach = e["length"], e["reach"]
    clips["eat"] = clip("loop", L, {
        "body": {"rotation": [(0, [-10, 0, 0]), (L, [-10, 0, 0])], "position": [(0, [0, -1, 0]), (L, [0, -1, 0])]},
        # Pushed forward so the muzzle reaches the ground ahead of the forelegs
        # instead of folding back under the chest.
        "head": {"rotation": [(t, [-reach + 6 * math.sin(2 * math.pi * t / (L / 4)), 0, 0])
                              for t in frange(0, L, L / 16)],
                 "position": [(0, [0, -0.5, -e["forward"]]), (L, [0, -0.5, -e["forward"]])]},
        "leg_fl": {"rotation": [(0, [8, 0, 0]), (L, [8, 0, 0])]},
        "leg_fr": {"rotation": [(0, [8, 0, 0]), (L, [8, 0, 0])]},
        "tail": {"rotation": [(0, [0, 6, 0]), (L / 2, [0, -6, 0]), (L, [0, 6, 0])]},
    })
    return clips


TEMPLATES = {"quadruped": (quadruped, QUADRUPED_DEFAULTS)}


def clip(mode, length, tracks, stride=None):
    return {"mode": mode, "length": round(length, 4), "tracks": tracks, "stride": stride}


def frange(a, b, step):
    n = round((b - a) / step)
    return [a + i * step for i in range(n + 1)]


def shift(keys, dt, period):
    """A looping track delayed by `dt`: the same curve, keyed from 0 to `period`."""
    def value(t):
        t = (t - dt) % period
        for (t0, v0), (t1, v1) in zip(keys, keys[1:]):
            if t0 <= t <= t1:
                f = (t - t0) / (t1 - t0) if t1 > t0 else 0
                return [a + (b - a) * f for a, b in zip(v0, v1)]
        return keys[-1][1]
    times = sorted({0.0, period} | {(t + dt) % period for t, _ in keys})
    return [(t, value(t)) for t in times]


# --- Creature sources -----------------------------------------------------------

def merge(defaults, override):
    out = dict(defaults)
    for k, v in override.items():
        out[k] = merge(defaults[k], v) if isinstance(v, dict) and isinstance(defaults.get(k), dict) else v
    return out


def load(df_id):
    with open(os.path.join(SRC, df_id + ".toml"), "rb") as f:
        src = tomllib.load(f)
    make, defaults = TEMPLATES[src["template"]]
    params = merge(defaults, src.get("proportions", {}))
    model = make(params)
    pivots = {name: pivot for name, _, pivot, _ in model["bones"]}
    # Extra cubes are given relative to their bone's pivot.
    for i, c in enumerate(src.get("cubes", [])):
        o = pivots[c["bone"]]
        model["cubes"].append((c.get("name", f"{c['bone']}_{i}"), c["bone"], c["part"],
                               [o[k] + c["from"][k] for k in range(3)], [o[k] + c["to"][k] for k in range(3)]))
    for name, c in src.get("clips", {}).items():
        tracks = {}
        for k in c.get("keys", []):
            tracks.setdefault(k["bone"], {}).setdefault(k["channel"], []).append((k["time"], k["value"]))
        model["clips"][name] = clip(c["mode"], c["length"], tracks)
    return src, model


# --- Texture painter --------------------------------------------------------------

FACES = ("north", "east", "south", "west", "up", "down")
# Gentle baked light on top of the engine's: tops a little lighter, bottoms darker.
FACE_LIGHT = {"up": 1.08, "down": 0.8, "north": 1.0, "south": 0.96, "east": 0.94, "west": 0.94}
RIM = 0.82
SPECKLE = 0.05


def face_size(face, lo, hi):
    w, h, d = (hi[i] - lo[i] for i in range(3))
    return {"north": (w, h), "south": (w, h), "east": (d, h), "west": (d, h), "up": (w, d), "down": (w, d)}[face]


def layout(cubes):
    """Shelf-pack every face of every cube at one texel per unit."""
    rects = []
    for ci, (name, _, _, lo, hi) in enumerate(cubes):
        for face in FACES:
            fw, fh = face_size(face, lo, hi)
            rects.append((ci, face, max(1, math.ceil(fw)), max(1, math.ceil(fh))))
    width = 64
    while max(r[2] for r in rects) > width:
        width *= 2
    placed = {}
    x = y = row = 0
    for ci, face, fw, fh in sorted(rects, key=lambda r: (-r[3], -r[2], r[0], r[1])):
        if x + fw > width:
            x, y, row = 0, y + row, 0
        placed[(ci, face)] = (x, y, fw, fh)
        x += fw
        row = max(row, fh)
    height = 16
    while height < y + row:
        height *= 2
    return width, height, placed


def rgb(hex_colour):
    h = hex_colour.lstrip("#")
    return tuple(int(h[i:i + 2], 16) for i in (0, 2, 4))


def shade(c, f):
    return tuple(max(0, min(255, round(v * f))) for v in c)


def paint(cubes, palette, decals, placed, size):
    img = Image.new("RGBA", size, (0, 0, 0, 0))
    px = img.load()
    names = [c[0] for c in cubes]
    for (ci, face), (x0, y0, fw, fh) in placed.items():
        part = cubes[ci][2]
        base = rgb(palette.get(f"{part}_{face}", palette[part]))
        light = FACE_LIGHT[face]
        for y in range(fh):
            for x in range(fw):
                f = light
                if fw >= 3 and fh >= 3 and (x in (0, fw - 1) or y in (0, fh - 1)):
                    f *= RIM
                noise = (zlib.crc32(f"{names[ci]}/{face}/{x}/{y}".encode()) % 1000) / 1000
                f *= 1 + SPECKLE * (2 * noise - 1)
                px[x0 + x, y0 + y] = shade(base, f) + (255,)
    for d in decals:
        ci = names.index(d["cube"])
        x0, y0, fw, fh = placed[(ci, d["face"])]
        colour = rgb(palette[d["colour"]])
        for y in range(d["y"], d["y"] + d.get("h", 1)):
            for x in range(d["x"], d["x"] + d.get("w", 1)):
                if 0 <= x < fw and 0 <= y < fh:
                    px[x0 + x, y0 + y] = colour + (255,)
    buf = io.BytesIO()
    img.save(buf, "PNG", optimize=True)
    return buf.getvalue()


# --- .bbmodel writer -------------------------------------------------------------

def num(v):
    v = round(float(v), 4)
    return int(v) if v == int(v) else v


def vec(v):
    return [num(x) for x in v]


def bbmodel(df_id, src, model):
    template = src["template"]
    cubes = model["cubes"]
    width, height, placed = layout(cubes)
    png = paint(cubes, src["palette"], src.get("decals", []), placed, (width, height))
    bone_uuid = {name: uid(template, "bone", name) for name, *_ in model["bones"]}
    pivots = {name: pivot for name, _, pivot, _ in model["bones"]}

    elements = []
    children = {name: [] for name in bone_uuid}
    for ci, (name, bone, _, lo, hi) in enumerate(cubes):
        e_uuid = uid(df_id, "cube", name)
        faces = {}
        for face in FACES:
            x, y, fw, fh = placed[(ci, face)]
            faces[face] = {"uv": [x, y, x + fw, y + fh], "texture": 0}
        elements.append({
            "name": name, "box_uv": False, "rescale": False, "locked": False, "render_order": "default",
            "allow_mirror_modeling": True, "from": vec(lo), "to": vec(hi), "autouv": 0, "color": 0,
            "origin": vec(pivots[bone]), "faces": faces, "type": "cube", "uuid": e_uuid,
        })
        children[bone].append(e_uuid)

    groups = []
    for name, parent, pivot, rest in model["bones"]:
        groups.append({
            "name": name, "origin": vec(pivot), "rotation": vec(rest), "color": 0, "uuid": bone_uuid[name],
            "export": True, "mirror_uv": False, "isOpen": True, "locked": False, "visibility": True, "autouv": 0,
        })

    def outline(name):
        kids = list(children[name])
        kids += [outline(b) for b, parent, *_ in model["bones"] if parent == name]
        return {"uuid": bone_uuid[name], "isOpen": True, "children": kids}

    animations = []
    for cname, c in model["clips"].items():
        animators = {}
        for bone, channels in c["tracks"].items():
            keyframes = []
            for channel, keys in channels.items():
                for t, v in keys:
                    keyframes.append({
                        "channel": channel, "data_points": [dict(zip("xyz", vec(v)))],
                        "uuid": uid(df_id, cname, bone, channel, f"{t:.4f}"), "time": num(t), "color": -1,
                        "interpolation": "catmullrom" if c["mode"] == "loop" else "linear",
                    })
            animators[bone_uuid[bone]] = {"name": bone, "type": "bone", "keyframes": keyframes}
        animations.append({
            "uuid": uid(df_id, "clip", cname), "name": cname, "loop": c["mode"], "override": False,
            "length": num(c["length"]), "snapping": 24, "selected": False, "anim_time_update": "",
            "blend_weight": "", "start_delay": "", "loop_delay": "", "animators": animators,
        })

    return {
        "meta": {"format_version": "5.0", "model_format": "free", "box_uv": False},
        "name": df_id,
        "model_identifier": df_id,
        "visible_box": [1, 1, 0],
        "variable_placeholders": "",
        "variable_placeholder_buttons": [],
        "timeline_setups": [],
        "unhandled_root_fields": {},
        "resolution": {"width": width, "height": height},
        "elements": elements,
        "groups": groups,
        "outliner": [outline("root")],
        "textures": [{
            "path": "", "name": df_id + ".png", "folder": "", "namespace": "", "id": "0",
            "width": width, "height": height, "uv_width": width, "uv_height": height, "particle": False,
            "render_mode": "default", "render_sides": "auto", "frame_time": 1, "frame_order_type": "loop",
            "frame_order": "", "frame_interpolate": False, "visible": True, "internal": True, "saved": True,
            "uuid": uid(df_id, "texture"),
            "source": "data:image/png;base64," + base64.b64encode(png).decode(),
        }],
        "animations": animations,
    }


def rotate(p, deg):
    """`p` rotated by Euler degrees in three.js ZYX order (X first), like the loader."""
    x, y, z = p
    a, b, c = (math.radians(d) for d in deg)
    y, z = y * math.cos(a) - z * math.sin(a), y * math.sin(a) + z * math.cos(a)
    x, z = x * math.cos(b) + z * math.sin(b), -x * math.sin(b) + z * math.cos(b)
    x, y = x * math.cos(c) - y * math.sin(c), x * math.sin(c) + y * math.cos(c)
    return [x, y, z]


def rest_bounds(model):
    """The rest pose's box: every cube corner through its bones' rest rotations."""
    bones = {name: (parent, pivot, rest) for name, parent, pivot, rest in model["bones"]}
    points = []
    for _, bone, _, lo, hi in model["cubes"]:
        for corner in ([x, y, z] for x in (lo[0], hi[0]) for y in (lo[1], hi[1]) for z in (lo[2], hi[2])):
            b = bone
            while b:
                parent, pivot, rest = bones[b]
                corner = [r + o for r, o in zip(rotate([c - o for c, o in zip(corner, pivot)], rest), pivot)]
                b = parent
            points.append(corner)
    return [min(p[i] for p in points) for i in range(3)], [max(p[i] for p in points) for i in range(3)]


def manifest_entry(df_id, src, model):
    lo, hi = rest_bounds(model)
    clips = {}
    for name, c in model["clips"].items():
        entry = {"loop": c["mode"], "length": num(c["length"])}
        if c["stride"]:
            entry["stride"] = num(c["stride"])
            entry["speed"] = num(c["stride"] / c["length"])
        clips[name] = entry
    return {
        "name": src["name"],
        # draft until Dawid approves it at the review gate (set in the source).
        "status": src.get("status", "draft"),
        "model": f"models/creatures/{df_id}.bbmodel",
        "template": src["template"],
        "bounds": [vec(lo), vec(hi)],
        "budget": model["budget"],
        "required_clips": model["required"],
        "cubes": len(model["cubes"]),
        "bones": len(model["bones"]),
        "clips": clips,
    }


def main(ids):
    if not ids:
        ids = sorted(f[:-5] for f in os.listdir(SRC) if f.endswith(".toml"))
    os.makedirs(OUT, exist_ok=True)
    try:
        with open(MANIFEST) as f:
            manifest = json.load(f)
    except FileNotFoundError:
        manifest = {}
    for df_id in ids:
        src, model = load(df_id)
        path = os.path.join(OUT, df_id + ".bbmodel")
        with open(path, "w") as f:
            json.dump(bbmodel(df_id, src, model), f, indent=1)
            f.write("\n")
        manifest[df_id] = manifest_entry(df_id, src, model)
        print(f"{df_id}: {len(model['cubes'])} cubes, {len(model['bones'])} bones, "
              f"{len(model['clips'])} clips -> {os.path.relpath(path, REPO)}")
    with open(MANIFEST, "w") as f:
        json.dump(dict(sorted(manifest.items())), f, indent=1)
        f.write("\n")


if __name__ == "__main__":
    main(sys.argv[1:])
