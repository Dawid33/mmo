---
name: vertical-chunk-stacking-landed
description: "crates/game/src/volume.rs: Column/SubChunk/ReadVol/WriteVol column model; design decisions + what's deferred"
metadata: 
  node_type: memory
  type: project
  originSessionId: 28e57965-868f-4ae5-b52f-ebaed7339f28
---

Vertical chunk stacking data-model foundation merged to develop 2026-07-06 (merge b8f306d). Spec: `docs/superpowers/specs/2026-07-06-vertical-chunk-stacking-design.md`.

**What landed** (`crates/game/src/volume.rs`): `Column` = `base_sub_y: i32` + `below`/`above` sentinel `Block`s + sparse `Vec<SubChunk>`; `SubChunk` = `Air | Dense(Box<Chunk>)`; `ReadVol`/`WriteVol` traits at whole-block granularity (`BlockPos`). `ChunkCoords.y` is now `i32`. Worldgen emits 64 `Column`s/region; `Region::from_columns` flattens `Dense` sub-chunks into the existing per-chunk `create_mesh` path.

**Non-obvious design decisions** (chosen deliberately, don't "fix" without asking):
- Column model, NOT full-3D chunk coords — keeps the 2D `RegionCoords` topology / handoff / parking untouched. Verticality is region-local.
- Only `Air` collapses to a variant; interior all-stone stays `Dense` (gets no collider). Unbounded height via sentinels (stone below / air above), no height cap.
- `Column::normalize` is a canonical form (one bincode byte-rep per logical column) — required for the rollback hash bar. "Full canonical support" chosen: `WriteVol::set` growth fills with sentinel-matched content, and normalize edge-trims a `Dense` slice equal to the adjacent sentinel; `base_sub_y` reset to 0 on empty band ONLY when `below == above`. Don't weaken these — Task-3 review caught 2 Critical determinism bugs here.
- Mesher stays `&[Voxel]`-based (not genericized over `ReadVol`); `SerializedRegion` unchanged (parks materialized `Dense` chunks, columns never reach the blob).

**Deferred to when digging/gameplay lands** (documented, not bugs): (1) `BlockPos` indexing has no bounds check — add a `debug_assert!` with the first real `WriteVol` gameplay caller; (2) `Column` growth uses `Vec::insert(0,_)`/`remove(0)` = O(n²) for large vertical spans.

Unblocks [[tectonic-worldgen-research]] (its precondition was "after vertical chunks land").
