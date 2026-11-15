---
name: creative-movement-landed
description: Minecraft-creative player movement replaced the freecam 2026-09-20 (uncommitted on develop at time of writing); parry voxels-shape prediction bug found on the way
metadata:
  type: project
---

IMPLEMENTED 2026-09-20 (working tree on develop, user had not asked for a commit): the freecam in `CameraController` is replaced by Minecraft-creative movement — `crates/game/src/movement.rs` (`Motion` ECS component riding `EntityBundle`, pure `plan`/`settle`, m/s tunables), walk/sprint(Ctrl)/jump(Space)/gravity/0.6 m autostep, double-tap Space toggles flight, Space/Shift vertical, landing ends flight. `KeyE` is still the cursor-grab toggle; gravity applies even with controls off. `InputState` gained `KeyState::Tapped` so a press+release inside one tick is still seen once.

Non-obvious finding: rapier's KinematicCharacterController NEVER reported `grounded` (so snap-to-ground and stairs were dead too) against `Voxels` colliders, because parry's `contact_manifolds_voxels_shape` intersected un-loosened AABBs and the KCC rests one skin offset above the surface. Fixed in the fork by loosening only the shape's AABB by the full `prediction` (loosening both by half, like voxels_ball does, finds no voxel — the intersection box then sits above the top layer).

**Why:** user asked for creative-style body controls instead of freecam; see [[game-direction-entities]] for why state is an entity component, not on `Client`.
**How to apply:** don't re-plan. Deferred on purpose: Minecraft body size (capsule is still 1.35 m × 0.8 m with the eye at its centre — no eye-height offset), horizontal inertia, sneak. Client harness `cross_boundary` now flies; `just test-one client "--test harness_smoke --test crossing"` was run by the user 2026-09-20 and passed (7/7). Still pending: a live `just adapter=6700 run` feel pass, and `just clippy` output was never actually inspected.

**Jump retune 2026-09-22** (uncommitted): the Minecraft-equal constants (9 m/s launch, 32 m/s² gravity, 0.56 s hang) felt "like jumping through honey" to the user; sim arc was verified textbook via `crates/game/tests/jump_arc.rs` (real KCC), so it was purely a feel dial. Now `GRAVITY` 45, `JUMP_SPEED` 10.6 (same 1.25 m peak, 0.24 s rise) plus new `FALL_GRAVITY` = 1.5× (falls harder than it rises); airtime 0.6 → 0.45 s. Levers: height = v²/2g, rise = v/g, fall = sqrt(2h/g_fall). Live feel still to be judged by the user.
