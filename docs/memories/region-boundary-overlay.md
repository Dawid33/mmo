---
name: region-boundary-overlay
description: Home-region + simulated-window boundary overlay painted in voxel_pull.wgsl (F8); built 2026-10-29, not live-looked yet
metadata:
  type: project
---
Built 2026-10-29 (uncommitted): `crates/client/src/renderer/boundaries.rs` builds a `BoundaryUniform` (home rect + 8×8 bitmask of `RegionRoots` keys + tunables) → group(1) binding 5 of the pull path; `voxel_pull.wgsl` `apply_boundaries` paints a warm glowing home band (centred, + outside darken/desaturate) only. A blue dashed sim-window band was built then REMOVED at the user's request (only the play area is simulated at 256 m regions, so it sat right next to the home band). F8 / F3 panel toggle. User then asked for it all LESS prominent: darken 0.12, desaturate 0.2; then "a line, not a whole block": then thicker: quarter-block (4 voxel) world-width perimeter line, min 1.5 px (fwidth of the rect SDF), opacity 0.6, no glow; drawn OUTSIDE only (sd 0..width, never on the play area). User chose this look from 5 options (draped line, shader band, light curtain, outside dimming, markers).

**Why:** clarity of playable vs simulated area in the DF-style commander view.
**How to apply:** not drawn by the WebGL2 per-chunk Mesh fallback or brick far renderer; pull_smoke passed on Intel; live look + boundaries unit tests (client lib) still pending. Tune via the consts at the top of boundaries.rs.
