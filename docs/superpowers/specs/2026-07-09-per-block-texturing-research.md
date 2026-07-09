# Per-block texturing research

**Date:** 2026-07-09
**Status:** Implemented (shader with upright side faces + block-scale art;
`marker.png` kept as a UV-debugging aid)
**Goal:** author one texture per block material (e.g. `dirt.png`) and have it
span a whole block face (16×16 voxels = 1 m²), each voxel rendering its own
1/16² sub-tile of that texture — instead of today's behavior where the texture
tiles once **per voxel**.

## Current architecture (what the texture path does today)

Scales: 1 block = 16³ voxels = 1 m³ (`BLOCK_VOXELS = 16`, `crates/game/src/block.rs:14`);
a chunk is 32³ voxels = 2×2×2 blocks, so **chunk-local voxel coordinates are
block-aligned** (block boundaries at 0/16/32, chunk borders are block borders).

The render path:

1. **Mesher** (`renderer/meshing/binary.rs`) works on the 34³-padded **voxel**
   grid. Each `Voxel` carries the `BlockId` inherited from its parent block (or
   its chisel override). Greedy merging keys on `(BlockId, AO)` and freely
   merges across block boundaries.
2. **Emit** (`emit.rs`) packs 8-byte vertices (`pack.rs`, layout LOCKED):
   `word0: x:6|y:6|z:6|face:3|ao:2|cu:1|cv:1`, `word1: w:6|h:6|layer:16`.
   The texture-array layer is resolved per quad from
   `BlockTextureLayers: BlockId -> [top, side, bottom]` + `layer_for_face`.
3. **Shader** (`assets/shaders/voxel_texture.wgsl:122-123`) computes
   `uv = vec2(cu * quad_w, cv * quad_h)` — UVs span 0..quad-extent in *voxel
   cells*, and the Repeat sampler (`renderer/mod.rs:165-169`, nearest filter,
   no mips) wraps the texture **once per voxel**.
4. **Textures** (`block_textures.rs`) are stacked into a `texture_2d_array`;
   all layers must be square and the same size (first-loaded PNG fixes the
   size; mismatches fall back to the layer-0 magenta/black checkerboard).

So "per-voxel tiling" is purely a property of the UV formula in step 3.
Everything else (layer selection, merging, AO, packed format, prepass) is
already per-block or texture-scale-agnostic.

## Recommended design: position-derived UVs (shader-only change)

Replace the quad-extent UV with UVs derived from the decoded chunk-local
vertex position, divided by the block edge length:

```wgsl
// ua/va = cross_axes(face.axis()), same table as binary.rs::cross_axes.
// Implement as per-face constant mask pairs and dot():
//   u = dot(local_pos, U_MASK[face]);  v = dot(local_pos, V_MASK[face]);
out.uv = vec2<f32>(u, v) / 16.0;   // 16.0 = BLOCK_VOXELS
```

Why this works with zero Rust changes:

- **UV is affine in position**, so linear interpolation across a greedy quad of
  any size is exact — merging stays fully intact, including quads spanning
  multiple blocks: block boundaries land on integer UVs and the existing
  Repeat sampler wraps the texture once per block.
- **Cross-chunk continuity is automatic**: chunk-local coords are block-aligned
  (32 = 2×16), and chunk borders coincide with block borders, so the same
  world-space texel grid emerges in every chunk without any world offset.
- **Chisel detail behaves like carving**: a chiseled voxel keeps the sub-tile
  for its (u,v) position inside the block cell, so carved surfaces read as
  "cut into the material". Faces exposed inside the block show the same slice
  as the surface face along that axis — standard voxel-engine behavior.
- The prepass vertex stage doesn't touch UVs; `layer_for_face` (top/side/
  bottom triples, e.g. grass) is unchanged.

The `cu`, `cv`, `w`, `h` packed fields become unused by the forward shader
(they exist only to build the old UV). **Leave the packed layout alone** in
this change — it's a LOCKED Rust↔WGSL contract with pinned tests. Reclaiming
those 14 bits (future: light levels, texture variants) is a separate,
coordinated `pack.rs` + WGSL + test change.

### Asset workflow

Nothing in `build_layers` needs to change — it's already size-agnostic:

- Author block textures at **128×128 or 256×256** (8/16 texels per voxel edge).
  At today's 16×16 each voxel would get exactly 1 texel, which reads as
  "Minecraft blocks" at 1 m scale rather than a material with sub-voxel detail.
- All block PNGs must share one size (existing uniform-size rule; the
  first-loaded texture sets it, mismatches degrade to the checkerboard). The
  procedural missing-texture already scales via its `size` parameter.
- VRAM is a non-issue: 256²×RGBA8 ≈ 262 KB/layer, ~26 MB for 100 materials.

### Caveats / follow-ups (not blockers)

1. **Face orientation.** ~~`cross_axes` picks a fixed (ua, va) pair per axis~~
   RESOLVED during implementation: naive cross-axis sampling put image-up
   along +Z on X faces (rim rendered as a vertical stripe) and along -Y on Z
   faces (rim at the block bottom) — fatal for directional art like
   `grass_side`, and unfixable in the art since X and Z faces would need
   different rotations. The shader's `FACE_U_MASK`/`FACE_V_MASK` now sample
   side faces upright (U = horizontal axis, V = -Y; Repeat wraps negative v).
   These masks are a sampling choice decoupled from `cross_axes()`, which
   only orders quad extents. Remaining (accepted): opposite side faces mirror
   horizontally — irrelevant for natural art, revisit if blocks carry glyphs.
2. **Minification aliasing.** The array image is built with one mip level and
   a nearest sampler. Fine at 16², but 256² art will shimmer at distance.
   Follow-up: generate mips for the array texture (manual downsample in
   `setup_scene`, layer-by-layer) and switch to nearest-with-linear-mip.
   Per-block UVs don't make this worse than per-voxel tiling at equal texel
   density — it's a texture-resolution consequence, not a UV one.
3. **Verification.** The UV formula lives only in WGSL (no Rust mirror), so
   the check is visual: `just run` with a real non-uniform `dirt.png` (e.g.
   add an asymmetric marker) and confirm one image per block face, continuity
   across chunk borders, and correct wrap across merged quads.

## Alternatives considered and rejected

- **Pack a block-local UV origin into spare vertex bits** (word0 has 7 spare,
  word1 has 4): the mesher would compute each quad's offset inside its block.
  Breaks for greedy quads spanning multiple blocks unless quads are split at
  block boundaries, requires a coordinated LOCKED-format change, and buys
  nothing the decoded position doesn't already provide. More moving parts for
  the same pixels.
- **Split quads at block boundaries + clamped per-block UV rects**: inflates
  vertex count up to 4× on uniform chunk faces and only matters for atlas
  (sub-rect) packing where Repeat can't wrap. We use a `texture_2d_array`,
  where hardware Repeat handles the wrap per layer — no reason to give up
  merging.
- **Fragment-shader `fract(world_pos)` UVs**: also works, but world-space
  derivation needs the world offset handled at the render boundary and buys
  nothing over chunk-local vertex decode (block grid is chunk-aligned).

## Post-research changes reviewed (2026-07-09, develop @ 1d9bb05)

Phase 3 visibility culling and the pixel-art texture set landed after this
research; none of the UV-path files changed (`voxel_texture.wgsl`, `pack.rs`,
`emit.rs`, `binary.rs`, `block_textures.rs` — verified by diff). Updates:

- **DepthPrepass now runs every frame on native** (`visibility.rs::
  enable_native_occlusion` adds `DepthPrepass` + `OcclusionCulling` to the
  camera for HZB culling), not just for shadows. Still no interference: the
  shader's `PREPASS_PIPELINE` branch decodes position only and never touches
  UVs, and the forward-only UV change leaves prepass output identical.
- **Real 16×16 pixel-art textures exist now** (590f402: stone, sand, water,
  grass top/side + dirt; `blocks.ron` has 6 blocks). Two consequences:
  - The art was authored for **per-voxel tiling** ("dirt.png speckle style").
    Under per-block UVs a 16×16 spans the whole block → exactly 1 texel per
    voxel (16 texels/m — precisely Minecraft's density). The pipeline needs
    nothing, but the speckle art will read as flat single-color voxels and
    likely wants re-authoring for the new mapping (same or larger size).
  - `grass_side.png` is directional (green rim on top), so the face-
    orientation caveat is now observable. Note the position-derived formula
    has the *same* orientation semantics as today's `cv * quad_h` (both grow
    along the +va axis), so whatever looks upright today stays upright — the
    per-face flip polish is unchanged in scope.
- Cave-culling BFS (`ChunkConnectivity`, `MeshOutput`) sits between the mesh
  task and entity visibility — orthogonal to UVs/material. The `MeshingTask`
  now returns `MeshOutput`, but `quads_to_mesh` and the packed format are
  untouched, so the change-footprint table below still holds.

## Change footprint (when implemented)

| File | Change |
|------|--------|
| `assets/shaders/voxel_texture.wgsl` | UV formula: per-face cross-axis masks, `/ 16.0`; update the tiling comment |
| `assets/blocks/*.png` | New higher-res block-material art (uniform size) |
| `crates/client/src/renderer/meshing/pack.rs` / `emit.rs` | Comment updates only (`cu/cv/w/h` now vestigial for UV) |
| everything else | untouched — mesher, packed format, `build_layers`, prepass, netcode |
