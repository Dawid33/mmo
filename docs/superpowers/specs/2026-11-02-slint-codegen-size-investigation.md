# Slint generated-code size investigation (2026-11-02)

**Status: both fixes APPLIED 2026-11-02** — the `LayerView` change in
`~/Projects/ui/crates/widgets/ui/graphical-set.slint`, and the Fortress-only overlay as
`single_theme_overlay` in `crates/slint-ui/build_common.rs` (the "in this repo" option below). After
both, a cold build of all three generated crates (in parallel) took 14.6 s wall.

Question: the Slint compiler emits tens of MB of Rust per `slint-ui-*` crate. Can we make it
generate less, to cut compile time?

**Answer: yes — two changes, both in how the `widgets` library (`~/Projects/ui`) is written or
fed to the compiler, cut the game crate's rustc time 22.5 s → 5.5 s (−76%) and the frontend's
25.6 s → 7.6 s (−70%).** Nothing in the game's own `.slint` screens needs to change.

## Method

No files in either repo were changed. The real compiled `slint-ui-game` / `slint-ui-frontend`
build scripts were re-run with `DEP_WIDGETS_UI_DIR` pointed at patched scratch copies of
`~/Projects/ui/crates/widgets/ui`, and the generated file was compiled by invoking `rustc` directly
with the dev-profile flags of those crates (`opt-level=0`, `debuginfo=line-tables-only`,
`-Zthreads=8`, `-Zembed-metadata=no`, pinned to 16 cores) against the already-built `slint` rmeta.
Generated code was broken down by brace-matching top-level items. Slint 1.18.1.

## Results

| variant | crate | generated .rs | build script | rustc wall | rustc CPU | rustc RSS | rlib |
|---|---|---|---|---|---|---|---|
| baseline | game | 12.46 MB | 5.3 s / 1.3 GB | **22.5 s** | 70 s | 5.0 GB | 579 MB |
| + LayerView fix | game | 5.37 MB | 4.7 s | **8.1 s** | 22 s | 1.7 GB | 157 MB |
| + single theme | game | 4.41 MB | 2.5 s / 0.67 GB | **5.5 s** | 15 s | 1.6 GB | 140 MB |
| + pruned unused styles | game | 3.98 MB | 0.34 s | 5.5 s | 15 s | 1.6 GB | 137 MB |
| baseline | frontend | 16.13 MB | 5.0 s | **25.6 s** | 74 s | 5.7 GB | 635 MB |
| + LayerView fix | frontend | 8.04 MB | 4.7 s | **11.4 s** | 22 s | 1.9 GB | 154 MB |
| + single theme | frontend | 5.85 MB | 2.5 s | **7.6 s** | 17 s | 1.7 GB | 138 MB |
| + pruned unused styles | frontend | 5.43 MB | 0.42 s | 7.6 s | 17 s | 1.7 GB | 135 MB |

(Rows are cumulative. The tools crate uses the same widgets and should behave the same; not measured.)

## Cause 1 (dominant): `LayerView` is force-inlined, duplicating its repeaters at every use

Slint's Rust generator does NOT inline components by default (`SLINT_INLINING` unset → false), but
`passes/inlining.rs` still force-inlines an instance when (`component_requires_inlining` /
`element_require_inlining`):

- the component's ROOT binds `visible`, `opacity`, `drop-shadow-*`, `inner-shadow-*`,
  `cache-rendering-hint`, a percentage `width`/`height`, or certain animations;
- the instance has children (the component uses `@children`) or forwards slots.

When a component is inlined, every `for` / `if` inside it becomes a brand-new repeated sub-component
(struct + `ItemTree` + `RepeatedItemTree` + `PinnedDrop` impls) at that use site.

`widgets/ui/graphical-set.slint`'s `LayerView` has `visible: layer.present;` on its root, and holds
three `if`s with nested `for`s over `LayerBox` → `PieceView` (two more `for`s). So each of its ~44
uses in the game crate emitted ~8 repeater components: the baseline game file had **441** repeated
components (`InnerComponent_rectangle_N` ×249, `InnerComponent_layerbox_N` ×132), ~60% of the
file. LayerView has no `@children`, so moving the condition into the `if`s is equivalent:

```diff
-    visible: layer.present;
     clip: !layer.outer;
-    if !layer.tiling.enabled: LayerBox {
+    if layer.present && !layer.tiling.enabled: LayerBox {
 ...
-    if layer.tiling.enabled && layer.tiling.columns.length == 0: Rectangle {
+    if layer.present && layer.tiling.enabled && layer.tiling.columns.length == 0: Rectangle {
 ...
-    if layer.tiling.enabled && layer.tiling.columns.length > 0: Rectangle {
+    if layer.present && layer.tiling.enabled && layer.tiling.columns.length > 0: Rectangle {
```

Repeated components: 441 → 97. Pixel equivalence is expected (absolute-positioned, no layout, no
children) but NOT verified — run `~/Projects/ui`'s reftest after applying it.

Rule for the widgets library going forward: **never bind `visible`/`opacity`/shadows on a component's
root element** — put them on an inner element, or fold them into `if` conditions.

## Cause 2: both themes are compiled in

`generated/styles.slint` (21 MB of Slint, written by `widgets/build.rs`) holds `FactorioStyles` and
`FortressStyles`, dispatched at runtime by `StylesFor.x(t) = t == fortress ? Fortress… : Factorio…`.
So both themes' style tables and all **6,936** sprite PNGs (3,466 per theme) are in every crate,
although the game only ever sets `Theme.fortress` (`ui/theme.slint`). A Fortress-only styles file
(Factorio globals deleted, the ternaries reduced to the Fortress branch; `Theme` enum and
`ActiveTheme` kept so `GameTheme` still compiles) takes rustc 8.1 → 5.5 s (game) / 11.4 → 7.6 s
(frontend) and halves the build script's time and memory.

Ways to ship it (the widgets import `generated/styles.slint` by relative path, so the library path
mapping alone can't redirect them):

- **(recommended) in this repo:** `crates/slint-ui/build_common.rs` materialises a single-theme
  overlay of the widgets ui dir in `OUT_DIR` (copy the ~20 small widget `.slint` files, write the
  rewritten `generated/styles.slint`, point `images/` + `fonts/` at the originals by symlink or by
  rewriting `@image-url` to absolute paths) and maps `widgets` to it. Assert the rewrite matched,
  like `redirect_embedded_images` does, so a widgets codegen change fails the build loudly.
  Trade-off: the in-game theme switch becomes compile-time (it is fixed to Fortress today anyway).
- **upstream in `~/Projects/ui`:** `widgets/build.rs` also emits a per-theme tree (or the styles
  split into per-theme files behind a theme-selected index) so consumers pick a theme by library
  path. Cleaner, but changes the other repo's codegen.

## Not worth doing

- **Pruning unused styles** (keeping only the 27 `Styles.*` actually referenced): build script
  2.5 → 0.3 s, but rustc unchanged — Slint already drops unused global properties from the Rust;
  it only keeps embedding every image file the `.slint` references (`include_bytes!` statics are
  cheap to compile). Only worth it if the build-script seconds matter.
- `SLINT_INLINING`: already off for Rust; turning it on would multiply the code.

## Residual (after both fixes)

What's left is mostly legitimate: one repeated component per `for`/`if` in the game's own screens,
the screen roots, and the Fortress style table (1.6 MB, 28% of the frontend file — the used styles'
nested struct literals). A further ~10–13% comes from components inlined because their instances
have children (`FrameFace` and other `@children` widgets), which re-emits their `if`/`for` layers per
use; restructuring those is possible but a smaller win.
