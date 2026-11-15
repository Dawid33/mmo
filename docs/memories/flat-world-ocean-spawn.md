---
name: ""
metadata: 
  node_type: memory
  originSessionId: 2e30d0fb-761c-4454-8dbd-b212638650e9
---

User report 2026-07-08: "world is flat and there is a gap between chunks". Two separate causes.

**Gaps (fixed, uncommitted as of 2026-07-08):** `build_chunk_mesh` passed the bare 32³ chunk to `greedy_quads` with `[0..31]`, but greedy_quads only meshes the *interior* of [min,max] — boundary voxels were never meshed and fully solid chunks produced **no mesh at all**. Fix: air-padded 34³ buffer (`PaddedChunkShape`), positions shifted back by the pad. Regression tests: `boundary_voxels_are_meshed`, `full_chunk_mesh_spans_full_extent` in meshing.rs. Border faces between adjacent solid chunks are now always emitted (overdraw); neighbour-aware padding is a possible future optimization.

**Ocean spawn (RESOLVED 2026-07-09, develop 1d9bb05):** root cause was NOT the seed — spawn (block 8,8) sat ~0.001 noise cells from the Perlin lattice origin, where every octave is ~0 for every seed (512 seeds all gave y≈35). Fix: `SAMPLE_OFFSET_X/Z_M` in pipeline.rs shifts all noise reads off the lattice zero, plus `WORLD_SEED = 0x00D5_C0DE_8D78` picked by `crates/worldgen/examples/seed_scan.rs` (spawn y=70, dry flat ±48 m, mostly-land ±256 m). Re-run seed_scan if terrain shaping changes.

**Flat (still open design decision):** no terrain detail at 1–50 m wavelengths (jaggedness is gated to high-offset inland), so the playable area is smooth. Candidate fixes: add mid/high-frequency surface detail or rescale horizontal frequencies. Waiting on user's choice of trade-offs — see [[design-questions-need-tradeoffs]].
