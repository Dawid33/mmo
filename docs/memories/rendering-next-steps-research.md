---
name: rendering-next-steps-research
description: "Voxel rendering research DONE — 2026-07-09 survey (face pulling top pick) + 2026-07-10 deep follow-up (impostors/brickmaps/raymarching); both reports in docs/superpowers/specs/; don't re-research"
metadata: 
  node_type: memory
  type: project
  originSessionId: 6ff11edb-334b-4759-9667-9ae588fb1367
---

Deep research on voxel.wiki rendering techniques for AFTER the 4-phase rendering program
([[voxel-rendering-phase1-landed]]) completed 2026-07-09. Full verified report:
`docs/superpowers/specs/2026-07-09-voxel-rendering-next-steps-research.md`.

Ranked outcome: (1) per-quad SSBO face pulling — ~4-5× memory past phase 2, works on core
browser WebGPU (read-only storage in vertex stage is allowed; compat mode is not), open
question is Bevy 0.18 pipeline fit; (2) flood-fill light engine — bounded scope: no shadows
/ moving sun, packed quad record is near-full so light needs format decision; (3) pooled
buffers + multidraw + orientation buckets — native-only (no browser MDI), only if
draw-submission-bound. Warning: skirts can leave LOD-border holes (godot_voxel#63) — phase 4
should anticipate a transition-mesh fallback if observed.

**Follow-up deep research DONE 2026-07-10** (further optimizations past face pulling):
`docs/superpowers/specs/2026-07-10-voxel-rendering-further-optimizations-research.md`.
Ranked: (1) Ascendant-style quad-per-voxel far-field impostors (WebGPU-feasible, replaces
far-ring meshing); (2) brickmap/XBrickMap raw 8³ bricks as the editable GPU-residency
substrate; (3) Aokana streaming ideas only — its SVDAG pipeline is blocked (no runtime
edits + 64-bit atomics absent from browser WebGPU); (4) mesh shaders/bindless native-only,
park. MDI-on-browser status is contested (2 claims refuted both ways) — feature-detection
spike required, not docs. Unresearched gaps: water/transparency, distant shadows,
atmosphere/fog, virtual textures, TAA, Teardown-class writeups.

**Face pulling PLANNED 2026-07-10** (user chose Option A — full custom render path — over
the Material-safe compaction fallback, knowing the ~10× scope):
`docs/superpowers/plans/2026-07-10-face-pulling-custom-render-path.md`. Key verified
constraint behind the fork: Bevy 0.18.1's stock mesh pipeline cannot do vertex pulling
(allocator skips attribute-less meshes, allocator.rs:479; DrawMesh passes slab offset as
base_vertex, bevy_pbr mesh.rs:3283 — shader-invisible, native AND WebGPU). Plan =
8B/quad records + custom Opaque3d/Opaque3dPrepass/Shadow phase items; accepted
regression: chunk-vs-chunk HZB occlusion (cave-culling BFS remains); legacy Material
path kept behind a RenderPath toggle. Don't re-plan; don't re-litigate A-vs-B.

**Why:** avoid re-running either survey; claims were adversarially verified (refuted claims
listed in each report).
**How to apply:** when planning post-phase-4 rendering work, start from the reports, not from
scratch.
