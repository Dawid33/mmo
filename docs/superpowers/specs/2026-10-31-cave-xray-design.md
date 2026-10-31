# See into dug spaces without the cut — design / handoff

Status: IMPLEMENTED 2026-10-31 (uncommitted): `voxel_pull/xray.rs`,
`meshing/interior.rs`, `game::xray_hit`; client unit tests + live look
pending. Context: the commander camera
work of 2026-10-31 (free camera with the cut off; "Hide above" unchanged —
Dawid: never change it; implied ground meshed as solid) — see CLAUDE.md's
`renderer/commander/` paragraph and memory `commander-camera-controls`.

## Intent (Dawid's words)

"When I'm in hide above mode, I can see the inside of underground dug out
areas. I want to see these without it enabled, so ceilings will need to be
very slightly translucent or gone entirely, walls of dug out areas as well."

So: with the cut OFF, the normal view of the world, every dug-out space
under the surface is seen **from the inside, as the cut shows it** — its
floor, its far walls, whatever stands in it — because the terrain between
the camera and that space (the ground and rock above it = its ceiling, the
near side) is removed or nearly invisible. Everywhere else the world looks
exactly as now. A cutaway that follows the fortress, instead of a plane.

**Success:** fly over the fortress with the cut off and the rooms and
tunnels below read as open, lit rooms in place under the landscape; you can
see what is in them and designate in them; the landscape around them is
untouched.

## Goals

- Cut off: any terrain surface lying between the camera and the visible
  interior of a dug space is not drawn (or drawn very faintly), so the
  interior (floor, far walls, contents) shows through it.
- The interiors are lit well enough to read (they sit in the sun's shadow).
- Designation picking agrees with what is seen: the hover lands on the
  floor / wall you see through the removed ceiling.
- No change at all with the cut on ("Hide above" stays exactly as it is).
- Client-local; no sim / server / protocol change.

## Non-goals

- Glowing x-ray of spaces through solid rock (the first version of this
  doc — rejected by Dawid: he wants to see INSIDE the spaces, not their
  shapes through rock).
- Literal translucency of all rock (see Rejected).
- Fog of war for undiscovered natural caves (none exist yet; separate).
- Regions other than home; LOD rings; water; the wasm WebGL2 fallback
  (no storage buffers there — feature off, log once).

## Anti-goals

- A cost that scales with the world rather than the fortress (the client is
  CPU-bound on ~343k near bricks — memory `same-fps-cpu-bound-bricks`).
- Holes in ordinary terrain: nothing may vanish unless a dug interior is
  actually behind it at that pixel.
- Touching "Hide above" behaviour, the camera, or `slint_overlay.rs`
  (another session edits it).
- A raymarcher (memory `brick-far-renderer-unwanted`).

## What counts as a dug space (my call)

Air **strictly below the column's original worldgen surface block**
(`air_y < ColumnGround.surface` for that block column; the recipe is never
changed by digging and is mirrored on the client for every loaded region in
`commander/tiles.rs`). Strictly below, so half-block slopes (air inside the
surface block) don't count. A pit dug from the surface counts.

An **interior face** is a terrain face whose air side is such a cell. These
are exactly the faces that line dug spaces: floors (facing up), ceilings
(facing down), walls (facing into the space).

## How it works (the decided shape; details are the builder's)

1. **Mark interior faces in the mesh.** The mesher sets one reserved bit of
   the quad record (`meshing/records.rs`: w1 bits 28–31 are reserved and
   asserted zero; the layout is locked to `assets/shaders/voxel_pull.wgsl`
   — change both, update `reserved_bits_stay_zero`). The mesh task gets the
   column surfaces for its brick the way `implied_neighbor` gets ground info
   in `meshing/queue.rs::spawn_mesh_tasks`. Each brick also records whether
   it has any interior face.
2. **Interior depth pass.** Before the opaque terrain, draw ONLY the
   interior faces of flagged bricks (back faces culled, as normal) into a
   depth-only target: per pixel, the nearest visible interior surface —
   what you would see of the space if nothing were in front of it. Cost
   scales with the fortress.
3. **Opaque terrain discards occluders.** In the normal terrain fragment
   shader, with the feature on: a fragment that is NOT an interior face and
   lies in front of the interior depth at its pixel (by more than an
   epsilon) is an occluder → discard it, or keep a sparse ordered-dither
   fraction of it (`CEILING_GHOST`, e.g. 0.1) for the "very slightly
   translucent" look. The interior faces themselves draw normally and are
   then visible. The near walls of a space are interior faces seen from
   behind — already back-face culled — so the whole near side opens.
   Entities in the space are no longer hidden by depth.
4. **Light the interiors.** Interior faces skip the sun's shadow and take
   extra ambient (a constant gain, like `commander::underground_lighting`
   does for the underground camera), so a room seen from above isn't black.
5. **Picking.** `designate::update_hover` (cut off → `pick_visible_cell`)
   and `visible::visible_ground` must follow the same rule on the CPU: walk
   the ray through the tile mirror; if it enters dug-space air (by the
   definition above) within reach, the pick is the first solid block AFTER
   that air (the floor / far wall seen through the opening); otherwise the
   first solid block as now. Pure function next to `pick_visible_cell` in
   `game::orders`, unit-tested.

Pass plumbing pointers: near terrain is the pulled-quad path
(`renderer/voxel_pull/`, passes are systems in the per-camera `Core3d`
schedule — Bevy 0.19 notes in CLAUDE.md); the existing depth-prepass
pipeline variant (`voxel_pull/pipeline.rs`, `DEPTH_PREPASS`) and
`far_voxel/pass.rs` (samples `ViewDepthTexture` from a custom pass) are the
nearest precedents. GPU face-direction culling (`voxel_pull_cull.wgsl`)
applies to the interior pass as to opaque. Shadows: the shadow pass must
NOT discard occluders (the ground above still casts its shadow; step 4
handles the interior light instead).

## Controls (my call)

- On automatically whenever the cut is off — this is what the cut-off view
  IS now — with an F3 dev-panel checkbox (`renderer/dev_ui.rs`,
  `DevToggle`) to turn it off for comparison. No new key.
- With the cut on it is inactive (the cut already opens everything at the
  level).
- Tunables as named constants (memory `configurable-tunables-preference`):
  `CEILING_GHOST` (0 = gone, default ~0.1), the occluder depth epsilon, the
  interior ambient gain.

## Known limits (accepted for v1)

- A space is opened only where its interior is visible at a pixel; looking
  at a sealed room from the side shows its interior through the hillside —
  that is the intent.
- Stacked spaces: the upper room's floor is an interior face, so it hides
  the room below it (as real geometry would). The cut still slices levels.
- Bricks at the near-window edge or not yet meshed simply aren't opened yet.

## Rejected alternatives

- **Glow / x-ray through rock** (previous draft): shows shapes, not
  insides; Dawid wants insides.
- **Alpha-blend all rock**: sorted blended layers over the whole near window
  — far too costly here and visually soup.
- **Plane cut only** ("Hide above"): exists; Dawid wants this without it.
- **Hollow view from a camera inside the rock** (works today): requires
  flying into rock.

## Left to the builder

- Depth target format / whether the interior pass reuses the prepass
  pipeline with a shader def; how the per-brick interior bit reaches the
  draw list (must not add a CPU walk over all bricks).
- Discard vs dither pattern details; epsilon.
- Tests: face classification (pure fn), record bit round-trip, the picking
  rule (pure fn in `game::orders`, game tests), WGSL composition tests; a
  self-screenshot smoke of a dug room under terrain if practical
  (`just pull-smoke` style). Live look + perf: ask Dawid before running.

## Builder-check calls (2026-10-31, my call, open to override)

- **Terrain only is lit.** Entities standing in a dug room keep Bevy's
  standard lighting (sun shadow), so they read darker than the boosted
  floor. Lifting them needs a CPU "inside a dug space" tag per entity —
  a follow-up if v1 shows it matters.
- **Pulled paths only.** `GpuDriven` (default) and `GroupedDirect`; the
  native `PerChunk` Mesh/Material path (and the WebGL2 fallback) gets no
  x-ray — feature off there, logged once.
- **Only terrain is an occluder.** Entities, voxel bodies and markers above
  a room keep drawing over it; making them see-through would need the
  interior depth bound into every entity material.

## Open (Dawid's)

None outstanding. Calls marked "my call" above are open to override.
