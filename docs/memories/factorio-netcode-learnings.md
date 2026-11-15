---
name: factorio-netcode-learnings
description: Factorio lockstep/latency-hiding/tick-cost research done 2026-11-07; report + ranked recs in specs
metadata:
  type: project
---
RESEARCH DONE 2026-11-07: docs/superpowers/specs/2026-11-07-factorio-netcode-learnings.md (FFF-sourced).
Key: Factorio never rolls back real state (latency-state overlay rebuilt per tick); per-tick cost ∝ active work (active lists, due-tick buckets, algorithmic movers). Top recs: tile-path walkers instead of rapier for creatures, region timer queue, never drop sends / resync region on DESYNC, heavy-mode serialize-restore test, per-client RTT lead. Nothing implemented.
**Why:** user asked to learn from Factorio for low tick cost + networking.
**How to apply:** don't re-research; start from the ranked table in §6. Related: [[client-prediction-runaway]], [[netcode-dropped-packet-gap]].
