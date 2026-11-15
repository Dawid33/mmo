---
name: ui-slint-for-player-ui
description: All UI is Slint (crates/slint-ui on ~/Projects/ui), F3 dev panel included; bevy_ui and egui removed from the build 2026-10-30
metadata:
  node_type: memory
  type: feedback
  originSessionId: a46897df-a729-4143-828f-939d05252892
  modified: 2026-10-20T16:10:49.641Z
---

All game UI should be built in **Slint** where possible (`crates/slint-ui`, the Factorio-style
widget library in `~/Projects/ui`, rendered via `renderer/slint_overlay.rs`). Bevy-side UI is
only for the F3 debugging screen (`renderer/dev_ui.rs`, which is egui today).

**Why:** Dawid added the Slint UI library on 2026-10-20 as the game's UI and said so explicitly.
**How to apply:** any plan/design with a screen, panel, menu, HUD element or loading indicator
puts it in Slint; don't propose new `bevy_ui` screens. In-world overlays (designation tints,
markers, cutaway) are renderer work, not UI. Supersedes the "keep bevy_ui" conclusion in
[[ui-options-research]].

**2026-10-30:** Dawid extended this to the dev panel too: bevy_ui, bevy_text and egui are out of the build
(plan docs/superpowers/plans/2026-10-30-slint-only-ui-and-unused-deps.md). Layers: HudUi (pass-through:
crosshair, selection band, minimap, hotbar), GameUi, DevPanelUi (F3), composited by renderer/slint_composite.rs.
Don't propose bevy_ui or egui for anything.
