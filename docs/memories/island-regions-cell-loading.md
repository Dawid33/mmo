---
name: island-regions-cell-loading
description: "Cell mode (island = one lockstep region, server-authored CellChanges) was built 2026-09-25 then REMOVED 2026-10-30 at Dawid's request; whole regions + seams instead"
metadata:
  type: project
---

Cell mode (spec docs/superpowers/specs/2026-09-25-island-regions-cell-loading-design.md, phases 1–3 built) was REMOVED 2026-10-30 (develop 4aa9eb5..c590a9a, plan docs/superpowers/plans/2026-10-30-remove-cell-mode.md).

**Why:** Dawid: it doesn't fit the DF gameplay — full-replica lockstep means every client simulates every active cell (every fortress) of an island on one thread; "it is not feasible for clients to simulate the whole world, in the future the server might be made up of many computers". Chosen instead: "the old way" — whole regions, seams with handoff/ghosts, fortresses as separate regions, adventure travel crosses seams.

**How to apply:** don't propose cell loading / island-sized lockstep regions again. REGION_BLOCKS is 64/128/256; a 512 m size needs only a LOD_RING_TABLE row + choices entry (done 2026-10-30, cba2e5f: play regions are 512 m, LOD [(2,16)], snapshot ~50 MiB raw / 3.9 MiB wire; live run pending). Island worldgen (islands.rs, `just island=`) stayed. Golden constants for the removal were re-recorded only in the working tree (anim WIP shared the files) and 2-block goldens not re-recorded. Related: [[region-size-dial]], [[region-threading-tests]].
