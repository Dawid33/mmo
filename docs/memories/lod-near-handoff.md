---
name: lod-near-handoff
description: LOD→near-chunk handoff now waits for the WHOLE near region to mesh (2026-09-23, uncommitted); was "any one chunk meshed" + 600-frame deadline
metadata:
  type: project
---
IMPLEMENTED 2026-09-23 (uncommitted, develop): `ClientUpdateEvent::RemoveLod { key, near_takeover }` (instance sets it when the key's region is in the near window). `lod_rings::retire_lod_roots` releases a takeover retiree only when every region of the key is fully meshed (`near_regions_meshed`: no child of the region root with `PendingSnapshotVoxels` or `VoxelData` w/o `ChunkConnectivity`, held `NEAR_HANDOFF_HOLD_FRAMES`=3) or LOD-covered; takeovers are exempt from `RETIREMENT_MAX_FRAMES`. Old rule released on the first meshed chunk → holes. smoke green; the 4 new lod_rings unit tests were only type-checked (client test binary = slow opt-in); live look pending.
Known residual: LOD + near overlap for the whole region meshing time (LOD any-solid can poke ≤1 cell above near terrain). Reverse direction (near→LOD when a region leaves the window) untouched. Related: [[streamed-lod-rings-design]], [[chunk-loading-speed-investigation]].
