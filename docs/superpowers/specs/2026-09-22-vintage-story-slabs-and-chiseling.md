# How Vintage Story does slabs and chiseling (research)

2026-09-22. Question: how do Vintage Story slabs work, and how do they relate to
its voxel-level chiseling? This came up because our slab (`BlockShape::BottomSlab`,
see `2026-09-22-block-and-half-block-worldgen.md`) is a special case that can't be
edited at the voxel level.

**Sources.** I read the C# code, on `master` as of 2026-09-22, in
`anegostudios/vssurvivalmod`, `vsapi` and `vsessentialsmod`. The block JSON came from
the 1.22.7 server tarball. Labels: **[SRC]** = verified in source or JSON; **[WIKI]** =
from the wiki or forums; **[INF]** = inference.

## 1. Slabs are shaped blocks, not microblocks [SRC]

- A slab is an ordinary JSON block. Its variant groups are `rot` (up / down / north /
  south / east / west) and `cover` (free / snow). One variant exists per orientation,
  e.g. `cobblestoneslab-granite-down-free`. Its shape is a single element
  `[0,0,0]→[16,8,16]`, rotated per variant.
- Collision and selection use one half-cell box, rotated by variant.
- **Placement** (`BehaviorOmniRotatable.TryPlaceBlock`, default "auto" mode). Only the
  clicked face and the hit point inside that face decide the result:
  - Clicking the centre (a 0.6×0.6 square) of a top face gives a bottom slab in the
    cell above.
  - Clicking near an edge gives a vertical slab against that edge.
  - Clicking the upper or lower band of a side face gives a top or bottom slab.
  - Two other modes, "horizontal" (by hit Y) and "vertical" (by the player's yaw), are
    toggled by putting the slab in the crafting grid.
- **No merging.** Two slabs never combine into a full block. Slabs are not replaceable,
  so a placement always goes into the neighbouring cell. A slab drops one bottom slab.
  The only placement that merges is snow layers (`BlockLayered`: layer n → n+1, and
  7 → snow block).
- **Placing on top of a bottom slab** puts the block in the cell above, floating half a
  block above the slab's top. VS accepts this. It only refuses to attach blocks that need
  support to the slab's open face (`CanAttachBlockAt`). A mod ("Place On Slabs") exists to
  fix it and calls the fix "pretty intrusive to the engine".
- **Selection is shape-accurate** (`AABBIntersectionTest`). The ray steps through whole
  cells and tests every selection box of the block in that cell. It returns the face, the
  `HitPosition` inside the block, and `SelectionBoxIndex`. Aiming at the empty half of a
  slab's cell misses the slab.
- **Stepping.** `StepHeight` = 0.6, so a half slab is climbable and a full block is not.
  The step is a smooth rise inside the physics (`TryStepSmooth`): about 3 blocks/s walking,
  6 sprinting and 1.5 sneaking. It is not a teleport, and the camera does nothing extra.
- **Rendering.**
  - Side textures are cropped, not squashed: a bottom slab's sides use the lower half of
    the texture, anchored to the world grid.
  - Only the slab's full face counts as opaque for culling and ambient occlusion.
  - A bottom slab holds liquid up to half height on its sides.
    [INF] Fluids live in a separate layer, so water and a slab can share a cell.

## 2. Chiseling (microblocks) [SRC]

- **Conversion.** A chisel click on a chiselable block (`ItemChisel`) replaces it with a
  different block, `chiseledblock`, which carries a block entity. A slab can be chiseled
  (`canChisel: true`). With `chiselShapeFromCollisionBox`, the microblock is seeded from
  the slab's collision box, so a bottom slab becomes one cuboid `0..16 × 0..8 × 0..16`.
- **Which blocks can be chiseled:** full cubes by default, or blocks that opt in with
  `canChisel`. Also:
  - a world setting (`microblockChiseling` off / stonewood / all);
  - a hardness limit (resistance ≤ 100);
  - pre-made ruin microblocks cannot be chiseled further.
- **Storage** (`BlockEntityMicroBlock`) is a **greedy cuboid list** on the 16³ grid.
  - Each cuboid is one `uint`: 4 bits each for min x/y/z and for max−1 x/y/z, plus 8 bits
    of palette index.
  - A per-block material palette (`int[] BlockIds`, up to 256 materials).
  - Cached per-face flags: solid, almost solid, and emits AO.
  - Saved and sent over the network as a tree of int arrays. An edit travels client →
    server as a small voxel packet; the server then resends the whole block entity to
    clients.
- **Editing always round-trips through a dense grid.** The cuboid list is expanded to a
  16³ bool array plus a material array, edited, then re-merged (`RebuildCuboidList`: scan
  x, y, z and grow each box along X, Y and Z while every voxel is filled, unvisited and the
  same material). The result depends on scan order and isn't a minimal box set, but it is
  deterministic.
- **Collision = selection = one box per cuboid.** There is no further merge and no hard
  limit: a checkerboard is 4096 boxes. While a chisel is held, the selection switches to
  one box per brush-sized voxel (1, 2, 4 or 8).
- **Mesh.** An 18³ working array includes the neighbouring microblocks' face planes, so
  faces between chiseled blocks are culled per voxel. Faces are merged greedily per plane.
  UVs are world-anchored. A carved surface can use an `inside-*` texture.
- **Reverting.**
  - Removing the last voxel turns the block into air.
  - A chiseled block that is completely full again stays a microblock; it never reverts
    to its original block.
  - Breaking a chiseled block drops it with all its data.
- **Worldgen.** Procedural terrain in VS is only whole cubes plus snow layers. Terrain
  never contains slabs or microblocks; they appear only in hand-built schematics (ruins,
  traders). The softer look comes from decoration: snow layers, loose stones, grass tufts
  and grass-overlay textures.

## 3. What this means for us

- **VS keeps slabs as a special case.** They become editable by converting to microblocks
  on the first chisel. That is our "seed chisel data from `derive_voxels` on first carve"
  option.
- **VS's microblock storage is the compact representation we lack.** A slab becomes one
  `uint`. Its storage is canonical because it is always re-derived from a dense grid by a
  fixed scan. That answers our `ChiselData` comment that "a merged-cuboid list lacks"
  invertibility and hashability: undo swaps the whole chunk value anyway, and
  re-deriving from the dense grid makes the hash a pure function of the voxels.
- **VS's collision (one box per cuboid) is not something we should copy.** Our character
  controller's seam fixes rely on parry `Voxels` shapes, which handle internal edges
  between flush cells. A compound of cuboids would bring the chunk-border jitter back
  (see the KCC seam-jitter fixes). Cuboids should instead become cells on our
  block / half-block / voxel grids.
- **Worth copying regardless of storage:**
  - shape-accurate raycast returning the hit point inside the block;
  - placement by region of the clicked face;
  - a physics step-up that rises over time instead of popping.
