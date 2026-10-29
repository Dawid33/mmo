#!/usr/bin/env python3
"""How far the creature models are: every roster creature
(docs/superpowers/specs/2026-10-29-df-creatures/creatures.csv) against the
manifest (assets/models/creatures/creatures.json), per category, plus the next
ones to build in the agreed order (docs/superpowers/plans/2026-10-29-creature-models-handoff.md).

python3 tools/creatures/progress.py [--next N]

Placeholders (centaur, chimera, griffon, wagon) need no model. Giants are the
base creature scaled, so a giant counts as covered once its base has a model.
"""
import csv
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
ROSTER = os.path.join(REPO, "docs", "superpowers", "specs", "2026-10-29-df-creatures", "creatures.csv")
MANIFEST = os.path.join(REPO, "assets", "models", "creatures", "creatures.json")

# Build order: template pilots, then the game shortlist, then categories.
PILOTS = ["DOG", "DWARF", "BIRD_CHICKEN", "FISH_CARP", "ADDER", "SPIDER_CAVE_GIANT", "CAVE_BLOB"]
SHORTLIST = ["CAT", "WOLF", "BEAR_BLACK", "COUGAR", "COW", "SHEEP", "GOAT", "PIG", "DONKEY", "HORSE",
             "DEER", "ELK", "WILD_BOAR", "HARE", "FOX", "BADGER", "FISH_SALMON", "FISH_PIKE", "RAT",
             "CRUNDLE", "CROCODILE_CAVE", "TROGLODYTE", "CAVE_DRAGON", "BLIND_CAVE_OGRE", "GREMLIN",
             "OLM_GIANT", "MAGMA_CRAB", "IMP_FIRE"]
CATEGORY_ORDER = ["domestic", "wild", "civ", "underground", "fantastic", "semimegabeast", "megabeast",
                  "animalperson", "vermin"]


def main(args):
    rows = list(csv.DictReader(open(ROSTER)))
    try:
        manifest = json.load(open(MANIFEST))
    except FileNotFoundError:
        manifest = {}
    built = {k.upper(): v.get("status", "draft") for k, v in manifest.items()}
    ids = {r["id"] for r in rows}

    def status(r):
        if r["category"] == "meta":
            return "n/a"
        if r["category"] == "giant":
            base = r["copy_tags_from"]
            return "covered" if base and base in built else "needs base"
        return built.get(r["id"], "todo")

    print(f"{'category':<14} {'approved':>8} {'draft':>6} {'todo':>6}  (covered giants / total)")
    totals = {"approved": 0, "draft": 0, "todo": 0}
    for cat in CATEGORY_ORDER + ["giant"]:
        rs = [r for r in rows if r["category"] == cat]
        s = [status(r) for r in rs]
        if cat == "giant":
            print(f"{cat:<14} {'':>8} {'':>6} {'':>6}  {s.count('covered')}/{len(rs)} covered by their base")
            continue
        n = {k: s.count(k) for k in totals}
        for k in totals:
            totals[k] += n[k]
        print(f"{cat:<14} {n['approved']:>8} {n['draft']:>6} {n['todo']:>6}")
    print(f"{'total':<14} {totals['approved']:>8} {totals['draft']:>6} {totals['todo']:>6}")

    count = int(args[args.index("--next") + 1]) if "--next" in args else 10
    order = [i for i in PILOTS + SHORTLIST if i in ids]
    for cat in CATEGORY_ORDER:
        order += sorted((r["id"] for r in rows if r["category"] == cat), key=lambda i: i)
    seen, todo = set(), []
    by_id = {r["id"]: r for r in rows}
    for i in order:
        if i in seen:
            continue
        seen.add(i)
        if status(by_id[i]) == "todo":
            todo.append(i)
    unknown = [i for i in PILOTS + SHORTLIST if i not in ids]
    if unknown:
        print(f"\n(order lists ids not in the roster: {', '.join(unknown)})")
    names = ", ".join(f"{i} ({by_id[i]['name']})" for i in todo[:count])
    print(f"\nnext {min(count, len(todo))}: {names}")


if __name__ == "__main__":
    main(sys.argv[1:])
