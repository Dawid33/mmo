---
name: lod-sea-walls-and-texture-scale
description: "2026-09-20 root causes of dark LOD ring \"gaps\" over sea and enlarged far textures; diagnosed; BOTH FIXED same day, see lod-dh-voxy-research"
metadata: 
  node_type: memory
  type: project
  originSessionId: 2b6a6732-2572-44bf-abd5-38074a229ef9
  modified: 2026-09-20T16:24:52.127Z
---

INVESTIGATED 2026-09-20 (findings: docs/superpowers/specs/2026-09-20-lod-seams-and-texture-scale-findings.md). Both fixed 2026-09-20 as part of [[lod-dh-voxy-research]] (SEA_TOP_BLOCK, world-voxel UVs + mips).

1. Dark bands at far ring boundaries over water are camera-facing water WALLS, not holes. True sea top = 65 blocks (SEA_LEVEL=64 blocks, water fills `by <= sea`); any-solid LOD rounding puts it at 68 blocks in the factor-64 ring and 80 in factor-256 (measured; factors 2/4/16 exact at 65). Both `coarse_tile` and `downsample_chunks` agree. Recommended fix: fill water `by < sea` so the surface (64 blocks = 1024 voxels) sits on the coarsest LOD lattice — changes world output (re-check spawn seed, crc tests).
2. Enlarged textures: all three terrain shaders use `uv = local_pos/16` in chunk-local cells while LOD factor lives in Transform.scale → texture magnified ×factor. Fix = scale UVs by chunk scale AND add a mip chain (array texture is nearest, 1 mip) or far rings shimmer.
3. Dark contour lines on near terrain are real side faces at ~0.17× ambient-only lighting, not gaps.

**Why:** avoids re-diagnosing from the same screenshot symptoms.
**How to apply:** if asked to fix LOD seams/texture scale, start from the findings doc; see [[voxel-rendering-phase1-landed]] and [[rendering-next-steps-research]] for the mip-chain backlog item.
