---
name: slint-codegen-size
description: Why Slint emits so much Rust here (LayerView force-inlining + both themes) and the measured fixes
metadata:
  node_type: memory
  type: project
  originSessionId: 324c7644-c50b-413b-9129-1de431a770f2
  modified: 2026-11-02T12:18:44.044Z
---

APPLIED 2026-11-02 (docs/superpowers/specs/2026-11-02-slint-codegen-size-investigation.md): LayerView fix in ~/Projects/ui (uncommitted there; reftest render pixel-identical, 202 pages) + `single_theme_overlay` in crates/slint-ui/build_common.rs (Fortress-only; Theme.factorio now does nothing).
- Dominant cause: `~/Projects/ui` widgets `LayerView` root has `visible:` → Slint force-inlines it, duplicating its if/for repeaters per use (441 repeater types). Moving `layer.present` into the `if`s: game rustc 22.5→8.1 s, frontend 25.6→11.4 s.
- Both themes (Factorio+Fortress, 6,936 sprites) compiled in; Fortress-only styles: →5.5 s / 7.6 s.
- Pruning unused styles doesn't change rustc time (only build script).
**How to apply:** widgets rule — never bind visible/opacity/shadow on a component root. Related: [[client-compile-time]], [[compile-time-investigation]].
