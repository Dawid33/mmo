---
name: main-menu-landed
description: "Main menu / Factorio-style map generator / Load world / local server per world / in-process leave — IMPLEMENTED 2026-10-31 (develop facacbf..d6973c6), live GUI run pending"
metadata:
  node_type: memory
  type: project
  originSessionId: 8cfec512-ff28-4a64-8555-84cd2bef5a43
  modified: 2026-10-31T17:20:17.091Z
---

Implemented 2026-10-31 via subagent-driven development (plan docs/superpowers/plans/2026-10-30-main-menu-and-map-generator.md, spec …/specs/2026-10-30-main-menu-and-map-generator-design.md). Menu is the default (`just play`); dev recipes export VOXEL_SKIP_MENU=1. Client is now a session state machine (AppState, SessionScoped, add_session_reset/SessionLocal); a child server per world (VOXEL_SINGLE_PLAYER, exits on last leave or stdin EOF). Worlds in worlds/<slug>/.

**Why:** Dawid's request; decisions: child server per world (one world per server app, multiplayer may shard one world), clean in-process exit to menu, spawn region loads behind the modal arrival map, worlds/ in repo.

**How to apply:** don't re-plan. Open items: the live GUI run (checklist in final review report, now in the ledger dir if kept) incl. preset tuning by eye; pre-existing failing client tests from the x-ray commit (atmosphere shadows, commander eye_tile, lod_rings plain_removal); `just clippy` fails workspace-wide on untouched crates. Related: [[ui-slint-for-player-ui]], [[world-forge-prototype]].
