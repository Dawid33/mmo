---
name: fortress-hud-panels
description: EU5-style fortress HUD (top-left tabbed panel + top-right menu column/context box) replaced the F9 panel 2026-11-02; uncommitted; client unit tests + live look pending
metadata:
  type: project
---
IMPLEMENTED 2026-11-02 (uncommitted, mixed with Dawid's own uncommitted edits): spec docs/superpowers/specs/2026-11-02-fortress-hud-panels-design.md, plan docs/superpowers/plans/2026-11-02-fortress-hud-panels.md. Top-left `ui/fortress-panel.slint` (header value/happiness/food placeholders "0"; collapsible Tabs Governance/People/Designations/Stocks), top-right `ui/menu-column.slint` (Settings/Encyclopedia stub windows + context box), F9 panel and its Adventure/First-person buttons REMOVED by Dawid's choice (Adventure has no UI entry now). People/Designations lists are deliberately bare — Dawid expects to redo them.
Pending: `just test-one client "--lib fortress_panel::"` / `"--lib slint_overlay::"` not run (type-checked only); no live/screenshot look; deferred minors listed in the final report (narrow-window overlap <700 logical px, per-frame People rebuild, dev panel fixed 284 px shift). Related: [[intuitive-ui-research]], [[fortress-ui-landed]].
