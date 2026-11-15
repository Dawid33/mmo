---
name: configurable-tunables-preference
description: "Expose feature tunables (radii, thresholds, cadences) as named constants even when there's no UI to change them"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: efa0784a-96e3-446e-85a8-77ba951a02f8
---

When a design has a tunable value (e.g. far-region view radius, refresh throttle), the user wants it exposed as a **named constant** in the code so it can be changed later, rather than hardcoded inline or gated behind a config/UI. There is no UI to configure these — a single editable constant is the accepted mechanism.

**Why:** keeps future tuning cheap without over-building configuration infrastructure now (YAGNI on the UI, but not on the seam).

**How to apply:** for any magic number that shapes behavior, define `pub const NAME: T = ...` with a doc comment, in the crate that owns the concept (usually `game`). Surfaced during the far-region-visual-chunks brainstorm (spec 2026-07-07); see [[game-direction-entities]] for the broader "prefer generic/extensible mechanisms" leaning.
