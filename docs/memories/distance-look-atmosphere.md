---
name: distance-look-atmosphere
description: 2026-09-22 "trippy distance" fix — fog, hemisphere ambient, riser fade, anisotropy; tunables in renderer/atmosphere.rs; live look pending
metadata:
  type: project
---
IMPLEMENTED 2026-09-22 (uncommitted): user's "distance looks trippy" (black contour lines on terraces, brown/green moiré bands, black backlit hills) was NOT shadows — it was ambient 100 vs a 10 klx sun (shadowed/back faces ~7 %), no fog in the custom shaders, and sub-pixel dirt risers aliasing.

Fix: `crates/client/src/renderer/atmosphere.rs` owns sun (5 klx, ~40° elevation), sky-tinted ambient 900, and `DistanceFog` (exponential, visibility 2 km, colour = skybox horizon × BRIGHTNESS × exposure) attached per camera. Shaders (voxel_pull.wgsl, far_voxel.wgsl) apply Bevy's fog uniform themselves, scale ambient by SKY_SIDE hemisphere (0.75 for sides), and fade side faces to their block's TOP layer + up normal when a block is < 24→6 px tall (was 4→1; raised after the user saw brown contour zebra on camera-facing slopes — risers dominate at grazing angles, grass_side is ~80 % dirt) (pull path gets a `top_layers` storage buffer, group 1 binding 4, from `BlockTextureLayers::side_to_top`). Sampler is now linear + anisotropy 16; `crisp_uv` in shaders keeps the nearest look up close. Per-chunk fallback (voxel_texture.wgsl) gets fog via PBR + hemisphere via diffuse_occlusion, no riser fade.

**Why:** live look not yet confirmed by the user; tunables may need a pass.
**How to apply:** tune constants in atmosphere.rs / the shader consts, don't re-diagnose. voxel_pull.wgsl now has a naga validation test (voxel_pull::pipeline::tests). Related: [[lod-dh-voxy-research]], [[terraced-ground-fix]].
