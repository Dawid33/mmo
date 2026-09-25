#!/usr/bin/env python3
"""Census of Blockbench / Bedrock model files: which format features each file
uses, so a loader knows what it must handle (or refuse) before it meets it.

    scripts/bbmodel_census.py [DIR_OR_FILE ...]      (default: target/test-models)

Reads `.bbmodel` (Blockbench project, 3.x-5.x), `.geo.json` (Bedrock geometry)
and `.animation.json` (Bedrock / GeckoLib animations). Prints one row per
model, then a per-feature list of the files that exercise it. Read-only;
stdlib + optional Pillow (partial-alpha texture detection).

Spec: docs/superpowers/specs/2026-09-22-blockbench-model-loading-investigation.md
"""
import base64
import io
import json
import math
import os
import sys
from collections import Counter, defaultdict

try:
    from PIL import Image
except ImportError:  # the alpha check is skipped without Pillow
    Image = None

# feature -> what it means for a loader (printed in the legend)
FEATURES = {
    "v3": "format_version 3.x: oldest layout, inline outliner groups",
    "v4": "format_version 4.x: inline outliner groups, pre-5.0 animation axes",
    "v5": "format_version 5.x: top-level groups[]; animation axes changed",
    "mesh": "mesh elements (free-form polygons) - v1 plans to skip them",
    "ngon": "mesh faces with >4 vertices (needs triangulation)",
    "tri": "mesh faces with 3 vertices",
    "other_elem": "non-cube/mesh elements (locator, null_object, texture_mesh...)",
    "rot_multi": "cube rotated on 2+ axes (free/bedrock formats)",
    "rot_odd": "cube rotation not a multiple of 22.5 deg",
    "frac": "cube bounds off the integer pixel grid (not ChiselData-able)",
    "inflate": "inflated cubes (geometry grows, UVs don't)",
    "mirror": "mirror_uv cubes (flip faces, swap east/west)",
    "box_uv": "box-UV cubes (layout derived from uv_offset + size)",
    "face_uv": "per-face UV cubes",
    "face_rot": "face UV rotation 90/180/270",
    "face_null": "faces with texture null (face absent)",
    "face_multi_tex": "one cube's faces use different textures",
    "zero_thick": "zero-thickness cubes (planes: double-sided, z-fighting)",
    "hidden": "elements/groups with visibility false or export false",
    "deep": "outliner depth > 6",
    "multi_tex": "more than one texture",
    "tex_path": "texture not embedded (path only) - needs external file",
    "tex_npot": "texture size not a power of two",
    "tex_anim": "animated texture (frames stacked vertically)",
    "tex_uvsize": "texture uv_width/height differs from its pixel size",
    "tex_mode": "texture render_mode not default (emissive/additive/layered)",
    "alpha_partial": "texture has partially transparent texels (needs Blend)",
    "anim": "has animations",
    "molang": "keyframe values that are molang expressions (strings)",
    "catmullrom": "catmullrom keyframes",
    "bezier": "bezier keyframes",
    "step": "step keyframes",
    "prepost": "pre/post discontinuity keyframes (2 data points)",
    "effects": "sound/particle/instruction/timeline tracks",
    "anim_dangling": "animator uuid/bone not present in the model",
    "big_turn": "rotation keys >180 deg apart (quat slerp takes the short way)",
    "multi_geo": "geo.json with several geometries",
    "binding": "bedrock bone binding (molang attachment)",
    "poly_mesh": "bedrock poly_mesh",
    "locator": "bedrock locators",
}


def is_num(v):
    return isinstance(v, (int, float)) and not isinstance(v, bool)


def odd_angle(a):
    return is_num(a) and abs(a / 22.5 - round(a / 22.5)) > 1e-6


def frac(vals):
    return any(is_num(v) and abs(v - round(v)) > 1e-6 for v in vals)


def pow2(n):
    return n > 0 and n & (n - 1) == 0


def texture_features(t, f):
    src = t.get("source") or ""
    if not src.startswith("data:"):
        f.add("tex_path")
        return None
    w, h = t.get("width"), t.get("height")
    img = None
    if Image is not None:
        try:
            img = Image.open(io.BytesIO(base64.b64decode(src.split(",", 1)[1]))).convert("RGBA")
            w, h = img.size
            alphas = img.getchannel("A").getextrema()
            hist = img.getchannel("A").histogram()
            if sum(hist[1:255]) > 0:
                f.add("alpha_partial")
        except Exception:
            pass
    if w and h:
        if not (pow2(w) and pow2(h)):
            f.add("tex_npot")
        uw, uh = t.get("uv_width"), t.get("uv_height")
        if uw and uh and (uw, uh) != (w, h) and h % max(uh, 1) == 0 and h // uh > 1:
            f.add("tex_anim")
        elif uw and uh and (uw, uh) != (w, h):
            f.add("tex_uvsize")
    if (t.get("frame_time") or 1) > 1 and "tex_anim" not in f:
        f.add("tex_anim")
    if t.get("render_mode", "default") not in ("default", "normal", ""):  # "normal" = pre-4.x spelling
        f.add("tex_mode")
    return (w, h)


def outliner_depth(nodes, d=1):
    best = d - 1
    for n in nodes:
        if isinstance(n, dict):
            best = max(best, d, outliner_depth(n.get("children", []), d + 1))
    return best


def outliner_names(nodes, out):
    for n in nodes:
        if isinstance(n, dict):
            out.add(n.get("uuid"))
            if n.get("visibility") is False or n.get("export") is False:
                out.add("__hidden__")
            outliner_names(n.get("children", []), out)


def keyframe_features(kfs, f, stats):
    rot_keys = []
    for k in kfs:
        interp = k.get("interpolation", "linear")
        if interp in ("catmullrom", "bezier", "step"):
            f.add(interp)
        dps = k.get("data_points", [])
        if len(dps) > 1:
            f.add("prepost")
        ch = k.get("channel")
        if ch not in ("rotation", "position", "scale"):
            f.add("effects")
        for dp in dps:
            for axis in ("x", "y", "z"):
                v = dp.get(axis)
                if isinstance(v, str):
                    try:
                        float(v)
                    except ValueError:
                        if v.strip():
                            f.add("molang")
                            stats["molang"] += 1
        if ch == "rotation" and dps:
            try:
                rot_keys.append((k.get("time", 0), [float(dps[0].get(a, 0) or 0) for a in "xyz"]))
            except (TypeError, ValueError):
                pass
        stats["keyframes"] += 1
    rot_keys.sort()
    for (_, a), (_, b) in zip(rot_keys, rot_keys[1:]):
        if any(abs(x - y) > 180 for x, y in zip(a, b)):
            f.add("big_turn")


def census_bbmodel(path, d):
    f, stats = set(), Counter()
    fv = str(d.get("meta", {}).get("format_version", "?"))
    f.add("v" + fv.split(".")[0] if fv[0] in "345" else "v?")
    elems = d.get("elements", [])
    types = Counter(e.get("type", "cube") for e in elems)
    stats.update({"cubes": types.get("cube", 0), "meshes": types.get("mesh", 0)})
    if types.get("mesh"):
        f.add("mesh")
    if set(types) - {"cube", "mesh"}:
        f.add("other_elem")
    meta_box = d.get("meta", {}).get("box_uv", False)
    for e in elems:
        if e.get("visibility") is False or e.get("export") is False:
            f.add("hidden")
        t = e.get("type", "cube")
        if t == "mesh":
            for face in e.get("faces", {}).values():
                n = len(face.get("vertices", []))
                if n > 4:
                    f.add("ngon")
                elif n == 3:
                    f.add("tri")
            continue
        if t != "cube":
            continue
        rot = e.get("rotation", [0, 0, 0]) or [0, 0, 0]
        if sum(1 for a in rot if is_num(a) and a) >= 2:
            f.add("rot_multi")
        if any(odd_angle(a) for a in rot):
            f.add("rot_odd")
        if frac(e.get("from", []) + e.get("to", [])):
            f.add("frac")
        if e.get("inflate"):
            f.add("inflate")
        if e.get("mirror_uv"):
            f.add("mirror")
        if any(abs(a - b) < 1e-6 for a, b in zip(e.get("from", [0, 0, 0]), e.get("to", [1, 1, 1]))):
            f.add("zero_thick")
        f.add("box_uv" if e.get("box_uv", meta_box) else "face_uv")
        texs = set()
        for face in (e.get("faces") or {}).values():
            if face.get("rotation"):
                f.add("face_rot")
            if face.get("texture") is None:
                f.add("face_null")
            else:
                texs.add(face.get("texture"))
        if len(texs) > 1:
            f.add("face_multi_tex")
    # hierarchy
    outl = d.get("outliner", [])
    depth = outliner_depth(outl)
    stats["depth"] = depth
    if depth > 6:
        f.add("deep")
    groups = d.get("groups")
    stats["groups"] = len(groups) if groups is not None else sum(
        1 for _ in iter_inline_groups(outl))
    known = set()
    outliner_names(outl, known)
    known |= {g.get("uuid") for g in (groups or [])}
    if "__hidden__" in known or any(g.get("visibility") is False for g in (groups or [])):
        f.add("hidden")
    known |= {e.get("uuid") for e in elems}
    # textures
    texs = d.get("textures", [])
    stats["textures"] = len(texs)
    if len(texs) > 1:
        f.add("multi_tex")
    sizes = [texture_features(t, f) for t in texs]
    stats["tex"] = ",".join(f"{w}x{h}" for w, h in filter(None, sizes)) or "-"
    # animations
    anims = d.get("animations", [])
    stats["anims"] = len(anims)
    if anims:
        f.add("anim")
    for a in anims:
        for uid, an in (a.get("animators") or {}).items():
            if an.get("type") in ("effect",) or uid == "effects":
                f.add("effects")
            elif uid not in known:
                f.add("anim_dangling")
            keyframe_features(an.get("keyframes", []), f, stats)
    return f, stats


def iter_inline_groups(nodes):
    for n in nodes:
        if isinstance(n, dict):
            yield n
            yield from iter_inline_groups(n.get("children", []))


def census_geo(path, d):
    f, stats = set(), Counter()
    geos = d.get("minecraft:geometry")
    if geos is None:  # format 1.8: "geometry.name": {...}
        geos = [v for k, v in d.items() if k.startswith("geometry.")]
    if len(geos) > 1:
        f.add("multi_geo")
    fv = str(d.get("format_version", "?"))
    stats["fv"] = fv
    for g in geos:
        desc = g.get("description", {})
        tw, th = desc.get("texture_width", g.get("texturewidth")), desc.get("texture_height", g.get("textureheight"))
        if tw and th:
            stats["tex"] = f"{tw}x{th}"
            if not (pow2(int(tw)) and pow2(int(th))):
                f.add("tex_npot")
        bones = g.get("bones", [])
        stats["groups"] += len(bones)
        parent = {b["name"]: b.get("parent") for b in bones if "name" in b}

        def depth(n, seen=()):
            p = parent.get(n)
            return 1 if not p or p in seen or p not in parent else 1 + depth(p, seen + (n,))
        stats["depth"] = max([stats["depth"]] + [depth(n) for n in parent])
        if stats["depth"] > 6:
            f.add("deep")
        for b in bones:
            if b.get("binding"):
                f.add("binding")
            if b.get("poly_mesh"):
                f.add("poly_mesh")
            if b.get("locators"):
                f.add("locator")
            if b.get("mirror"):
                f.add("mirror")
            for c in b.get("cubes", []):
                stats["cubes"] += 1
                rot = c.get("rotation", [0, 0, 0])
                if sum(1 for a in rot if a) >= 2:
                    f.add("rot_multi")
                if any(odd_angle(a) for a in rot):
                    f.add("rot_odd")
                if frac(c.get("origin", []) + c.get("size", [])):
                    f.add("frac")
                if c.get("inflate"):
                    f.add("inflate")
                if c.get("mirror"):
                    f.add("mirror")
                if 0 in c.get("size", [1]):
                    f.add("zero_thick")
                uv = c.get("uv")
                if isinstance(uv, dict):
                    f.add("face_uv")
                    if any(fc.get("uv_rotation") for fc in uv.values() if isinstance(fc, dict)):
                        f.add("face_rot")
                else:
                    f.add("box_uv")
    return f, stats


def census_anim(path, d):
    f, stats = set(), Counter()
    anims = d.get("animations", {})
    stats["anims"] = len(anims)
    if anims:
        f.add("anim")

    def walk_val(v):
        if isinstance(v, str):
            try:
                float(v)
            except ValueError:
                f.add("molang")
                stats["molang"] += 1
        elif isinstance(v, list):
            for x in v:
                walk_val(x)
        elif isinstance(v, dict):
            if "pre" in v and "post" in v:
                f.add("prepost")
            if v.get("lerp_mode") == "catmullrom":
                f.add("catmullrom")
            for k, x in v.items():
                if k != "lerp_mode":
                    walk_val(x)

    for a in anims.values():
        if any(k in a for k in ("sound_effects", "particle_effects", "timeline")):
            f.add("effects")
        for bone in (a.get("bones") or {}).values():
            for ch in bone.values():
                stats["keyframes"] += len(ch) if isinstance(ch, dict) else 1
                walk_val(ch)
    return f, stats


def main(argv):
    roots = argv or ["target/test-models"]
    files = []
    for r in roots:
        if os.path.isfile(r):
            files.append(r)
        for dp, _, fs in os.walk(r):
            files += [os.path.join(dp, x) for x in fs
                      if x.endswith((".bbmodel", ".geo.json", ".animation.json"))]
    rows, by_feature = [], defaultdict(list)
    for p in sorted(set(files)):
        try:
            d = json.load(open(p, encoding="utf-8-sig"))
        except Exception as e:
            rows.append((p, f"PARSE ERROR: {e}", {}, set()))
            by_feature["parse_error"].append(p)
            continue
        if p.endswith(".bbmodel"):
            kind, (f, s) = d.get("meta", {}).get("model_format", "?"), census_bbmodel(p, d)
            ver = d.get("meta", {}).get("format_version", "?")
        elif p.endswith(".animation.json"):
            kind, (f, s), ver = "anim", census_anim(p, d), d.get("format_version", "?")
        else:
            kind, (f, s), ver = "geo", census_geo(p, d), d.get("format_version", "?")
        rows.append((p, f"{kind} {ver}", s, f))
        for x in f:
            by_feature[x].append(p)

    base = os.path.commonpath([r[0] for r in rows]) if len(rows) > 1 else ""
    short = lambda p: os.path.relpath(p, base) if base else p
    print(f"{'file':58} {'format':16} {'grp':>4} {'dep':>3} {'cube':>5} {'mesh':>4} "
          f"{'tex':>3} {'anim':>4} {'keys':>5} {'molang':>6}  textures")
    for p, fmt, s, f in rows:
        print(f"{short(p)[:58]:58} {fmt[:16]:16} {s.get('groups',0):>4} {s.get('depth',0):>3} "
              f"{s.get('cubes',0):>5} {s.get('meshes',0):>4} {s.get('textures',0):>3} "
              f"{s.get('anims',0):>4} {s.get('keyframes',0):>5} {s.get('molang',0):>6}  {s.get('tex','')}")
    print(f"\n{len(rows)} files. Features (count: files):")
    for feat in list(FEATURES) + ["parse_error"]:
        ps = by_feature.get(feat)
        if not ps:
            continue
        names = ", ".join(os.path.basename(p) for p in ps[:6]) + (" ..." if len(ps) > 6 else "")
        print(f"  {feat:15} {len(ps):>3}  {FEATURES.get(feat, 'file failed to parse')}\n"
              f"  {'':15}      {names}")
    unused = [k for k in FEATURES if k not in by_feature]
    if unused:
        print("\nNot exercised by any file: " + ", ".join(unused))


if __name__ == "__main__":
    main(sys.argv[1:])
