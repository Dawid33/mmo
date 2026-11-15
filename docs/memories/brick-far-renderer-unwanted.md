---
name: brick-far-renderer-unwanted
description: User considers the brick-raymarch far renderer slow and useless — never suggest VOXEL_FAR_RENDER=brick or brick-mode A/Bs
metadata:
  node_type: memory
  type: feedback
  originSessionId: 03d0f4ec-da04-4b4f-8728-3e2556038250
  modified: 2026-09-25T17:29:49.055Z
---

Don't suggest the brick far renderer (`VOXEL_FAR_RENDER=brick`, `far_voxel` brick raymarch) for testing, A/B comparisons or fixes. Raster is the only far path that matters.

**Why:** User (2026-09-25): "brick is slow and useless, dunno why it even exists. don't suggest it".

**How to apply:** Investigate and fix LOD issues in the raster path only (`meshing::lod`, `lod_rings.rs`). Brick code may be a candidate for deletion later — ask before removing it. Related: [[island-regions-cell-loading]].
