---
name: compile-time-investigation
description: 2026-10-29 static compile-time/deps investigation — findings doc, top levers (debug info 91% of 2.5 GB client, region-dial target thrash, slint 9.8 MB codegen); nothing implemented
metadata:
  type: project
---
Investigated 2026-10-29 (static only, no timing builds — box was busy): docs/superpowers/specs/2026-10-29-compile-time-and-dependencies-investigation.md.
Top levers: dev debug="full" on all deps (client debug binary 2.5 GB, 91% DWARF); test (64) vs play (256) region dial share target/debug and rebuild game→client on every alternation; slint-ui generates 9.8 MB Rust; unused deps listed (macros proc-macro has 7 extra deps).
**Why:** user asked how to cut deps + compile time.
**How to apply:** 2026-10-30 plan for unused deps + Slint-only UI (bevy_ui dropped): docs/superpowers/plans/2026-10-30-slint-only-ui-and-unused-deps.md (Slint already 1.18.1 = latest). Executed 2026-10-30 (unused deps, block-mesh ilattice, bevy_ui/egui gone; client deps 701→653). Not done: debug-info / target-dir / slint codegen levers; next step = measure with `cargo build --timings` on an idle box (ask first, see [[check-before-running-benchmarks]]), then apply in the doc's order. Don't re-survey.
