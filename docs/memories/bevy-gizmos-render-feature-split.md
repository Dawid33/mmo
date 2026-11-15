---
name: bevy-gizmos-render-feature-split
description: "Bevy 0.18 gizmos silently draw nothing unless the separate bevy_gizmos_render cargo feature is enabled; fixed 787b660, gizmo_smoke.rs is the probe"
metadata: 
  node_type: memory
  type: project
  originSessionId: 999c64c7-e724-4ea1-8d7a-a9c862fc168a
---

Bevy 0.18 split gizmos into two cargo features: `bevy_gizmos` (API/storage —
`Gizmos` systems compile and run) and `bevy_gizmos_render` (the render
backend). With `default-features = false` and only `bevy_gizmos` listed, every
`gizmos.line()` call succeeds and NOTHING renders — no panic, no warning, on
every GPU. This hid the F3 collider wireframes for two days and the F5
chunk/region grid on arrival; fixed in root Cargo.toml (commit 787b660,
2026-07-10).

**Why:** the workspace pins a hand-picked bevy feature list, so any Bevy
upgrade that splits/renames features can silently drop a render backend while
the API keeps compiling.

**How to apply:** if a visual Bevy feature runs without error but shows
nothing, suspect the feature list before the code — check
`bevy_internal-<ver>/Cargo.toml` for `<thing>` vs `<thing>_render` pairs and
weak `?/` wiring. `crates/client/examples/gizmo_smoke.rs` is a self-
screenshotting probe (mesh vs gizmo, writes /tmp/gizmo_smoke.png) that proves
the pipeline in seconds without a human looking at a window; the same pattern
(spawn `Screenshot::primary_window()` + `save_to_disk` + read the PNG) works
for any "is it actually drawing" question. Related: [[egpu-adapter-selection]].
