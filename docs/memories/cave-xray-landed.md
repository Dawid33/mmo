---
name: cave-xray-landed
description: "Dug-space x-ray (cut-off view sees inside dug rooms) IMPLEMENTED 2026-10-31, uncommitted; client unit tests + live look pending"
metadata:
  node_type: memory
  type: project
  originSessionId: be4b1af2-6d5f-45d2-a031-d4f19f6dc316
  modified: 2026-10-31T12:50:10.591Z
---

Spec docs/superpowers/specs/2026-10-31-cave-xray-design.md implemented 2026-10-31 (uncommitted, on develop alongside another session's commander-camera edits).

- Mesher: `meshing/interior.rs` `InteriorGround` (brick's 2×2 columns + 1-block ring of ORIGINAL surfaces from `TileMirror::home_surface`); interior = air strictly below surface (`game::is_dug_air`); merge-key bit 48; record w0 bit 31 `INTERIOR_BIT` (w1 kept equal to pack.rs word1); brick marker `InteriorFaces`; `AwaitingGround` re-mesh when home ground arrives late.
- Render: `voxel_pull/xray.rs` — per-view interior depth (TextureCache, single-sampled) drawn by `xray_interior_pass` (Core3d, before main_opaque_pass_3d) from `InteriorChunks`; forward `XRAY` variant (group(2) depth; stand-in 1×1 always bound) discards occluders with `CEILING_GHOST` Bayer dither, interior faces skip sun shadow + `INTERIOR_AMBIENT_GAIN`.
- Picking: `game::xray_hit`/`xray_through`/`pick_xray_cell`; `visible_ground(.., xray)`, `designate::pick_visible_xray`, move-order hover.
- Builder-check calls recorded in the spec: terrain-only lighting, pulled paths only, only terrain is an occluder (voxel bodies use the same pipeline so they ARE x-rayed).

**Why:** Dawid wants rooms readable with "Hide above" off; "Hide above" itself must never change.
**How to apply:** don't re-plan; next steps are `just test-one client "--lib xray interior records:: tiles:: visible:: pipeline:: dev_ui::"` (ask first — slow) and a live look (ask before running). Related: [[fortress-ui-landed]], [[configurable-tunables-preference]].
