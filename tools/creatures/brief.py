#!/usr/bin/env python3
"""A creature's brief, the first step of the per-creature process
(docs/superpowers/specs/2026-10-29-creature-asset-pipeline.md): its row from the
DF roster (docs/superpowers/specs/2026-10-29-df-creatures/creatures.csv), plus
its body plan and attacks from the local DF raws, turned into a size, a
suggested template and the clips it needs.

python3 tools/creatures/brief.py <DF_ID or name>   (e.g. DOG, SPIDER_CAVE_GIANT, "giant cave spider")

Raws: DF_RAWS, default /usr/share/games/dwarf-fortress/gamedata/raw/objects.
Without them the brief still prints, minus body plan and attacks.
"""
import csv
import glob
import math
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
ROSTER = os.path.join(REPO, "docs", "superpowers", "specs", "2026-10-29-df-creatures", "creatures.csv")
RAWS = os.environ.get("DF_RAWS", "/usr/share/games/dwarf-fortress/gamedata/raw/objects")
UNITS_PER_BLOCK = 16

TEMPLATE_CLIPS = {
    "quadruped": ["idle", "walk", "run", "attack", "hurt", "death", "lie", "sleep", "eat"],
    "biped": ["idle", "walk", "run", "attack", "hurt", "death", "lie", "sleep", "eat"],
    "bird": ["idle", "walk", "run", "attack", "hurt", "death", "lie", "sleep", "eat"],
    "fish": ["swim_idle", "swim", "attack", "hurt", "death", "flop"],
    "serpent": ["idle", "walk", "run", "attack", "hurt", "death", "lie", "sleep"],
    "arthropod": ["idle", "walk", "run", "attack", "hurt", "death", "lie", "eat"],
    "blob": ["idle", "walk", "attack", "hurt", "death"],
}

# What a flag adds, beyond the template's core clips.
FLAG_CLIPS = [
    ("FLIER", ["fly", "hover", "takeoff", "land"]),
    ("AMPHIBIOUS", ["swim", "swim_idle"]),
    ("MOUNT", ["ridden"]),
    ("MOUNT_EXOTIC", ["ridden"]),
    ("WEBBER", ["web_shoot"]),
    ("LARGE_PREDATOR", ["alert", "roar"]),
    ("AMBUSHPREDATOR", ["alert"]),
    ("CURIOUSBEAST_ITEM", ["pick_up", "carry"]),
    ("CURIOUSBEAST_EATER", ["pick_up", "carry"]),
    ("CURIOUSBEAST_GUZZLER", ["pick_up", "carry"]),
]

WORKER_CLIPS = ["mine", "chop", "build", "craft", "carry", "pick_up", "put_down", "sit", "drink",
                "swing", "shoot", "cheer", "cower"]


def raw_blocks():
    blocks = {}
    for f in sorted(glob.glob(os.path.join(RAWS, "creature_*.txt"))):
        text = open(f, encoding="latin-1").read()
        parts = re.split(r"\[CREATURE:([^\]]+)\]", text)
        for i in range(1, len(parts), 2):
            blocks[parts[i]] = parts[i + 1]
    return blocks


def tokens(block, blocks, depth=0):
    """The block's tags, with a COPY_TAGS_FROM source's tags first."""
    tags = re.findall(r"\[([^\]]+)\]", block)
    for t in tags:
        if t.startswith("COPY_TAGS_FROM:") and depth < 4:
            src = blocks.get(t.split(":")[1])
            if src:
                return tokens(src, blocks, depth + 1) + tags
    return tags


def body_plan(tags):
    parts = [p for t in tags if t.startswith("BODY:") for p in t.split(":")[1:]]
    joined = ":".join(parts)
    if "QUADRUPED" in joined:
        plan = "quadruped"
    elif "2WINGS" in joined and ("ARMLESS" in joined or "2LEGS" in joined):
        plan = "bird"  # birds are HUMANOID_ARMLESS_NECK:2WINGS in the raws
    elif "HUMANOID" in joined:
        plan = "biped"
    elif re.search(r"FIN", joined) and "LEGS" not in joined:
        plan = "fish"
    elif re.search(r"6LEGS|8LEGS|INSECT|SPIDER|ARACHNID|CRAB|SCORPION", joined):
        plan = "arthropod"
    elif "LEGS" not in joined and ("BASIC_1PARTBODY" in joined or "SNAKE" in joined):
        plan = "serpent"
    elif not parts:
        plan = None
    else:
        plan = "?"
    return plan, parts


def material_emissions(tags):
    """Names of the creature's material-emission interactions (breath, spit,
    web spray): each `CAN_DO_INTERACTION:MATERIAL_EMISSION` and its ADV_NAME."""
    names = []
    for i, t in enumerate(tags):
        if t == "CAN_DO_INTERACTION:MATERIAL_EMISSION":
            name = next((u.split(":", 2)[2] for u in tags[i + 1:i + 12] if u.startswith("CDI:ADV_NAME:")),
                        "emission")
            if name not in names:
                names.append(name)
    return names


def emission_clip(name):
    low = name.lower()
    if "web" in low:
        return "web_shoot"
    if "breath" in low:
        return "breathe"
    return re.sub(r"[^a-z]+", "_", low).strip("_")


def attacks(tags):
    names = []
    for t in tags:
        if t.startswith("ATTACK:"):
            name = t.split(":")[1].lower()
            if name not in names:
                names.append(name)
    return names


def main(df_id):
    rows = list(csv.DictReader(open(ROSTER)))
    match = [r for r in rows if r["id"] == df_id.upper()] or [r for r in rows if r["name"] == df_id.lower()]
    if not match:
        sys.exit(f"{df_id} is not in {os.path.relpath(ROSTER, REPO)} (by id or name)")
    r = match[0]
    df_id = r["id"]
    flags = set(r["flags"].split())
    blocks = raw_blocks() if os.path.isdir(RAWS) else {}
    tags = tokens(blocks[df_id], blocks) if df_id in blocks else []
    plan, parts = body_plan(tags)
    if any(f.startswith("VERMIN") for f in flags):
        plan = plan or "?"
    hits = attacks(tags)
    emissions = material_emissions(tags)

    size = int(r["adult_size_cm3"] or 0)
    # A box of the creature's true volume at w : h : l = 1 : 1 : 2, in units:
    # a lower bound — the style guide's chunky proportions run larger.
    side = (size / 2) ** (1 / 3) / 100 * UNITS_PER_BLOCK if size else 0

    print(f"# {r['name']} ({df_id})\n")
    print(f"- category: {r['category']}, source file creature_{r['source_file']}.txt")
    print(f"- adult size: {size} cm³ ≈ {size / 1000:g} kg; box of that volume at 1:1:2 ≈ "
          f"{side:.1f} × {side:.1f} × {2 * side:.1f} units (w × h × l)")
    print(f"- biomes: {r['biomes'] or '—'}")
    print(f"- products: {r['products'] or '—'}")
    print(f"- flags: {r['flags'] or '—'}")
    if r["description"]:
        print(f"- DF: {r['description']}")
    print(f"- body plan: {':'.join(parts) or 'unknown (no raws)'}")
    print(f"- attacks: {', '.join(hits) or '—'}; emissions: {', '.join(emissions) or '—'}")
    print()

    if any(f.startswith("VERMIN") for f in flags):
        print("Vermin: one `move` loop (and `death` if it can be killed on screen); no full rig.")
        return
    if r["category"] in ("giant",):
        print(f"Giant variant: reuse the base creature's model scaled up (`{r['copy_tags_from'] or '?'}`).")
    template = plan if plan in TEMPLATE_CLIPS else None
    print(f"Suggested template: {template or '? (pick by hand)'}")
    clips = list(TEMPLATE_CLIPS.get(template, []))
    # The first attack is the template's `attack`; the rest are optional extras.
    optional = [f"attack_{a}" for a in hits[1:]]
    for flag, extra in FLAG_CLIPS:
        if flag in flags:
            clips += [c for c in extra if c not in clips]
    if "AQUATIC" in flags and template != "fish":
        clips += [c for c in ("swim", "swim_idle") if c not in clips]
    for clip in map(emission_clip, emissions):
        # A breath weapon is part of how it fights; spitting and the like are extras.
        target = clips if clip in ("breathe", "web_shoot") else optional
        if clip not in clips and clip not in target:
            target.append(clip)
    if "LAYS_EGGS" in flags and "COMMON_DOMESTIC" in flags:
        clips.append("brood")
    if template == "biped" and ({"INTELLIGENT", "CAN_LEARN"} & flags):
        clips += [c for c in WORKER_CLIPS if c not in clips]
    print(f"Clips ({len(clips)}): {', '.join(clips)}" + (f" (attack = {hits[0]})" if hits else ""))
    if optional:
        print(f"Optional: {', '.join(optional)}")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    main(sys.argv[1])
