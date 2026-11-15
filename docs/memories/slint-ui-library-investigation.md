---
name: slint-ui-library-investigation
description: 2026-10-20 investigation of using ~/Projects/ui (Factorio-GUI recreation in Slint) in the Bevy client vs porting it to bevy_ui
metadata:
  type: project
---
INVESTIGATED + option A IMPLEMENTED 2026-10-20 (uncommitted; crates/slint-ui + client renderer/slint_overlay.rs, commander mode, F9 panel; type-checked + smoke ok, NOT run live, spike timing/screenshot/wasm-size deliberately skipped per user). Doc: docs/superpowers/specs/2026-10-20-slint-ui-library-in-bevy-investigation.md.
~/Projects/ui = Factorio 2.0 GUI generated from style.lua, reftested (Δ≤3) ONLY via Slint's software renderer.
Recommendation: option A = Slint software renderer → Bevy Image overlay (no shared wgpu, no 2nd event loop; slint default-features off); option B (bevy_ui port reusing codegen) only if mod/data-driven UI must share the look — traps: linear-space blending, parley text, taffy layout.
Blockers either way: Wube art not shippable; Slint royalty-free licence needs attribution; widgets/build.rs panics without a Factorio install.
**Why:** the user asked for this investigation; it supersedes part of the 2026-09-22 "Slint: No" verdict, which was aimed at the GPU integration.
**How to apply:** next step is the 4-step spike in §5 of the doc; ask before running its timing step ([[check-before-running-benchmarks]]).
