---
name: player-height-and-world-editing
description: 2026-09-22 player body resized to 1.8 m (two blocks) with an eye offset; world editing investigated, design written, nothing built
metadata:
  type: project
---
Player body is now 1.8 m tall, 0.4 m radius, eyes at 1.62 m. The metre tunables live in `game::movement` (PLAYER_HEIGHT, PLAYER_EYE_HEIGHT, PLAYER_EYE_OFFSET) and the capsule derives from them. The user asked for "two blocks"; 1.8 m was chosen because an exact 2.0 m body can't fit a two-block gap. The KCC skin is now pinned to an absolute CHARACTER_SKIN = 0.216, because rapier's default is 1% of the body height and it drifted when the body grew. The client camera settles at body + EYE_OFFSET via SimTarget.offset. Uncommitted as of 2026-09-22.

World editing: v1 IMPLEMENTED 2026-09-22 (uncommitted), crates/game/src/edit.rs. Left click breaks, right click places dirt. It deviates from the spec's named-target design: the sim raycasts during the tick from mouse-button input (InputState now tracks buttons), so nothing new crosses the wire. Colliders are swapped in place via mutate_collider_safe, which uses new ColliderSet pop_modified in the rapier fork. Rollback is bit-exact under Full. Not done: cross-region edits, hotbar block choice, highlight, persistence. Not live-tested.

The 1.8 m body broke the `just test-threads` corner walk, because the body arrives sunk ~3 units into the floor after the seam dip. Fixed with movement::depenetration_lift, since the fork KCC has no penetration recovery. Run test-threads after any body or movement change.

**Why:** the user asked on 2026-09-22 for the height change and an investigation of editing.
**How to apply:** don't re-investigate editing; extend edit.rs (next steps are in the spec's "What was built" section). See [[creative-movement-landed]] and [[kcc-voxel-seam-jitter]].
