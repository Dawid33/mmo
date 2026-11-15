---
name: client-perf-text-diagnostics
description: "just diag / gpu-log / trace-chrome emit PLAIN TEXT perf data (CPU-vs-GPU verdict, per-system table) — use these, not Tracy/hotspot, when the analysis has to come back into a Claude session"
metadata: 
  node_type: memory
  type: project
  originSessionId: 65bac300-cfe2-4232-b1c0-0e6e280d5524
---

Added 2026-07-11 (uncommitted on `develop` at the time of writing): a text-emitting
performance pipeline for the Bevy client, built specifically so results can be pasted back
into an LLM session. The pre-existing tools (Tracy, Pyroscope+Grafana, hotspot) all require
a human to look at a GUI, so they are useless for that.

- `just diag` — `DIAG=1` adds Bevy's `RenderDiagnosticsPlugin` (`crates/client/src/renderer/diag.rs`);
  `scripts/diag_summary.py` reduces the log to frame_time vs summed per-pass GPU ms, i.e. the
  **CPU-bound vs GPU-bound verdict**, plus a sorted pass table with `fragment_shader_invocations`
  (the overdraw tell).
- `just gpu-log` — `amdgpu_top` JSON, GPU busy % from outside the process; no rebuild.
- `just trace-chrome` — `trace_chrome` feature → Chrome Trace JSON → `scripts/trace_summary.py`
  gives the per-ECS-system table. The follow-up when `diag` says CPU-bound. Only flushes on a
  clean exit — close the window, don't Ctrl-C.

**Why:** a "15 FPS" report is unactionable until you know which side is saturated; these three
answer that in one command each, in text.

**How to apply:** run them in RELEASE (dev is opt-level 1, ~2.5x off — an FPS number from
`just client` measures the build, not the game), and read the `AdapterInfo` line first
([[egpu-adapter-selection]] — the unpinned adapter was itself the 15 FPS). Note the client
presents with vsync, so ~16.7ms frames are idle-waiting on present, not saturated; the diag
verdict calls this out rather than mislabelling it CPU-bound.
