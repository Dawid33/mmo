# Slint-only UI + unused dependencies — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove every unused dependency the 2026-10-29 investigation found, drop Bevy's UI
stack (`bevy_ui`, `bevy_ui_render`, `bevy_text`, `ui_picking`, `default_font`), and remove egui
(`bevy_egui`) by rebuilding the F3 dev panel in Slint. Slint becomes the only UI toolkit in the
client.

**Architecture:** Slint layers are already software-rendered into a window-sized `Image`, but
that image reaches the screen through a `bevy_ui` `ImageNode`. A small custom `Core3d` pass
(`renderer/slint_composite.rs`) replaces it. The pass blits every Slint layer's texture, in
order, onto the camera marked `UiCamera`, in the slot `bevy_ui_render` used: after
`Core3dSystems::PostProcess`, before `upscaling`. Everything still drawn with `bevy_ui` moves
into a new **pass-through** Slint component `HudUi`, which never takes input or the pointer:

- the crosshair
- the selection band
- the minimap
- the hotbar

The interactive `GameUi` layer stays above it, commander mode only, unchanged. The F3 dev
panel becomes a third, interactive Slint layer, `DevPanelUi`, above both, in both camera
modes, still behind the `dev_ui` feature (which no longer pulls any dependency). The hotbar's
generic flexbox widget tree (`game::ui`) becomes a typed `HotbarView`. That tree was only ever
laid out by `bevy_ui`, and nothing else uses it.

**Tech Stack:** Bevy 0.19.1 (custom `Core3d` systems, `SpecializedRenderPipelines`,
`FullscreenShader`), Slint 1.18.1 software renderer (`crates/slint-ui`, widgets from
`~/Projects/ui`), WGSL.

**Spec:** `docs/superpowers/specs/2026-10-29-compile-time-and-dependencies-investigation.md`
(§4 dependency table, §5 two text stacks). The `bevy_ui` surface inventory is in "Current state"
below.

## Current state (verified 2026-10-30)

- **Slint is already 1.18.1, the newest release** (`Cargo.lock`: `slint`, `slint-build`,
  `i-slint-core`, `i-slint-compiler`, `i-slint-renderer-software` = 1.18.1; `../ui/Cargo.toml`
  pins `1.18.1`; `cargo search` → 1.18.1). The "upgrade to 1.18" request needs no version bump.
  Task 0 re-checks it at execution time.
- `bevy_ui` users in `crates/client`:
  - `renderer/slint_overlay.rs:~169`: the layer's own `ImageNode` + `GlobalZIndex(1000)`
  - `renderer/hud.rs`: the crosshair, two `Node` bars, first-person only
  - `renderer/commander/minimap.rs` `setup_minimap`: frame + two `ImageNode`s (base 256², overlay 440²)
  - `renderer/commander/selection.rs` `setup_selection_box` / `update_ground_pick`: the
    rubber-band `Node`
  - `renderer/ui_bridge.rs`: `game::UiUpdate` → `Node`/`Text`. Its tree is only the hotbar,
    with no slot actions; filled slots are drawn as a tint, never an icon.
  - `renderer/ui_input.rs`: `Pointer<Click>` observer `on_widget_click`, with nothing to click
  - `bevy::ui::IsDefaultUiCamera` used as "the active view camera" in `bridge.rs`
    (insert/move/tests), `commander/mod.rs:385`, `dev_ui.rs`, `grid_debug.rs`
  - the compile probe `renderer/mod.rs` `bevy_ui_features_enabled`
- The egui F3 panel (`renderer/dev_ui.rs`, 166 lines, `dev_ui` feature → `dep:bevy_egui`,
  wired in `main.rs:300`):
  - **Stats:** FPS, frame ms, camera position, home and viewer region, connection,
    terrain near/far backends, entity count.
  - **Six checkboxes:** region boundaries (F8), chunk grid (F5), cave culling (F4), hide LOD
    (F6), hide near terrain (F7), collider wireframes.
  - **A "Complete all orders" button**, which sends `Order::DevCompleteAll`.
  - **Behaviour:** anchored top-right at 8 px; F3 toggles it; while open it sets
    `CursorPolicy::overlay_open`; its egui context follows `IsDefaultUiCamera`.
  - The widgets library has `Checkbox { text, in-out checked, toggled(bool) }` and
    `Button { text, clicked }`, so the Slint panel uses the same Factorio look as `GameUi`.
  - `Pickable::IGNORE` on all of the above
- Feature resolution was checked offline (scratch crate, with egui still in). Without the five UI features,
  `bevy_ui`, `bevy_ui_render`, `bevy_text`, `bevy_sprite(_render)`, parley 0.9, fontique 0.9,
  swash, harfrust 0.6, skrifa 0.42, read-fonts 0.39, taffy, `bevy_input_focus` and
  `bevy_clipboard` all leave the graph, with gizmos kept. `bevy_picking` stayed only because
  `bevy_egui/picking` pulled it, so it should go in Task 9, along with egui's own
  `epaint`/harfrust 0.12 stack.
- There are uncommitted edits by another session in `crates/client/Cargo.toml`, `justfile`,
  `slint_overlay.rs`, `examples/{asset_viewer,model_view}.rs` and `slint-ui`. **Do not revert
  them or commit them as part of this plan.** Commit only your own hunks
  (`git add -p`). If one of your files has foreign hunks, leave those unstaged.

## Global Constraints

- `game`, `server`, `worldgen` stay Bevy-free and windowing-free.
- Work on `develop` in the main checkout; no worktrees.
- Builds and tests go through `just` (core-pinned). Never run whole suites. Verify with
  `just smoke`, plus the named `just test-one …` targets in each task. Approving this plan
  covers the `just test-one client "--lib <filter>"` runs named below (the slow opt-in). Run
  nothing broader without asking.
- `just determinism-wasm` after any task that changes `game`'s dependencies (Tasks 1, 2, 7).
- Tag every new test that takes >100 ms `#[ignore = "slow: >100ms — run via just test-full"]`.
  All slint-ui render tests are tagged that way already; run them with
  `just test-one slint-ui "--test <name> -- --include-ignored"`.
- Live checks use `adapter=Intel` (`just adapter=Intel …`); the 6700 is often unplugged.
- Tunables are named constants. Every Slint size that must match a Rust const gets a
  `debug_assert_eq!` against a Slint `global`, following the `WorldMapConfig` pattern in
  `connect_game_ui`.
- Pixels must look as they do today: straight-alpha upload (keep `unpremultiply`) and
  `BlendState::ALPHA_BLENDING`, like `bevy_ui`.
- Keep the justfile and CLAUDE.md in sync. Task 9 edits CLAUDE.md's UI paragraphs.

## Review Focus

1. **Window resize or a scale-factor change** (dragging the window between monitors). The
   composite must sample the layer's *current* texture at 1:1 and never stretch an old one.
   Task 4 pins that extraction reads the handle each frame, plus a live resize check.
2. **No `UiCamera`, or the marked camera changes** (first-person body camera moving between
   local players; commander ↔ first-person). The UI is drawn on exactly one camera, and none
   is no-op, not a panic. Task 3 pins the marker-follow tests; Task 4 pins the no-camera case.
3. **The pass-through HUD stealing the pointer.** A band or minimap pixel under the cursor
   must never set `owns_pointer` / `CursorPolicy::ui_pointer`; otherwise box-select drags and
   minimap drags break mid-gesture. Task 4 pins `layer_owns_pointer`.
4. **Layer order and the F3 panel over everything.** `GameUi` (menu, map, tool bar) must
   draw above `HudUi` (minimap, hotbar), as the old `GlobalZIndex(1000)` did, and the dev panel
   above both, in first-person mode too, where `GameUi` doesn't exist. Task 4 pins
   `sorted_layers`; Task 8 pins the panel's order constant and its first-person wiring.
5. **HDR vs LDR targets and WebGL2.** The pipeline is specialised on the view target's format,
   and wasm uses WebGL2 (no storage buffers; texture + sampler only). Task 4 pins WGSL
   validity and keys on `ViewTarget::main_texture_format()`. Task 9 runs a wasm type-check.

---

### Task 0: Confirm Slint 1.18 is current

**Files:** none unless a newer 1.18.x exists.

- [ ] **Step 1:** Run `cargo search slint --limit 1` and `grep -A1 '^name = "slint"$' Cargo.lock`.
  Expected: both say `1.18.1`.
- [ ] **Step 2:** Only if crates.io shows a newer 1.18.x: bump `slint`, `slint-build`
  (root `Cargo.toml`) and `i-slint-core` (`crates/slint-ui/Cargo.toml`, keep the `=` pin), and
  `../ui/Cargo.toml` the same. Then run `cargo update -p slint -p slint-build -p i-slint-core`,
  `just smoke`, and `just test-one slint-ui "--test world_map -- --include-ignored"`.
  Commit separately: `chore: slint 1.18.x`. Otherwise record "no bump needed" in the final
  report and move on.

### Task 1: Remove unused dependencies

**Files:**
- Modify: `crates/macros/Cargo.toml`, `crates/game/Cargo.toml`, `crates/server/Cargo.toml`,
  `crates/server/src/lib.rs:134-136`, `crates/client/Cargo.toml` (your hunk only),
  `crates/bbmodel/Cargo.toml`, `Cargo.toml` (`papaya` workspace entry)

**Interfaces:** none (no API change).

- [ ] **Step 1: Re-verify each removal before deleting it.** For each crate below, run
  `grep -rnP "(?<![A-Za-z0-9_])<ident>(::|!)" crates/<crate>/{src,tests,examples,benches,build.rs}`
  and expect no hits. For `game`, also grep `crates/macros/src/lib.rs`: any ident that appears
  in `quote!` output must stay in `game`. Expected to stay in `game` because of that:
  `borrow`, `derive_more`, `crossbeam`, `log`, `assert-json-diff`, `serde_json`, `slotmapd`.
- [ ] **Step 2: Remove**
  - `macros`: everything except `syn`, `quote`, `proc-macro2` (drop `derive_more`, `borrow`,
    `bincode`, `crc32fast`, `crossbeam`, `log`, `assert-json-diff`).
  - `game`:
    - drop `bytemuck`, `arrayvec`, `either`, `spade`, `foldhash`
    - drop the never-enabled optional deps `vec_map`, `web-time`, `slab`, `ordermap`,
      `hashbrown`, `obj`, `ena` and their commented-out feature lines
    - replace the copy-pasted taffy `[features]` block with:
      ```toml
      [features]
      default = []
      ```
      First grep for `cfg(feature` in `crates/game`. Keep any feature a `cfg` actually reads,
      e.g. `profile`.
  - `server`: drop `papaya`, `slotmapd`, `bincode`, `crc32fast`; root `Cargo.toml` drops the
    `papaya` workspace entry if nothing else uses it.
  - `server` `rcgen`: bump the workspace pin to `0.14` (same as `wtransport`'s) and change
    `lib.rs:136` from `cert.key_pair.serialize_der()` to `cert.signing_key.serialize_der()`
    (0.14's `CertifiedKey { cert, signing_key }`).
  - `client`: drop `bincode`, `crc32fast` and `serde_json`, each only if Step 1 confirms no use.
    `crc32fast` *is* used in `meshing/queue.rs`; keep it.
  - `bbmodel`: drop `log`.
- [ ] **Step 3: Verify**
  - `just smoke` → passes inside 60 s
  - `just determinism-wasm` → passes
  - `cargo tree -d -e normal --target x86_64-unknown-linux-gnu | grep -c '^rcgen'` → `1`
- [ ] **Step 4: Commit** `chore: drop unused dependencies`

### Task 2: `block-mesh` fork without `ilattice` / glam 0.19

**Files:**
- Modify: `crates/block-mesh/Cargo.toml`, `crates/block-mesh/src/{lib,bounds,greedy,simple}.rs`,
  `crates/block-mesh/src/geometry/{axis,face}.rs`

**Interfaces:** none of the APIs `game` (`ndshape::ConstShape*`, `Voxel`, `MergeVoxel`,
`VoxelVisibility`), `client` or `worldgen` use may change. Check with
`grep -rn "block_mesh::" crates/{game,client,worldgen}`.

- [ ] **Step 1:** Make `ilattice` an optional dependency behind a new feature `ilattice`
  (`ilattice = { version = "0.1", optional = true }`), off by default. Gate every
  `ilattice`-using item with `#[cfg(feature = "ilattice")]`. If a used API needs it internally,
  replace that `ilattice` type with plain `[u32; 3]`/`[i32; 3]` math instead of gating.
- [ ] **Step 2: Verify**
  - `cargo tree -i ilattice --target x86_64-unknown-linux-gnu` → "did not match any packages"
  - `cargo tree -i glam@0.19.0` → no match
  - `just smoke` → passes
  - `just test-one client "--lib meshing::"` → passes
  - `just determinism-wasm` → passes
- [ ] **Step 3: Commit** `chore(block-mesh): ilattice optional, drop glam 0.19 from game`

### Task 3: `UiCamera` marker replaces `IsDefaultUiCamera`

**Files:**
- Create: `crates/client/src/renderer/slint_composite.rs` (marker only in this task)
- Modify: `renderer/mod.rs` (module + re-export), `renderer/bridge.rs:751,946,1064-1105` +
  tests `~1232,1993-1997,2051-2052`, `renderer/commander/mod.rs:385`, `renderer/dev_ui.rs:11,53,86`,
  `renderer/grid_debug.rs:6,168,180`, `examples/asset_viewer.rs:265`, `examples/model_view.rs`
  (its camera spawn; your hunk only)

**Interfaces:**
- Produces: `pub struct UiCamera;` in `renderer::slint_composite`, re-exported as
  `renderer::UiCamera`, with
  `#[derive(Component, Clone, Copy, Default, Debug, bevy::render::extract_component::ExtractComponent)]`.
  This is "the camera the UI is composited onto". At most one is active.

- [ ] **Step 1: Update the tests first.** In `bridge.rs`'s existing tests, replace each
  `contains::<bevy::ui::IsDefaultUiCamera>()` with `contains::<UiCamera>()`. That covers the
  test at ~1232 and the marker-follows-local-player tests at ~1993/2051.
- [ ] **Step 2:** Run `just test-one client "--lib bridge::"`. Expected: FAIL to compile or
  assert (no `UiCamera` yet).
- [ ] **Step 3:** Add `UiCamera`. Wherever `IsDefaultUiCamera` is inserted or removed, insert
  or remove `UiCamera` **alongside** it; `bevy_ui` still needs its marker until Task 9. Switch
  every *query* (dev_ui, grid_debug, `bridge.rs:1077` `Has<…>`) to `UiCamera`. In grid_debug's
  "no camera" log message, change the name to `UiCamera`. The examples add `UiCamera` to their
  single camera.
- [ ] **Step 4:** Run `just test-one client "--lib bridge::"`. Expected: PASS. Then `just smoke`.
- [ ] **Step 5: Commit** `refactor(client): UiCamera marks the UI's camera`

### Task 4: Slint composite pass (replaces the `ImageNode`)

**Files:**
- Modify: `crates/client/src/renderer/slint_composite.rs`, `renderer/slint_overlay.rs`
  (`SlintLayerPlugin`, `setup_slint_layer`, `slint_input`, `render_slint_layer`'s doc)
- Create: `assets/shaders/slint_composite.wgsl`
- Test: in-file `#[cfg(test)]` in both `.rs` files

**Interfaces:**
- Consumes: `UiCamera` (Task 3).
- Produces, in `slint_composite.rs`:
  - `pub struct SlintCompositePlugin;`, added by `SlintLayerPlugin::build` if not already added
    (`app.is_plugin_added`)
  - `#[derive(Resource, Default, Clone)] pub struct SlintLayerTextures(pub Vec<(i32, Handle<Image>)>);`
    in the main world. Each layer pushes `(order, target)` at setup.
  - `pub fn sorted_layers(layers: &[(i32, AssetId<Image>)]) -> Vec<AssetId<Image>>`:
    ascending order, stable for ties.
  - render world: `ExtractedSlintLayers(Vec<AssetId<Image>>)` (sorted), extracted in
    `ExtractSchedule`. Bind groups are built per layer in `PrepareBindGroups`. A
    `SlintCompositePipeline` (layout: `texture_2d<f32>` + filtering-off sampler) is created in
    `RenderStartup` and specialised by `SlintCompositeKey { format: TextureFormat }` via
    `SpecializedRenderPipelines`. A per-view `ViewSlintComposite(CachedRenderPipelineId)` is
    prepared for views with `UiCamera`.
  - `pub fn slint_composite_pass(...)` in `Core3d`:
    `.after(Core3dSystems::PostProcess).before(bevy::core_pipeline::upscaling::upscaling)`.
    One render pass on `view_target.get_unsampled_color_attachment()` and the camera viewport;
    for each layer, set its bind group and `draw(0..3, 0..1)`. Reference:
    `bevy_ui_render-0.19.1/src/render_pass.rs` `ui_pass`.
- Produces, in `slint_overlay.rs`:
  - `pub const GAME_LAYER_ORDER: i32 = 1_000;` (replaces `OVERLAY_Z`) and
    `pub const HUD_LAYER_ORDER: i32 = 0;`
  - `SlintLayerPlugin::order(self, i32) -> Self` (default `GAME_LAYER_ORDER`) and
    `SlintLayerPlugin::passthrough(self) -> Self`: the layer is sized and rendered but never
    fed input.
  - `pub fn layer_owns_pointer(passthrough: bool, over_ui: bool, captured: bool) -> bool`,
    used by `slint_input` to set `SlintLayer::owns_pointer`.
- WGSL: `#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput`;
  `@group(0) @binding(0) var layer: texture_2d<f32>; @group(0) @binding(1) var layer_sampler: sampler;`
  The fragment returns `textureSample(layer, layer_sampler, in.uv)`. The vertex state is
  `FullscreenShader::to_vertex_state()`; the blend is `BlendState::ALPHA_BLENDING`.

- [ ] **Step 1: Write the failing tests**
  ```rust
  // slint_composite.rs
  #[test]
  fn layers_draw_in_ascending_order_stable_on_ties() {
      let id = |n: u128| AssetId::<Image>::Uuid { uuid: bevy::asset::uuid::Uuid::from_u128(n) };
      let (a, b, c) = (id(1), id(2), id(3));
      assert_eq!(sorted_layers(&[(1000, a), (0, b), (0, c)]), vec![b, c, a]);
  }
  #[test]
  fn composite_shader_is_valid_wgsl() { /* validate_shader on assets/shaders/slint_composite.wgsl
      with a stub for bevy_core_pipeline::fullscreen_vertex_shader (struct FullscreenVertexOutput
      { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32> }), the same way
      slice_plane_shader_is_valid_wgsl does */ }
  #[test]
  fn no_ui_camera_extracts_no_view_pipeline() { /* MinimalPlugins render-free check: with
      SlintLayerTextures non-empty and no UiCamera, the view query used by prepare is empty */ }
  // slint_overlay.rs
  #[test]
  fn passthrough_layer_never_owns_the_pointer() {
      assert!(!layer_owns_pointer(true, true, true));
      assert!(layer_owns_pointer(false, true, false));
      assert!(layer_owns_pointer(false, false, true)); // a press that started on the UI
      assert!(!layer_owns_pointer(false, false, false));
  }
  ```
  If `no_ui_camera_extracts_no_view_pipeline` can't run without a render app, replace it with a
  pure `fn composite_views(cams: impl Iterator<Item = (Entity, bool /*has UiCamera*/)>) -> Vec<Entity>`
  and test that empty input and no marker both give `vec![]`.
- [ ] **Step 2:** Run `just test-one client "--lib slint_"`. Expected: FAIL (missing items).
- [ ] **Step 3: Implement.** In `setup_slint_layer`, delete the `ImageNode`/`Node`/`GlobalZIndex`
  spawn; push `(order, target.clone())` into `SlintLayerTextures` instead. Keep the
  un-premultiplied `Rgba8UnormSrgb` upload and `ImageSampler::nearest()` unchanged. In
  `slint_input`, always resize; skip dispatch when `passthrough`; set `owns_pointer` via
  `layer_owns_pointer`. Fix the module doc's "Pixels:" bullet.
- [ ] **Step 4:** Run `just test-one client "--lib slint_"`. Expected: PASS. Then `just smoke`.
- [ ] **Step 5: Live check**
  - Run `just adapter=Intel asset-viewer --model creatures/dog --shot target/ui-shot/composite.png`,
    then open the PNG. Expected: the left model list and right clip panel are drawn over the 3D
    view with no dark fringes. Before Step 3, take the same shot on the old code as
    `before.png` and compare them side by side.
  - Run `just adapter=Intel run`. Check that the tool bar, Esc menu, M map and F9 work, then
    resize the window and drag it to the other monitor if there is one. Expected: no stretching
    or offset.
- [ ] **Step 6: Commit** `feat(client): composite Slint layers in a Core3d pass`

### Task 5: `HudUi` — crosshair and selection band

**Files:**
- Create: `crates/slint-ui/ui/hud.slint`; `crates/slint-ui/tests/hud.rs`
- Modify: `crates/slint-ui/ui/index.slint` (`export { HudUi, HudConfig } from "hud.slint";`),
  `crates/client/src/renderer/hud.rs`, `renderer/commander/selection.rs`, `renderer/mod.rs`
  (plugin wiring), `renderer/slint_overlay.rs` (a `HudLayerPlugin`)

**Interfaces:**
- Slint (`hud.slint`):
  ```slint
  export global HudConfig {
      out property <length> crosshair-arm: 16px;  out property <length> crosshair-width: 2px;
      out property <length> minimap-size: 220px;   out property <length> minimap-margin: 12px;
      out property <length> slot-size: 48px;
  }
  export component HudUi inherits Window {
      background: transparent;
      in property <bool> crosshair-visible;
      in property <bool> band-visible;
      in property <length> band-x; in property <length> band-y;
      in property <length> band-width; in property <length> band-height;
  }
  ```
  Crosshair: two white `Rectangle`s centred, arm × width and width × arm.
  Band: fill `rgba(102, 255, 102, 0.12)`, 1px border `rgba(102, 255, 102, 0.9)` (= `BOX_FILL` /
  `BOX_BORDER`).
- Rust:
  - `pub struct HudLayerPlugin;` adds
    `SlintLayerPlugin::new(HudUi::new).order(HUD_LAYER_ORDER).passthrough()` and the push system
    `push_hud_state`, in **both** camera modes.
  - `#[derive(Resource, Default)] pub struct SelectionBand(pub Option<Rect>)` in
    `selection.rs`, in window logical px; written by `update_ground_pick` in place of the `Node`
    query.
  - `pub fn band_rect(press: Option<Vec2>, cursor: Option<Vec2>, held: bool) -> Option<Rect>`:
    `Some` only when held and the drag exceeds `DRAG_THRESHOLD_PX`.
  - `pub fn crosshair_visible(grab: CursorGrabMode, orbit: bool) -> bool`, taken out of
    `update_crosshair_visibility`.
- Delete: `Crosshair`, `setup_hud`, `update_crosshair_visibility`, `setup_selection_box`,
  `SelectionBox`. `HudStatus` stays in `hud.rs`.

- [ ] **Step 1: Write the failing tests**
  - `client` `selection.rs`: `band_appears_only_past_the_drag_threshold`. Covers
    `band_rect(Some((10,10)), Some((12,12)), true) == None`;
    `Some((10,10)), Some((40,30)), true` → `Rect::from_corners((10,10),(40,30))`;
    the same with `held=false` → `None`.
  - `client` `hud.rs`: `crosshair_shows_only_when_locked_and_not_orbiting`. Covers
    `(Locked,false)`→true, `(Locked,true)`→false, `(None,false)`→false.
  - `slint-ui` `tests/hud.rs` (slow-tagged, 800×600 `Overlay<HudUi>`):
    - `crosshair_drawn_at_centre_when_visible`: `opaque_at(400,300)` true when visible,
      false when not.
    - `band_drawn_inside_its_rect`: with band 100,100 200×50, `opaque_at(100,100)` (border)
      true, `opaque_at(99,99)` false.
    - `nothing_drawn_by_default`: no pixel opaque at (400,300), (5,595) or (100,100).
- [ ] **Step 2:** Run the three targets: `just test-one client "--lib selection::"`,
  `just test-one client "--lib hud::"`, `just test-one slint-ui "--test hud -- --include-ignored"`.
  Expected: FAIL.
- [ ] **Step 3: Implement** the component, the plugin, `push_hud_state` (sets properties only
  on change) and the two pure functions, and delete the `bevy_ui` spawns. `push_hud_state`
  converts `SelectionBand` to UI px by dividing by the layer's `ui_scale`, the same conversion
  `slint_input` uses. Add a `debug_assert_eq!` that `HudConfig` `minimap-size`/`margin` equal
  `MINIMAP_SIZE_PX`/`MINIMAP_MARGIN_PX`.
- [ ] **Step 4:** Rerun the three targets. Expected: PASS. Then `just smoke`.
- [ ] **Step 5: Live check**
  - `just adapter=Intel run`: box-select draws the green band; a drag that starts on the band
    edge keeps selecting.
  - `VOXEL_CAMERA=first-person just adapter=Intel client`: the crosshair shows while the cursor
    is locked.
- [ ] **Step 6: Commit** `feat(client): crosshair and selection band in Slint`

### Task 6: Minimap in `HudUi`

**Files:**
- Modify: `crates/slint-ui/ui/hud.slint`, `crates/slint-ui/tests/hud.rs`,
  `crates/client/src/renderer/commander/minimap.rs`, `renderer/slint_overlay.rs`
  (`push_hud_state`)

**Interfaces:**
- Slint: `in property <bool> minimap-visible; in property <image> minimap-base; in property <image> minimap-overlay;`.
  It is drawn bottom-left at `minimap-margin`, size `minimap-size`, with background
  `rgb(5, 13, 26)` (= srgb 0.02/0.05/0.1) and a 2px `rgba(0,0,0,0.8)` outline drawn *outside*
  the box (matching `Outline::new(2px, 0, …)`). Both images stretch to the box with
  `image-rendering: pixelated`.
- Rust: `MinimapImages { base: Handle<Image>, overlay: Handle<Image> }` becomes
  `#[derive(Resource)] pub struct MinimapPixels { pub base: Vec<[u8; 4]>, pub overlay: Vec<[u8; 4]>, pub base_dirty: bool, pub overlay_dirty: bool }`
  (edges `BASE_PX`, `OVERLAY_PX`). `rebuild_minimap_base` / `draw_minimap_overlay` write these
  buffers and set the dirty flags. `push_hud_state` uploads each dirty buffer with
  `SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice` + `SlintImage::from_rgba8`, as the world
  map does at `slint_overlay.rs:~762`, then clears the flag. `minimap-visible` is true in
  commander mode only.
- Unchanged: `minimap_pointer`, `screen_rect`, `MinimapPointer`. The HUD layer is
  pass-through, so Bevy-side pointer handling over the minimap works as before.

- [ ] **Step 1: Write the failing tests**
  - `slint-ui` `tests/hud.rs`: `minimap_drawn_bottom_left_when_visible`. On 800×600:
    `opaque_at(12+110, 600-12-110)` true when visible, false when not.
  - `client` `minimap.rs`: adapt the existing tests to read `MinimapPixels` instead of
    `Assets<Image>`, plus `drawing_marks_the_overlay_dirty` (after `draw_minimap_overlay`
    runs, `overlay_dirty` is true).
- [ ] **Step 2:** Run `just test-one client "--lib minimap::"` and
  `just test-one slint-ui "--test hud -- --include-ignored"`. Expected: FAIL.
- [ ] **Step 3: Implement**, then delete `setup_minimap`'s spawn (keep the resource init) and
  the `blank` helper.
- [ ] **Step 4:** Rerun both targets. Expected: PASS. Then `just smoke`.
- [ ] **Step 5: Live check:** `just adapter=Intel run`. The minimap shows terrain, entity dots
  and the camera footprint; a click or drag on it moves the camera; the M map and Esc menu
  draw above it.
- [ ] **Step 6: Commit** `feat(client): minimap drawn by Slint`

### Task 7: Hotbar as a typed projection in `HudUi`; delete `ui_bridge` / `ui_input`

**Files:**
- Modify: `crates/game/src/ui.rs`, `crates/game/src/lib.rs` (re-exports / `UiChannel` stays),
  `crates/game/tests/ui_roundtrip.rs`, `crates/client/src/instance.rs:~970-1005` + its tests at
  `~2798-2868`, `crates/client/src/bevy_harness.rs:22,96`, `renderer/bridge.rs:536,1121,2069`,
  `renderer/mod.rs:44-45,115-120,157`, `crates/slint-ui/ui/hud.slint`, `crates/slint-ui/tests/hud.rs`
- Delete: `crates/client/src/renderer/ui_bridge.rs`, `crates/client/src/renderer/ui_input.rs`

**Interfaces:**
- `game::ui` keeps `UiAction` (the sim event; `region.rs` handles it) and `IconId::of`.
  It deletes `WidgetId`, `Val`, `FlexDirection`, `JustifyContent`, `AlignItems`, `Color`,
  `Rect`, `Style`, `Content`, `Widget`, `WidgetTree`, `build_player_ui` and `diff_ui`. It adds:
  ```rust
  #[derive(Debug, Clone, PartialEq, Default)]
  pub struct HotbarView { pub slots: Vec<Option<SlotView>> }
  #[derive(Debug, Clone, Copy, PartialEq)]
  pub struct SlotView { pub icon: IconId, pub count: u32 }
  pub fn build_hotbar(data: &crate::GameData, client: crate::ClientId) -> HotbarView; // unknown client → default
  pub enum UiUpdate { Hotbar(HotbarView), SetCursorGrab(bool) }
  ```
  A slot is `Some` only when its stack's `IconId::of(kind)` is `Some`, same as today.
- The client instance keeps `prev_hotbar: HotbarView` and sends `UiUpdate::Hotbar` only when
  the projection changed. `SetCursorGrab` logic is unchanged.
- The client drains `UiUpdates` in a new `drain_ui_updates` in `slint_overlay.rs`. `Hotbar` is
  stored in `#[derive(Resource, Default)] pub struct Hotbar(pub HotbarView)`; `SetCursorGrab`
  sets `CursorPolicy::sim_grab`, exactly as `ui_bridge` did.
- Slint: `export struct HotbarSlot { filled: bool, count: string }`,
  `in property <[HotbarSlot]> hotbar;`. Drawn bottom-centre with these values from the old
  `game::ui` styles: `HorizontalLayout` spacing 4px, padding 6px, background
  `rgba(0,0,0,0.4)`; each slot `slot-size` square with background `rgba(25,25,25,0.6)`; a
  filled slot is tinted `rgb(102,77,51)` (the old icon stand-in); the count text sits
  bottom-right. It shows in both camera modes, as today.

- [ ] **Step 1: Write the failing tests**
  - `game` `tests/ui_roundtrip.rs`: rewrite around `build_hotbar`, as
    `move_item_swaps_hotbar_slots`. Before the `UiAction::MoveItem { from: 0, to: 1 }` event,
    `slots[0]` is `Some(SlotView{..})` and `slots[1]` is whatever it was; after, they are
    swapped. Also `unknown_client_has_empty_hotbar`.
  - `client` `instance.rs`: adapt the two channel tests. The first now asserts
    `matches!(u, game::UiUpdate::Hotbar(_))`; the cursor-grab test is unchanged apart from the
    type. Add `unchanged_hotbar_is_not_resent`: two `project_ui` calls with no state change
    send one `Hotbar`.
  - `slint-ui` `tests/hud.rs`: `hotbar_slots_drawn_bottom_centre`. Give it three slots, the
    middle filled; `opaque_at(400, 600-6-24)` is true.
- [ ] **Step 2:** Run `just test-one game "--test ui_roundtrip"`,
  `just test-one client "--lib instance::"` and
  `just test-one slint-ui "--test hud -- --include-ignored"`. Expected: FAIL.
- [ ] **Step 3: Implement.** Delete `ui_bridge.rs` and `ui_input.rs` with their wiring
  (`on_widget_click` observer, `UiWidgetMap`, `UiRoot`). `UiUpdates` moves into
  `slint_overlay.rs` with the same shape, `pub struct UiUpdates(pub Option<Receiver<UiUpdate>>)`.
  Update its users in `bridge.rs` and `bevy_harness.rs`.
- [ ] **Step 4:** Rerun the three targets. Expected: PASS. Then run `just smoke` and
  `just determinism-wasm`. `game` changed, though nothing hashed did.
- [ ] **Step 5: Commit** `refactor: hotbar is a typed projection drawn by Slint`

### Task 8: F3 dev panel in Slint (`DevPanelUi`); delete egui

**Files:**
- Create: `crates/slint-ui/ui/dev-panel.slint`, `crates/slint-ui/tests/dev_panel.rs`
- Modify: `crates/slint-ui/ui/index.slint` (`export { DevPanelUi } from "dev-panel.slint";`),
  `crates/client/src/renderer/dev_ui.rs` (rewrite), `crates/client/Cargo.toml` (your hunk only:
  `dev_ui = []`, delete the `bevy_egui` dependency line), `crates/client/src/main.rs:300`
  (unchanged call, new plugin body)

**Interfaces:**
- Slint (`dev-panel.slint`, imports `Button, Checkbox, FrameFace, LabelFace, Styles` from
  `@widgets`):
  ```slint
  export struct DevStat { label: string, value: string }
  export struct DevToggle { label: string, checked: bool }
  export component DevPanelUi inherits Window {
      background: transparent;
      in-out property <bool> open;
      in property <[DevStat]> stats;
      in property <[DevToggle]> toggles;
      callback toggle(int, bool);   // index into `toggles`, new state
      callback complete-orders();
  }
  ```
  When `open`, it draws one `FrameFace` anchored 8 px from the top-right holding a two-column
  stats grid, a line, one `Checkbox` per toggle (`toggled(v) => root.toggle(i, v)`), a line and
  a `Button { text: "Complete all orders"; }`. Nothing is drawn when closed.
- Rust (`dev_ui.rs`, still `#[cfg(feature = "dev_ui")]`):
  - `pub const DEV_UI_KEY: KeyCode = KeyCode::F3;` (kept), `#[derive(Resource, Default)] pub struct DevUiOpen(pub bool)` (kept)
  - `pub const DEV_LAYER_ORDER: i32 = 2_000;` in `slint_overlay.rs`, beside the other two
  - `pub struct DevUiPlugin;` adds `SlintLayerPlugin::new(DevPanelUi::new).order(DEV_LAYER_ORDER).with_init(connect_dev_panel)`
    in **both** camera modes, plus `toggle_dev_ui` (unchanged behaviour: F3 flips `DevUiOpen`
    and mirrors it into `CursorPolicy::overlay_open`), `push_dev_panel` (sets `open`, `stats`
    and `toggles` only when changed; stats at most every `DEV_STATS_INTERVAL_S: f32 = 0.25`) and
    `apply_dev_actions`.
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum DevToggle { Boundaries, ChunkGrid, CaveCulling, HideLod, HideNear, Colliders }`
    with `pub const ALL: [DevToggle; 6]` in that order (the egui order) and
    `pub fn label(self) -> &'static str` returning exactly `"Region boundaries (F8)"`,
    `"Chunk grid (F5)"`, `"Cave culling (F4)"`, `"Hide LOD (F6)"`, `"Hide near terrain (F7)"`,
    `"Collider wireframes"`.
  - `pub fn dev_stats(fps: Option<f64>, frame_ms: Option<f64>, pos: Option<Vec3>, status: &HudStatus, terrain: &TerrainRenderConfig, entities: usize) -> Vec<(String, String)>`.
    The rows are FPS, Frame, Position, Home region, Viewer region, Connection, Terrain,
    Entities, with exactly the egui formats (`{f:.0}`, `{ms:.2} ms`, `{:.1} / {:.1} / {:.1}`,
    `({x}, {z})`, `{:?}`, `near {:?}, far {:?}`); `None` → `"--"`.
  - `#[derive(Debug, Clone, Copy, PartialEq)] pub enum DevAction { Toggle(DevToggle, bool), CompleteOrders }`,
    queued like `GameUiActions` (an `Rc<RefCell<Vec<_>>>` non-send resource).
    `apply_dev_actions` writes `BoundaryOverlay::enabled`, `GridDebug::enabled`,
    `CullingConfig::graph_enabled`, `TerrainDebug::hide_lod`/`hide_near` and
    `DebugFlagsRes.0.set_collider_wireframes(v)`. `CompleteOrders` sends
    `GameEventKind::Order(client, Order::DevCompleteAll)` when `LocalPlayer` is known.
  - The camera position comes from `Query<&GlobalTransform, With<UiCamera>>`. Delete
    `follow_ui_camera` and every `bevy_egui` import.
- The panel is interactive (not pass-through), so it owns the pointer where it draws, as
  `GameUi` does.

- [ ] **Step 1: Write the failing tests**
  - `client` `dev_ui.rs`:
    - `stats_rows_match_the_old_panel`: `dev_stats(Some(59.6), Some(16.6667), Some(Vec3::new(1.0, 2.25, -3.0)), &HudStatus::default(), &cfg, 42)`
      gives `("FPS","60")`, `("Frame","16.67 ms")`, `("Position","1.0 / 2.2 / -3.0")` (Rust
      rounds 2.25 → "2.2"; assert whatever `format!("{:.1}", 2.25f32)` gives),
      `("Home region","--")`, `("Entities","42")`; all `None` inputs give `"--"`.
    - `toggles_keep_the_old_order_and_labels`: `DevToggle::ALL.map(DevToggle::label)` equals
      the six strings above.
    - `toggle_actions_write_their_resource`: with a `World` holding the five resources plus
      `DebugFlagsRes`, applying `DevAction::Toggle(t, true)` for each `t` sets exactly that flag.
    - `complete_orders_needs_a_local_player`: with `LocalPlayer(None)` nothing is sent; with
      `Some(7)`, `GameEventKind::Order(7, Order::DevCompleteAll)` is.
    - `dev_layer_is_on_top`: `DEV_LAYER_ORDER > GAME_LAYER_ORDER && GAME_LAYER_ORDER > HUD_LAYER_ORDER`.
  - `slint-ui` `tests/dev_panel.rs` (slow-tagged, 1280×720):
    - `closed_panel_draws_nothing`: `opaque_at(1270, 10)` false.
    - `open_panel_draws_top_right`: `opaque_at(1270, 10)` true.
    - `clicking_a_checkbox_fires_toggle`: set two toggles; click the second checkbox's
      position (read it from the rendered layout the way `tests/world_map.rs` finds the Play
      button) → `toggle(1, true)` recorded.
    - `clicking_the_button_fires_complete_orders`.
- [ ] **Step 2:** Run `just test-one client "--lib dev_ui::"` and
  `just test-one slint-ui "--test dev_panel -- --include-ignored"`. Expected: FAIL.
- [ ] **Step 3: Implement.** Rewrite `dev_ui.rs` and drop `bevy_egui` from the client
  manifest. Check that `cargo tree -i bevy_egui --target x86_64-unknown-linux-gnu` → no match
  and `cargo check -p client --no-default-features` → OK.
- [ ] **Step 4:** Rerun both targets. Expected: PASS. Then `just smoke`.
- [ ] **Step 5: Live check (adapter=Intel)**
  - `just run`: F3 opens the panel top-right over the tool bar and menu; stats update; each
    checkbox flips its overlay (the F4–F8 keys flip the checkbox too, since state is pushed
    from the resources); "Complete all orders" clears painted designations; the cursor is free
    while it is open.
  - `VOXEL_CAMERA=first-person just client`: F3 works there too.
- [ ] **Step 6: Commit** `feat(client): F3 dev panel in Slint, drop egui`

### Task 9: Drop Bevy's UI features; docs

**Files:**
- Modify: root `Cargo.toml` (bevy features), `renderer/bridge.rs` / `renderer/commander/mod.rs`
  (stop inserting `IsDefaultUiCamera`), every `Pickable::IGNORE` left, `renderer/mod.rs`
  (delete test `bevy_ui_features_enabled`), `CLAUDE.md`,
  `docs/superpowers/specs/2026-10-29-compile-time-and-dependencies-investigation.md`
  (mark §4/§5 done), memory `ui-slint-for-player-ui.md`

- [ ] **Step 1:** Remove `"bevy_ui"`, `"bevy_ui_render"`, `"bevy_text"`, `"bevy_picking"` (if
  listed), `"ui_picking"` and `"default_font"` from the workspace `bevy` features. Delete every
  remaining `IsDefaultUiCamera` and `Pickable` use.
  `grep -rn "IsDefaultUiCamera\|Pickable\|bevy::ui\|ImageNode\|TextFont\|BackgroundColor" crates/client`
  → no hits.
- [ ] **Step 2: Verify the graph**
  - `cargo tree -i bevy_text --target x86_64-unknown-linux-gnu` → no match
  - the same for `bevy_ui`, `bevy_sprite`, `parley@0.9.0`, `taffy`, `bevy_picking`, `egui`,
    `epaint` → no match. If `bevy_picking` is still there, find what pulls it with
    `cargo tree -i bevy_picking -e features` and drop that feature too.
  - record `cargo tree -p client -e normal --prefix none --target x86_64-unknown-linux-gnu | sort -u | wc -l`
    (was 701) in the commit message
- [ ] **Step 3: Verify the build**
  - `just smoke` → passes
  - `just test-one client "--lib bridge::"` and `"--lib slint_"` → pass
  - wasm type-check: `nice -n 10 taskset -c 0-15 cargo check -p client --target wasm32-unknown-unknown`
    → no errors
- [ ] **Step 4: Live check (adapter=Intel)**
  - `just run`: tool bar, Esc menu, M map + Play, F9, minimap, box-select, hotbar
  - `just asset-viewer --model creatures/dog --shot target/ui-shot/final.png`: compare with
    Task 4's `before.png`
  - `VOXEL_CAMERA=first-person just client`: crosshair and hotbar
  - the F3 Slint panel opens in both camera modes
- [ ] **Step 5: Docs**
  - CLAUDE.md `crates/client` paragraph: the sim-state hotbar is the `game::ui::HotbarView`
    projection drawn by `HudUi`; client-local HUD (crosshair, selection band, minimap) is
    `HudUi`, a pass-through Slint layer; layers reach the screen through
    `renderer/slint_composite.rs` on the `UiCamera`; delete the `bevy_ui` sentences.
  - CLAUDE.md `crates/slint-ui` paragraph: replace "window-sized `ImageNode` (`GlobalZIndex`
    1000)" with the composite pass and `GAME_LAYER_ORDER`/`HUD_LAYER_ORDER`; mention
    `HudUi`/`HudConfig`.
  - CLAUDE.md: replace the `renderer/dev_ui.rs` "egui F3 developer panel" sentence with the
    Slint `DevPanelUi` layer (`dev_ui` feature, no dependency; `--no-default-features` drops the
    panel).
  - Update the memory note `ui-slint-for-player-ui.md`: no exception for the dev panel any more,
    egui is gone. Update the investigation doc's status lines.
- [ ] **Step 6: Commit** `chore(client): drop bevy_ui/bevy_text/egui — Slint is the only UI`
