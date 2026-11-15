---
name: game-direction-entities
description: The MMO is Minecraft-like; players are entities like NPCs — prefer entity-generic mechanisms over player-special-casing
metadata: 
  node_type: memory
  type: project
  originSessionId: 53828178-a245-4802-99ff-8647b2a56282
---

Stated by Dawid on 2026-07-04: the game will be Minecraft-like, and the
player is an entity like other NPC entities — NPCs will also carry colliders
and meshes.

**Why:** design decisions that special-case the player (player-only collider
paths, player-only render markers) will have to be generalized later anyway.

**How to apply:** when adding sim or render features for the player, route
them through entity-generic mechanisms (e.g. the `EntityKind` component and
`attach_capsule_collider_safe` from the 2026-07-04 collider/avatar spec)
rather than player-specific ones. The player should just be the first entity
kind wired through a generic path.
