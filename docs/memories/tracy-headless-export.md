---
name: tracy-headless-export
description: "Tracy data can be pulled as text without the GUI — tracy-capture + tracy-csvexport v0.13.1 built at ~/Software/tracy, pipeline verified 2026-09-19"
metadata: 
  node_type: memory
  type: reference
  originSessionId: 54f38fa5-f082-440e-adff-9017b688d52f
  modified: 2026-09-19T15:09:20.612Z
---

RESEARCH DONE 2026-09-19; report: `docs/superpowers/specs/2026-09-19-tracy-headless-export-research.md`. Don't re-research.

- Tools built at `~/Software/tracy/{capture,csvexport}/build/tracy-{capture,csvexport}`, tag **v0.13.1** — must match `tracy-client-sys 0.28.0` in Cargo.lock (protocol is version-locked; rebuild tools if Bevy bumps tracy-client).
- Verified end to end with a probe on the exact crate versions (NOT yet with the real Bevy client): start `tracy-capture -o x.tracy -f [-s N] [-p port]` FIRST (it waits; client buffers from t=0 so the join phase is captured), run client with `TRACY_NO_EXIT=1` (+`TRACY_PORT`), then `tracy-csvexport` (aggregate / `-e` self / `-u` per-event / `-u -p` plots / `-g` GPU / `-m` messages / `-f` filter).
- Limits: `-u` dump is huge (filter + script-reduce, never read whole); no frame-time table; span field values are part of zone names; no export of sampling/ctx-switch/lock/memory data.
- Instrumentation gap: first-party crates (client/game/server/worldgen) have ZERO tracing spans — Tracy and trace-chrome see Bevy spans only; client sim threads in instance.rs are unnamed. tracing events (and `log::` lines via Bevy's LogTracer, so the `[perf]` timeline) become Tracy messages. The report doubles as the handoff for the perf-profiling agent (data catalogue + implementation order).
- `just tracy-capture` recipe is only sketched in the report, not implemented. Related: [[client-perf-text-diagnostics]], [[perf-capture-tooling-landed]].
