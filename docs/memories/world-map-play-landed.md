---
name: world-map-play-landed
description: Esc menu + M world map + Play (move session to a region tile) IMPLEMENTED 2026-10-29 on develop 4ad7eee..99244f0; client unit tests + live run still pending
metadata:
  type: project
---
World map + Play landed 2026-10-29 (develop 4ad7eee..99244f0, 17 commits, subagent-driven; spec docs/superpowers/specs/2026-10-29-world-map-play-design.md, plan docs/superpowers/plans/2026-10-29-world-map-play.md). Toolchain bumped to 1.99.1 in the same run (clippy now red: macros/bbmodel/slotmapd/block-mesh — follow-up).

User decisions: server sends per-region data, client builds the map (never an image); viewing a tile never generates its region; button is "Play" not "Embark" (embark = future); menu doesn't pause; Slint + ../ui widgets; multiple players may share a tile; fortress players change tile only via the map (adventure-mode walking = future).

**Why:** DF-style fortress mode — pick a region of the forged world and play there.
**How to apply:** still pending as of 2026-10-29: `just test-one client "--lib slint_overlay::"` / `"--lib commander::"` (never run) and a live run (Esc/M/zoom/Play/refusal/Esc during box pick). Ledger with rulings: .superpowers/sdd/2026-10-29-world-map-play/progress.md. Related: [[game-direction-pivot]], [[ui-slint-for-player-ui]].
