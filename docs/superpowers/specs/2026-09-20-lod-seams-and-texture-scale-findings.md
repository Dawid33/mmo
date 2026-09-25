# LOD seams ("gaps") and enlarged far textures — findings

2026-09-20. Investigation only, no code changed. Source: a screenshot looking across open sea
with two dark horizontal bands near the horizon and visibly coarser water texture beyond each.

## 1. The dark bands are walls, not holes: far rings put the sea too high

Band colour is `(9,26,32)` against water `(57,109,126)` — the water texture at ~0.17×, i.e. an
ambient-only (sun-facing-away) **side face**, complete with the texture's light speckles as big
dark-purple texels. A hole would show sky. The wall is one enlarged texel ≈ one LOD cell tall.

Cause: `worldgen::SEA_LEVEL = 64` is in blocks and water fills `by <= sea`
(`crates/worldgen/src/lib.rs:121`), so the true surface is the top of block 64 = **65 blocks**
(1040 voxels). Every LOD path is "any solid sub-voxel → whole coarse cell solid"
(`game::lod::downsample2`; `coarse_tile`'s `top_cell = surface.max(sea).div_euclid(cb)`), which
rounds a surface UP to the next cell boundary. 1040 is a multiple of 2, 4 and 16 but not of 64
or 256. Measured with a throwaway probe on an ocean column (region −4,−4):

| factor | 2 | 4 | 16 | 64 | 256 |
|---|---|---|---|---|---|
| sea surface (blocks) | 65 | 65 | 65 | **68** | **80** |

Same numbers from `coarse_tile` (pristine) and `downsample_chunks` (authoritative overlay), so
the two far paths agree with each other — both are wrong by the same amount. Result: a 3-block
camera-facing water wall at the 16→64 ring boundary (8.5 regions out) and a 12-block one at
64→256 (16.5 regions). 12 blocks at twice the distance ⇒ the farther band is ~2× thicker on
screen, as in the screenshot. The 1-cell skirt in `meshing/lod.rs::lod_quads` is irrelevant
here: it only covers a coarse ring sitting *lower* than its neighbour.

Land has the same upward bias in the far rings (mean +1.5 blocks at 64, +7.5 at 256), just far
less visible than on a flat sea.

### Fix options

- **A (recommended): put the sea surface on the coarsest cell lattice.** Fill water `by < sea`
  so the top is 64 blocks = 1024 voxels, a multiple of 256. Every factor then reproduces the sea
  exactly under the existing rule; no LOD or client change. Cost: world output changes (ocean one
  block lower) — touches `lib.rs:121`, `coarse_tile`'s `top_cell`/water test, `material.rs`
  beach thresholds, the `SEA_LEVEL + 2.0` ocean-spawn check in `pipeline.rs:379`, crc-pinned
  worldgen tests, and the spawn seed should be re-checked with `examples/seed_scan.rs`.
- **B: round-to-nearest in the far rings.** Sea lands at 64 in rings 64/256 (one block *below*
  the near 65 — a back-facing, invisible step) and the land bias disappears too. Cost: for a
  generic 3D merge "nearest" means majority-solid, which erodes thin features (small islands,
  player builds) — the reason any-solid was chosen — and `coarse_tile` and `downsample_chunks`
  must change in lockstep or visited regions step against pristine ones inside a ring.
- A is one invariant ("sea level is a multiple of the coarsest LOD cell") and fixes the visible
  artefact; B is the follow-up if land cliffs at far ring boundaries ever become noticeable.

## 2. Enlarged textures: UVs are in chunk-local cells, LOD chunks are scaled by the transform

`voxel_pull.wgsl:85`, `voxel_texture.wgsl:151` and `far_voxel.wgsl::face_uv` all compute
`uv = local_pos / 16`. An LOD chunk's `local_pos` is in LOD cells and the ring factor lives only
in `Transform.scale` (`lod_rings.rs:203`), so one texture repeat spans `16 × factor` voxels: the
texture is magnified 2×/4×/16×/64×/256× per ring (a single texel is a whole block at factor 16,
16 blocks at 256).

Fix: multiply UVs by the chunk's scale (pull path: `length(chunk_transforms[i][0].xyz)`; mesh
path: same from the model matrix; brick path: its per-brick cell size). **Must ship with a mip
chain**: the array texture is `ImageSamplerDescriptor::nearest()` with one mip level
(`renderer/mod.rs:265`), so true-scale UVs at factor ≥ 16 are heavily minified and would turn
into shimmering noise instead of magnified texels. Per-layer mips + linear mip filtering (keep
nearest mag for the near look) make distant terrain converge to the block's average colour —
already listed as P0 in `docs/voxel-renderer-potential-optimisations-report.md`.

## 3. Not a bug: dark contour lines on near terrain

Darkest pixels there are `(23,8,4)` — dirt texture at the same ~0.17× ambient-only level: side
faces of the voxel-resolution terrace steps facing away from the sun (the sun-facing ones on the
right of the frame are lit red-brown). Real geometry, but ambient is low enough that every
shadowed side face reads as a black crack; raising ambient / adding sky light would soften both
these and any residual ring steps.
