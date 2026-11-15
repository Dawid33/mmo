---
name: region-crossing-pause
description: Flying across region seams paused 300-550 ms; root cause (stale SyncClock stalled new home) FIXED 2026-09-23 uncommitted; sustained-flight backlog is the open follow-up
metadata:
  node_type: memory
  type: project
  originSessionId: 6f066a53-b530-4185-b773-8a738775090b
  modified: 2026-09-23T18:12:34.025Z
---

Measured 2026-09-23 with `just fly-probe` (autopilot, RX 6700, 5x5 window):
- PAUSE ROOT CAUSE: right after a home flip, the new home's SyncClock arrived ~350 ms stale (queued behind the 5 region snapshots the window shift requests: server writer encodes ~30 ms + sends each serially; client reader decodes ~55 ms each inline; game thread loads each). `align_region` then sat the new home out 6-10 ticks. FIX (instance.rs `HOME_STALL_MIN_MS`): home is never stalled for being ahead within the server's 1 s input-hold horizon; slew closes it. home_stalled_ticks 6-10 -> 0, home tick gap 350-557 -> 50-140 ms.
- Also landed: one region tree spawned + one despawned per frame in the bridge (`RetiredRegionRoots`), retire_lod_roots backlog scan 7.4 -> 1.1 ms/frame, interpolate skips pending-voxel chunks, new_region moves the snapshot instead of cloning a whole Rollback.
- Autopilot never presses E/grabs the cursor: `InputEvent::FpsCam(bool)` (new sim input, idempotent, no SetFreeCam); forward_input/look ignore mouse motion unless the window is focused + grabbed (user complained the camera moved with the mouse over other windows).
- OPEN: (1) sustained flight outruns chunk streaming — ~20k chunk ENTITIES per region but only ~2.2k ever mesh; backlog never drains after ~4 crossings, RSS 1 -> 3.8 GB in 45 s, fps 60 -> ~39; fix = don't spawn entities for empty/buried chunks (asked user). (2) intermittent ~2 s frame in `prepare_windows` (GPU wait) late in flight; VRAM flat, not reproduced every run. (3) the wire HOL itself (bulk snapshots vs realtime packets on one serial writer/reader) still delays PlayerRegion confirm to 100-900 ms.
- Client bridge/bevy_input unit tests were edited but only type-checked (client tests are the slow opt-in).

**Why:** don't re-diagnose the crossing pause; the probe numbers above are the baseline.
**How to apply:** start follow-ups from `just fly-probe` output; related [[chunk-loading-speed-investigation]], [[lod-near-handoff]], [[input-slot-rubber-banding]].

UPDATE 2026-09-23 (later): `just shuttle-probe [region|chunk]` added (autopilot back-and-forth across one seam/chunk edge; `scripts/crossing_summary.py` table). Clean region run: 31 flips/30 s, 0 stalled ticks, 50 ms tick gaps, frames <=28 ms — BUT every flip reloads 5 regions (near window flaps; no hysteresis) — cheap only because the spawn seam's regions are light. The "intermittent 2 s GPU frame" correlates with eGPU Thunderbolt `pciehp: Timeout on hotplug command` (~1.2 s each, 40-70 s into a run; once escalated to DeviceLost) — hardware/driver, not the game.

UPDATE 2026-09-25: near-window HYSTERESIS IMPLEMENTED (uncommitted): `RegionCoords::recentered` (game/src/protocol.rs, unit-tested) + client `advance_window_center` — the near window AND LOD rings recentre only WINDOW_RECENTER_MARGIN_M = 16 m into a neighbour; `VOXEL_WINDOW_MARGIN_M` overrides (0 = old). A/B on Intel iGPU, laptop on BATTERY (frames ~22 ms baseline both runs): snapshots/crossing 5 -> 0, worst frame p50 56 -> 32 ms (max 99 -> 37), pose hold p50 80 -> 51 ms, corrections p50 19 -> 2, main stalls 4 -> 0. CORRECTION: DeviceLost is NOT explained by the eGPU link — it also happened on Intel with nothing in dmesg; both losses were in churn runs (margin 0 / pre-hysteresis), neither sticky run lost it — cause OPEN. The RX 6700 eGPU was unplugged on 2026-09-23 20:13 (pciehp Link Down).
