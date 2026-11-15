---
name: tick-rate-500-broke-threaded-world-test
description: "TICK_RATE 200→500ms (a853794) makes threaded_world::roaming_client_cycles_regions_across_threads fail — timing assumption, fix test window or revisit with tick tuning"
metadata: 
  node_type: memory
  type: project
  originSessionId: 18cd1174-a5b7-4116-82c5-f79304cf5f04
---

`server/tests/threaded_world.rs::roaming_client_cycles_regions_across_threads` fails on develop since the user's tick-period change a853794 (`TICK_RATE` 200→500ms, 2026-07-10): the test sleeps 600ms and asserts ≥9 `GameEvent` tick packets from 9 region threads (threaded_world.rs ~line 164-172). At 500ms/tick each region fires ~1× in the window and startup skew drops some — observed "saw 5 events". Verified empirically 2026-07-10: same HEAD passes with TICK_RATE=200. Not caused by the worldgen [[column-optimization-landed]] work. Fix belongs with the tick tuning: either scale the test's sleep/threshold off `TICK_RATE` (e.g. sleep 2×TICK_RATE+margin) or settle the tick period first. CLAUDE.md notes TICK_RATE is deliberately in flux.
