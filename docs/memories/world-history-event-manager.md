---
name: world-history-event-manager
description: "User's rule that every world change must be an in-game-triggerable event; world event manager design (2026-11-01) and DF-style player model"
metadata:
  node_type: memory
  type: project
  originSessionId: affff7f8-c5ee-4bf7-bff1-32636f3002fc
  modified: 2026-11-01T15:20:59.362Z
---

**Standing rule (user, 2026-11-01), for every future feature:** anything that changes the world must be an event that can also happen, and be triggered, in fortress play. A world event manager (outside the tick world, one shared calendar at fortress speed) owns history; worldgen is the same system fast-forwarded once. Players never approve or veto an event; they only interfere through gameplay. Played forts are expected to diverge from unplayed ones.

Player model (user): DF-like — embark only onto empty land at a distance from sites, with people chosen from your civ's forts; fortress mode is always a disembodied camera; retire → fort goes abstract; adventure = a created character; one mode at a time per player; never take over an existing fort. Today's Play-any-region and the player dwarf body are dev scaffolding.

Design: docs/superpowers/specs/2026-11-01-world-history-events-design.md; plans 2026-11-01-world-history-{1,2,3}-*.md. ALL 3 PLANS IMPLEMENTED 2026-11-02 (develop a9ac0db..0c04c76, every task + final review clean). Pending: client unit tests written but never run (history_view, session_reset); live `just play` check of embark/retire/return; deferred: EmbarkSelection civ index → CivId and per-frame kinds Vec in slint_overlay.rs (waited on user first-person WIP). Don’t re-plan.

**Why:** the world should feel alive and the player be part of it. **How to apply:** for any new world-affecting feature, design its abstract resolver, in-fortress version and apply-to-region-on-load together. See [[game-direction-pivot]].
