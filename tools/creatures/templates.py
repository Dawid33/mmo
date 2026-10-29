"""Body templates for tools/creatures/build.py: each turns a creature's proportions
into a skeleton (bones + cubes) and then, once the source's extra bones and cubes
are on it, into the template's clips. Spec and style guide:
docs/superpowers/specs/2026-10-29-creature-asset-pipeline.md.

A model is a dict: bones [(name, parent, pivot, rest)], cubes [(name, bone, part,
from, to)] (model units, absolute, rest pose), clips {name: clip(...)}, budget and
the required clip names. Creatures face -Z with their feet on y = 0. Signs (XYZ
Euler applied X first, as the loader does): +X swings a hanging limb's far end
forward (-Z) and lifts a forward-pointing head's nose; a +Z-pointing tail goes
down; +Y turns a forward-pointing head to the creature's left (-X); +Z swings a
hanging limb towards +X (the creature's right).
"""
import math


# --- Clip helpers -------------------------------------------------------------------

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


def osc(T, amp, phase=0.0, base=(0, 0, 0), cycles=1, n=8):
    """A looping sine track: `base + amp * sin(2π·cycles·t/T + phase)`, n keys a cycle."""
    count = n * cycles
    return [(T * k / count, [b + a * math.sin(2 * math.pi * k / n + phase) for a, b in zip(amp, base)])
            for k in range(count + 1)]


def hold(v, length):
    return [(0, list(v)), (length, list(v))]


def at(length, *points):
    """Keys at fractions of a clip's length: at(L, (0, v0), (0.5, v1), (1, v2))."""
    return [(length * f, list(v)) for f, v in points]


def bounce(T, height, cycles=1):
    """A looping hop: 0 → height → 0, `cycles` times."""
    keys = []
    for c in range(cycles):
        t0 = T * c / cycles
        keys += [(t0, [0, 0, 0]), (t0 + T / cycles / 2, [0, height, 0])]
    return keys + [(T, [0, 0, 0])]


# --- Geometry helpers ------------------------------------------------------------------

def rotate(p, deg):
    """`p` rotated by Euler degrees in three.js ZYX order (X first), like the loader."""
    x, y, z = p
    a, b, c = (math.radians(d) for d in deg)
    y, z = y * math.cos(a) - z * math.sin(a), y * math.sin(a) + z * math.cos(a)
    x, z = x * math.cos(b) + z * math.sin(b), -x * math.sin(b) + z * math.cos(b)
    x, y = x * math.cos(c) - y * math.sin(c), x * math.sin(c) + y * math.cos(c)
    return [x, y, z]


def rest_points(model, bones=None):
    """Every cube corner in the rest pose, through its bones' rest rotations;
    only the cubes of `bones` if given."""
    chain = {name: (parent, pivot, rest) for name, parent, pivot, rest in model["bones"]}
    points = []
    for _, bone, _, lo, hi in model["cubes"]:
        if bones is not None and bone not in bones:
            continue
        for corner in ([x, y, z] for x in (lo[0], hi[0]) for y in (lo[1], hi[1]) for z in (lo[2], hi[2])):
            b = bone
            while b:
                parent, pivot, rest = chain[b]
                corner = [r + o for r, o in zip(rotate([c - o for c, o in zip(corner, pivot)], rest), pivot)]
                b = parent
            points.append(corner)
    return points


def lift_for(model, rot):
    """How far to raise the root so the model, turned by `rot` about the origin
    (the root's pivot), rests on the ground rather than through it."""
    return max(0.0, -min(rotate(p, rot)[1] for p in rest_points(model)))


def box(model, bones=None):
    pts = rest_points(model, bones)
    return [min(p[i] for p in pts) for i in range(3)], [max(p[i] for p in pts) for i in range(3)]


def bone_names(model, prefix):
    return [name for name, *_ in model["bones"] if name.startswith(prefix)]


# --- Quadruped ---------------------------------------------------------------------------

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
    if tl > 0:
        cubes.append(("tail", "tail", "tail", [-tw / 2, ty - tw / 2, bl / 2 - 1],
                      [tw / 2, ty + tw / 2, bl / 2 - 1 + tl]))
    return {"bones": bones, "cubes": cubes}


def quadruped_clips(p, model):
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

    at_ = p["attack"]
    L, lunge = at_["length"], at_["lunge"]
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


# --- Biped (humanoid, Minecraft style: one segment per limb) ----------------------------

BIPED_BUDGET = {"cubes": 24, "bones": 12}
BIPED_CORE = ["idle", "walk", "run", "attack", "hurt", "death", "lie", "sleep", "eat"]
# The fortress worker set: every biped gets it (civ races, animal people, the
# intelligent cavern folk; a beast that never works just never plays them).
BIPED_WORK = ["mine", "chop", "build", "craft", "carry", "pick_up", "put_down", "sit", "drink",
              "swing", "shoot", "cheer", "cower"]

BIPED_DEFAULTS = {
    "leg": [4, 9],               # thickness, hip to sole
    "foot": 2,                   # boot height at the bottom of the leg (0: no boot cube)
    "leg_inset": 1,              # legs moved in from the torso's sides
    "body": [10, 10, 6],         # torso width, height, depth
    "arm": [3, 10],              # thickness, shoulder to fingertip
    "hand": 3,                   # hand length at the end of the arm (0: no hand cube)
    "head": [8, 8, 7],           # width, height, depth
    "walk": {"cycle": 0.9, "swing": 28, "bob": 0.4},
    "run": {"cycle": 0.55, "swing": 50, "bob": 1.0, "lean": 12},
    "idle": {"length": 4.0, "look": 20, "breath": 0.2},
}


def biped(p):
    lw, ll = p["leg"]
    bw, bh, bd = p["body"]
    aw, al = p["arm"]
    hw, hh, hd = p["head"]
    top = ll + bh
    bones = [
        ("root", None, [0, 0, 0], [0, 0, 0]),
        ("body", "root", [0, ll, 0], [0, 0, 0]),       # the hips: moves the whole figure
        ("torso", "body", [0, ll, 0], [0, 0, 0]),      # bends at the waist
        ("head", "torso", [0, top, 0], [0, 0, 0]),     # the neck
    ]
    cubes = [
        ("torso", "torso", "torso", [-bw / 2, ll, -bd / 2], [bw / 2, top, bd / 2]),
        ("head", "head", "head", [-hw / 2, top, -hd / 2], [hw / 2, top + hh, hd / 2]),
    ]
    hand = p["hand"]
    for side, s in (("l", -1), ("r", 1)):
        x = s * (bw / 2 + aw / 2)
        name = f"arm_{side}"
        bones.append((name, "torso", [x, top - aw / 2, 0], [0, 0, 0]))
        cubes.append((name, name, "arms", [x - aw / 2, top - al + hand, -aw / 2], [x + aw / 2, top, aw / 2]))
        if hand:
            cubes.append((f"hand_{side}", name, "hands", [x - aw / 2, top - al, -aw / 2],
                          [x + aw / 2, top - al + hand, aw / 2]))
    foot = p["foot"]
    lx = bw / 2 - lw / 2 - p["leg_inset"]
    for side, s in (("l", -1), ("r", 1)):
        x = s * lx
        name = f"leg_{side}"
        bones.append((name, "body", [x, ll, 0], [0, 0, 0]))
        cubes.append((name, name, "legs", [x - lw / 2, foot, -lw / 2], [x + lw / 2, ll, lw / 2]))
        if foot:
            # A boot, a unit longer at the toe so the facing reads from above.
            cubes.append((f"foot_{side}", name, "feet", [x - lw / 2, 0, -lw / 2 - 1], [x + lw / 2, foot, lw / 2]))
    return {"bones": bones, "cubes": cubes}


def mirror(v):
    """A left-side rotation for the right side: Y and Z flip."""
    return [v[0], -v[1], -v[2]]


def arms(left, right=None):
    """Tracks for both arms from the left arm's keys (mirrored unless `right` given)."""
    right = right if right is not None else [(t, mirror(v)) for t, v in left]
    return {"arm_l": {"rotation": left}, "arm_r": {"rotation": right}}


def biped_clips(p, model):
    lw, ll = p["leg"]
    clips = {}
    back = lift_for(model, [90, 0, 0])
    side = lift_for(model, [0, 0, 90])

    def legs_walk(T, A):
        a = [(0, [A, 0, 0]), (T / 2, [-A, 0, 0]), (T, [A, 0, 0])]
        b = [(0, [-A, 0, 0]), (T / 2, [A, 0, 0]), (T, [-A, 0, 0])]
        return {"leg_l": {"rotation": a}, "leg_r": {"rotation": b}}

    def bob(T, h):
        return [(0, [0, 0, 0]), (T / 4, [0, h, 0]), (T / 2, [0, 0, 0]), (3 * T / 4, [0, h, 0]), (T, [0, 0, 0])]

    w = p["walk"]
    T, A = w["cycle"], w["swing"]
    tracks = legs_walk(T, A)
    tracks.update(arms([(0, [-0.7 * A, 0, 0]), (T / 2, [0.7 * A, 0, 0]), (T, [-0.7 * A, 0, 0])],
                       [(0, [0.7 * A, 0, 0]), (T / 2, [-0.7 * A, 0, 0]), (T, [0.7 * A, 0, 0])]))
    tracks["body"] = {"position": bob(T, w["bob"])}
    tracks["head"] = {"rotation": osc(T, [2, 0, 0], cycles=2, n=4)}
    clips["walk"] = clip("loop", T, tracks, stride=2 * ll * math.sin(math.radians(A)))

    r = p["run"]
    T, A = r["cycle"], r["swing"]
    tracks = legs_walk(T, A)
    tracks.update(arms([(0, [-A, 0, -8]), (T / 2, [A, 0, -8]), (T, [-A, 0, -8])],
                       [(0, [A, 0, 8]), (T / 2, [-A, 0, 8]), (T, [A, 0, 8])]))
    tracks["body"] = {"position": bob(T, r["bob"])}
    tracks["torso"] = {"rotation": hold([-r["lean"], 0, 0], T)}
    tracks["head"] = {"rotation": hold([r["lean"] * 0.6, 0, 0], T)}
    clips["run"] = clip("loop", T, tracks, stride=2 * ll * math.sin(math.radians(A)) * 1.5)

    i = p["idle"]
    L = i["length"]
    clips["idle"] = clip("loop", L, {
        "torso": {"position": osc(L, [0, i["breath"], 0], cycles=2)},
        "head": {"rotation": at(L, (0, [0, 0, 0]), (0.2, [0, i["look"], 0]), (0.35, [0, i["look"], 0]),
                                (0.5, [0, 0, 0]), (0.7, [-5, -i["look"], 0]), (0.85, [-5, -i["look"], 0]),
                                (1, [0, 0, 0]))},
        **arms(osc(L, [0, 0, -3], cycles=2), osc(L, [0, 0, 3], cycles=2)),
    })

    L = 0.5
    clips["attack"] = clip("once", L, {
        "arm_r": {"rotation": at(L, (0, [0, 0, 0]), (0.3, [-30, 0, 0]), (0.5, [95, 10, 0]), (0.65, [95, 10, 0]),
                                 (1, [0, 0, 0]))},
        "arm_l": {"rotation": at(L, (0, [0, 0, 0]), (0.5, [30, 0, 0]), (1, [0, 0, 0]))},
        "torso": {"rotation": at(L, (0, [0, 0, 0]), (0.3, [0, -20, 0]), (0.5, [-8, 25, 0]), (1, [0, 0, 0]))},
        "body": {"position": at(L, (0, [0, 0, 0]), (0.5, [0, 0, -1.5]), (1, [0, 0, 0]))},
    })

    L = 0.35
    clips["hurt"] = clip("once", L, {
        "torso": {"rotation": at(L, (0, [0, 0, 0]), (0.3, [15, 0, 6]), (1, [0, 0, 0]))},
        "head": {"rotation": at(L, (0, [0, 0, 0]), (0.3, [15, 0, 0]), (1, [0, 0, 0]))},
        "body": {"position": at(L, (0, [0, 0, 0]), (0.3, [0, 0, 1.5]), (1, [0, 0, 0]))},
        **arms(at(L, (0, [0, 0, 0]), (0.3, [-15, 0, -25]), (1, [0, 0, 0]))),
    })

    # Falls on its back: a quarter turn about the feet, lifted so the back rests
    # on the ground.
    L = 1.2
    clips["death"] = clip("hold", L, {
        "root": {"rotation": at(L, (0, [0, 0, 0]), (0.5, [95, 0, 0]), (0.65, [87, 0, 0]), (1, [90, 0, 0])),
                 "position": at(L, (0, [0, 0, 0]), (0.5, [0, back, 0]), (1, [0, back, 0]))},
        "head": {"rotation": at(L, (0, [0, 0, 0]), (0.5, [20, 0, 0]), (1, [5, 30, 0]))},
        **arms(at(L, (0, [0, 0, 0]), (0.5, [-40, 0, -50]), (1, [-20, 0, -60]))),
        "leg_l": {"rotation": at(L, (0, [0, 0, 0]), (1, [0, 0, -10]))},
        "leg_r": {"rotation": at(L, (0, [0, 0, 0]), (1, [0, 0, 15]))},
    })

    L = 4.0
    clips["lie"] = clip("loop", L, {
        "root": {"rotation": hold([90, 0, 0], L), "position": hold([0, back, 0], L)},
        "torso": {"scale": osc(L, [0, 0, 0.04], base=(1, 1, 1), cycles=1)},
        **arms(hold([0, 0, -8], L)),
    })
    L = 6.0
    clips["sleep"] = clip("loop", L, {
        "root": {"rotation": hold([0, 0, 90], L), "position": hold([0, side, 0], L)},
        "torso": {"scale": osc(L, [0.04, 0, 0], base=(1, 1, 1))},
        "head": {"rotation": hold([-10, 0, 0], L)},
        **arms(hold([35, 0, 0], L)),
        "leg_l": {"rotation": hold([30, 0, 0], L)},
        "leg_r": {"rotation": hold([40, 0, 0], L)},
    })

    L = 1.6
    clips["eat"] = clip("loop", L, {
        "arm_r": {"rotation": osc(L, [10, 0, 0], base=(115, 30, 0), cycles=2)},
        "arm_l": {"rotation": hold([30, 0, 0], L)},
        "head": {"rotation": osc(L, [4, 0, 0], base=(-5, 0, 0), cycles=4, n=4)},
    })

    T = 1.0
    clips["mine"] = clip("loop", T, {
        **arms(at(T, (0, [60, -15, 0]), (0.4, [170, -15, 0]), (0.6, [45, -15, 0]), (1, [60, -15, 0]))),
        "torso": {"rotation": at(T, (0, [-10, 0, 0]), (0.4, [6, 0, 0]), (0.6, [-22, 0, 0]), (1, [-10, 0, 0]))},
        "head": {"rotation": hold([-10, 0, 0], T)},
    })
    clips["chop"] = clip("loop", T, {
        **arms(hold([80, -20, 0], T)),
        "torso": {"rotation": at(T, (0, [-8, -50, 0]), (0.35, [-12, 35, 0]), (0.5, [-12, 35, 0]),
                                 (1, [-8, -50, 0]))},
    })
    T = 0.7
    clips["build"] = clip("loop", T, {
        "arm_r": {"rotation": at(T, (0, [60, 10, 0]), (0.35, [140, 10, 0]), (0.5, [40, 10, 0]), (1, [60, 10, 0]))},
        "arm_l": {"rotation": hold([50, -20, 0], T)},
        "torso": {"rotation": hold([-20, 0, 0], T)},
        "head": {"rotation": hold([-15, 0, 0], T)},
    })
    T = 1.2
    clips["craft"] = clip("loop", T, {
        "arm_l": {"rotation": osc(T, [10, 0, 0], base=(45, -20, 0), cycles=2)},
        "arm_r": {"rotation": osc(T, [10, 0, 0], phase=math.pi, base=(50, 20, 0), cycles=2)},
        "torso": {"rotation": hold([-10, 0, 0], T)},
        "head": {"rotation": osc(T, [0, 8, 0], base=(-20, 0, 0))},
    })

    w = p["walk"]
    T, A = w["cycle"], w["swing"]
    tracks = legs_walk(T, A * 0.8)
    tracks.update(arms(osc(T, [3, 0, 0], base=(75, -15, 0), cycles=2, n=4)))
    tracks["body"] = {"position": bob(T, w["bob"])}
    tracks["torso"] = {"rotation": hold([5, 0, 0], T)}
    clips["carry"] = clip("loop", T, tracks, stride=2 * ll * math.sin(math.radians(A * 0.8)))

    # Ends holding the thing in front (carry's pose); put_down starts there.
    L = 1.2
    bend = (0, [-55, 0, 0])
    clips["pick_up"] = clip("once", L, {
        "torso": {"rotation": at(L, (0, [0, 0, 0]), (0.4, bend[1]), (0.6, bend[1]), (1, [5, 0, 0]))},
        **arms(at(L, (0, [0, 0, 0]), (0.4, [60, -10, 0]), (0.6, [60, -15, 0]), (1, [75, -15, 0]))),
        "head": {"rotation": at(L, (0, [0, 0, 0]), (0.4, [-20, 0, 0]), (1, [0, 0, 0]))},
    })
    clips["put_down"] = clip("once", L, {
        "torso": {"rotation": at(L, (0, [5, 0, 0]), (0.4, bend[1]), (0.6, bend[1]), (1, [0, 0, 0]))},
        **arms(at(L, (0, [75, -15, 0]), (0.4, [60, -15, 0]), (0.6, [55, -5, 0]), (1, [0, 0, 0]))),
        "head": {"rotation": at(L, (0, [0, 0, 0]), (0.4, [-20, 0, 0]), (1, [0, 0, 0]))},
    })

    # On the ground, legs out in front: the hips drop until the level legs rest on it.
    L = 0.6
    drop = lw / 2 - ll
    clips["sit"] = clip("hold", L, {
        "body": {"position": at(L, (0, [0, 0, 0]), (1, [0, drop, 0]))},
        "leg_l": {"rotation": at(L, (0, [0, 0, 0]), (1, [88, 0, -4]))},
        "leg_r": {"rotation": at(L, (0, [0, 0, 0]), (1, [88, 0, 4]))},
        "torso": {"rotation": at(L, (0, [0, 0, 0]), (1, [6, 0, 0]))},
        **arms(at(L, (0, [0, 0, 0]), (1, [30, 0, -5]))),
    })

    L = 1.6
    clips["drink"] = clip("loop", L, {
        "arm_r": {"rotation": osc(L, [6, 0, 0], base=(140, 30, 0))},
        "arm_l": {"rotation": hold([10, 0, -5], L)},
        "head": {"rotation": osc(L, [4, 0, 0], base=(22, 0, 0))},
        "torso": {"rotation": hold([8, 0, 0], L)},
    })

    L = 0.8
    clips["swing"] = clip("once", L, {
        "arm_r": {"rotation": at(L, (0, [0, 0, 0]), (0.35, [170, -10, 0]), (0.55, [30, 20, 0]),
                                 (0.7, [30, 20, 0]), (1, [0, 0, 0]))},
        "arm_l": {"rotation": at(L, (0, [0, 0, 0]), (0.35, [20, 0, -10]), (1, [0, 0, 0]))},
        "torso": {"rotation": at(L, (0, [0, 0, 0]), (0.35, [5, -25, 0]), (0.55, [-15, 25, 0]), (1, [0, 0, 0]))},
        "body": {"position": at(L, (0, [0, 0, 0]), (0.55, [0, 0, -1.5]), (1, [0, 0, 0]))},
    })

    # A bow: side-on, the left arm out at the target, the right drawing to the chin.
    L = 1.4
    clips["shoot"] = clip("once", L, {
        "torso": {"rotation": at(L, (0, [0, 0, 0]), (0.25, [0, -50, 0]), (0.85, [0, -50, 0]), (1, [0, 0, 0]))},
        "head": {"rotation": at(L, (0, [0, 0, 0]), (0.25, [0, 45, 0]), (0.85, [0, 45, 0]), (1, [0, 0, 0]))},
        "arm_l": {"rotation": at(L, (0, [0, 0, 0]), (0.25, [90, 50, 0]), (0.85, [90, 50, 0]), (1, [0, 0, 0]))},
        "arm_r": {"rotation": at(L, (0, [0, 0, 0]), (0.25, [90, 60, 0]), (0.6, [90, 95, 0]), (0.7, [90, 95, 0]),
                                 (0.75, [80, 120, 0]), (1, [0, 0, 0]))},
    })

    T = 0.8
    clips["cheer"] = clip("loop", T, {
        **arms(osc(T, [12, 0, 0], base=(165, 0, -20), cycles=2, n=4)),
        "body": {"position": bounce(T, 1.5, cycles=2)},
        "head": {"rotation": hold([12, 0, 0], T)},
    })
    T = 1.0
    clips["cower"] = clip("loop", T, {
        "torso": {"rotation": osc(T, [0, 3, 0], base=(-30, 0, 0), cycles=4, n=4)},
        "head": {"rotation": hold([-20, 0, 0], T)},
        **arms(hold([150, -40, 10], T)),
        "body": {"position": hold([0, 0, 1], T)},
    })
    return clips


# --- Wings (any template: a bird's, a dragon's, a bat's, a harpy's) -----------------------
#
# A wing's cube is modelled spread: span along -X (the left wing; the right is
# mirrored), chord behind the shoulder, one unit thick. At rest it is folded back
# along the body (rest [90, ±90, 0]: chord hanging down, span pointing back);
# the fly clips add the inverse of that fold to spread it, then flap about Z.

WING_FOLD = {"wing_l": [90, 90, 0], "wing_r": [90, -90, 0]}
WING_CLIPS = ["fly", "hover", "takeoff", "land"]


def add_wings(model, w):
    """`w`: bone (parent), at ([x, y, z] of the left shoulder, absolute), span,
    chord, part."""
    x, y, z = w["at"]
    for name, s in (("wing_l", -1), ("wing_r", 1)):
        pivot = [s * abs(x), y, z]
        model["bones"].append((name, w.get("bone", "body"), pivot, WING_FOLD[name]))
        lo = [pivot[0] - w["span"] if s < 0 else pivot[0], pivot[1] - 0.5, pivot[2]]
        hi = [pivot[0] if s < 0 else pivot[0] + w["span"], pivot[1] + 0.5, pivot[2] + w["chord"]]
        model["cubes"].append((name, name, w.get("part", "wings"), lo, hi))


def spread(flap, name):
    """A wing's rotation key: spread (fold undone), flapped `flap` degrees down."""
    f = WING_FOLD[name]
    return [-f[0], -f[1], flap if name == "wing_l" else -flap]


def wing_clips(model, w):
    lo, hi = box(model)
    # The game owns a flier's altitude: fly and hover stay where the entity is;
    # takeoff is a flapping hop off the ground, land a flare onto it.
    hop = w.get("hop", max(2, round((hi[1] - lo[1]) * 0.3)))
    T, A = w.get("cycle", 0.5), w.get("flap", 45)
    legs = bone_names(model, "leg")
    body = w.get("body", "body")
    tuck = w.get("tuck", -60)

    def flapping(T, A, n=8):
        return {name: {"rotation": [(t, spread(v[2], name)) for t, v in osc(T, [0, 0, A], n=n)]}
                for name in ("wing_l", "wing_r")}

    clips = {}
    tracks = flapping(T, A)
    tracks["root"] = {"position": osc(T, [0, 0.6, 0], phase=math.pi / 2)}
    tracks[body] = {"rotation": hold([-8, 0, 0], T)}
    for leg in legs:
        tracks[leg] = {"rotation": hold([tuck, 0, 0], T)}
    clips["fly"] = clip("loop", T, tracks, stride=(hi[2] - lo[2]) * 1.5)

    T2 = T * 0.7
    tracks = flapping(T2, A * 1.2)
    tracks["root"] = {"position": osc(T2, [0, 0.4, 0], phase=math.pi / 2)}
    tracks[body] = {"rotation": hold([w.get("hover_pitch", 25), 0, 0], T2)}
    for leg in legs:
        tracks[leg] = {"rotation": hold([-10, 0, 0], T2)}
    clips["hover"] = clip("loop", T2, tracks)

    L = 0.9
    beats = [(0, 0), (0.15, -A), (0.3, A), (0.45, -A), (0.6, A), (0.75, -A * 0.5), (1, A * 0.3)]
    up = {name: {"rotation": [(0, [0, 0, 0])] + [(L * f, spread(a, name)) for f, a in beats[1:]]}
          for name in ("wing_l", "wing_r")}
    up["root"] = {"position": at(L, (0, [0, 0, 0]), (0.2, [0, -0.5, 0]), (0.5, [0, hop, 0]), (1, [0, 0, 0]))}
    up[body] = {"rotation": at(L, (0, [0, 0, 0]), (0.2, [10, 0, 0]), (1, [-8, 0, 0]))}
    for leg in legs:
        up[leg] = {"rotation": at(L, (0, [0, 0, 0]), (0.3, [0, 0, 0]), (1, [tuck, 0, 0]))}
    clips["takeoff"] = clip("once", L, up)

    beats = [(0, A * 0.3), (0.2, -A), (0.4, A), (0.6, -A * 0.5), (0.8, 0)]
    down = {name: {"rotation": [(L * f, spread(a, name)) for f, a in beats] + [(L, [0, 0, 0])]}
            for name in ("wing_l", "wing_r")}
    down["root"] = {"position": at(L, (0, [0, 0, 0]), (0.6, [0, hop * 0.5, 0]), (0.8, [0, -0.5, 0]), (1, [0, 0, 0]))}
    down[body] = {"rotation": at(L, (0, [-8, 0, 0]), (0.6, [15, 0, 0]), (1, [0, 0, 0]))}
    for leg in legs:
        down[leg] = {"rotation": at(L, (0, [tuck, 0, 0]), (0.6, [10, 0, 0]), (1, [0, 0, 0]))}
    clips["land"] = clip("once", L, down)
    return clips


# --- Bird -------------------------------------------------------------------------------

BIRD_BUDGET = {"cubes": 20, "bones": 10}
BIRD_CORE = ["idle", "walk", "run", "attack", "hurt", "death", "lie", "sleep", "eat"]

BIRD_DEFAULTS = {
    "body": [6, 6, 8],           # width, height, length
    "leg": [1, 4],               # thickness, hip to sole
    "foot": [3, 1, 3],           # toes: width, height, length
    "head": [3, 4, 3],           # width, height, length
    "neck": 1,                   # head bottom above the body top's front
    "head_back": 2,              # head's back edge behind the body's front
    "beak": [1, 1, 2],           # width, height, length (0 length: none)
    "tail": [4, 1, 4],           # width, thickness, length
    "tail_raise": 45,
    "wing": None,                # [span, chord]; default from the body
    "flier": True,               # the fly set (fly, hover, takeoff, land)
    "brood": False,              # a brood clip, for egg layers in nest boxes
    "walk": {"cycle": 0.7, "swing": 28, "bob": 0.3, "nod": 1.0},
    "run": {"cycle": 0.4, "swing": 45, "lean": 12},
    "fly": {},                   # wing_clips overrides: cycle, flap, hop, tuck
}


def bird(p):
    bw, bh, bl = p["body"]
    lw, ll = p["leg"]
    fw, fh, fl = p["foot"]
    hw, hh, hl = p["head"]
    top = ll + bh
    hz = -bl / 2 + p["head_back"]
    neck_y = top - 1
    bones = [
        ("root", None, [0, 0, 0], [0, 0, 0]),
        ("body", "root", [0, ll + bh / 2, 0], [0, 0, 0]),
        ("head", "body", [0, neck_y, hz - hl / 2], [0, 0, 0]),
        ("tail", "body", [0, top - 1, bl / 2 - 1], [-p["tail_raise"], 0, 0]),
    ]
    cubes = [("body", "body", "body", [-bw / 2, ll, -bl / 2], [bw / 2, top, bl / 2])]
    hy = top + p["neck"]
    cubes.append(("head", "head", "head", [-hw / 2, hy - 1, hz - hl], [hw / 2, hy - 1 + hh, hz]))
    if p["neck"] > 1:
        nw = max(1, hw - 1)
        cubes.append(("neck", "head", "head", [-nw / 2, neck_y, hz - hl + 0.5], [nw / 2, hy, hz - 0.5]))
    kw, kh, kl = p["beak"]
    if kl:
        ky = hy - 1 + hh * 0.35
        cubes.append(("beak", "head", "beak", [-kw / 2, ky, hz - hl - kl], [kw / 2, ky + kh, hz - hl]))
    tw, tt, tl = p["tail"]
    if tl:
        cubes.append(("tail", "tail", "tail", [-tw / 2, top - 1 - tt / 2, bl / 2 - 1],
                      [tw / 2, top - 1 + tt / 2, bl / 2 - 1 + tl]))
    lx = max(lw, round(bw / 4))
    for side, s in (("l", -1), ("r", 1)):
        x = s * lx
        name = f"leg_{side}"
        bones.append((name, "body", [x, ll, 0], [0, 0, 0]))
        cubes.append((name, name, "legs", [x - lw / 2, 0, -lw / 2], [x + lw / 2, ll + 1, lw / 2]))
        if fl:
            cubes.append((f"foot_{side}", name, "legs", [x - fw / 2, 0, -fl + lw / 2], [x + fw / 2, fh, lw / 2]))
    span, chord = p["wing"] or [bl - 1, bh - 1]
    model = {"bones": bones, "cubes": cubes}
    add_wings(model, {"at": [-(bw / 2 + 0.5), top - 0.5, -bl / 2 + 1], "span": span, "chord": chord,
                      "part": "wings"})
    return model


def bird_required(p):
    return BIRD_CORE + (WING_CLIPS if p["flier"] else []) + (["brood"] if p["brood"] else [])


def bird_clips(p, model):
    bw, bh, bl = p["body"]
    lw, ll = p["leg"]
    side = lift_for(model, [0, 0, 90])
    clips = {}

    def legs(T, A):
        return {"leg_l": {"rotation": [(0, [A, 0, 0]), (T / 2, [-A, 0, 0]), (T, [A, 0, 0])]},
                "leg_r": {"rotation": [(0, [-A, 0, 0]), (T / 2, [A, 0, 0]), (T, [-A, 0, 0])]}}

    def out(deg):
        """Folded wings held a little away from the body."""
        return {"wing_l": {"rotation": [(0, [0, 0, -deg])]}, "wing_r": {"rotation": [(0, [0, 0, deg])]}}

    w = p["walk"]
    T, A = w["cycle"], w["swing"]
    tracks = legs(T, A)
    # The head holds still, then catches up: a nod forward each step.
    tracks["head"] = {"position": osc(T, [0, 0, w["nod"]], base=(0, 0, -w["nod"] / 2), cycles=2, n=4)}
    tracks["body"] = {"position": osc(T, [0, w["bob"], 0], phase=-math.pi / 2, base=(0, w["bob"], 0), cycles=2, n=4)}
    tracks["tail"] = {"rotation": osc(T, [0, 6, 0])}
    clips["walk"] = clip("loop", T, tracks, stride=2 * ll * math.sin(math.radians(A)))

    r = p["run"]
    T, A = r["cycle"], r["swing"]
    tracks = legs(T, A)
    tracks["body"] = {"rotation": hold([-r["lean"], 0, 0], T),
                      "position": osc(T, [0, 0.4, 0], base=(0, 0.4, 0), cycles=2, n=4)}
    tracks["head"] = {"rotation": hold([r["lean"], 0, 0], T)}
    for name, track in out(18).items():
        track["rotation"].append((T, track["rotation"][0][1]))
        tracks[name] = track
    clips["run"] = clip("loop", T, tracks, stride=2 * ll * math.sin(math.radians(A)) * 1.4)

    L = 4.0
    clips["idle"] = clip("loop", L, {
        "body": {"position": osc(L, [0, 0.15, 0], cycles=2)},
        "head": {"rotation": at(L, (0, [0, 0, 0]), (0.15, [0, 35, 0]), (0.3, [0, 35, 0]), (0.4, [-10, 0, 0]),
                                (0.55, [-10, -35, 0]), (0.75, [0, -35, 0]), (1, [0, 0, 0]))},
        "tail": {"rotation": osc(L, [0, 5, 0], cycles=3)},
    })

    L = 0.5
    clips["attack"] = clip("once", L, {
        "head": {"rotation": at(L, (0, [0, 0, 0]), (0.3, [20, 0, 0]), (0.5, [-45, 0, 0]), (1, [0, 0, 0])),
                 "position": at(L, (0, [0, 0, 0]), (0.5, [0, -1, -2]), (1, [0, 0, 0]))},
        "body": {"rotation": at(L, (0, [0, 0, 0]), (0.5, [-15, 0, 0]), (1, [0, 0, 0]))},
        "wing_l": {"rotation": at(L, (0, [0, 0, 0]), (0.4, [0, 0, -45]), (1, [0, 0, 0]))},
        "wing_r": {"rotation": at(L, (0, [0, 0, 0]), (0.4, [0, 0, 45]), (1, [0, 0, 0]))},
    })
    L = 0.35
    clips["hurt"] = clip("once", L, {
        "body": {"position": at(L, (0, [0, 0, 0]), (0.3, [0, 0.5, 1.5]), (1, [0, 0, 0])),
                 "rotation": at(L, (0, [0, 0, 0]), (0.3, [8, 0, 8]), (1, [0, 0, 0]))},
        "head": {"rotation": at(L, (0, [0, 0, 0]), (0.3, [20, 0, 0]), (1, [0, 0, 0]))},
        "wing_l": {"rotation": at(L, (0, [0, 0, 0]), (0.3, [0, 0, -35]), (1, [0, 0, 0]))},
        "wing_r": {"rotation": at(L, (0, [0, 0, 0]), (0.3, [0, 0, 35]), (1, [0, 0, 0]))},
    })
    L = 1.0
    clips["death"] = clip("hold", L, {
        "root": {"rotation": at(L, (0, [0, 0, 0]), (0.6, [0, 0, 93]), (0.75, [0, 0, 88]), (1, [0, 0, 90])),
                 "position": at(L, (0, [0, 0, 0]), (0.6, [0, side, 0]), (1, [0, side, 0]))},
        "head": {"rotation": at(L, (0, [0, 0, 0]), (1, [-30, 0, 0]))},
        "leg_l": {"rotation": at(L, (0, [0, 0, 0]), (1, [-30, 0, 0]))},
        "leg_r": {"rotation": at(L, (0, [0, 0, 0]), (1, [-20, 0, 0]))},
    })

    # Sitting on the ground: legs folded forward under the belly.
    drop = -(ll - lw / 2)
    for name, L, head in (("lie", 4.0, [0, 0, 0]), ("sleep", 6.0, [-20, 150, 0])):
        clips[name] = clip("loop", L, {
            "body": {"position": osc(L, [0, 0.2, 0], base=(0, drop, 0))},
            "leg_l": {"rotation": hold([85, 0, 0], L)},
            "leg_r": {"rotation": hold([85, 0, 0], L)},
            "head": {"rotation": hold(head, L), "position": hold([0, -1 if head[1] else 0, 0], L)},
        })
    if p["brood"]:
        L = 3.0
        clips["brood"] = clip("loop", L, {
            "body": {"position": osc(L, [0, 0.3, 0], base=(0, drop + 0.5, 0)),
                     "scale": hold([1.12, 1.05, 1.06], L),
                     "rotation": osc(L, [0, 5, 0], cycles=2)},
            "leg_l": {"rotation": hold([85, 0, 0], L)},
            "leg_r": {"rotation": hold([85, 0, 0], L)},
            "wing_l": {"rotation": hold([0, 0, -15], L)},
            "wing_r": {"rotation": hold([0, 0, 15], L)},
            "head": {"rotation": osc(L, [0, 20, 0], base=(10, 0, 0))},
        })

    L = 1.2
    clips["eat"] = clip("loop", L, {
        "body": {"rotation": hold([-15, 0, 0], L)},
        "head": {"rotation": osc(L, [15, 0, 0], base=(-55, 0, 0), cycles=3, n=4),
                 "position": hold([0, -1, -1], L)},
        "tail": {"rotation": hold([10, 0, 0], L)},
    })

    if p["flier"]:
        clips.update(wing_clips(model, p["fly"]))
    return clips


# --- Fish (and whales: `flukes` swims with vertical strokes) ------------------------------

FISH_BUDGET = {"cubes": 14, "bones": 8}
FISH_CLIPS = ["swim_idle", "swim", "attack", "hurt", "death", "flop"]

FISH_DEFAULTS = {
    "body": [4, 6, 8],           # width, height, length
    "head": [4, 5, 3],           # in front of the body
    "tail": [2, 4, 3],           # the tail stalk behind it
    "fin": [1, 6, 3],            # caudal fin: thickness, height (width for flukes), length
    "pectoral": [3, 2],          # side fins: span, length (0 span: none)
    "dorsal": [1, 2, 4],         # thickness, height, length on the body top (0 height: none)
    "flukes": False,             # a whale's tail: horizontal fin, vertical strokes
    "swim": {"cycle": 0.8, "amp": 18},
}


def fish(p):
    bw, bh, bl = p["body"]
    hw, hh, hl = p["head"]
    tw, th, tl = p["tail"]
    ft, fh, fl = p["fin"]
    cy = bh / 2
    bones = [
        ("root", None, [0, 0, 0], [0, 0, 0]),
        ("body", "root", [0, cy, 0], [0, 0, 0]),
        ("head", "body", [0, cy, -bl / 2], [0, 0, 0]),
        ("tail", "body", [0, cy, bl / 2], [0, 0, 0]),
        ("tail_fin", "tail", [0, cy, bl / 2 + tl], [0, 0, 0]),
    ]
    cubes = [
        ("body", "body", "body", [-bw / 2, 0, -bl / 2], [bw / 2, bh, bl / 2]),
        ("head", "head", "head", [-hw / 2, cy - hh / 2, -bl / 2 - hl], [hw / 2, cy + hh / 2, -bl / 2]),
        ("tail", "tail", "body", [-tw / 2, cy - th / 2, bl / 2], [tw / 2, cy + th / 2, bl / 2 + tl]),
    ]
    z0 = bl / 2 + tl
    if p["flukes"]:
        cubes.append(("tail_fin", "tail_fin", "fins", [-fh / 2, cy - ft / 2, z0], [fh / 2, cy + ft / 2, z0 + fl]))
    else:
        cubes.append(("tail_fin", "tail_fin", "fins", [-ft / 2, cy - fh / 2, z0], [ft / 2, cy + fh / 2, z0 + fl]))
    ps, pl = p["pectoral"]
    if ps:
        for name, s in (("fin_l", -1), ("fin_r", 1)):
            pivot = [s * bw / 2, max(0.5, cy - bh / 4), -bl / 2 + 1]
            bones.append((name, "body", pivot, [0, 0, s * -25]))
            lo = [pivot[0] - ps if s < 0 else pivot[0], pivot[1] - 0.5, pivot[2]]
            hi = [pivot[0] if s < 0 else pivot[0] + ps, pivot[1] + 0.5, pivot[2] + pl]
            cubes.append((name, name, "fins", lo, hi))
    dt, dh, dl = p["dorsal"]
    if dh:
        cubes.append(("dorsal", "body", "fins", [-dt / 2, bh, -dl / 2], [dt / 2, bh + dh, dl / 2]))
    return {"bones": bones, "cubes": cubes}


def fish_clips(p, model):
    bw, bh, bl = p["body"]
    axis = 0 if p["flukes"] else 1   # whales stroke up and down, fish side to side

    def bend(a):
        v = [0, 0, 0]
        v[axis] = a
        return v

    has_fins = any(b[0] == "fin_l" for b in model["bones"])
    belly = lift_for(model, [0, 0, 180])
    side = lift_for(model, [0, 0, 90])
    clips = {}

    s = p["swim"]
    T, A = s["cycle"], s["amp"]
    tracks = {
        "body": {"rotation": osc(T, bend(A * 0.25))},
        "head": {"rotation": osc(T, bend(-A * 0.2))},
        "tail": {"rotation": osc(T, bend(A), phase=-math.pi / 2)},
        "tail_fin": {"rotation": osc(T, bend(A * 1.2), phase=-math.pi)},
    }
    if has_fins:
        tracks["fin_l"] = {"rotation": osc(T, [0, 10, 0], base=(0, 30, 0))}
        tracks["fin_r"] = {"rotation": osc(T, [0, -10, 0], base=(0, -30, 0))}
    total = box(model)[1][2] - box(model)[0][2]
    clips["swim"] = clip("loop", T, tracks, stride=total * 0.7)

    L = 3.0
    tracks = {
        "body": {"position": osc(L, [0, 0.3, 0])},
        "tail": {"rotation": osc(L, bend(8), phase=-math.pi / 2, cycles=2)},
        "tail_fin": {"rotation": osc(L, bend(12), phase=-math.pi, cycles=2)},
    }
    if has_fins:
        tracks["fin_l"] = {"rotation": osc(L, [0, 0, 15], cycles=3)}
        tracks["fin_r"] = {"rotation": osc(L, [0, 0, -15], cycles=3)}
    clips["swim_idle"] = clip("loop", L, tracks)

    L = 0.5
    clips["attack"] = clip("once", L, {
        "body": {"position": at(L, (0, [0, 0, 0]), (0.4, [0, 0, -0.5 * bl]), (1, [0, 0, 0]))},
        "head": {"rotation": at(L, (0, [0, 0, 0]), (0.3, [15, 0, 0]), (0.45, [-10, 0, 0]), (1, [0, 0, 0]))},
        "tail": {"rotation": at(L, (0, [0, 0, 0]), (0.2, bend(30)), (0.4, bend(-30)), (1, [0, 0, 0]))},
    })
    L = 0.35
    clips["hurt"] = clip("once", L, {
        "body": {"rotation": at(L, (0, [0, 0, 0]), (0.3, [0, 0, 25]), (1, [0, 0, 0])),
                 "position": at(L, (0, [0, 0, 0]), (0.3, [1, 0.5, 0]), (1, [0, 0, 0]))},
        "tail": {"rotation": at(L, (0, [0, 0, 0]), (0.3, bend(25)), (1, [0, 0, 0]))},
    })
    # Belly up, floating.
    L = 1.0
    clips["death"] = clip("hold", L, {
        "root": {"rotation": at(L, (0, [0, 0, 0]), (0.6, [0, 0, 170]), (1, [0, 0, 180])),
                 "position": at(L, (0, [0, 0, 0]), (0.6, [0, belly, 0]), (1, [0, belly, 0]))},
        "tail": {"rotation": at(L, (0, [0, 0, 0]), (1, bend(10)))},
    })
    # Stranded on its side, flipping head and tail off the ground.
    T = 0.6
    clips["flop"] = clip("loop", T, {
        "root": {"rotation": hold([0, 0, 90], T),
                 "position": [(t, [0, side + v[1], 0]) for t, v in bounce(T, max(1.0, bh * 0.3))]},
        "tail": {"rotation": osc(T, [0, 35, 0])},
        "tail_fin": {"rotation": osc(T, [0, 20, 0], phase=-math.pi / 2)},
        "head": {"rotation": osc(T, [0, -15, 0])},
    })
    return clips


# --- Serpent (a tail-rooted chain: rotating a segment moves everything in front of it) ----

SERPENT_BUDGET = {"cubes": 16, "bones": 12}
SERPENT_CLIPS = ["idle", "walk", "run", "attack", "hurt", "death", "lie", "sleep"]

SERPENT_DEFAULTS = {
    "segments": 5,
    "seg": [3, 2, 6],            # width, height, length of the first segment
    "taper": 0.5,                # the last segment's width and height as a fraction of the first
    "head": [4, 2, 4],
    "walk": {"cycle": 1.4, "amp": 22},
    "run": {"cycle": 0.7, "amp": 28},
}


def serpent(p):
    n = p["segments"]
    sw, sh, sl = p["seg"]
    hw, hh, hl = p["head"]
    total = hl + n * sl
    zh = -total / 2 + hl          # the neck: back of the head, front of seg0
    bones = [("root", None, [0, 0, 0], [0, 0, 0])]
    cubes = [("head", "head", "head", [-hw / 2, 0, zh - hl], [hw / 2, hh, zh])]
    for k in reversed(range(n)):
        parent = "root" if k == n - 1 else f"seg{k + 1}"
        f = 1 - (1 - p["taper"]) * k / max(1, n - 1)
        w, h = max(1, round(sw * f * 2) / 2), max(1, round(sh * f * 2) / 2)
        bones.append((f"seg{k}", parent, [0, sh / 2, zh + (k + 1) * sl], [0, 0, 0]))
        cubes.append((f"seg{k}", f"seg{k}", "body", [-w / 2, 0, zh + k * sl], [w / 2, h, zh + (k + 1) * sl]))
    bones.append(("head", "seg0", [0, sh / 2, zh], [0, 0, 0]))
    return {"bones": bones, "cubes": cubes}


def serpent_clips(p, model):
    n = p["segments"]
    sw, sh, sl = p["seg"]
    belly = lift_for(model, [0, 0, 180])
    clips = {}

    def slither(T, A):
        tracks = {f"seg{k}": {"rotation": osc(T, [0, A, 0], phase=2 * math.pi * k / n)} for k in range(n)}
        tracks["head"] = {"rotation": osc(T, [0, -A * 0.9, 0], phase=-2 * math.pi / n)}
        return tracks

    for name in ("walk", "run"):
        s = p[name]
        clips[name] = clip("loop", s["cycle"], slither(s["cycle"], s["amp"]),
                           stride=n * sl * (0.35 if name == "walk" else 0.45))

    L = 4.0
    clips["idle"] = clip("loop", L, {
        "head": {"rotation": at(L, (0, [5, 0, 0]), (0.25, [10, 25, 0]), (0.5, [5, 0, 0]), (0.75, [10, -25, 0]),
                                (1, [5, 0, 0]))},
        "seg0": {"rotation": osc(L, [0, 6, 0])},
        "seg1": {"rotation": osc(L, [0, -4, 0])},
    })

    # Rears up (the front two segments lift), then strikes forward and down.
    L = 0.6
    clips["attack"] = clip("once", L, {
        "seg1": {"rotation": at(L, (0, [0, 0, 0]), (0.35, [15, 0, 0]), (0.55, [5, 0, 0]), (1, [0, 0, 0]))},
        "seg0": {"rotation": at(L, (0, [0, 0, 0]), (0.35, [30, 0, 0]), (0.55, [-5, 0, 0]), (1, [0, 0, 0]))},
        "head": {"rotation": at(L, (0, [0, 0, 0]), (0.35, [-35, 0, 0]), (0.55, [-10, 0, 0]), (1, [0, 0, 0]))},
        "root": {"position": at(L, (0, [0, 0, 0]), (0.35, [0, 0, 1]), (0.55, [0, 0, -sl * 0.6]),
                                (1, [0, 0, 0]))},
    })
    L = 0.35
    clips["hurt"] = clip("once", L, {
        "seg0": {"rotation": at(L, (0, [0, 0, 0]), (0.3, [10, 25, 0]), (1, [0, 0, 0]))},
        "seg2": {"rotation": at(L, (0, [0, 0, 0]), (0.3, [0, -20, 0]), (1, [0, 0, 0]))},
        "head": {"rotation": at(L, (0, [0, 0, 0]), (0.3, [20, 0, 0]), (1, [0, 0, 0]))},
    })
    L = 1.0
    clips["death"] = clip("hold", L, {
        "root": {"rotation": at(L, (0, [0, 0, 0]), (0.6, [0, 0, 175]), (1, [0, 0, 180])),
                 "position": at(L, (0, [0, 0, 0]), (0.6, [0, belly, 0]), (1, [0, belly, 0]))},
        **{f"seg{k}": {"rotation": at(L, (0, [0, 0, 0]), (1, [0, 12 * (-1) ** k, 0]))} for k in range(n - 1)},
    })

    # Coiled: every joint turns the same way, closing the chain into a ring.
    theta = 360 / (n + 1)
    for name, L, head in (("lie", 4.0, [15, theta, 0]), ("sleep", 6.0, [-5, theta + 20, 0])):
        tracks = {f"seg{k}": {"rotation": hold([0, theta, 0], L)} for k in range(n - 1)}
        tracks[f"seg{n - 1}"] = {"rotation": osc(L, [0, 0, 0], base=(0, 0, 0))}
        tracks[f"seg{n - 1}"]["scale"] = osc(L, [0.05, 0.05, 0], base=(1, 1, 1))
        tracks["head"] = {"rotation": osc(L, [3, 0, 0], base=head)}
        clips[name] = clip("loop", L, tracks)
    return clips


# --- Arthropod (insects, spiders, crabs: 6 or 8 legs, two cubes each) ---------------------

ARTHROPOD_BUDGET = {"cubes": 30, "bones": 16}
ARTHROPOD_CLIPS = ["idle", "walk", "run", "attack", "hurt", "death", "lie", "eat"]

ARTHROPOD_DEFAULTS = {
    "body": [4, 3, 4],           # thorax: width, height, length
    "head": [3, 3, 3],
    "abdomen": [5, 4, 6],        # behind the thorax (0 length: none)
    "abdomen_raise": 10,
    "legs": 8,                   # 6 or 8 (or 10 for crabs: the first pair is the claws' arms)
    "leg": [1, 6, 4],            # thickness, reach out from the body side, clearance under the body
    "splay": 35,                 # front legs angled forward, back legs back, by this much
    "webber": False,             # web_shoot
    "walk": {"cycle": 0.6, "swing": 20, "lift": 18},
    "run": {"cycle": 0.35, "swing": 25, "lift": 22},
}


def arthropod(p):
    bw, bh, bl = p["body"]
    hw, hh, hl = p["head"]
    aw, ah, al = p["abdomen"]
    t, reach, lh = p["leg"]
    cy = lh + bh / 2
    bones = [
        ("root", None, [0, 0, 0], [0, 0, 0]),
        ("body", "root", [0, cy, 0], [0, 0, 0]),
        ("head", "body", [0, cy, -bl / 2], [0, 0, 0]),
    ]
    cubes = [
        ("body", "body", "body", [-bw / 2, lh, -bl / 2], [bw / 2, lh + bh, bl / 2]),
        ("head", "head", "head", [-hw / 2, cy - hh / 2, -bl / 2 - hl], [hw / 2, cy + hh / 2, -bl / 2]),
    ]
    if al:
        bones.append(("abdomen", "body", [0, cy, bl / 2], [-p["abdomen_raise"], 0, 0]))
        ay = max(cy - ah / 2, 0.5)
        cubes.append(("abdomen", "abdomen", "abdomen", [-aw / 2, ay, bl / 2 - 0.5], [aw / 2, ay + ah, bl / 2 + al]))
    per_side = p["legs"] // 2
    for i in range(per_side):
        z = -bl / 2 + bl * (i + 0.5) / per_side
        ang = p["splay"] * (1 - 2 * i / max(1, per_side - 1)) if per_side > 1 else 0
        for side, s in (("l", -1), ("r", 1)):
            name = f"leg_{side}{i + 1}"
            pivot = [s * bw / 2, lh + 1, z]
            # Splayed about Y: forward for the front pair (left: -Y, right: +Y).
            bones.append((name, "body", pivot, [0, s * ang, 0]))
            x0, x1 = s * bw / 2, s * (bw / 2 + reach)
            cubes.append((f"{name}a", name, "legs", [min(x0, x1), lh + 1 - t, z - t / 2], [max(x0, x1), lh + 1, z + t / 2]))
            xt = x1 - s * t
            cubes.append((f"{name}b", name, "legs", [min(xt, x1), 0, z - t / 2], [max(xt, x1), lh + 1 - t, z + t / 2]))
    return {"bones": bones, "cubes": cubes}


def arthropod_required(p):
    return ARTHROPOD_CLIPS + (["web_shoot"] if p["webber"] else [])


def arthropod_clips(p, model):
    bw, bh, bl = p["body"]
    t, reach, lh = p["leg"]
    legs = [(name, -1 if name[4] == "l" else 1, int(name[5:]) - 1) for name in bone_names(model, "leg_")]
    rest = {name: r for name, _, _, r in model["bones"]}
    has_abdomen = "abdomen" in rest
    top = lift_for(model, [0, 0, 180])
    clips = {}

    def gait(T, A, lift):
        tracks = {}
        for name, s, i in legs:
            phase = math.pi * ((i + (s > 0)) % 2)
            r = rest[name]
            # Forward: left -Y, right +Y; up: left -Z, right +Z (raises the outer end).
            swing = osc(T, [0, s * A, 0], phase=phase, base=r)
            up = [(tt, [r[0], v[1], s * lift * max(0.0, math.cos(2 * math.pi * tt / T + phase))])
                  for (tt, v) in swing]
            tracks[name] = {"rotation": up}
        return tracks

    for name in ("walk", "run"):
        g = p[name]
        tracks = gait(g["cycle"], g["swing"], g["lift"])
        tracks["body"] = {"position": osc(g["cycle"], [0, 0.2, 0], cycles=2, n=4)}
        clips[name] = clip("loop", g["cycle"], tracks,
                           stride=2 * reach * math.sin(math.radians(g["swing"])) * (1 if name == "walk" else 1.3))

    L = 3.0
    tracks = {name: {"rotation": osc(L, [0, s * 3, 0], phase=i, base=rest[name])} for name, s, i in legs}
    tracks["head"] = {"rotation": at(L, (0, [0, 0, 0]), (0.3, [5, 12, 0]), (0.6, [0, -12, 0]), (1, [0, 0, 0]))}
    if has_abdomen:
        tracks["abdomen"] = {"scale": osc(L, [0.04, 0.04, 0.02], base=(1, 1, 1), cycles=2)}
    clips["idle"] = clip("loop", L, tracks)

    # Rears: the front pair up, then the head lunges.
    L = 0.6
    tracks = {
        "body": {"rotation": at(L, (0, [0, 0, 0]), (0.35, [18, 0, 0]), (0.55, [-6, 0, 0]), (1, [0, 0, 0])),
                 "position": at(L, (0, [0, 0, 0]), (0.35, [0, 1, 1]), (0.55, [0, 0, -bl * 0.4]), (1, [0, 0, 0]))},
        "head": {"rotation": at(L, (0, [0, 0, 0]), (0.35, [15, 0, 0]), (0.55, [-25, 0, 0]), (1, [0, 0, 0]))},
    }
    for name, s, i in legs:
        if i == 0:
            r = rest[name]
            tracks[name] = {"rotation": at(L, (0, r), (0.35, [r[0], r[1], s * 45]), (0.55, [r[0], r[1] + s * 20, s * 10]),
                                           (1, r))}
    clips["attack"] = clip("once", L, tracks)

    L = 0.35
    clips["hurt"] = clip("once", L, {
        "body": {"rotation": at(L, (0, [0, 0, 0]), (0.3, [-8, 0, 10]), (1, [0, 0, 0])),
                 "position": at(L, (0, [0, 0, 0]), (0.3, [0, 0.5, 1.5]), (1, [0, 0, 0]))},
    })

    # On its back, legs curled in.
    L = 1.0
    tracks = {"root": {"rotation": at(L, (0, [0, 0, 0]), (0.5, [0, 0, 175]), (1, [0, 0, 180])),
                       "position": at(L, (0, [0, 0, 0]), (0.5, [0, top, 0]), (1, [0, top, 0]))}}
    for name, s, i in legs:
        r = rest[name]
        tracks[name] = {"rotation": at(L, (0, r), (1, [r[0], r[1], -s * 50]))}
    clips["death"] = clip("hold", L, tracks)

    # Resting: the body lowered, the legs raised at the hip so their tips still
    # touch the ground.
    drop = lh * 0.6
    tip = [-reach, -(lh + 1 - t), 0]

    def tip_y(phi):
        return lh + 1 - drop + rotate(tip, [0, 0, -phi])[1]

    phi = 0
    while phi < 80 and tip_y(phi) < 0:
        phi += 1
    L = 4.0
    tracks = {"body": {"position": osc(L, [0, 0.15, 0], base=(0, -drop, 0))}}
    for name, s, i in legs:
        r = rest[name]
        tracks[name] = {"rotation": hold([r[0], r[1], s * phi], L)}
    clips["lie"] = clip("loop", L, tracks)

    L = 1.0
    clips["eat"] = clip("loop", L, {
        "body": {"rotation": hold([-6, 0, 0], L)},
        "head": {"rotation": osc(L, [8, 0, 0], base=(-20, 0, 0), cycles=3, n=4)},
    })

    if p["webber"]:
        L = 1.0
        tracks = {"body": {"rotation": at(L, (0, [0, 0, 0]), (0.3, [12, 0, 0]), (0.8, [12, 0, 0]), (1, [0, 0, 0]))}}
        if has_abdomen:
            tracks["abdomen"] = {
                "rotation": at(L, (0, [0, 0, 0]), (0.3, [-45, 0, 0]), (0.8, [-45, 0, 0]), (1, [0, 0, 0])),
                "scale": at(L, (0, [1, 1, 1]), (0.45, [1, 1, 1]), (0.55, [1.15, 1.15, 0.9]), (0.7, [1, 1, 1]),
                            (1, [1, 1, 1]))}
        clips["web_shoot"] = clip("once", L, tracks)
    return clips


# --- Blob (squash and stretch about the ground under it) ----------------------------------

BLOB_BUDGET = {"cubes": 10, "bones": 6}
BLOB_CLIPS = ["idle", "walk", "attack", "hurt", "death"]

BLOB_DEFAULTS = {
    "body": [8, 6, 8],
    "top": [6, 2, 6],            # a smaller cube on top (0 height: none)
    "hop": 0,                    # walk hop height (0: creeps)
    "walk": {"cycle": 0.8},
}


def blob(p):
    bw, bh, bl = p["body"]
    tw, th, tl = p["top"]
    cubes = [("body", "body", "body", [-bw / 2, 0, -bl / 2], [bw / 2, bh, bl / 2])]
    if th:
        cubes.append(("top", "body", "top", [-tw / 2, bh, -tl / 2], [tw / 2, bh + th, tl / 2]))
    return {"bones": [("root", None, [0, 0, 0], [0, 0, 0]), ("body", "root", [0, 0, 0], [0, 0, 0])],
            "cubes": cubes}


def blob_clips(p, model):
    bw, bh, bl = p["body"]
    clips = {}
    L = 2.0
    clips["idle"] = clip("loop", L, {"body": {"scale": osc(L, [0.04, -0.06, 0.04], base=(1, 1, 1))}})
    T = p["walk"]["cycle"]
    tracks = {"body": {"scale": at(T, (0, [1.12, 0.84, 1.12]), (0.3, [0.92, 1.16, 0.95]), (0.6, [1, 1, 1.08]),
                                   (1, [1.12, 0.84, 1.12]))}}
    if p["hop"]:
        tracks["root"] = {"position": at(T, (0, [0, 0, 0]), (0.25, [0, 0, 0]), (0.55, [0, p["hop"], 0]),
                                         (0.85, [0, 0, 0]), (1, [0, 0, 0]))}
    clips["walk"] = clip("loop", T, tracks, stride=bl * 0.6)
    L = 0.6
    clips["attack"] = clip("once", L, {
        "body": {"scale": at(L, (0, [1, 1, 1]), (0.25, [1.15, 0.8, 0.9]), (0.45, [0.85, 0.95, 1.4]), (1, [1, 1, 1])),
                 "position": at(L, (0, [0, 0, 0]), (0.25, [0, 0, 1]), (0.45, [0, 0, -bl * 0.35]), (1, [0, 0, 0]))},
    })
    L = 0.35
    clips["hurt"] = clip("once", L, {
        "body": {"scale": at(L, (0, [1, 1, 1]), (0.3, [1.2, 0.7, 1.2]), (1, [1, 1, 1])),
                 "position": at(L, (0, [0, 0, 0]), (0.3, [0, 0, 1]), (1, [0, 0, 0]))},
    })
    L = 1.0
    clips["death"] = clip("hold", L, {
        "body": {"scale": at(L, (0, [1, 1, 1]), (0.3, [0.9, 1.15, 0.9]), (1, [1.5, 0.15, 1.5]))},
    })
    return clips


# --- Vermin form (tiny ambient creatures: one move loop and a death) ----------------------

VERMIN_BUDGET = {"cubes": 8, "bones": 6}
VERMIN_CLIPS = ["move", "death"]

VERMIN_DEFAULTS = {
    "body": [2, 1, 3],
    "head": [1.5, 1, 1],         # in front of the body (0 length: none)
    "legs": 0.5,                 # clearance: a dark slab under the body (0: on the ground)
    "wings": None,               # [span, chord]: spread flat, they flutter
    "move": "scurry",            # scurry | flutter | swim | crawl | hop
    "cycle": None,               # move cycle seconds (default per style)
}


def vermin(p):
    bw, bh, bl = p["body"]
    hw, hh, hl = p["head"]
    lh = p["legs"]
    cy = lh + bh / 2
    bones = [("root", None, [0, 0, 0], [0, 0, 0]), ("body", "root", [0, cy, 0], [0, 0, 0])]
    cubes = [("body", "body", "body", [-bw / 2, lh, -bl / 2], [bw / 2, lh + bh, bl / 2])]
    if hl:
        bones.append(("head", "body", [0, cy, -bl / 2], [0, 0, 0]))
        cubes.append(("head", "head", "head", [-hw / 2, lh, -bl / 2 - hl], [hw / 2, lh + hh, -bl / 2]))
    if lh:
        cubes.append(("legs", "body", "legs", [-bw / 2 - 0.5, 0, -bl / 2 + 0.5], [bw / 2 + 0.5, lh, bl / 2 - 0.5]))
    if p["wings"]:
        span, chord = p["wings"]
        for name, s in (("wing_l", -1), ("wing_r", 1)):
            pivot = [s * bw / 2, lh + bh, -bl / 2 + 0.5]
            bones.append((name, "body", pivot, [0, 0, s * 10]))
            lo = [pivot[0] - span if s < 0 else pivot[0], pivot[1] - 0.25, pivot[2]]
            hi = [pivot[0] if s < 0 else pivot[0] + span, pivot[1] + 0.25, pivot[2] + chord]
            cubes.append((name, name, "wings", lo, hi))
    return {"bones": bones, "cubes": cubes}


def vermin_clips(p, model):
    bw, bh, bl = p["body"]
    style = p["move"]
    has = {name for name, *_ in model["bones"]}
    T = p["cycle"] or {"scurry": 0.3, "flutter": 0.16, "swim": 0.5, "crawl": 1.4, "hop": 0.6}[style]
    tracks = {}
    if style == "scurry":
        tracks["body"] = {"position": osc(T, [0, 0.25, 0], cycles=2, n=4), "rotation": osc(T, [0, 6, 0])}
    elif style == "flutter":
        tracks["root"] = {"position": osc(T * 6, [0, 1, 0], base=(0, 2, 0))}
        T = T * 6   # one bob of the body over six beats of the wings
        if "wing_l" in has:
            tracks["wing_l"] = {"rotation": osc(T, [0, 0, 55], base=(0, 0, -10), cycles=6, n=4)}
            tracks["wing_r"] = {"rotation": osc(T, [0, 0, -55], base=(0, 0, 10), cycles=6, n=4)}
    elif style == "swim":
        tracks["body"] = {"rotation": osc(T, [0, 15, 0])}
        if "head" in has:
            tracks["head"] = {"rotation": osc(T, [0, -10, 0])}
    elif style == "crawl":
        tracks["body"] = {"scale": osc(T, [0, -0.08, 0.15], base=(1, 1, 1))}
    elif style == "hop":
        tracks["root"] = {"position": bounce(T, max(1.0, bh * 1.5))}
        tracks["body"] = {"rotation": at(T, (0, [0, 0, 0]), (0.25, [15, 0, 0]), (0.75, [-10, 0, 0]), (1, [0, 0, 0]))}
    clips = {"move": clip("loop", T, tracks, stride=bl * 1.5)}
    belly = lift_for(model, [0, 0, 180])
    L = 0.6
    clips["death"] = clip("hold", L, {
        "root": {"rotation": at(L, (0, [0, 0, 0]), (1, [0, 0, 180])),
                 "position": at(L, (0, [0, 0, 0]), (0.4, [0, belly + 1, 0]), (1, [0, belly, 0]))},
    })
    return clips


# --- Registry ----------------------------------------------------------------------------

class Template:
    def __init__(self, defaults, skeleton, clips, budget, required):
        self.defaults, self.skeleton, self.clips, self.budget = defaults, skeleton, clips, budget
        self.required = required if callable(required) else (lambda p, r=required: list(r))


TEMPLATES = {
    "quadruped": Template(QUADRUPED_DEFAULTS, quadruped, quadruped_clips, QUADRUPED_BUDGET, QUADRUPED_CLIPS),
    "biped": Template(BIPED_DEFAULTS, biped, biped_clips, BIPED_BUDGET, BIPED_CORE + BIPED_WORK),
    "bird": Template(BIRD_DEFAULTS, bird, bird_clips, BIRD_BUDGET, bird_required),
    "fish": Template(FISH_DEFAULTS, fish, fish_clips, FISH_BUDGET, FISH_CLIPS),
    "serpent": Template(SERPENT_DEFAULTS, serpent, serpent_clips, SERPENT_BUDGET, SERPENT_CLIPS),
    "arthropod": Template(ARTHROPOD_DEFAULTS, arthropod, arthropod_clips, ARTHROPOD_BUDGET, arthropod_required),
    "blob": Template(BLOB_DEFAULTS, blob, blob_clips, BLOB_BUDGET, BLOB_CLIPS),
    "vermin": Template(VERMIN_DEFAULTS, vermin, vermin_clips, VERMIN_BUDGET, VERMIN_CLIPS),
}
