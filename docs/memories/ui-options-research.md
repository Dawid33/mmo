---
name: ui-options-research
description: UI direction decided + Bevy 0.19.1 upgrade and egui F10 dev panel landed 2026-09-22; in-world UI = render-to-texture + raycast virtual pointer, no taffy in engine
metadata:
  type: project
---

DECIDED + IMPLEMENTED 2026-09-22 (uncommitted, on develop). Spec with the architecture table (§0): docs/superpowers/specs/2026-09-22-ui-state-and-options.md.

- **User context:** taffy-in-engine was originally wanted for UI drawn on meshes in the world and clicked through the world. It had been ruled out earlier to keep that complexity out of the engine, and it stays out. The in-world case is client-side: bevy_ui renders into an image render target on the mesh, and a mesh raycast's UV drives a `PointerId::Custom` (Bevy 0.19 `examples/ui/render_ui_to_texture.rs`). The sim only sees the logical `UiAction`, checked against reach.
- **Bevy upgraded 0.18 → 0.19.1** (user: "do the bevy upgrade"). Key changes:
  - the render graph became `Core3d` systems (`voxel_pull::gpu::add_near_gpu_systems`, `far_voxel::pass::far_voxel_pass`)
  - `VoxelPullPipeline` and `FarVoxelPipeline` are created in `RenderStartup`
  - the view layout key comes from `ViewKeyCache`, and WGSL fog sits behind `DISTANCE_FOG`
  - `fetch_directional_shadow` gained a 5th argument
  - `renderer::render_plugin()` disables `EXPERIMENTAL_COOPERATIVE_MATRIX` (it NVK-panicked)
  Verified: `pull_smoke` passes on NVK and the iGPU, and a live tiny-world run shows HUD, hotbar, F3 and F10 working.
- **egui** (user: "egui is fine"): F10 panel in `renderer/dev_ui.rs` behind the default-on `dev_ui` feature. `renderer/cursor.rs` `CursorPolicy` is now the only cursor-grab writer, and `forward_input` drops clicks and mouse motion while an overlay is open.
- **Still open:** grow `game::ui` into a serde schema; make the hotbar drive block placement; inventory screen with drag and drop; chat on `EditableText`.
- Two client lib tests (`instance::manager_tests::{world_scale_tiny…, lod_requests_are_paced…}`) fail from other uncommitted LOD work, not this.

**Why:** the user will come back to UI work, so don't re-research or re-plan it.
**How to apply:** after any Bevy bump, run `just adapter=… pull-smoke`, because shader tests use stubs. Related: [[chat-commands-investigation]], [[modding-wasm-research]], [[egpu-adapter-selection]].
