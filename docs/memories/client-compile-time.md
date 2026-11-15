---
name: client-compile-time
description: 2026-11-01 client compile-time investigation — feature unification, slint-ui opt 0, client lto=off; numbers + rejected levers
metadata:
  type: project
---
Investigated 2026-11-01, changes applied uncommitted (Cargo.toml + .cargo/config.toml + CLAUDE.md note).
- `-p client` vs root `--bin client` resolved different features on 26 deps → separate copies of rapier/game/worldgen/client and Slint/slint-ui per selection. Fixed with nightly `resolver.feature-unification = "workspace"` (wasm graph diff checked: only displaydoc/typenum/crossbeam-utils features, harmless; actual wasm build not run).
- slint-ui: 27.5 MB generated index.rs; 360 s at opt 1 (284 s LLVM: thin-local LTO + passes, 16 CGUs) → 62 s at opt 0. lto=off+256 CGUs only → 240 s.
- client lib: local ThinLTO made a dead-fn edit rewrite 242/256 CGUs (~36 s bin rebuild) → `-Clto=off` via profile rustflags: 19 CGUs, 14–17 s.
- Rejected: -Zthreads=8 on client (144 CGUs rewritten, slower); codegen-units=2048 (no merging, still 382/641 rewritten). Optional extra: client line-tables-only debuginfo ≈ −2–4 s.
- 2026-11-02 follow-up (uncommitted): Slint screens split into leaf crates slint-ui-game (10.6 MB gen) / slint-ui-frontend (13.6 MB) / slint-ui-tools (asset viewer + ControlsHarness, out of game build); slint-ui = facade re-exporting game+frontend; shared crates/slint-ui/build_common.rs; generated crates get -Zthreads=8 (non-incremental → safe; 42.5→34.8 s). .slint edit end-to-end ~35–42 s, client relink ~3 s. 50/50 slint-ui tests + wasm32 check pass.
- Dropped unused fork dev-deps (slotmapd fxhash, parry3d rand — benches only) → client test builds share all of Bevy with bin builds.
- worldgen dev-dep `rapier3d/profiler` KEPT ON PURPOSE: it keeps region-64 test copies of game chain apart from region-512 play copies (cargo fingerprints VOXEL_REGION_BLOCKS but doesn't key artifacts → shared copy would flip-flop). Don't "fix" it.
- game function-body edit: ~7 s (client+server) — no lto issue there. Client edit stays ~15–18 s.
- Remaining idea: split the 50k-line client crate (biggest lever left); client line-tables-only debuginfo (−2–4 s). Widget-lib generated bulk (Component_rectangle/layerbox, theme globals) only fixable in ~/Projects/ui.
Follows [[compile-time-investigation]] (2026-10-29 doc).
**Why:** user asked to speed client builds. **How to apply:** don't re-measure these; build on the remaining ideas.
