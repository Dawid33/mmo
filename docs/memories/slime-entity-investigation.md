---
name: slime-entity-investigation
description: Slime mob S1 IMPLEMENTED 2026-09-22 (uncommitted) — game/src/slime.rs + client avatar squish; spec docs/superpowers/specs/2026-09-22-slime-entity-investigation.md §6; live look pending
metadata:
  type: project
---
INVESTIGATED then S1 IMPLEMENTED 2026-09-22 (uncommitted). User chose: ambient wildlife, Minecraft cube, anywhere on surface, spawn rates as named consts (100-tick interval, 50%, cap 8/region). Edge leash keeps slimes inside their region (no handoff/cold-region wake). Live client look + perf with full regions NOT done; avatar client tests not run. Spec: docs/superpowers/specs/2026-09-22-slime-entity-investigation.md.
Key findings: slime would be the FIRST non-player entity (EntityKind only has Player). Generic already: ECS, Motion/plan/settle, handoff/ghosts, parking. Player-only seams: movement driver inside CameraController (camera.rs), per-tick pose emit only for camera entities (physics.rs), single-kind avatar, ColliderSpec capsule-only. TICK_RATE 50 ms == Minecraft tick so MC hop constants port 1:1. Recommended: in-sim MobController, stateless hash RNG h(tick,key,salt), small capsule collider, render-only squish, skip KCC while sitting, per-region cap.
Open questions posed to user: v1 scope (ambient vs feed/plort loop), cube vs blob look, habitat, density.
**How to apply:** don't re-research or re-plan S1; next slices are S2 curious/sizes, S3 feeding. Fits [[game-direction-entities]].
