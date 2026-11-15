---
name: radial-wheel-widget
description: RadialMenu widget BUILT 2026-10-30 in ~/Projects/ui (a3d5587..d82e011); REPLACED the fortress tool bar same day (hold Q, uncommitted)
metadata:
  type: project
---

`RadialMenu` / `RadialMenuFace` live in ~/Projects/ui `crates/widgets/ui/radial-menu.slint`. Dawid chose option C, so the wedges are Factorio `button` nine-slice art bent onto ring segments at build time (`codegen/radial.rs` + `wedges.rs`; 2–8 items, 7 states, 490 PNGs). Picking is by angle; there are headless tests and goldens in `crates/widget-tests`; the docs are `docs/radial-wheel.md`. Spec and plan are in mmo `docs/superpowers/{specs,plans}/2026-10-29-radial-wheel-widget*`.

**Why:** a pie menu for the fortress UI, styled like the rest of the Factorio widget set.
**How to apply:** don't re-plan it. Open items:
- DONE 2026-10-30, uncommitted: the wheel replaced the bottom tool bar in `GameUi` (hold Q — Alt + click is taken by Cinnamon/Xfce/KDE for window moves, so no Alt binding; `slint_overlay::tool_wheel`, rules in `crates/slint-ui/src/wheel.rs`; modal as a `Screens` entry). Level and priority readouts moved to the F9 Fortress panel. Not live-tested.
- mmo's `Cargo.toml` has no `[profile.dev.build-override] opt-level = 3`, so rebaking costs about 38 s there.
- The ui widget-themes plan will need to recolour `images/radial/`.

See also [[ui-slint-for-player-ui]] and [[slint-ui-library-investigation]].
