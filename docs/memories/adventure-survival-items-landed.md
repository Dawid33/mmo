---
name: adventure-survival-items-landed
description: Adventure survival + items + player identity plan IMPLEMENTED 2026-11-05 (develop 311faaa..9931b79); live GUI check pending by user
metadata:
  node_type: memory
  type: project
  originSessionId: 8a2e9b19-e138-4cb9-82fc-50c24ae70b29
  modified: 2026-11-05T20:09:53.296Z
---

Plan docs/superpowers/plans/2026-11-05-adventure-survival-items.md was executed subagent-driven on 2026-11-05 (commits 311faaa..9931b79 on develop, not pushed). It covers stable player identities (PlayerToken/PlayerStore, park/restore across disconnects and restarts, the newest session takes over), items/inventory/hotbar, hold-to-mine, cell-keyed item piles (no entities, merge on deposit), player tree felling (drops at the resting centre of mass), chests, pile rendering and the inventory UI.

**Why:** the user wants Minecraft-survival first-person Adventure, with entity counts kept low. Player identity is meant to be served by an external DB later (it plugs in behind `PlayerStore`).

**How to apply:**
- Don't re-plan this work.
- Still open: the interactive live check (checklist in TODO.md / the task 13 report) and the browser localStorage path.
- Deferred minors are listed in TODO.md.
- Save-format bumps this round: region files v6, player files v5, history v3 (v2 still loads).

Related: [[game-direction-pivot]].
