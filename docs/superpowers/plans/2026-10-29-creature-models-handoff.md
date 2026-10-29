# Handoff: a Blockbench model for every Dwarf Fortress creature

Written 2026-10-29 for a long-running `/goal` session. Read this first, then the spec
(`docs/superpowers/specs/2026-10-29-creature-asset-pipeline.md`, which holds the style guide)
and the roster (`docs/superpowers/specs/2026-10-29-df-creatures/README.md` + `creatures.csv`).

## The goal

Every creature in the DF roster has a model in `assets/models/creatures/` with the animations
its brief calls for, built with the creature pipeline, passing `just creature-check`, and
self-reviewed from its contact sheet. Concretely, `just creature-progress` shows **0 todo** in
every category and every giant **covered by its base**. Placeholders (centaur, chimera,
griffon, wagon) need nothing.

Suggested `/goal` text:

> Build a Blockbench model with animations for every creature in the DF roster, following
> docs/superpowers/plans/2026-10-29-creature-models-handoff.md: templates and their pilots first,
> then creatures in `just creature-progress` order, each through brief → source → build → check
> → contact-sheet self-review → commit, as `draft`. Done when `just creature-progress` shows no
> todo and every giant covered.

**Approval is Dawid's, not yours.** Everything you build stays `status = "draft"`. He reviews
in `just asset-viewer` / the sheets in batches and flips sources to `"approved"` (or sends
notes). Don't wait for him between creatures; don't mark anything approved yourself.

## Where things stand

| Piece | State |
|---|---|
| Roster (767 creatures, categorised, flags, sizes) | done: `docs/superpowers/specs/2026-10-29-df-creatures/` |
| Spec + style guide | done |
| Authoring tool `tools/creatures/build.py` (`just creatures [ids]`) | done; one template: **quadruped** |
| Texture painter (in build.py) | done: palette per part, per-face overrides `<part>_<face>`, rim, speckle, decals |
| Manifest `assets/models/creatures/creatures.json` | written by build.py; it is also the progress tracker (`status`) |
| Brief `tools/creatures/brief.py` (`just creature-brief <id or name>`) | done; heuristics, override when wrong |
| Lint `crates/bbmodel/tests/creatures.rs` (`just creature-check`) | done |
| Contact sheet `tools/creatures/sheet.py` (`just creature-sheet <id>`) | done → `target/creature-sheets/<id>.png` |
| Progress `tools/creatures/progress.py` (`just creature-progress [--next N]`) | done |
| Asset viewer (`just asset-viewer`) | done; `--model --anim --time --yaw --pitch --shot` |
| **Dog** (quadruped pilot) | built, `draft`, awaiting Dawid's review |
| Templates biped, bird, fish, serpent, arthropod, blob; vermin form; giant variants | **not built — your first work** |
| Blockbench opening a generated file | **unverified** (Blockbench isn't installed here); ask Dawid to try the dog once |
| Commit | nothing of the creature work is committed yet (see "Committing") |

## Order of work

1. **Commit what exists** (see "Committing").
2. **Templates, each with its pilot**, in this order. For each: add the template to
   `TEMPLATES` in build.py (bones, cubes from proportions, parametric clips, budget, required
   clip list), write the pilot's source, iterate on its contact sheet until every clip reads.
   - `biped` — pilot **DWARF**. Bones: root, body (hips), torso, head, arm_l/r, leg_l/r (one
     segment each, Minecraft style). Core clips + the worker set the brief lists (mine, chop,
     build, craft, carry, pick_up, put_down, sit, drink, swing, shoot, cheer, cower). Dwarves
     are 2 m in the fortress UI research (`voxel-selection-construction-ui` decisions) — check
     with Dawid if unsure; DF's 60 kg box says ~1.5 m.
   - `bird` — pilot **BIRD_CHICKEN**. Bones: body, head, wing_l/r, leg_l/r, tail. Fliers add
     fly / hover / takeoff / land (wings flap, legs tuck). `brood` for domestic egg layers.
   - `fish` — pilot **FISH_CARP**. Bones: body, head, tail (2 segments), fin_l/r. swim (body
     S-wave through the segments), swim_idle, attack, hurt, death (belly up, hold), flop.
   - `serpent` — pilot **ADDER**. A chain of 4–6 segment bones; slither = a travelling sine
     wave of Y rotations down the chain; coil for lie/sleep.
   - `arthropod` — pilot **SPIDER_CAVE_GIANT**. Body, abdomen, head, 6 or 8 legs (a count
     parameter); walk = alternating tripod / tetrapod gait; web_shoot.
   - `blob` — pilot **CAVE_BLOB**. One or two cubes; motion is scale squash and stretch (the
     slime's render-only squash in `crates/client/src/renderer/avatar.rs` is the reference).
   - **vermin form** — a tiny template (one or two bones, a single `move` loop, optional
     `death`), shared by most vermin whatever their body plan.
   - **giant variants** — a source key like `variant_of = "dog"` + `scale = 2.5` that reuses the
     base creature's source (proportions × scale, same palette/clips, slower cycles), so
     `just creatures` emits `giant_<base>.bbmodel` without a hand-written source each.
     `progress.py` already counts a giant covered once its base exists (`copy_tags_from`); once
     variants exist, make it count the variant instead.
3. **Then creatures in `just creature-progress` order**: the shortlist (pets, livestock, game
   animals, the cavern set), then domestic, wild, civ races, underground, fantastic,
   semi-megabeasts, megabeasts, animal people (biped + the base animal's head and palette), and
   vermin. Group work by template — a batch of quadrupeds goes fast once the template is right.

## Per-creature loop

```sh
just creature-brief "<name or DF id>"      # size, suggested template, clips, attacks
$EDITOR tools/creatures/src/<df_id>.toml   # lowercase DF id: dog.toml, bird_chicken.toml
just creatures <df_id>
just creature-check
just adapter=Intel creature-sheet <df_id>  # read target/creature-sheets/<df_id>.png
```

Fix whatever the sheet shows (look at it — every tile, against the clip's intent: is the head
where it should be, do the legs swing the right way, does it lie on the ground, not through
it), rebuild, re-sheet. Then commit that creature (or a small batch).

- **Source file** — the dog (`tools/creatures/src/dog.toml`) is the worked example: template,
  `status`, `[proportions]` (any template default can be overridden, gait tables too), extra
  `[[cubes]]` **relative to their bone's pivot**, `[palette]` (one colour per part, optional
  `<part>_<face>`), `[[decals]]` (face texels as seen from outside, row 0 at the top), and
  creature-only clips as keyframe lists under `[clips.<name>]`.
- **Size** — the brief prints the true-volume box; the style guide wants chunky, slightly
  exaggerated proportions, so models run larger than that box (the dog's is ~2×). Keep
  relative sizes believable across creatures (a cat smaller than a dog, a horse much bigger).
- **Clips** — the template's required list is enforced by the lint; the brief's extras
  (fly set, swim, ridden, roar, web_shoot, breathe, brood, worker set) are to be made when the
  template supports them. Its "Optional" list (secondary attacks, spit) can be skipped.
- **Brief heuristics are guesses.** If the body plan maps to the wrong template (bats, harpies,
  centaur-like creatures, many-headed hydras), pick the right one by hand and note why at the
  top of the source.

## Conventions you must keep

- **Never hand-edit a `.bbmodel`**: the next build overwrites it. Change the source or the
  template. A Blockbench touch-up from Dawid must be carried back into the source.
- **Bones bind by UUID** derived from `(template, bone name)`. Renaming a template bone
  changes it for every creature of that template; rebuild them all (`just creatures`) and
  re-run the check and the pilots' sheets.
- **Changing a template changes every creature built from it.** After a template edit:
  `just creatures`, `just creature-check`, and re-sheet the pilot plus one or two others.
- **Facing −Z, feet on y = 0, 16 units = 1 block = 1 m.** Signs (right-handed, XYZ Euler as the
  loader applies them): +X rotation swings a hanging leg's foot **forward** and lifts a
  forward-pointing head's **nose up**; a +Z-pointing tail goes **down**. Positions in clips are
  offsets in model units.
- **Loop modes**: gaits and states `loop`, one-shots `once`, settle-into poses (`death`,
  `sit`) `hold`. Loops must end where they start (catmull-rom keys, last = first).
- **Budgets** live with each template and are linted; raise one only with a reason.
- **Deterministic output** — no randomness without a fixed seed; rebuilding an unchanged
  source must give the same bytes (check with `git diff` after `just creatures`).

## Repo rules that bite here

- **No worktrees**; work on `develop` in the main checkout (CLAUDE.md).
- **Other sessions edit this checkout at the same time** (on 2026-10-29 one was building the
  designation toolbar in `crates/client/src/renderer/slint_overlay.rs` and editing
  `crates/game/src/orders.rs`). Stage only your own files (`git add <paths>`, never `-A`), and
  never revert or "fix" changes you didn't make. If the client stops compiling because of
  someone's half-finished edit, wait or work on sources/templates (the tool and lint don't need
  the client).
- **Builds go through `just`** (it pins to 80% of cores). The viewer build is Bevy-sized; the
  sheet recipe builds it incrementally.
- **GPU**: shots default to the Intel iGPU (`adapter=Intel`); the RX 6700 eGPU is often
  unplugged and the NVK T600 fallback is slow. A screenshot run opens a window briefly.
- **Don't run the whole test suites**; `just creature-check` (instant) and `just smoke` (~20 s)
  are the checks. Tag any new test slower than 100 ms `#[ignore = "slow: >100ms — run via just test-full"]`.
- **Keep the justfile and CLAUDE.md in sync** when you add or change a recipe (CLAUDE.md has a
  "Creature models" paragraph — extend it as templates land).

## Committing

The creature work so far is uncommitted and mixed with another session's changes. Files that
are this work: `tools/creatures/`, `assets/models/creatures/`, `crates/bbmodel/tests/creatures.rs`,
the spec and this handoff, `docs/superpowers/specs/2026-10-29-df-creatures/`, the asset viewer
(`crates/client/examples/asset_viewer.rs`, `crates/slint-ui/ui/{asset-viewer,index}.slint`,
`crates/slint-ui/tests/asset_viewer.rs`, `crates/slint-ui/src/lib.rs`, `crates/slint-ui/build.rs`,
`crates/client/src/renderer/mod.rs`, the `file_watcher` feature in `crates/client/Cargo.toml`
+ its `Cargo.lock` additions), and the creature/viewer recipes in the `justfile` and
paragraphs in `CLAUDE.md`. **`crates/client/src/renderer/slint_overlay.rs` holds both** the
generic `SlintLayerPlugin` refactor (this work) and the other session's toolbar code — if its
toolbar work is still uncommitted, commit `slint_overlay.rs` together with that session or
after it has committed, never a partial hunk guess. Use `git add -p` for the justfile and
CLAUDE.md if they also carry someone else's hunks. Commit message style: conventional
(`feat(creatures): …`), ending with the attribution lines the session gives you.

## Known gaps to keep in mind

- `just creature-sheet` crops the viewer's middle 800×700 px; a very long or tall creature
  (dragon, sea serpent) is framed from its bounds, so it still fits, but check the crop.
- The manifest's `stride`/`speed` are template estimates (2·leg·sin(swing)); nothing in the game
  consumes them yet. Keep them sensible; wiring creatures into the sim is a separate project.
- Animal people are 181 sources; generate them the way giants are planned (a variant keyed on
  the base animal: `animal_person_of = "badger"`) rather than by hand if the biped template
  allows a swapped head — design that when you get there.
- DF 0.47 raws are the source of truth for briefs (`DF_RAWS` overrides the path). Creatures
  added in DF v50+ are not in the roster.
