---
name: entity-seam-crossing-handoff
description: Seam crossing now planned as the seam-transparent regions program (2026-11-06); supersedes the 2026-10-29 entity crossing plan
metadata:
  type: project
---
2026-11-06: the user accepted the seam-transparent design.
- Spec: docs/superpowers/specs/2026-11-06-region-seam-crossing-overview-and-prior-art.md
- Plan: docs/superpowers/plans/2026-11-06-seam-transparent-regions.md (7 phases; nothing implemented yet)
- The 2026-10-29 entity-seam-crossing plan is a pointer now; its text is at git bc7cdfd.

Key decisions:
- **Shared tick NUMBERING, never lockstep.** The user rejected neighbour barriers because they become world lockstep transitively.
- Transfers apply at max(T+1, the receiver's next tick), recorded in the receiver's own stream.
- Catch-up burst of 5 ticks, never skip a tick number.
- Intents apply at T+1 everywhere.
- Halo of 8 blocks.
- Wake policy: players wake the target, people are absorbed, everything else goes to a mailbox.

**Why:** the user expects seams to be an endless bug source and wants gameplay code written as if there were no regions.

**How to apply:** don't re-plan or re-research this. Implement phase by phase. Tree felling keeps its "fall inside the region" fallback until Phase 7.

Related: [[trees-plant-life-investigation]], [[region-threading-tests]].
