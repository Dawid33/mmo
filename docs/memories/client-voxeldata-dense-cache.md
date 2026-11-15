---
name: client-voxeldata-dense-cache
description: "The client's per-chunk VoxelData was a dense 64 KiB array (~183 MB/region, 75% of client heap); now RLE-encoded in bridge.rs — decode() at mesh time only"
metadata: 
  node_type: memory
  type: project
  originSessionId: befcc3f3-5dd6-43d2-a0d5-a88e2807c9bc
---

The render bridge attaches a `VoxelData` component per chunk entity
(`crates/client/src/renderer/bridge.rs`). It used to hold the dense 32³ array
straight from `game::derive_voxels` — 32768 voxels × 2 B (`Voxel` = `BlockId(u16)`)
= **64 KiB per chunk**. At ~2800 non-empty chunks per region that is **~183 MB per
region**, which heaptrack measured as **75% of the client's peak heap** and the
largest single consumer by an order of magnitude. The production 3×3 near window
therefore carried ~1.6 GB of `VoxelData` alone.

Fixed 2026-07-11 by run-length encoding it (`VoxelData(Vec<(u16, BlockId)>)` with
`new()`/`decode()`/`edit()`/`runs()`). Runs are cheap by construction: a chunk is
8 blocks of 16³, each uniform unless chiselled, so every run is ≥16 voxels and a
plain air/stone chunk is one run. Nothing retained the dense array — the mesher
expands it transiently in `spawn_mesh_tasks` to build `PaddedVoxels`, and
`boundary_zones` reads it once — so `decode()` on demand is enough.

Measured: peak heap 245 MB → 84 MB for a one-region client; bevy_input suite
peak RSS 29.5 GB (OOM-killed) → 510 MB.

**Why:** if you add a consumer of `VoxelData`, do NOT store the `decode()` output
on an entity or resource — that reintroduces the 64 KiB/chunk cost. Decode,
use, drop.

**How to apply:** the RLE↔dense round trip is exercised by the meshing suite
(`cargo test -p client --lib -- --include-ignored meshing::`, 83 tests) — the
carve-a-tunnel visibility test and boundary-zone dirty propagation are the ones
that would catch a bad encode. Related: [[meshing-startup-burst]],
[[voxel-rendering-phase1-landed]].
