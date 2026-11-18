# Block visuals research — per-axis textures, transitions, decoration, "Hytale-like"

**Date:** 2026-11-18
**Status:** Research and design only. Nothing in here is built; the owner picks from the proposal below.
**Brief:** `docs/superpowers/plans/2026-11-18-block-visuals-investigation-handoff.md`.
**Builds on:** `2026-07-09-per-block-texturing-research.md` (position-derived UVs, as built),
`2026-09-20-lod-seams-and-texture-scale-findings.md` (LOD texture scale), `2026-09-22-blockbench-model-loading-investigation.md`,
and `docs/superpowers/plans/2026-11-18-block-trees.md` (trees become whole blocks, which is about to happen).

## Summary

| Topic | Recommendation | Record / shader change? | Effort |
|---|---|---|---|
| 1. Per-axis textures | **One block id per axis** (`oak_log` = Y, plus registry-generated `oak_log_x` / `oak_log_z`), a per-face layer table, and rotated bark layers generated when the layers are built | None. Mesh-time layer choice only | S |
| 2. Transitions | **Overlay fringe quads** (the Homestead and Hytale technique): one extra cut-out quad on each boundary block's top face, with a mask-indexed fringe layer, driven by a `blend` registry property. Before that, **random variants and rotations** plus **macro colour noise** in the shader | No record layout change. Overlays need a depth-biased cut-out pipeline variant. Variants need a small per-layer table and an integer block origin in the shader | Variants S, overlays M–L |
| 3. Decoration | **A split.** Purely visual tufts and pebbles are derived on the client from hash rolls and never enter the sim. Harvestable plants are sim blocks with a non-cube `shape`. Both are drawn by **one new instanced decoration pass**, never through the quad record | None to the record. A new pass with its own shader (instance attributes, so it works on WebGL2) | M + M |
| 4. "Hytale-like" | The highest-value differences, in order: foliage density (3), variants and rotations (2), tint, transitions (2), colour grading, leaf planes and sway, then 32 px art and coloured block light (both large) | — | — |

The common thread is that the locked 8-byte quad record never needs to change. Everything above is resolved either
at mesh time by choosing the **layer**, in the shader from the **position** (which it already decodes), or in a
**separate pass**. That keeps both GPU paths and the WebGL2 per-chunk path in step without a format migration.

---

## What the engine has today (the facts the options rest on)

**Record.** The source of truth is `crates/client/src/renderer/meshing/records.rs:4-5`:
`w0: x:6 | y:6 | z:6 | face:3 | ao0..3:2×4 | flip:1 | cutout:1 | interior:1`,
`w1: w:6 | h:6 | layer:11 | bx:3 | by:3 | bz:3`. Both words are 32/32 bits.
- Mirrors:
  - `assets/shaders/voxel_pull.wgsl` (decode at 175-193, 330, 358, 362)
  - `assets/shaders/voxel_pull_depth.wgsl` (177-193, 226; its CUTOUT variant samples the texture, with copies of the `FACE_U_MASK` and `FACE_V_MASK` tables at 29-39)
  - `meshing/chunk_mesh.rs::records_to_mesh_data` (68-93), which expands records into **`pack.rs` vertices** for the WebGL2 per-chunk path. That format is `x:9|y:9|z:9|face:3|ao:2` + `w:6|h:6|layer:16`, decoded by `assets/shaders/voxel_texture.wgsl:73-92, 260-279`. It drops the cutout, interior and flip bits.
- Bits that could be reclaimed if ever needed:
  - flip (the shader can recompute it from the four AO values, `ao.rs:78`)
  - cutout (the shader never reads it; the cutout range comes from `face_ends`, so it is needed only on the CPU before packing)
  - about 3 high bits of `layer` (182 layers are used out of 2048)
- Face values 6 and 7 of the 3-bit `face` field are unused.
- Stale comments, for whoever touches these files next:
  - `records.rs:9` says "56 layers" (it is 182).
  - The `reserved_bits_stay_zero` test (`records.rs:337`) still calls w1 bits 28-31 "reserved" although the brick bits take 23-31.
  - The `voxel_pull_depth.wgsl:6` header leaves out the interior bit.

**Orientation never reaches the mesher.**
- `game::Voxel` is `{ block: BlockId }` (`crates/game/src/voxel.rs:368-371`), and `BrickView::voxels` builds `Voxel::new(b.id)` (`voxel.rs:295-314`). So `Block.data` is dropped before meshing.
- `Block.data: [u8; 3]` is fully used on every tree block: 18-bit tree id + 6-bit limb (`block.rs:94-129`). It is zero everywhere else.

**Texture choice.**
- `BlockTextureLayers: BlockId → [top, side, bottom]` (`renderer/block_textures.rs:19`), with `pack::layer_for_face` (`pack.rs:61-67`) picking one per face, and `quads_to_records` (`records.rs:131`) writing it.
- `side_to_top()` (`block_textures.rs:28-35`) maps each side layer to its block's top layer. It drives the distant "riser fade" (`voxel_pull.wgsl:104-116, 552-560`) and the cutaway cap (`:447`).
- The texture array is 182 layers of 16×16 `Rgba8UnormSrgb`, with a full mip chain built in linear light (`mod.rs:1105-1170`), a linear ×16 anisotropic sampler, and `crisp_uv` texel snapping on magnification (`voxel_pull.wgsl:371`).
- UVs are position-derived, one repeat per block (`voxel_pull.wgsl:354-357`; `texture_position` at :265 is **chunk-local**, plus an LOD phase). Side faces are upright: ±X uses U=z, V=−y; ±Z uses U=x, V=−y; ±Y uses U=x, V=z.
- There is **no UV rotation, no variant and no tint.**

**Greedy merging.**
- The merge key is `block << 32 | ao[4 bytes] | interior` (`binary.rs:308-310`). Bits 49-63 of the u64 key are free.
- Differing AO prevents a merge (test `differing_ao_does_not_merge`, `binary.rs:618-633`).
- Anything that changes a face's layer must either be a function of the block id or join the key.

**Neighbours.** The mesher sees a 34³ padded voxel grid per 32³ brick, which is 2×2×2 blocks. The shell is filled from all 26 neighbour bricks (`neighbors.rs:12-24, 57-82`). That is one voxel of every in-plane, edge and corner neighbour block: enough to tell each top face's eight horizontal neighbour materials.

**Far LOD.**
- It is the same `binary::mesh_quads` with no cutout (`lod.rs:55-100`), the same `quads_to_records`, and the same layer table.
- A downsampled cell's material is the most frequent top-most solid id (`game::lod::merge_group`). AO fades out by scale 16.
- So only what is part of the **BlockId** survives into the LOD.

**Cut-out.**
- Leaves are `RenderMode::Cutout`. Their records sort after the opaque ones (`face_ends[7]`) and are drawn by a separate `AlphaMask3d` pull pipeline with no back-face culling, plus CUTOUT depth and shadow variants (`voxel_pull/pipeline.rs:96-152, 308-345`).
- The WebGL2 per-chunk path has one `AlphaMode::Mask(0.5)` material and no separate cutout pipeline (`mod.rs:1179-1183`).

**Sim and world.**
- There is no plant, flower or decoration concept anywhere; `mc-import` even discards Minecraft `plant` blocks (`crates/mc-import/src/table.rs:17`).
- Block properties are `non_solid`, `cutout`, `half_collider`, `liquid`, `hardness` and `drops` (`block_props.rs:53-65`). There is no shape flag.
- Pathfinding treats any non-solid block as passable (`pathfind.rs:38-44`). `blocks_ray` is "not air and not liquid", so a non-solid block still stops the pick ray and can be mined.
- The only non-block item is `stick` (`items.rs:13`).

**Client knowledge of climate.**
- The client gets `ColumnGround` (surface, strata, water; **no biome**; `game/src/ground.rs:37-59`) in each region snapshot, and one `WorldMap` of `TileSummary` per region (biome, `temperature_c`, `precip_mm`, …; `game/src/world_map.rs:10-47`) once on connect.
- The forged world (`biome_at`, `column_climate`, `HabitatSource`) is **not** on a networked client.
- Edits arrive as `SetBlocks` and are applied at `renderer/bridge.rs:1168-1189`. Water (`renderer/water/`) is already client-derived geometry that is invalidated per edited chunk. That is the precedent for client-derived decoration.

**Look.**
- One fixed sun with 4-cascade CSM (1 on WebGL2) and a blue ambient (`atmosphere.rs`).
- Exponential-squared distance fog, a procedural gradient skybox, baked 4-corner vertex AO, SMAA.
- Bevy's default tonemapping and exposure. No bloom, no `ColorGrading`, no block light or sky light.
- Static, untextured, unanimated water.
- Slopes are chisel staircases from worldgen (`worldgen/src/slopes.rs`).
- Every block texture is 16×16, made by `just block-textures` / `just tree-textures` (Pillow, deterministic).

---

## 1. Texture per axis (oriented blocks)

### What the reference does

- **Minecraft:** logs, pillars, hay and bone blocks have a blockstate property `axis=x|y|z`. The model for `x`/`z` is the same cube model rotated with `"x": 90` (and `"y": 90`), so the end texture lands on the axis faces and the side texture is **rotated** on the faces around it. The state comes from the face the player clicked when placing. Source: https://minecraft.wiki/w/Blockstates_definition.
- **Hytale:** has per-face cube textures and rotation fields (`randomRotation`, `variantRotation`) according to a reverse-engineered protocol listing. That source is unofficial (https://hydex-pre.pages.dev/packages/com-hypixel-hytale-protocol/blocktype).
- **Vintage Story:** expresses the same thing as block variants (`-ud`, `-ns`, `-we`) with rotated shapes (https://apidocs.vintagestory.at/json-docs/jsondocs/Vintagestory.ServerMods.NoObf.BlockType.html).

### Options here

**(a) One block id per axis.**
- **How:** `blocks.ron` gains `oriented: true` (or a `TextureSpec::Axis { end, side }`). At load the registry expands one definition into three ids: the base id is the Y variant (`oak_log`, existing saves keep their meaning), and generated `oak_log_x` / `oak_log_z` get new ids. `BlockId` is a `u16` and 168 are used, so 7 logs + pillar + hay + bone = 10 families cost 20 ids.
- **Mesh:** the per-block layer table becomes six faces (or `[end, side, side_rot]` plus the axis). `quads_to_records` picks the layer per face. **No record, shader or WebGL2 change.** The merge key already holds the block id, so an X log never merges with a Z log.
- **Chisel:** a chiselled voxel's palette entry is a BlockId, so a chiselled X log keeps its orientation for free.
- **LOD:** the far LOD picks a cell's BlockId, so a lying log is still lying at a distance.
- **Sim:**
  - Placing: the player's place intent picks the variant from the clicked face's axis. That is a pure function in `game` over the registry.
  - Drops: all three variants drop the family's base item (`drop: Same` maps to the base id; a new field `family` or a naming rule).
  - Items: there is no new item.
  - Hashing: ids are already hashed. Rotating a placed block is a block swap, which is an ordinary undoable edit.
- **Tree id:** it stays in `data`. A felled or placed log keeps no tree id (tree 0) anyway, and a worldgen log keeps it untouched. Block-trees' `rasterize` picks the variant from the segment's dominant axis (a lying branch-sized log becomes `oak_log_x`).
- **Cost:** a registry expansion (`game::registry`), the layer table and its riser fade (`side_to_top` must map an X log's side to its own end or bark, not to the Y log's top), placement, and drop mapping.

**(b) Orientation in `Block.data`.**
- `data` is full on tree blocks, so this is only possible for non-tree logs. A worldgen tree's lying limb could not be oriented without shrinking the tree id.
- Worse, orientation would have to travel past `BrickView::voxels`: `Voxel` would grow (it is serialised and hashed sim state; `voxel.rs:368` derives `StateHash`), the merge key would need the bits, and the LOD's `merge_group` would have to carry them.
- More plumbing than (a), for one fewer registry row.

**(c) A separate per-block state store** (for example a map of oriented cells per chunk). It has the same plumbing as (b), plus a new rollback-wrapped structure and a new delta kind. Rejected.

### Bark grain on sideways logs

Yes, the side texture needs rotating. Bark grain runs along image V, which is world Y on side faces, so:

| Variant | Faces showing rings | Faces needing bark rotated 90° | Faces with normal bark |
|---|---|---|---|
| X log | ±X | ±Y, ±Z (grain along U) | — |
| Z log | ±Z | ±X (grain along U) | ±Y (grain along V = z) |

There are two ways to do it:
- **Rotated layers** (recommended): `build_layers` rotates a side texture by 90° on the CPU and stores it as an extra array layer (about 7 bark + 3 other layers). No shader change, and the WebGL2 path is identical.
- **A UV-rotate bit:** there is no free bit unless `flip` or `cutout` is reclaimed (see above). That changes the locked record and every mirror for something the layer does already. Rejected.

### Recommendation

**(a), with rotated layers.** It is the only option where orientation reaches chisel, merging, the LOD and the WebGL2
path with no plumbing, and it leaves `Block.data` to the tree id.

**It would be wrong if:** many more directional states arrive (stairs facing four ways, doors with hinge sides,
furnaces facing), because ids would multiply (N families × states). At that point a real block-state field that
reaches the mesher (`Voxel` growing a state byte) is cheaper than id expansion. Logs, pillars, hay and bone do
not justify it.

---

## 2. Transitions between neighbouring blocks

### What the references do (sourced)

- **Homestead** (Minecraft 1.20.1 Fabric modpack by Tokimi: https://modrinth.com/modpack/homestead,
  https://www.curseforge.com/minecraft/modpacks/homestead-cozy). Read from the pack's own `Homestead 2.0.1.mrpack`:
  - The terrain look comes from **one resource pack, "Stay True"** (https://www.curseforge.com/minecraft/texture-packs/stay-true), drawn by the **Continuity** mod (https://modrinth.com/mod/continuity), which implements OptiFine's CTM format on Fabric.
  - The shaders it ships (Complementary Reimagined and others) are **off by default** (`enableShaders=false`).
  - It has no Better Grass mod.
  - Stay True is a **16×16** pack.
- **The grass edge** is an **OptiFine CTM `method=overlay`** rule (https://optifine.readthedocs.io/ctm.html):
  - File `ctm/_overlays/grass_block/grass_block.properties` with `matchBlocks=` about 150 ground blocks (dirt, path, podzol, stone, ores, mud, …), `faces=top`, `connect=block`, `connectBlocks=grass_block`, `tiles=0-16` (the **17-tile overlay set**: edges and corners), `tintIndex=0`, `tintBlock=grass_block`, `layer=cutout`.
  - That is: on the **top face of a dirt (etc.) block next to grass**, a cut-out fringe tile is drawn, chosen by which neighbours are grass and **tinted with grass's biome colour**. Grass appears to creep over the edge. Nothing is blended; the soft line is drawn into the fringe art.
  - Stay True has 62 overlay rules (gravel onto ground, dirt onto stone, stone types onto each other, plank onto plank), 17 `horizontal`, 6 `random`, 3 `vertical` and 2 full `ctm`.
- **Random variants with rotation, all in vanilla:** `blockstates/grass_block.json` lists 36 equally weighted entries (9 textures × `"y": 0/90/180/270`). `dirt_path` uses weights (200 for the base, 5 for rare variants). About 150 blockstates are overridden like this.
- **Biome colormaps:** `grass.png` and `foliage.png` are 256×256, indexed by temperature × downfall and multiplied into grey textures (https://minecraft.wiki/w/Biome). Stay True replaces them.
- **Hytale (official),** "Exploring Hytale's block tech" (https://www.hytale.com/news/2019/2/exploring-hytale-s-block-tech):
  - **Transition textures:** an extra quad at the block boundary, defined per block with a list of valid target blocks (grass, sand and gravel blend "without hard lines").
  - Per-block **RGB tint**.
  - Randomly rotated leaves.
  - The reverse-engineered `BlockType` (unofficial) has `transitionTexture`, `transitionToGroups`, `transitionToTag`, `tint`, `biomeTint`, `randomRotation` and `variantRotation`.
  - A third-party copy of the Update 6 notes says biome-border colour blending was added for tints (https://www.bisecthosting.com/blog/hytale-update-6-patch-notes-new-features-revealed-launch). Not confirmed officially.
- **Vintage Story** (https://apidocs.vintagestory.at/api/Vintagestory.API.Client.CompositeTexture.html):
  - `Alternates` (a random texture per placed block), `Rotation`, `Tiles` (basic connected textures), `BlendedOverlays`.
  - `BlockType.ClimateColorMap` / `SeasonColorMap` and `RandomizeRotations` (position-seeded).
  - Its grass-on-soil is **three soil block states** (sparse, patchy, grassy; https://wiki.vintagestory.at/Soil), not a texture blend.

So **neither reference does true material blending**. Both draw a fringe on the boundary, pick variants and rotations
at random, and tint. True splatting belongs to smooth-terrain engines: Transvoxel, or the per-vertex weights with
triplanar mapping of GPU Gems 3 ch. 1.

### Catalogue of techniques, costed for this engine

| Technique | Look | Mesher | Record | Shader | Memory | WebGL2 | LOD | Art |
|---|---|---|---|---|---|---|---|---|
| **Random variant + 90° rotation per block** (MC blockstates, VS `Alternates`) | Breaks the repeating grid; no edges change | none | none | layer += `hash(block) % n`, UV rotated on tops; per-layer table `(n, rotatable)` | n× layers per varied material | same in `voxel_texture.wgsl` | works (block phase) | 2–4 variants per common material |
| **Macro colour noise** (low-frequency world-space brightness/hue variation) | Large areas stop looking stamped | none | none | a few lines | none | same | yes, and it helps most there | none |
| **Biome tint** (colormap × grey texture) | Gradual colour across climates | none | none | tint lookup per fragment for tintable layers | small tint texture | same | yes | grey or desaturated variants of grass and leaves |
| **Overlay fringe quads** (CTM overlay, Hytale transition texture) | Soft, art-drawn boundary over the lower block | one extra cut-out record per boundary top face, chosen from the 8-neighbour mask | none (the layer selects the fringe tile) | none for the pull path; a depth-biased cut-out pipeline variant | 17–47 layers per source material, which can be composited at load | needs its own material with depth bias (the cutout bit is dropped there) | near only (drop at scale > 1) | one mask set, or per-material fringes |
| **"Better grass"** (OptiFine: a grass side shows the top when the block diagonally below is grass) | Slopes and steps read as continuous grass | layer choice from the neighbour; the chosen layer joins the merge key | none | none | none | same | no | none |
| **Shader neighbour lookup** (fragment samples a block-id 3D texture, blends materials with noise) | True soft blending, any pair | none | none | yes, larger | a surface block-id texture per visible region | integer 3D textures exist in GLES3, but it is a new upload path | hard | none |
| **Per-vertex material weights (splatting)** | Smooth gradients | per-corner neighbour material; **breaks merging** like AO | **needs a second record word** or a side buffer | yes | +4–8 B/quad | `pack.rs` changes too | hard | none |

**Neighbour data:** all of the mesh-time techniques can use the padded shell. Each top face of a 16×16-voxel block
cell has its 8 horizontal neighbour blocks inside the 34³ grid (in-brick, or the 1-voxel shell for the brick's
edge cells; corners come from the edge and corner neighbours `gather_padded` already fetches).

**Merging:**
- **Overlays do not touch base merging.** The base top faces merge exactly as today, and the overlays are separate records added for boundary blocks only.
- A fringe quad covers one block top (16×16 voxels, within the 33-voxel `w`/`h` limit), so it is **one record per boundary block**.
- Edge length is what counts: a straight 64 m grass/dirt edge costs 64 extra records, and a speckled patchwork costs about one per patch block.
- On a 256 m region with, say, 10 % boundary top faces among ~65k surface blocks, that is ~6.5k records ≈ 52 KB. That is small next to the ~57k quads per 256 m LOD tile that `lod_tiles.rs:52` mentions, and far below the near brick counts.
- Per-vertex weights would hurt much more: like AO, they make every quad whose corner materials differ unmergeable, and they need bits the record does not have.

**Z-fighting (the overlays' real risk):** a fringe record lies in the same plane as the base face. The pull path
needs a cut-out pipeline variant with a depth bias (or a 1/256-voxel lift along the normal in the vertex shader for
records flagged as overlay). Two ways to flag them:
- **The range way (recommended):** sort overlays into a third range after cutout and give `face_ends` one more entry. That changes `face_ends`' layout, which is CPU and draw-range plumbing, not the record.
- **The bit way:** reclaim the shader-unused `cutout` bit (the range already says cutout) and reuse it as "overlay". That is a locked-format change and needs every mirror.

On the WebGL2 path the overlays go into a second mesh per chunk with their own `AlphaMode::Mask` material and a depth bias.

**Which pairs blend:** a registry property, in the style of Hytale's `transitionToGroups` and CTM's
`connectBlocks`:

```ron
(id: 3, name: "grass", …, blend: (priority: 30, onto: ["soil", "stone"], fringe: "grass")),
(id: 2, name: "dirt",  …, blend_group: "soil"),
```

- A higher-priority block draws its fringe onto a lower one's top face when the lower one is in a listed group.
- Only declared pairs blend. All pairs is wrong: stone onto planks, or grass onto glass, must never smear.
- **Fringe art from one mask set:** at load, `build_layers` composites `source texture × fringe mask` for each mask shape. 8 primitive masks (4 edges, 4 corners) compose the 47 CTM cases (or OptiFine's 17-tile set), so a new fringe material needs **no new art**. Hand-drawn per-material fringes (ragged grass blades) can replace any of them later.
- Memory: 47 × 6 sources × 16² × 4 B ≈ 290 KB.
- The layer budget (2048) holds it. Note that `side_to_top` must ignore overlay layers.

### Recommendation

Do it in three steps, cheapest look first:
1. **Variants + rotations + macro noise** (shader, a per-layer table, and 2–4 variant textures per common ground material). It removes the "grid" before any edge work.
   - The shader needs **integer world block coordinates** for a stable hash. `texture_position` is chunk-local, and render-space `world_position` may move with the floating origin, so hand the shader each chunk's integer block origin. The pull path already indexes `chunk_transforms` per chunk; the per-chunk path would use a material uniform.
   - Rotating UVs per block breaks the derivative continuity at block edges, so sample with `textureSampleGrad` using the unrotated derivatives to avoid mip seams.
2. **Biome tint** (see the tint note under §3: the WorldMap tiles are enough for a smooth gradient).
3. **Overlay fringe quads**, with a `blend` registry property and mask-composited fringe layers.

"Better grass" is a cheap add-on alongside step 3. Shader lookups and splatting stay rejected.

**It would be wrong if** the owner wants true gradient blending (sand fading into grass over 2–3 m, as in heightmap
terrain games). Fringes give a one-block soft edge, not a gradient. The shader-lookup technique is then the next
step, and it still needs no record change.

---

## 3. Non-block objects: flowers, grass tufts, bushes, small rocks

### What the references do

- **Minecraft:** flowers and tall grass are **blocks** with a cross model (two diagonal planes, `block/cross`), random XZ offset (`offset_type`), biome tint, and instant break. They are in the world's state and saved.
- **Hytale (official):** model blocks and hybrid blocks; leaves cut through the block at several angles, are randomly rotated, and sway in the vertex shader in response to nearby entities (https://www.hytale.com/news/2019/2/exploring-hytale-s-block-tech).
- **Vintage Story:** `RandomDrawOffset` (up to 1/3 block), `RandomSizeAdjust`, `VertexFlags` (wind), and **decor** blocks that attach to a face (https://apidocs.vintagestory.at/json-docs/jsondocs/Vintagestory.GameContent.BlockBehaviorDecor.html).

### Rendering options

**(R1) Cross quads in the record path.**
- Face values 6 and 7 are free, so "diagonal plane A/B" could be encoded with no new bits. Plants would then be emitted as cutout records.
- But the meaning of `face` changes in every mirror (normal tables, `CORNER_PERM`, `face_ends`' per-face ranges, the depth shader, `pack.rs` and `voxel_texture.wgsl`).
- Positions snap to the voxel grid, there is no per-instance scale or tint, and sway would apply to every cutout leaf too.
- It works but bends the locked contract. Not recommended.

**(R2) Bevy entities with bbmodel meshes** (`renderer/model.rs`). This works today, and Bevy batches identical mesh and material handles automatically. It is fine for hundreds to low thousands, not for 10⁵ tufts. Good only for a handful of large props.

**(R3) A new instanced decoration pass** (recommended).
- **Instances:** each a compact instance (e.g. 16 B: position, kind, yaw, scale, tint index, sway phase) over a tiny set of meshes (cross quad, double cross, a few Blockbench-derived low-poly props baked into one vertex buffer).
- **WebGL2:** use **instance vertex attributes**, not vertex-stage storage buffers. `config.rs:92-95` shows WebGL2 has none, which is exactly why the per-chunk path exists, so one pipeline serves every backend.
- **Pipeline:** an alpha-mask pipeline with double-sided faces.
- **Sway:** wind in the vertex shader (world-position noise × the vertex's height in the mesh).
- **Shadows:** a CUTOUT shadow variant like the leaves have, or none for tufts.
- **Distance:** the instance list is built per chunk on the compute pool (wasm: `renderer::compute::JobKind`, which carries what it reads). It is thinned by distance with a stable rule (keep an instance if `hash < density(distance)`, so nothing pops in a pattern) and ends at about 48–64 m. Beyond that, grass colour and macro noise carry the look.
- **Cost:** 10⁵ instances × 16 B = 1.6 MB, drawn in a handful of instanced draws.

### Data model: sim blocks or pure decoration?

| Need | Purely visual (client-derived) | Sim block (`shape: Cross / Model`) |
|---|---|---|
| Harvest (flowers, herbs, berries) | impossible | yes: a drop like any block (`items.rs`) |
| A player tramples or clears a patch | only by changing the ground under it | mined like leaves (hardness ~0) |
| Pathfinding | invisible to it | `solid: false` makes it passable already (`pathfind.rs:38-44`) |
| Rollback / hashing | none (never in sim state) | part of the chunk's blocks, already hashed. Density costs nothing extra in a fixed-size chunk array, a little in saves after compression |
| Server / store / network | none | the cell travels in snapshots and deltas like any block |
| Edits | redraws from `SetBlocks` like water (`bridge.rs:1168`) | ordinary |
| Density | as dense as the GPU allows | keep sparse |

**Recommended split:**
- **Purely visual decoration:** grass tufts, tiny pebbles, fallen leaves, small ferns.
  - Derived on the client, per block top, from `ColumnGround.surface` + the top block id (grass, dry_grass, podzol, sand, snow) + `hash(world block x, z, salt)`.
  - No store, no network, no sim, and no region seams (global coordinates).
  - It exists only where the block above the surface is air, so a player clearing or building over a patch removes it through the normal `SetBlocks` path. **What it breaks:** a player cannot pick or cut one tuft, and two clients only agree because the hash and inputs are identical (they are: same snapshot, same pure function). Both are acceptable for non-interactive cover.
- **Gameplay plants as sim blocks:** flowers, herbs, berry bushes, mushrooms, saplings.
  - `blocks.ron` gains `shape: Cross` / `Model("bush_small")` (implying no faces from the mesher), `solid: false`, `replaceable: true` (placing a block into the cell replaces it), `hardness` ~0, and a drop item.
  - The mesher skips non-cube shapes when making records and **emits a decoration instance instead**, so these plants share pass R3 and never touch the record.
  - `blocks_ray` keeps them pickable (good for harvesting). A "replaceable" check lets a placement target the plant's cell.
  - `Block.data` is free on them, so it could later hold growth stage or berry count.
  - Placed by worldgen in a lattice pass like trees (`trees/placement.rs`: an 8-block lattice, `hash01`, habitat niche responses, higher priority wins). A finer 2–4-block lattice with per-species niches (temperature, moisture, shade near trees) keeps it pure (`generate_region`), seam-safe (reject near seams as trees do) and park/regenerate-safe.
- **Bushes:** small ones are `Model` plants; large ones are a few cutout leaf blocks, as trees already are.
- **Rocks and fallen branches:** visual by default. If they become pick-up-able (a stone for a knapping skill), promote them to `Model` blocks.

**Tint and density need climate on the client.** Two sources, both cheap:
- **(i)** The `WorldMap` tiles already sent: `temperature_c` and `precip_mm` per region, bilinearly interpolated between region centres for a smooth tint and density gradient. A procedural or offline world gets no map, so fall back to the surface block id.
- **(ii)** A climate byte pair per column in `ColumnGround`. That is sim-snapshot data in `game` and must stay deterministic and travel in snapshots, so take it only if 256 m-scale gradients turn out too coarse.
- Start with (i).

### Art needed (all original, the game's own)

| Asset | Size | Count (first slice) | Source |
|---|---|---|---|
| Grass tuft cross textures | 16×16 (or 16×32 tall) cutout | 4–6, plus dry and snowy variants | procedural Pillow like `tools/textures/gen.py`, or hand-drawn |
| Flowers / herbs | 16×16 cutout | 6–8 | hand-drawn; the game's own species |
| Mushrooms, ferns | 16×16 cutout | 2–3 each | hand-drawn |
| Bushes, rocks, branch, stump | Blockbench, 16 px/m, ≤ ~100 triangles each | 3 + 4 + 2 + 2 | Blockbench (`crates/bbmodel` loads it), or a `tools/creatures`-style TOML → bbmodel generator if many are wanted |

`just assets` is a poor fit. It makes high-poly TRELLIS `.glb` models (12k–30k faces; `tools/assets/generate.py:62-72`),
no client code loads glTF, and every model costs credits. At most use it for `--picture-only` concept pictures, with
the owner's approval before buying.

### Recommendation

R3 as the one pass for both kinds. Visual tufts first (no sim change, the biggest visible change for the effort),
then gameplay plants as `shape` blocks.

**It would be wrong if** the design wants ecology on decoration (grazing animals eating grass, fire spreading
through it, seasonal die-back): then grass cover must be sim state. Make it per-column ground state (a "cover" level
like Vintage Story's sparse, patchy, grassy soil) rather than per-tuft blocks, and keep the tufts as its visual.

---

## 4. "Hytale-like" in general

Official sources:
- https://www.hytale.com/news/2019/2/exploring-hytale-s-block-tech
- https://hytale.com/news/2025/12/an-introduction-to-making-models-for-hytale (32 px per unit for blocks and props, 64 for characters; "retro pixel-art textures" meets "hand-painted 3D"; light and shadow **painted into textures**, "we aren't using the industry standard PBR workflows"; in-house light propagation; a fixed set of shading modes; Blockbench with an official plugin)

Coloured block light is reported for its Update 4 (https://www.gamingonlinux.com/2026/03/hytale-update-4-is-another-absolute-whopper-with-over-500-new-blocks/page=1/).
Shadow, AO and water techniques are **not** documented officially. Everything below is about techniques and
direction. No assets, names or exact look are to be copied.

| Trait | Here today | What it would take | Change to the look |
|---|---|---|---|
| Texel density 32 px/m (MC and here: 16) | 16×16 everywhere | the pipeline is size-agnostic (uniform-size rule, mips built). It is **all art**: regenerate or redraw 182 textures at 32². With 16 voxels/m, 2 texels per voxel, so chisel cuts still land on texel lines | Large: the single biggest "not Minecraft" signal. Art cost L |
| Painted light and shadow in textures, stylised, no PBR | procedural colour-ramp textures, PBR `StandardMaterial` lighting on water only | art direction for the regenerated set (highlights and AO painted into each tile) | Large, comes with 32 px |
| Per-block variants and random rotation | none | §2 step 1 | Medium-large, cheap |
| Transition quads between materials | none | §2 step 3 | Medium |
| Tint (per block, per biome) | none | §2 step 2 | Medium |
| Mixed cube + model blocks | cubes + **voxel chisel** (unique to this engine) + bbmodel for creatures only | §3 `shape: Model` | Large in combination with flora |
| Leaves as interior planes, randomly rotated, swaying | cube cutout leaves, no sway | (a) sway: displace cutout records' vertices in `voxel_pull.wgsl` by world noise. It is a shader change on the already separate cutout range, and the per-chunk path needs the same in `voxel_texture.wgsl` keyed on the material. (b) Interior planes: leaves as `shape: Model("leaf_cross")` through R3 instead of cube faces | Medium (sway S, planes M) |
| Grass and foliage sway, entity push | none | R3 vertex shader; push = a few player positions in a uniform | Medium |
| Foliage density | none | §3 | Large |
| Coloured block light / light propagation | **none** (sun CSM + ambient, baked vertex AO) | a light-propagation pass (flood fill per chunk, sim-agnostic, client-side), light per vertex or in a 3D texture. The record has no room for per-corner light: that is the one feature here that would need a second record word or a side texture | Large (caves, interiors, night). Effort L |
| Colour grading / tonemapping | Bevy defaults, no grading, no bloom | pick a tonemapper and a `ColorGrading` (saturation, warm highlights) on the camera. Check what each tonemapper needs on WebGL2 (some need Bevy's LUT feature) | Medium for very little effort |
| Slopes / detail blocks | chisel slopes from worldgen | already ahead: Hytale has no general sub-block carving | — |
| Water | static, one colour, depth alpha, no waves | animated normals, depth-tinted colour, shoreline foam from `ColumnGround` water depth | Medium |
| Day/night, sky | fixed sun, gradient skybox | sun path + ambient curve + sky gradient over time | Medium (not a block topic) |

---

## Phased proposal

Each slice ships something visible on its own. The order puts the cheap, grid-breaking work first.

| # | Slice | What becomes visible | Touches | Effort |
|---|---|---|---|---|
| 1 | **Oriented blocks**: registry axis expansion, six-face layer table, rotated side layers, placement by clicked face, family drops; block-trees uses `_x`/`_z` for lying limbs | Fallen and placed logs, pillars and hay show rings on their ends | `game::registry`, `block_textures.rs`, `records.rs::quads_to_records`, `pack::layer_for_face`, `side_to_top`, placement in `game`, `worldgen/trees/rasterize` | S |
| 2 | **Variants, rotation, macro noise**: per-layer `(variants, rotatable)` table, integer chunk block origin to the shader, `textureSampleGrad`; 2–4 variants for grass, dirt, stone, sand, gravel | Ground stops tiling visibly; large areas get gentle variation | both terrain shaders (+ depth CUTOUT variant), `block_textures.rs`, `blocks.ron` (`variants:`), `tools/textures` | S |
| 3 | **Decoration pass + visual grass tufts**: R3 pipeline (instance attributes, alpha mask, sway), per-chunk instance build on the compute pool, distance thinning, invalidation on `SetBlocks` | Meadows, dry tufts on savanna, pebbles on gravel | new `renderer/decoration/`, `bridge.rs` edit hook, `JobKind`, a few textures | M |
| 4 | **Biome tint** from `WorldMap` tiles (bilinear), tintable layers greyed | Grass and leaves shift colour across climates | terrain shaders, decoration shader, a tint texture, grey variants | M |
| 5 | **Gameplay plants**: `shape: Cross / Model` blocks, `replaceable`, drops, worldgen plant lattice | Flowers, herbs and berry bushes to pick | `game::registry`, `block_props`, `items`, mesher skip + instance emit, `worldgen` lattice, art | M |
| 6 | **Transition overlays**: `blend` registry property, mask-composited fringe layers, overlay records in a third range, depth-biased pipeline variant, WebGL2 second mesh; plus "better grass" | Soft grass/dirt/sand/gravel edges | mesher (mask from the padded shell), `records.rs` ranges / `face_ends`, `voxel_pull/pipeline.rs`, `chunk_mesh.rs`, `block_textures.rs` | M–L |
| 7 | **Leaves: sway and interior planes** | Living tree crowns | cutout path of both terrain shaders; optionally leaves → R3 model | S–M |
| 8 | **Look pass**: tonemapper and colour grading; water normals and depth colour | Warmer, less flat image | `atmosphere.rs` camera setup, `renderer/water` | S (grading), M (water) |
| 9 | **32 px art with painted shading** (art project) | The whole world | `tools/textures`, every block PNG | L (art) |
| 10 | **Coloured block light** | Torches, caves, night | new client lighting pass; record second word or a 3D light texture | L |

## Open questions for the owner

1. **Texture resolution:** stay at 16 px/m or move to 32? It decides whether slices 2, 3 and 6's new art is drawn at 16 or 32, so ask before drawing variants.
2. **Grass tufts non-interactive:** OK that they cannot be picked or cut one by one, only removed with the ground?
3. **Which plants matter for gameplay** (herbs for quests, berries for food, dye flowers)? That sets slice 5's list.
4. **Orientation scope:** only logs, pillars, hay and bone? Or are stairs, doors and facing blocks coming soon? If they are, a real block-state byte beats id expansion (§1, "It would be wrong if").
5. **Blending ambition:** is a one-block soft fringe (Homestead and Hytale style) the target, or true multi-metre gradients (needs the shader-lookup technique)?
6. **Art provenance:** several block names follow Minecraft's (`deepslate`, `prismarine`, `terracotta`, `*_concrete`). The textures are generated by `tools/textures` and are the game's own, but as the look moves toward its own identity, should names and palette move too?
7. **Paid generation:** any use of `just assets` for decoration concepts? (`--picture-only` first, with approval before buying.)
8. **Block light** is the one feature that needs more per-quad data. Wanted on this horizon or later?
