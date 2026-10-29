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
import fcntl
import io
import json
import math
import os
import re
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


from templates import (TEMPLATES, WING_CLIPS, add_wings, at, clip, extra_clips, hold, osc,  # noqa: E402
                       rest_points, wing_clips)


# --- Creature sources -----------------------------------------------------------
#
# A source names a template and overrides its proportions, then adds to it:
#   [[bones]]  name, parent, pivot (relative to the parent's pivot), rest, and
#              optionally follow = "<bone>" (every clip moves it like that bone:
#              a hydra's extra heads) and mirror = true (also a "_r" twin of an
#              "_l" bone, mirrored in x).
#   [[cubes]]  name, bone, part, from, to (relative to the bone's pivot);
#              mirror = true adds the x-mirrored twin ("_l" → "_r" in the cube
#              and bone names).
#   [wings]    bone, at (left shoulder, relative to the bone's pivot), span,
#              chord, part, + fly-clip tunables: adds wing_l/wing_r and the fly set.
#   [clips.X]  mode, length, keys; merge = true adds the keys' tracks to the
#              template's clip X instead of replacing it.
#   extras     template-independent clips: alert, roar, breathe, swim, swim_idle,
#              ridden, pick_up, carry, spit (templates.extra_clips).
#   [budget]   raise a template budget (say why in a comment).
# Derived sources: variant_of = "<df_id>" + scale (a giant: the base scaled,
# slower cycles), animal_person_of = "<df_id>" (the biped with the base's head,
# tail and colours).

def merge(defaults, override):
    out = dict(defaults)
    for k, v in override.items():
        out[k] = merge(defaults[k], v) if isinstance(v, dict) and isinstance(defaults.get(k), dict) else v
    return out


def read_source(df_id):
    with open(os.path.join(SRC, df_id + ".toml"), "rb") as f:
        return tomllib.load(f)


def other_side(name):
    """The right-hand twin of a left-side name: `x_l`, `leg_l2`, `leg_fl` / `leg_bl`."""
    m = re.fullmatch(r"(.*_)l(\d*)", name) or re.fullmatch(r"(.*_[fb])l()", name)
    return f"{m.group(1)}r{m.group(2)}" if m else name


def load(df_id):
    src = read_source(df_id)
    if "variant_of" in src:
        base_src, model = load(src["variant_of"])
        return variant(base_src, src, model)
    if "animal_person_of" in src:
        return animal_person(src)
    return src, build_model(src)


def check_source(src, defaults):
    """Catch keys TOML silently put in the wrong table (a `decals = [...]` line written
    after `[proportions]` lands in it and would be ignored)."""
    unknown = sorted(set(src.get("proportions", {})) - set(defaults))
    if unknown:
        raise ValueError(f"[proportions] has keys the template does not know: {unknown} "
                         "(a top-level key written after a [table] header?)")
    bad = sorted(k for k, v in src.get("palette", {}).items() if not isinstance(v, str))
    if bad:
        raise ValueError(f"[palette] has non-colour entries: {bad}")


def build_model(src, params=None):
    t = TEMPLATES[src["template"]]
    check_source(src, t.defaults)
    params = params or merge(t.defaults, src.get("proportions", {}))
    model = t.skeleton(params)
    pivots = {name: pivot for name, _, pivot, _ in model["bones"]}
    follow = {}
    for b in src.get("bones", []):
        twins = [(b["name"], 1)] + ([(other_side(b["name"]), -1)] if b.get("mirror") else [])
        for name, s in twins:
            parent = other_side(b["parent"]) if s < 0 and other_side(b["parent"]) in pivots else b["parent"]
            o = pivots[parent]
            pivot = [o[0] + s * b["pivot"][0], o[1] + b["pivot"][1], o[2] + b["pivot"][2]]
            rest = b.get("rest", [0, 0, 0])
            model["bones"].append((name, parent, pivot, [rest[0], s * rest[1], s * rest[2]]))
            pivots[name] = pivot
            if "follow" in b:
                target = b["follow"]
                if s < 0 and other_side(target) != target:
                    # A sided target's twin already moves mirror-wise: follow it as is.
                    follow[name] = (other_side(target), 1)
                else:
                    follow[name] = (target, s)
    for i, c in enumerate(src.get("cubes", [])):
        name = c.get("name", f"{c['bone']}_{i}")
        twins = [(name, c["bone"], 1)]
        if c.get("mirror"):
            twins.append((other_side(name) if other_side(name) != name else name + "_m",
                          other_side(c["bone"]) if other_side(c["bone"]) in pivots else c["bone"], -1))
        for cname, bone, s in twins:
            o = pivots[bone]
            xs = sorted((s * c["from"][0], s * c["to"][0]))
            model["cubes"].append((cname, bone, c["part"], [o[0] + xs[0], o[1] + c["from"][1], o[2] + c["from"][2]],
                                   [o[0] + xs[1], o[1] + c["to"][1], o[2] + c["to"][2]]))
    wings = src.get("wings")
    if wings:
        wings = dict(wings)
        o = pivots[wings.get("bone", "body")]
        wings["at"] = [o[k] + wings["at"][k] for k in range(3)]
        add_wings(model, wings)
    model["clips"] = t.clips(params, model)
    model["required"] = t.required(params)
    if wings:
        model["clips"].update(wing_clips(model, wings))
        model["required"] += [c for c in WING_CLIPS if c not in model["required"]]
    # Template tracks for bones this creature does not have (a wingless bird).
    has = {b[0] for b in model["bones"]}
    for c in model["clips"].values():
        c["tracks"] = {b: ch for b, ch in c["tracks"].items() if b in has}
    extras = src.get("extras", [])
    model["clips"].update(extra_clips(extras, model, model["clips"]))
    model["required"] += [c for c in extras if c not in model["required"]]
    for name, (target, s) in follow.items():
        for c in model["clips"].values():
            if target in c["tracks"]:
                c["tracks"][name] = {ch: [(t, [v[0], s * v[1], s * v[2]] if ch == "rotation" else
                                           [s * v[0], v[1], v[2]] if ch == "position" else list(v))
                                          for t, v in keys]
                                     for ch, keys in c["tracks"][target].items()}
    for name, c in src.get("clips", {}).items():
        tracks = {}
        for k in c.get("keys", []):
            tracks.setdefault(k["bone"], {}).setdefault(k["channel"], []).append((k["time"], k["value"]))
        if c.get("merge"):
            base = model["clips"][name]
            for bone, channels in tracks.items():
                base["tracks"].setdefault(bone, {}).update(channels)
        else:
            model["clips"][name] = clip(c["mode"], c["length"], tracks)
    model["budget"] = merge(t.budget, src.get("budget", {}))
    return model


def scale_model(model, s):
    """`model` `s` times bigger, its cycles slower by √s (pendulum legs), on half units."""
    k = math.sqrt(s)

    def sc(v):
        return [round(x * s * 2) / 2 for x in v]

    out = dict(model)
    out["bones"] = [(n, parent, sc(pivot), rest) for n, parent, pivot, rest in model["bones"]]
    out["cubes"] = []
    for n, bone, part, lo, hi in model["cubes"]:
        lo, hi = sc(lo), sc(hi)
        hi = [max(h, l_ + 0.5) for l_, h in zip(lo, hi)]
        out["cubes"].append((n, bone, part, lo, hi))
    out["clips"] = {}
    for name, c in model["clips"].items():
        tracks = {bone: {ch: [(t * k, [x * s for x in v] if ch == "position" else v) for t, v in keys]
                         for ch, keys in channels.items()}
                  for bone, channels in c["tracks"].items()}
        out["clips"][name] = clip(c["mode"], c["length"] * k, tracks, c["stride"] and c["stride"] * s)
    return out


def scale_decals(decals, s, names=None, grow=None):
    """Decals moved with a face scaled by `s`. Their size grows by `grow` (default `s`):
    an animal person's head may be several times its animal's, and a 1-texel eye
    grown as much would cover the face."""
    grow = s if grow is None else grow
    out = []
    for d in decals:
        if names is not None and d["cube"] not in names:
            continue
        d = dict(d)
        for key in ("x", "y"):
            d[key] = int(math.floor(d[key] * s + 0.5))
        for key in ("w", "h"):
            d[key] = max(1, int(round(d.get(key, 1) * grow)))
        out.append(d)
    return out


def variant(base_src, src, model):
    s = src["scale"]
    out = dict(base_src)
    out.update({k: v for k, v in src.items() if k not in ("palette",)})
    out["template"] = base_src["template"]
    out["palette"] = {**base_src["palette"], **src.get("palette", {})}
    out["decals"] = scale_decals(base_src.get("decals", []), s)
    return out, grounded(scale_model(model, s))


# An animal person: a person's build (less stocky than a dwarf), the base
# animal's head on the neck and its tail at the hips, its colours on the body.
PERSON = {"leg": [4, 11], "foot": 2, "body": [9, 11, 5], "arm": [3, 11], "head": [7, 7, 7]}
PERSON_SIZE = 70000  # cm³: a human; animal people scale by the cube root from here


def animal_person(src):
    base_src, base = load(src["animal_person_of"])
    t = TEMPLATES["biped"]
    params = merge(t.defaults, merge(PERSON, src.get("proportions", {})))
    model = t.skeleton(params)
    pivots = {name: pivot for name, _, pivot, _ in model["bones"]}

    def graft(src_model, bones, onto, anchor, k):
        """The cubes of `bones` (their rest pose, absolute) moved so `anchor` (a
        point in the source model) lands on the bone `onto`'s pivot, scaled by k."""
        grafted = []
        for cube in src_model["cubes"]:
            if cube[1] not in bones:
                continue
            pts = rest_points({"bones": src_model["bones"], "cubes": [cube]})
            lo = [min(p[i] for p in pts) for i in range(3)]
            hi = [max(p[i] for p in pts) for i in range(3)]
            o = pivots[onto]
            grafted.append((cube[0], onto, cube[2], [o[i] + (lo[i] - anchor[i]) * k for i in range(3)],
                            [o[i] + (hi[i] - anchor[i]) * k for i in range(3)]))
        return grafted

    heads = subtree_of(base, "head") if any(b[0] == "head" for b in base["bones"]) else set()
    head_cubes = [c for c in base["cubes"] if c[1] in heads]
    if not head_cubes:
        # A headless base (a jellyfish, a sponge): the person keeps its own head.
        head_cubes = [next(c for c in model["cubes"] if c[0] == "head")]
        heads = set()
    volume = lambda c: (c[4][0] - c[3][0]) * (c[4][1] - c[3][1]) * (c[4][2] - c[3][2])  # noqa: E731
    head_cube = next((c for c in head_cubes if c[0] == "head"), max(head_cubes, key=volume))
    head_src = base if heads else model
    hpts = rest_points({"bones": head_src["bones"], "cubes": [head_cube]})
    hlo = [min(p[i] for p in hpts) for i in range(3)]
    hhi = [max(p[i] for p in hpts) for i in range(3)]
    # The base head brought to a person's head width (vermin heads grow a lot, a
    # whale's shrinks a lot).
    k = min(8.0, max(0.3, params["head"][0] / (hhi[0] - hlo[0])))
    # The main head cube sits on the neck; a trunk, beard or bill may hang lower.
    anchor = [(hlo[0] + hhi[0]) / 2, hlo[1], (hlo[2] + hhi[2]) / 2]
    base_bones = {name: (parent, pivot, rest) for name, parent, pivot, rest in base["bones"]}
    legless = src.get("legless", False)

    if legless:
        # The person's upper body on the animal's: the base without its head,
        # scaled to the torso, its neck under the hips, its bones kept (renamed
        # low_*) so its own gait still moves it.
        lower = [c for c in base["cubes"] if c[1] not in heads]
        lpts = rest_points({"bones": base["bones"], "cubes": lower})
        width = max(p[0] for p in lpts) - min(p[0] for p in lpts)
        length = max(p[2] for p in lpts) - min(p[2] for p in lpts)
        # Wide enough to carry the torso, but a long animal stays under 60 units.
        s_low = min(2.5, max(0.6, params["body"][0] / width), 60 / length)
        neck = base_bones["head"][1]
        ground = min(p[1] for p in lpts)
        hip = (neck[1] - ground) * s_low
        params["leg"] = [params["leg"][0], hip]
        params["foot"] = 0
        model = t.skeleton(params)
        model["bones"] = [b for b in model["bones"] if not b[0].startswith("leg_")]
        model["cubes"] = [c for c in model["cubes"] if c[1] not in ("leg_l", "leg_r") and (c[0] != "head" or not heads)]
        pivots = {name: pivot for name, _, pivot, _ in model["bones"]}

        def place(v):
            return [(v[0] - neck[0]) * s_low, hip + (v[1] - neck[1]) * s_low, (v[2] - neck[2]) * s_low]

        rename = {}
        for name, parent, pivot, rest in base["bones"]:
            if name in heads or parent is None:
                continue
            rename[name] = "low_" + name
        for name, parent, pivot, rest in base["bones"]:
            if name in rename:
                model["bones"].append((rename[name], rename.get(parent, "body"), place(pivot), rest))
        for name, bone, part, lo, hi in lower:
            model["cubes"].append(("low_" + name, rename.get(bone, "body"), part, place(lo), place(hi)))
        pivots = {name: pivot for name, _, pivot, _ in model["bones"]}
    elif heads:
        model["cubes"] = [c for c in model["cubes"] if c[0] != "head"]
    if heads:
        # A bird's neck cube would run down through the torso: the person has a neck.
        neckless = {"bones": base["bones"], "cubes": [c for c in base["cubes"] if c[0] != "neck"]}
        model["cubes"] += graft(neckless, heads, "head", anchor, k)

    tail_cubes = [c for c in base["cubes"] if c[1] in subtree_of(base, "tail")] if "tail" in base_bones else []
    if tail_cubes and not legless:
        lw, ll = params["leg"]
        bd = params["body"][2]
        tail_pivot = [0, ll + 2, bd / 2]
        model["bones"].append(("tail", "body", tail_pivot, [45, 0, 0]))
        pivots["tail"] = tail_pivot
        # The tail unrotated, from its root, at the head's scale (but never huge).
        _, bp, _ = base_bones["tail"]
        flat = {"bones": [(n, p, pv, [0, 0, 0] if n == "tail" else r) for n, p, pv, r in base["bones"]],
                "cubes": base["cubes"]}
        model["cubes"] += graft(flat, subtree_of(flat, "tail"), "tail", bp, min(k, 1.5))
        # Droop it (at most 45°) only as far as keeps a long tail off the ground.
        for droop in range(45, -50, -5):
            model["bones"][-1] = ("tail", "body", tail_pivot, [droop, 0, 0])
            if min(p_[1] for p_ in rest_points(model, {"tail"})) >= 0.5:
                break

    # The source's own cubes (a turtle person's shell), relative to the person's bones.
    pivots = {name: pivot for name, _, pivot, _ in model["bones"]}
    for i, c in enumerate(src.get("cubes", [])):
        o = pivots[c["bone"]]
        model["cubes"].append((c.get("name", f"{c['bone']}_{i}"), c["bone"], c["part"],
                               [o[k] + c["from"][k] for k in range(3)], [o[k] + c["to"][k] for k in range(3)]))
    # Ground contact (lie, death) measured without the tail: it lies flat along the
    # legs in those clips rather than hanging as it does standing.
    model["clips"] = t.clips(params, {"bones": model["bones"],
                                      "cubes": [c for c in model["cubes"] if c[1] != "tail"]})
    has = {b[0] for b in model["bones"]}
    for c in model["clips"].values():
        c["tracks"] = {b: ch for b, ch in c["tracks"].items() if b in has}
    if legless:
        model["clips"]["sit"]["tracks"].pop("body", None)
        # Lying down is the torso sinking back onto its own coils (turning the
        # whole figure about the hips would stand the tail up in the air).
        L = 1.2
        model["clips"]["death"] = clip("hold", L, {
            "torso": {"rotation": at(L, (0, [0, 0, 0]), (0.5, [95, 0, 10]), (0.65, [82, 0, 8]), (1, [85, 0, 10]))},
            "head": {"rotation": at(L, (0, [0, 0, 0]), (1, [-10, 30, 0]))},
            "arm_l": {"rotation": at(L, (0, [0, 0, 0]), (1, [-20, 0, -60]))},
            "arm_r": {"rotation": at(L, (0, [0, 0, 0]), (1, [-20, 0, 60]))},
        })
        L = 4.0
        model["clips"]["lie"] = clip("loop", L, {
            "torso": {"rotation": osc(L, [2, 0, 0], base=(80, 0, 0))},
            "arm_l": {"rotation": hold([10, 0, -10], L)},
            "arm_r": {"rotation": hold([10, 0, 10], L)},
        })
        # The lower body's own gait, re-timed to the person's clip.
        for name, sources in (("walk", ("walk", "swim", "move")), ("run", ("run", "swim", "move")),
                              ("idle", ("idle", "swim_idle", "move")), ("carry", ("walk", "swim", "move"))):
            c = model["clips"][name]
            b = next(base["clips"][x] for x in sources if x in base["clips"])
            f = c["length"] / b["length"]
            for bone, chans in b["tracks"].items():
                if bone in rename:
                    c["tracks"][rename[bone]] = {ch: [(t_ * f, v) for t_, v in keys] for ch, keys in chans.items()}
            if b["stride"]:
                c["stride"] = b["stride"] * s_low
    if "tail" in pivots:
        # Lying on its back, a drooping tail would point into the ground: swing it
        # down along the legs (the body's -y) for lie and death.
        droop = next(r for n, _, _, r in model["bones"] if n == "tail")[0]
        along = [90 - droop, 0, 0]
        if not legless:
            lie = model["clips"]["lie"]
            lie["tracks"]["tail"] = {"rotation": hold(along, lie["length"])}
            death = model["clips"]["death"]
            death["tracks"]["tail"] = {"rotation": at(death["length"], (0, [0, 0, 0]), (0.5, along), (1, along))}
        for name in ("idle", "walk", "run", "carry"):
            c = model["clips"][name]
            T = c["length"]
            c["tracks"]["tail"] = {"rotation": [(0, [0, 12, 0]), (T / 2, [0, -12, 0]), (T, [0, 12, 0])]}
    model["required"] = t.required(params)
    # The animal's own extras (its swim, alert, roar…), less riding, plus the source's.
    extras = [e for e in base_src.get("extras", []) if e != "ridden"] + src.get("extras", [])
    model["clips"].update(extra_clips(extras, model, model["clips"]))
    model["required"] += [c for c in extras if c not in model["required"]]
    # A legless person keeps the animal's whole body below the waist.
    budget = {"cubes": 32, "bones": 18} if legless else {}
    model["budget"] = merge(merge(t.budget, budget), src.get("budget", {}))

    palette = dict(base_src["palette"])
    body_part = next((c[2] for c in base["cubes"] if c[0] == "body"),
                     next(c[2] for c in base["cubes"] if c[1] not in heads))
    leg_part = next((c[2] for c in base["cubes"] if c[1].startswith("leg")), body_part)
    palette.setdefault("torso", palette[body_part])
    palette.setdefault("head", palette[body_part])
    palette.setdefault("arms", palette[body_part])
    palette.setdefault("hands", palette.get(leg_part, palette[body_part]))
    palette.setdefault("legs", palette.get(leg_part, palette[body_part]))
    palette.setdefault("feet", palette["legs"])
    palette.update(src.get("palette", {}))
    names = {c[0] for c in model["cubes"]}
    out = {"name": src["name"], "status": src.get("status", "draft"), "template": "biped",
           "palette": palette, "decals": scale_decals(base_src.get("decals", []), k, names, grow=min(k, 2)) + src.get("decals", [])}
    size = src.get("size", PERSON_SIZE)
    s = min(1.5, max(0.7, (size / PERSON_SIZE) ** (1 / 3)))
    return out, grounded(scale_model(model, s) if abs(s - 1) > 0.02 else model)


def grounded(model):
    """`model` moved up or down so its rest pose stands exactly on y = 0
    (scaling rounds to half units; a grafted part may hang a little low)."""
    dy = -min(p[1] for p in rest_points(model))
    if abs(dy) < 1e-6:
        return model
    out = dict(model)
    out["bones"] = [(n, parent, [pv[0], pv[1] + dy, pv[2]], r) for n, parent, pv, r in model["bones"]]
    out["cubes"] = [(n, b, part, [lo[0], lo[1] + dy, lo[2]], [hi[0], hi[1] + dy, hi[2]])
                    for n, b, part, lo, hi in model["cubes"]]
    return out


def subtree_of(model, root):
    names = {root}
    grew = True
    while grew:
        grew = False
        for name, parent, *_ in model["bones"]:
            if parent in names and name not in names:
                names.add(name)
                grew = True
    return names


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


def rest_bounds(model):
    """The rest pose's box: every cube corner through its bones' rest rotations."""
    points = rest_points(model)
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
    entries = {}
    failed = []
    for df_id in ids:
        try:
            src, model = load(df_id)
            data = bbmodel(df_id, src, model)
        except Exception as e:  # report and go on: one bad source must not stop a batch
            failed.append(df_id)
            print(f"{df_id}: FAILED: {type(e).__name__}: {e}", file=sys.stderr)
            continue
        path = os.path.join(OUT, df_id + ".bbmodel")
        with open(path, "w") as f:
            json.dump(data, f, indent=1)
            f.write("\n")
        entries[df_id] = manifest_entry(df_id, src, model)
        print(f"{df_id}: {len(model['cubes'])} cubes, {len(model['bones'])} bones, "
              f"{len(model['clips'])} clips -> {os.path.relpath(path, REPO)}")
    # Several builds may run at once (batches in parallel): the manifest's
    # read-modify-write holds a lock.
    with open(MANIFEST + ".lock", "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        try:
            with open(MANIFEST) as f:
                manifest = json.load(f)
        except FileNotFoundError:
            manifest = {}
        manifest.update(entries)
        tmp = MANIFEST + ".tmp"
        with open(tmp, "w") as f:
            json.dump(dict(sorted(manifest.items())), f, indent=1)
            f.write("\n")
        os.replace(tmp, MANIFEST)
    if failed:
        sys.exit(f"{len(failed)} failed: {' '.join(failed)}")


if __name__ == "__main__":
    main(sys.argv[1:])
