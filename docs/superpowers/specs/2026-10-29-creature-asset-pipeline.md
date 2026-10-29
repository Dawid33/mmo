# Creature asset pipeline (Blockbench models + animations)

2026-10-29. Goal (Dawid): a Blockbench model with animations for every Dwarf Fortress creature
(roster: `2026-10-29-df-creatures/`), made one by one through one repeatable process. This spec
is that process, the style guide every model follows, and the tools that run it. The first slice
— this spec, the authoring tool, the quadruped template and the **dog** as its pilot — is built
alongside it.

## Constraints that shaped it

- **No model service and no required manual work.** The service `assault_bot.bbmodel` came
  from needs hand intervention and is not usable; `just assets` (Leonardo → TRELLIS) makes
  static GLBs with no bones, so it cannot make animated creatures. Claude writes every model;
  Dawid reviews at a gate. Blockbench touch-ups are allowed, never required.
- **`.bbmodel` stays the format** (not glTF/Blender): `crates/bbmodel` + `BbAnimationPlayer`
  already load and play it, and the blocky look suits the voxel world.
- **Animations bind to bones by group UUID** (`bbmodel::animation::read`), not name. Every
  model of one template therefore uses the template's bone UUIDs, so a template's clips mean
  the same thing on every creature built from it.
- **Seen from the commander camera:** high, top-down, at a distance. Silhouette and motion
  read; finger and face detail do not.

## Style guide

| Rule | Value |
|---|---|
| Units | 1 Blockbench px = 1 world unit = 1/16 block; 1 block = 1 m (`UNITS_PER_METER` 16) |
| Size | from DF `BODY_SIZE` (cm³ ≈ g): pick dimensions whose box volume is the right order; chunky and slightly exaggerated (bigger head, thicker legs) beats exact |
| Facing | the creature looks down **−Z** (Bevy's forward, Blockbench's north) |
| Ground | the feet rest on y = 0 in the rest pose; the model is centred on x = 0, z ≈ 0 |
| Pivots | every bone's origin sits at its joint (hip, shoulder, neck, tail root) |
| Geometry | cubes only, whole-unit sizes (half units only where a part would vanish); per-template budgets — quadruped ≤ 24 cubes, ≤ 12 bones |
| Texture | one embedded PNG, **1 texel per unit** (16 per block, like the terrain), nearest-sampled; per-face UVs laid out by the tool |
| Colour | a small palette per creature; up faces lighter, down faces darker or belly colour, a darker 1-texel rim on each face, faint deterministic speckle; eyes/nose as decals |
| Clip names | lowercase, from the template's list (`idle`, `walk`, `run`, `attack`, `hurt`, `death`, `lie`, `sleep`, `eat`, …) plus creature extras; game code asks for clips by these names |
| Loop modes | movement and states `loop`; one-shots `once`; `death` and settle-into poses `hold` |
| Stride | every gait clip records how far one cycle carries the creature (units) in the manifest, so the game can match playback speed to ground speed and feet don't slide |
| Determinism | regenerating a model from its source is byte-stable (UUIDs derived from names, fixed texture layout and noise), so diffs show real changes |

## Pieces

1. **Templates** (`tools/creatures/templates.py`) — one function per body type that turns a
   creature's proportions into bones (fixed names and UUIDs), body cubes, and the template's
   clips, all parametric (leg swing, cycle time, bob, lunge). Planned set: quadruped (built),
   biped humanoid, bird, fish, serpent, arthropod, blob.
2. **Creature source** (`tools/creatures/src/<df_id>.toml`) — the template, proportions,
   palette, decals, extra cubes (ears, snout, horns), clip tweaks, creature-only clips. Small and
   reviewable; it is what gets edited.
3. **Authoring tool** (`tools/creatures/build.py`, `just creatures [ids…]`) — source → template
   → `assets/models/creatures/<df_id>.bbmodel` (format 5.0, `groups[]` + `outliner`), with the
   **texture painter** packing every cube face into one PNG and painting it from the palette. It
   also rewrites the **manifest** `assets/models/creatures/creatures.json`: per creature its
   model, template, collider box, required clips with loop modes, and per-gait cycle + stride.
4. **Brief generator** (`tools/creatures/brief.py`, `just creature-brief <df_id>`) — the row
   from `2026-10-29-df-creatures/creatures.csv` plus the creature's `ATTACK` tokens from the
   local DF raws: size, habitat, suggested template, required clip sets from flags (FLIER →
   fly set, AQUATIC → swim, MOUNT → ridden, WEBBER → web_shoot, one `attack_*` per attack).
5. **Lint** (`crates/bbmodel/tests/creatures.rs`, `just creature-check`) — loads every model
   in the manifest with the real parser: no reader warnings, every required clip present with
   its loop mode, budgets kept, size matching the manifest's collider.
6. **Review views** — `just asset-viewer` (list, play, scrub) and its `--shot` mode for
   screenshots without a human.

## Per-creature process

1. **Brief** — `just creature-brief <df_id>`; note anything unusual (seven heads, breath, shell).
2. **Reference** (optional) — a Leonardo front/side/top sheet for proportions and palette, for
   fantastic or unfamiliar creatures only; never the paid image-to-3D step.
3. **Source** — write `src/<df_id>.toml` from the template: proportions, extra cubes, palette,
   decals, clip tweaks and extra clips.
4. **Generate** — `just creatures <df_id>`.
5. **Self-review** — `just creature-check`, then asset-viewer shots: the rest pose from the
   commander's angle and from the side, and frames of every clip. Fix before showing anyone.
6. **Review gate** — Dawid opens it in `just asset-viewer`: approve, or notes back to step 3
   (or a Blockbench touch-up, which then has to be carried back into the source, or the
   next generate overwrites it).
7. **Register** — the manifest entry is written by the tool; on approval set `status =
   "approved"` in the source (the manifest carries it: it is the progress tracker), rebuild,
   commit.

**Order:** templates with their pilots first (dog, dwarf, chicken, carp, adder, giant cave
spider, cave blob), then batches per template in game priority (the roster's shortlist, then
other real animals, then cavern, fantastic and megabeast creatures). Vermin get one `move` loop.
Giants are the base model scaled (no new model). Animal people are the humanoid template with an
animal head and texture. ~400 models rather than 767.

## Quadruped template (first slice)

Bones: `root` (ground, for falls and death) → `body` (its centre) → `head` (neck joint),
`tail` (tail root), `leg_fl`, `leg_fr`, `leg_bl`, `leg_br` (hips). Legs are children of the body
so poses (sit, lie, eat) carry them; the walk's body bob is small enough not to lift the feet.

| Clip | Mode | Motion |
|---|---|---|
| `idle` | loop | breathing, head looks left/right, tail sway |
| `walk` | loop | diagonal pairs swing ±A, small bob, head nod, tail sway |
| `run` | loop | bound: front pair and back pair alternate at ±A_run, body pitches and bobs |
| `attack` | once | head up then bite down while the body lunges forward and back |
| `hurt` | once | jolt back and sideways, head up |
| `death` | hold | falls onto its side, legs stiff |
| `lie` | loop | legs folded, body lowered, slow breathing |
| `sleep` | loop | `lie` with the head down on the forepaws, slower breathing |
| `eat` | loop | head down to the ground, nibbling |

Creature extras live in the source (the dog adds `sit`).

## Open, decided later

- Wiring the manifest into the sim/renderer (DF state → clip, ground speed → playback speed) —
  when creatures become entities; until then the viewer is the consumer.
- Per-body-type IK or foot locking — not planned; stride matching should be enough at this scale.
- Variants (giants, colour morphs, animal people) as generator options, after the templates.
