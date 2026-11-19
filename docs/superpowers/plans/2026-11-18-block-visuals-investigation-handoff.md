# Handoff: block visuals investigation — per-axis textures, blended transitions, non-block decoration

**For:** a fresh session doing research and design only. **No code changes.** The output is one findings
document with a recommendation per topic and a phased proposal, for the owner to choose from.

**Asked by the owner (2026-11-18):** "investigate the following: texture per axis, blocks that influence each
other's textures so that there is a smooth transition between them like the Minecraft modpack Homestead, and
non-blocky objects like flowers and bushes, generally the kinds of textures / blocks and things seen in
Hytale."

**Context:** the trees are about to become whole blocks (`docs/superpowers/plans/2026-11-18-block-trees.md`).
Today's look is "Minecraft with chiseling". The owner wants it to move toward Hytale's look: richer textures,
softer transitions between materials, and plenty of small non-cube decoration. Read `CLAUDE.md` (Client
section, Assets) and `docs/game-design.md` first.

**Deliverable:** `docs/superpowers/specs/2026-11-18-block-visuals-research.md`. Commit only that path.
Work on `develop`, no worktrees. Don't launch the client or run benchmarks without asking: the owner runs their
own benchmarks on this machine.

---

## The four topics

### 1. Texture per axis (oriented blocks)

**Want:** a log lying along x or z shows its rings on the end faces and bark on the other four, like Minecraft's
rotated logs. The same idea covers pillars, hay, beams and any directional block.

**Questions:**
- Where does orientation live? Options: (a) a block id per axis (`oak_log_x`, `oak_log_z`), (b) bits in
  `Block.data`, (c) a separate per-block state. `Block.data` is `[u8; 3]` and fully used on tree blocks
  (`TREE_ID_BITS` 18 + limb 6, `crates/game/src/block.rs:94`). Non-tree blocks have it free. Is "orientation
  only on blocks with no tree id" acceptable once a log is felled or placed by a player? What does placing,
  rotating and serialising each option cost?
- Texture specification: `TextureSpec` (`crates/game/src/registry.rs:24`) has `All` and
  `Faces { top, side, bottom }`. The client maps `BlockId → [top, side, bottom]` array layers
  (`crates/client/src/renderer/block_textures.rs`). What is the smallest change that gives six faces or an
  axis-rotated `Faces`? Does the side texture also need rotating 90° on sideways logs (bark grain direction)?
- The quad record (`meshing/records.rs`, LOCKED layout mirrored by `assets/shaders/voxel_pull.wgsl`) has no
  free bits. `w0` is pos 18 + face 3 + ao 8 + flip, cutout and interior; `w1` is w, h, layer 11 and brick 9.
  Can orientation be resolved entirely at mesh time by picking the layer per face (no shader change)? UV
  rotation would need a bit. Check whether greedy merging (`meshing/binary.rs`) groups quads by layer only, or
  also by something orientation would add.
- What about chiseled voxels of an oriented block (`ChiselData` palettes), and the far LOD (`meshing/lod.rs`,
  `coarse.rs`)?

### 2. Transitions between neighbouring blocks (Homestead-style)

**Want:** where grass meets dirt, sand or stone, or stone meets gravel, the textures blend over the boundary
instead of a hard grid edge. Homestead is the reference. Research what that modpack actually does: connected
textures (CTM / Continuity-style), overlay or edge textures, random rotations and variants, biome tinting.
Name the techniques precisely and don't guess. Also study how Hytale blends terrain materials (its
blending/shading for ground transitions, texture variation, colour tinting).

**Questions:**
- Catalogue the techniques: connected textures by neighbour mask (CTM 47-tile, overlay tiles), splatting or
  blending in the shader with neighbour material weights, per-vertex material weights, random per-block
  variants and rotations, biome colour tint maps, triplanar or world-space UVs with noise-driven blending. For
  each: what the result looks like, what it costs on the mesher, the record format, the shader, texture memory
  and WebGL2, and what art it needs.
- Where would neighbour information come from? The mesher already sees neighbours (`meshing/neighbors.rs`,
  AO in `meshing/ao.rs`). AO's 4×2 bits per quad are a precedent for per-corner data. Could a per-quad
  "neighbour material" layer go into the record? It is full: say exactly what would have to give (a second
  record word, the `w`/`h` bits, a side buffer indexed by quad).
- Greedy merging: per-corner blend data breaks merging just as AO does. Measure (by reading code, not
  benchmarks) how merging handles AO today and what blending would do to quad counts.
- Which pairs blend? A registry property (blend group or priority, as some games use "grass overlays dirt")
  vs. all pairs.
- The bar: what must still hold on the web build (`voxel_pull` GPU path vs. the WebGL2 per-chunk path,
  `config.rs` `VOXEL_NEAR_RENDER`) and on the LOD rings.

### 3. Non-block objects: flowers, grass tufts, bushes, small rocks

**Want:** decoration that is not a cube: cross-quad plants (flowers, tall grass), small models (bushes,
mushrooms, pebbles, fallen branches), placed by worldgen per biome and rendered cheaply in large numbers.

**Questions:**
- Rendering: cross-quads through the existing cutout path (leaves use `cutout`, a separate alpha-tested pull
  pipeline) vs. instanced meshes (Blockbench models already load: `crates/bbmodel`, `renderer/model.rs`) vs.
  a GPU-instanced "decoration" pass with per-instance jitter, scale, rotation, tint and wind sway. Cost at view
  distance; the LOD story (drop, impostor, fade).
- Data model: are they blocks (a cell that is `solid: false` with a non-cube shape: collision, mining, the
  sim's `TileSource`) or not blocks at all (pure client decoration derived deterministically from worldgen and
  never in the sim)? Consider what gameplay needs: harvesting flowers or berries (`items.rs`), a player
  trampling them, pathfinding (`pathfind.rs`), rollback hashing (anything in sim state is hashed and must be
  deterministic). Recommend a split, e.g. purely visual grass tufts vs. harvestable plants as blocks.
- Placement: worldgen is pure (`generate_region`) and `Block.data` / `ColumnGround` exist. Should
  decoration be generated client-side from the same deterministic hash rolls (no store, no network), and what
  would that break (edits, a player clearing a patch)?
- Art: what can come from `just assets` (paid generation; check `--picture-only` first) or
  `tools/creatures`-style TOML sources? List what art is needed in what size.

### 4. "Hytale-like" in general

Look at Hytale's published media and dev blogs (the owner named it as the target look). Catalogue what makes
its world read differently from Minecraft: texture resolution and style, per-block variants, colour grading,
lighting and AO, foliage density, mixed cube and model blocks, sloped or detail blocks, water and so on. For each
item say whether this engine already has it (the voxel chisel, slopes `slopes.rs`, AO, atmosphere
`renderer/atmosphere.rs`, water `renderer/water`), what it would take, and how much it would change the look.
**Do not copy Hytale's assets, names or exact look.** This is about techniques and the general direction; the
game has its own art (`CLAUDE.md`: no recreations of existing games).

---

## Where to look

- Block layer and registry: `crates/game/src/block.rs`, `registry.rs`, `block_props.rs`,
  `assets/blocks/blocks.ron` (textures 16×16? check the sizes in `assets/blocks/`).
- Meshing and records: `crates/client/src/renderer/meshing/` (`records.rs` header first, then `binary.rs`,
  `ao.rs`, `neighbors.rs`, `chunk_mesh.rs`, `lod.rs`), `renderer/voxel_pull/`, `renderer/block_textures.rs`,
  `renderer/voxel_material.rs`, shaders `assets/shaders/voxel_pull.wgsl`, `voxel_texture.wgsl`.
- Earlier research to build on, not repeat: `docs/superpowers/specs/2026-07-09-per-block-texturing-research.md`,
  `2026-07-06-block-abstraction-data-model-design.md`, `2026-09-20-lod-seams-and-texture-scale-findings.md`,
  `2026-09-22-blockbench-model-loading-investigation.md`, and the voxel rendering research files from July.
- Worldgen ground and biomes: `crates/worldgen/src/material.rs` (ground cover per biome), `lib.rs`
  (`ColumnGround`), `habitat.rs`, `trees/` (placement lattice: a model for deterministic decoration
  placement).
- Fuller subsystem notes: `docs/claude-md-full-reference-2026-11-08.md` (grep it; not kept up to date).

## Constraints the findings must respect

- `game`, `server` and `worldgen` stay engine-agnostic (no Bevy). Anything in sim state is deterministic,
  hashed with `StateHash`, and rollback-safe.
- The quad record layout is LOCKED to the shaders. A proposal that changes it must say so plainly and list
  every mirror (`voxel_pull.wgsl`, `voxel_pull_depth.wgsl`, `pack.rs`, the WebGL2 path).
- The web build matters: no shared-memory threads, and WebGL2 has its own per-chunk path.
- UI is Slint only (relevant only if a proposal touches tools).
- Paid asset generation needs the owner's approval before buying.

## Findings document shape

For each of the four topics:
- **What the reference does:** precise technique names, with source links.
- **Options here:** 2–4, each with its look, cost (mesher, record, shader, memory, web, LOD), art needs and
  risks.
- **Recommendation**, and what would need to be true for it to be wrong.

Then:
- **A phased proposal:** small slices that each ship something visible. A likely order: per-axis textures,
  then cross-quad flora, then variants and tint, then blending.
- **Open questions for the owner.**
- **Effort estimate per slice** (S/M/L).
