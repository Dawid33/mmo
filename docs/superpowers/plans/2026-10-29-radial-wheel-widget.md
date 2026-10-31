# Radial Wheel Widget (baked Factorio wedges) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `RadialMenu` widget to `~/Projects/ui`'s `widgets` library. Its
wedges are Factorio's own `button` artwork (every state), remapped from the
nine-slice onto annular sectors at build time.

**Architecture:** The widgets build script (`crates/widgets/codegen/`) already
cuts Factorio sprites into PNGs and emits Slint data. A new pure module,
`radial.rs`, holds the wedge geometry and a polar nine-slice rasteriser.
`wedges.rs` feeds it the parsed `button` graphical sets and writes one PNG
(plus its `@half` copy) per (wedge count, wedge index, state). `emit.rs` then
writes those PNGs as a `RadialWheels` global. The hand-written
`ui/radial-menu.slint` draws the pre-positioned images unrotated, because the
software renderer has no transforms, and picks the hovered wedge by angle.

**Tech Stack:** Rust build script (`png` crate, already a dependency); Slint 1.18.1, software renderer only.

**Spec:** `docs/superpowers/specs/2026-10-29-radial-wheel-widget-investigation.md`
(option C; the renderer limits table explains why baking is needed).

## Global Constraints

- All work is in **`~/Projects/ui`** (branch `main`). The mmo repo only holds this plan and the spec.
- That tree has ~2,500 modified files under `crates/widgets/ui/generated/` from the user's own regeneration. **Never `git add -A` or `git add crates/widgets/ui/generated`**: stage only the paths each task names. New wedge PNGs go under `crates/widgets/ui/generated/images/radial/` and are staged by that directory alone.
- Slint renderer: software renderer only. No `rotation-angle`, no transforms, no radius clipping, no gradients on `Path`.
- The wedge art comes from the generated `button` style's 7 states, in `CLICKABLE_STATES` order: `default, hovered, clicked, disabled, selected, selected-hovered, selected-clicked`. No hand-picked colours.
- Geometry constants live in `codegen/radial.rs`, in GUI px at 100% scale: `RADIAL_INNER = 40.0`, `RADIAL_OUTER = 120.0`, `RADIAL_GAP = 4.0` (constant-width gap between neighbouring wedges), `RADIAL_COUNTS = 2..=8`, `START = -90°` (item 0 centred straight up, then clockwise on screen, y down), `OUT_SCALE = 2` (output px per GUI px: the full image is 2× like Factorio's sheets, and `@half` is 1:1 at 100%), `SUPERSAMPLE = 4` (4×4 samples per output px near edges).
- Mapping from the nine-slice onto a wedge: **top row → outer arc**, **bottom row → inner arc**, **left column → the start ray** (lower angle), **right column → the end ray**. So the bevel highlight faces outward all round the ring. This is intended.
- The codegen stays strict, following `docs/handoff.md`: any layer form the rasteriser doesn't handle (`centred`, `overall_tiling`, `monolith_size`) **panics**, naming the style and state. It is never silently skipped.
- Measure Rust tests with `cargo test -p <crate>` from `~/Projects/ui`, prefixed with `nice -n 10 taskset -c 0-15` (the mmo box's 80% core cap).

## Review Focus

1. **Pointer on the gap between two wedges.** It must pick the angularly nearest wedge and must not drop to -1, so the highlight never flickers while sweeping. Pinned in Task 6.
2. **Pointer outside the outer radius.** It still picks by angle, the normal radial-menu feel. Only the centre dead zone gives -1. Pinned in Task 6.
3. **Item count outside 2..=8** (0, 1, 9+). The widget draws nothing, `hovered` is -1 and clicks do nothing; it must not crash or index out of bounds. Pinned in Task 6.
4. **UI scale 1× vs 2× vs 1.5×.** The wheel centre must land on a whole GUI pixel and each wedge's offset must be an integer, so neighbouring wedges never get half-pixel seams. 1× must draw the `@half` images texel for pixel. Golden snapshots at 1× and 2× in Task 6, and the integral offsets asserted in Task 1.
5. **Disabled item under the pointer.** It is drawn in the `disabled` state, `hovered` is -1 and no `activated` fires. Pinned in Task 6.

---

### Task 1: Wedge geometry (pure)

**Files:**
- Create: `crates/widgets/codegen/radial.rs` (no `super::` imports: the test file includes it by path)
- Modify: `crates/widgets/codegen/mod.rs` (add `mod radial;`)
- Test: `crates/widgets/tests/radial_geometry.rs` (starts with `#[path = "../codegen/radial.rs"] mod radial;`)

**Interfaces:**
- Produces, all in **output px** (GUI px × `OUT_SCALE`), angles in radians, y down:
  - `pub struct Wedge { pub inner: f32, pub outer: f32, pub start: f32, pub span: f32, pub gap: f32 }`
  - `pub fn wedge(count: u32, index: u32) -> Wedge`: wedge `index` of `count`, centred on `START + index·2π/count`, `span = 2π/count`, radii and gap from the constants × `OUT_SCALE`.
  - `pub fn ray_distance(r: f32, delta: f32) -> f32`: signed distance from a point at radius `r`, `delta` radians past a ray, to that ray (a half-line from the origin). `r·sin(delta)` for `delta ∈ [-π/2, π/2]`, otherwise `r` (the nearest point is the origin). Positive is inside. This is what makes `count = 2` work: both edges lie on one diameter, but each is only a half-line.
  - `pub struct Edges { pub left: f32, pub right: f32, pub top: f32, pub bottom: f32 }`: signed inward distances to the start ray, the end ray, the outer arc and the inner arc.
  - `pub fn edges(w: &Wedge, x: f32, y: f32) -> Edges`: `left = ray_distance(r, wrap(θ − start)) − gap/2`, `right = ray_distance(r, wrap(start + span − θ)) − gap/2`, `top = outer − r`, `bottom = r − inner`; `wrap` into (−π, π].
  - `pub struct Bounds { pub x: i32, pub y: i32, pub width: u32, pub height: u32 }`, relative to the wheel centre.
  - `pub fn bounds(w: &Wedge, margin: f32) -> Bounds`: a conservative box around the wedge grown by `margin` on every side. It is built from the outer and inner arc end points, the cardinal angles inside the span, and the origin when `span ≥ π`. Every field is rounded **outward to even numbers**, so the `@half` copy and the GUI-px offset (`x / OUT_SCALE`) are whole.

- [ ] **Step 1: Write the failing tests** in `tests/radial_geometry.rs`:
  - `wedge_zero_is_centred_up`: `wedge(4, 0)` has `start + span/2 ≈ -π/2` and `span ≈ π/2`; `inner == 80.0`, `outer == 240.0`, `gap == 8.0`.
  - `edges_inside_and_outside`: for `w = wedge(4, 0)`, the point `(0, -160)` (straight up, mid radius) has all four `edges` fields `> 0`. `(0, -300)` has `top < 0`, and `(0, -50)` has `bottom < 0`.
  - `gap_is_constant_width`: for `w = wedge(6, 1)`, at radius 100 and at radius 230, the point exactly on the start ray has `left == -4.0` (±1e-3), so the gap does not widen with radius.
  - `half_disc_edges_are_half_lines`: for `w = wedge(2, 0)` (the upper half), the point `(-200, -1)` is near the left end of the diameter. It has `left` small (< 2) and `right ≥ 150`: it is far from the end ray, which runs the other way.
  - `bounds_are_even_and_cover`: for every `count in 2..=8` and `index < count`, `bounds(&wedge(count, index), 12.0)` has even `x, y, width, height`, and contains every sampled point with all `edges` fields ≥ −12 (sample on a 2-px grid over a 600×600 square).
- [ ] **Step 2: Run them; expect FAIL** (module missing): `cd ~/Projects/ui && nice -n 10 taskset -c 0-15 cargo test -p widgets --test radial_geometry`
- [ ] **Step 3: Implement** the interface above in `codegen/radial.rs`.
- [ ] **Step 4: Run again; expect PASS (5 tests).**
- [ ] **Step 5: Commit**: `git add crates/widgets/codegen/radial.rs crates/widgets/codegen/mod.rs crates/widgets/tests/radial_geometry.rs && git commit -m "Radial wheel: wedge geometry"`

### Task 2: Polar nine-slice rasteriser (pure)

**Files:**
- Modify: `crates/widgets/codegen/radial.rs`
- Test: `crates/widgets/tests/radial_geometry.rs` (same file, new tests)

**Interfaces:**
- Consumes: `Wedge`, `edges`, `Bounds` from Task 1.
- Produces:
  - `pub struct PieceImage { pub width: u32, pub height: u32, pub rgba: Vec<u8>, pub tile: (bool, bool) }`: straight-alpha RGBA, already tinted.
  - `pub struct NineLayer { pub pieces: [Option<PieceImage>; 9], pub border: [f32; 4], pub texel: f32, pub outer: bool, pub grow: [f32; 4] }`. `pieces` uses the `graphics::PIECES` order. `border` is `[left, right, top, bottom]` in output px. `texel` is output px per sheet texel (`layer.scale × OUT_SCALE`). `grow` is `[left, right, top, bottom]` output px each side is pushed outward, from `*_outer_border_shift`.
  - `pub fn raster(layers: &[NineLayer], w: &Wedge, b: &Bounds) -> Vec<u8>`: straight RGBA, `b.width × b.height`, the layers composited premultiplied-over **in slice order**. The caller orders them the way `GraphicalSetView` does.

Rasteriser rules (these decide behaviour, so they're spelled out here):

- For each sample point, `e = edges(w, x, y)`. For an `outer` layer, add `border[side] + grow[side]` to each side. For an inner layer, add only `grow`. The point is in the layer when all four are ≥ 0. An inner layer's points must also satisfy the unexpanded `edges ≥ 0`: this matches `clip: !layer.outer` in `graphical-set.slint`.
- Column: **left** if `e.left < border[0]`, sampled at texel `x = e.left / texel`. Else **right** if `e.right < border[1]`, at `x = piece_w − e.right / texel`. Else **centre**: tiled (`tile.0`) at `x = ((e.left − border[0]) / texel) mod piece_w`, otherwise stretched at `x = piece_w · (e.left − border[0]) / (e.left + e.right − border[0] − border[1])`. Rows follow the same rule with top/bottom.
- Sample each piece bilinearly in premultiplied colour, clamped to the piece's own rect. This is the same maths as `bake.rs::resample`; move the shared texel fetch into `radial.rs`, and have `bake.rs` keep its own until a later cleanup.
- Anti-aliasing: an output pixel whose centre is more than 1.5 output px from every edge, on every layer, takes one sample. Otherwise it takes `SUPERSAMPLE²` and averages them premultiplied.

- [ ] **Step 1: Write the failing tests.** Use synthetic layers whose nine pieces are each 4×4 flat colours: corners red, top green, bottom blue, left yellow, right magenta, centre grey. `border = [8.0; 4]`, `texel = 2.0`, not outer. Use `w = wedge(4, 0)` and `b = bounds(&w, 0.0)`. Helper `px(x, y)` reads the pixel at wheel coordinates.
  - `outer_arc_uses_top_row`: the pixel at `(0, -236)` (4 px inside the outer arc, mid angle) is green.
  - `inner_arc_uses_bottom_row`: `(0, -84)` is blue.
  - `middle_uses_centre`: `(0, -160)` is grey.
  - `start_ray_uses_left_column`: the point at radius 160, angle `start + 0.05` rad (ray distance ≈ 8, so `edges.left` ≈ 4 < border 8), is yellow.
  - `outside_is_transparent`: `(0, -260)` and the point in the gap on the start ray at radius 160 both have alpha 0.
  - `outer_layer_extends_past_the_arc`: the same layer with `outer: true`, rastered over `bounds(&w, 8.0)`, puts top-row colour at `(0, -244)` (4 px *outside* the arc), alpha 255.
  - `edges_are_antialiased`: some pixel along the outer arc has alpha strictly between 0 and 255.
- [ ] **Step 2: Run; expect FAIL** (`raster` missing): `nice -n 10 taskset -c 0-15 cargo test -p widgets --test radial_geometry`
- [ ] **Step 3: Implement `raster`** following the rules above.
- [ ] **Step 4: Run; expect PASS (12 tests).**
- [ ] **Step 5: Commit**: `git add crates/widgets/codegen/radial.rs crates/widgets/tests/radial_geometry.rs && git commit -m "Radial wheel: polar nine-slice rasteriser"`

### Task 3: Bake the `button` wedges and emit `RadialWheels`

**Files:**
- Create: `crates/widgets/codegen/wedges.rs`
- Modify: `crates/widgets/codegen/bake.rs` (expose sheet pixels and PNG writing), `graphics.rs` (`#[derive(PartialEq)]` on `Layer`, `GraphicalSet`), `elements.rs` (make `CLICKABLE_STATES` `pub`), `mod.rs` (call the baker; pass its result to `emit`), `emit.rs` (write the global), `crates/widgets/ui/types.slint` (new structs), and the root `Cargo.toml` (only if Step 4 measures it necessary)

**Interfaces:**
- Consumes: `radial::{wedge, bounds, raster, NineLayer, PieceImage, RADIAL_COUNTS, OUT_SCALE}`; `GraphicalSet::parse`; `Style::named(gui, "button")`; `elements::CLICKABLE_STATES`.
- Produces:
  - In `bake.rs`: `pub fn piece_pixels(&mut self, piece: &Piece) -> PieceImage` (cut and tint as `bake` does, no file written) and `pub fn write_rgba(&mut self, name: &str, width: u32, height: u32, rgba: &[u8]) -> (String, Option<String>)` (writes `{name}.png` and the `halve`d `{name}@half.png`, returning both paths relative to `generated/`).
  - In `wedges.rs`: `pub fn bake(gui: &GuiStyle, baker: &mut Baker) -> Lit`, the `[RadialWheel]` literal, one entry per count in `RADIAL_COUNTS` order.
  - In `types.slint`:
    ```slint
    // One wedge of a radial wheel: where its images sit, from the wheel's
    // centre, and an image per button state (CLICKABLE_STATES order).
    export struct RadialWedge { x: length, y: length, states: [SpriteImage] }
    export struct RadialWheel {
        count: int,
        // The element's side: the wheel plus its outer shadows.
        size: length,
        inner: length,
        outer: length,
        wedges: [RadialWedge],
    }
    ```
  - In `generated/styles.slint`: `export global RadialWheels { out property <[RadialWheel]> button: [...]; }`, with `count` = 2..=8 at index `count − 2`.

Baking rules:
- For each state, parse the state's graphical set the way `elements::button` does (falling back to `default_graphical_set`). Turn each present layer into a `NineLayer` via `piece_pixels`. Slice order is outer shadow, outer base, inner shadow, inner base, glow, matching `GraphicalSetView`.
- A state whose `GraphicalSet` equals an earlier state's reuses that state's images (no duplicate PNGs).
- One `Bounds` per wedge shared by all its states: the union of `bounds(w, margin)` with `margin` = the largest outer border + grow over all states' layers. `RadialWedge.x/y` = `bounds.x / OUT_SCALE` px (whole GUI px, guaranteed by Task 1). `RadialWheel.size` = `2 × (RADIAL_OUTER + margin / OUT_SCALE)` rounded up to even.
- File names: `images/radial/button-{count}-{index}-{state}.png` (+`@half`).
- Unsupported layer forms panic: `"radial wheel: button {state} {shadow|base|glow} layer is {centred|tiled|monolith}; the rasteriser only maps nine-slices"`.

- [ ] **Step 1: Write the bake and emit code.** A build-script module has no unit tests of its own; Step 3 is its check, Tasks 1–2 pin the maths, and Task 6 pins the pixels.
- [ ] **Step 2: Build.** `cd ~/Projects/ui && nice -n 10 taskset -c 0-15 cargo build -p widgets`. Expect success. `generated/styles.slint` should contain `export global RadialWheels` with 7 wheels, and `ls crates/widgets/ui/generated/images/radial | wc -l` should be ≤ 490 (35 wedges × ≤ 7 states × 2).
- [ ] **Step 3: Eyeball one wedge.** Open `generated/images/radial/button-6-0-default.png` and `…-hovered.png` (Read tool or an image viewer). They should show Factorio's grey button bevel, with the light edge on the outer arc, bent into a 60° sector with smooth edges. The hovered one is orange-tinted like `Styles.button.hovered`. Also check `du -sh generated/images/radial` (expect a few MB; report the number).
- [ ] **Step 4: Measure build-script time.** `touch crates/widgets/build.rs && time cargo build -p widgets`. If the baking adds more than 5 s, add `[profile.dev.build-override]\nopt-level = 3` to `~/Projects/ui/Cargo.toml`. Report the measured times. Tell the user the same table is needed in `~/Projects/mmo/Cargo.toml`, which also runs this build script; don't edit mmo's.
- [ ] **Step 5: Commit**: `git add crates/widgets/codegen crates/widgets/ui/types.slint crates/widgets/ui/generated/styles.slint crates/widgets/ui/generated/images/radial Cargo.toml && git commit -m "Radial wheel: bake button wedges from Factorio's button art"`. `styles.slint` carries the user's unrelated regeneration too, so check `git diff --cached --stat` first and tell the user if it carries more than the `RadialWheels` and `RadialWedge`/`RadialWheel` changes.

### Task 4: `RadialMenuFace` and `RadialMenu` widgets

**Files:**
- Create: `crates/widgets/ui/radial-menu.slint`
- Modify: `crates/widgets/ui/index.slint` (export `RadialMenu, RadialMenuFace, RadialItem` and add `RadialWheels` to the generated export line)

**Interfaces:**
- Consumes: `RadialWheels.button`, `SpriteView` (`image.slint`), `Styles.button` (font and font colour per state), `ButtonStyle`.
- Produces:
  ```slint
  export struct RadialItem { text: string, icon: SpriteImage, enabled: bool }

  // Fixed state, for the showcase and snapshot tests.
  export component RadialMenuFace {
      in property <[RadialItem]> items;
      in property <int> hovered: -1;
      in property <int> pressed: -1;      // drawn `clicked`
      in property <int> toggled: -1;      // drawn `selected*`
      in property <ButtonStyle> style: Styles.button;  // text font + colours only
      // preferred size = the wheel's `size`; draws nothing unless 2 <= items.length <= 8
  }

  export component RadialMenu {
      in property <[RadialItem]> items;
      in property <int> toggled: -1;
      in property <length> dead-zone: 24px;
      out property <int> hovered;          // -1: dead zone, disabled item, or no wheel
      callback activated(int);
  }
  ```
- Drawing: `wheel = RadialWheels.button[items.length - 2]`. Each wedge is a `SpriteView { stretch: false; sprite: wedge.states[state]; }` at `x = width/2 + wedge.x`, `y = height/2 + wedge.y`. The element is `wheel.size` square, so `width/2` is whole. The state index uses `CLICKABLE_STATES` order and the same precedence as `Button`: disabled → pressed → hovered → toggled → default, with the `selected-*` variants when toggled. Each item's icon (`SpriteView`, natural size) and text (`style.font`, the state's `font-color`, `overflow: elide`, width = chord at mid radius) are centred at mid radius on the wedge's centre angle. Use `cos`/`sin` there; only positions, no transforms.
- Picking (in `RadialMenu`, from one full-size `TouchArea`): with `dx, dy` the pointer relative to the centre in px floats, `-1` if `sqrt(dx²+dy²) < dead-zone` or the count is outside 2..=8. Otherwise `i = floor((atan2(dy, dx) − START + step/2) / step)`, normalised with `mod(mod(i, n) + n, n)` (don't rely on the sign of Slint's `mod`), then `-1` if `!items[i].enabled`. `clicked => if hovered >= 0 { activated(hovered) }`. `pressed` is fed from `touch.pressed ? hovered : -1`.

- [ ] **Step 1: Write `radial-menu.slint`** and the exports.
- [ ] **Step 2: Compile check:** `nice -n 10 taskset -c 0-15 cargo build -p showcase`. Expect success; a component that isn't used yet still has to compile through the `@widgets` index.
- [ ] **Step 3: Commit**: `git add crates/widgets/ui/radial-menu.slint crates/widgets/ui/index.slint && git commit -m "Radial wheel: RadialMenuFace and RadialMenu"`

### Task 5: Showcase page

**Files:**
- Modify: `crates/showcase/ui/showcase.slint` (new page after the last one; bump the page count the navigation uses)

**Interfaces:**
- Consumes: `RadialMenuFace`, `RadialMenu`, `RadialItem`, `Sprites` (icons, e.g. `Sprites.close`, `Sprites.left-arrow`).

- [ ] **Step 1: Add the page** `"radial wheel — 2..8 items; every state on 6; interactive"`. Row 1: `RadialMenuFace` for counts 2, 3, 4, 5, 6, 7, 8 with text items and `hovered: 0`. Row 2: six faces with 6 items showing default, hovered 2, pressed 2, item 3 disabled, toggled 1, and toggled 1 + hovered 1. Row 3: one interactive `RadialMenu` with 6 icon + text items (item 4 disabled) and a `Caption` showing `hovered` and the last `activated` index.
- [ ] **Step 2: Run** `cargo run` (the showcase is the default member). Page to the new section and check by eye: wedges meet with an even gap, there are no half-pixel seams at 1×, the hover highlight follows the pointer around the whole ring including past the outer edge, the centre dead zone gives -1, and the disabled wedge never highlights. Also run `SLINT_SCALE_FACTOR=2 cargo run` and check the same at 2×. Screenshot both for the user.
- [ ] **Step 3: Commit**: `git add crates/showcase/ui/showcase.slint && git commit -m "Showcase: radial wheel page"`

### Task 6: Headless tests: picking and golden snapshots

**Files:**
- Create: `crates/widget-tests/Cargo.toml` (`publish = false`; deps `slint`, `widgets`, `png = "0.18.1"`; build-dep `slint-build`), `crates/widget-tests/build.rs` (compile `ui/radial-probe.slint` with the `@widgets` library path, exactly as `crates/reftest/build.rs` does), `crates/widget-tests/src/lib.rs` (`slint::include_modules!();` + a `Headless` platform and `render(scale) -> (w, h, Vec<u8>)` modelled on `reftest/src/render.rs`), `crates/widget-tests/ui/radial-probe.slint`, `crates/widget-tests/tests/radial_menu.rs`, `crates/widget-tests/golden/radial-{1x,2x}.png`
- (The workspace already globs `crates/*`; no root change.)

**Interfaces:**
- Consumes: `RadialMenu`, `RadialMenuFace`, `RadialItem`.
- Produces: `RadialProbe inherits Window` with `in property <[RadialItem]> items`, `in property <bool> face-only`, `out property <int> hovered`, `callback activated(int)`. It holds the wheel centred at window (300, 300), in a 600×600 window.

- [ ] **Step 1: Write the failing tests** in `tests/radial_menu.rs`. Each test sets the platform once via a `OnceLock`, dispatches `WindowEvent::PointerMoved` / `PointerPressed` / `PointerReleased` at window coordinates, and reads `hovered`. Use 6 enabled items unless stated; `at(deg, r)` gives the point at angle `deg` (0 = up, clockwise) and radius `r` from (300, 300).
  - `picks_by_angle`: `at(0, 80)` → 0, `at(60, 80)` → 1, `at(300, 80)` → 5.
  - `gap_picks_nearest_not_none`: `at(29.9, 80)` → 0, `at(30.1, 80)` → 1. Neither is -1.
  - `outside_ring_still_picks`: `at(120, 280)` → 2.
  - `dead_zone_is_none`: `at(45, 10)` → -1.
  - `disabled_item_is_none_and_not_activated`: item 2 disabled; `at(120, 80)` → -1. Pressing and releasing there fires no `activated`.
  - `click_activates_hovered`: a click at `at(240, 80)` fires `activated(4)` exactly once.
  - `unsupported_counts_draw_nothing`: for 0, 1 and 9 items, `hovered` is -1 at `at(0, 80)`, and the 1× render is fully transparent.
  - `snapshot_1x` and `snapshot_2x`: `face-only` with 6 items, item 3 disabled and hovered 1, rendered at scale 1 and 2, compared **byte-exact** with `golden/radial-{1x,2x}.png`. When `UPDATE_GOLDEN=1`, write the file instead and pass. A mismatch writes `target/widget-tests/radial-{scale}x-actual.png` and fails, naming it.
- [ ] **Step 2: Run; expect the snapshot tests to FAIL** (golden missing) and the rest to pass: `nice -n 10 taskset -c 0-15 cargo test -p widget-tests`
- [ ] **Step 3: Record the goldens** with `UPDATE_GOLDEN=1 cargo test -p widget-tests snapshot`, then **look at both PNGs**. Six wedges, the hovered one orange, the disabled one greyed, labels legible. Show them to the user before committing.
- [ ] **Step 4: Run again without the env var; expect PASS (9 tests).**
- [ ] **Step 5: Commit**: `git add crates/widget-tests Cargo.lock && git commit -m "Radial wheel: headless picking tests and golden snapshots"`

### Task 7: Library docs

**Files:**
- Modify: `~/Projects/ui/docs/handoff.md` (a short "Composite widgets with no Factorio original" section). `docs/handoff.md` is untracked in that repo; ask the user before committing it, and if they say no, leave the edit uncommitted.

- [ ] **Step 1: Write the section.** `RadialMenu` reworks `button` art into wedges in `codegen/radial.rs` + `wedges.rs`. There is no game capture, so reftest can't diff it: it is verified by `crates/widget-tests` goldens (`UPDATE_GOLDEN=1` to re-record after a deliberate art change) and the showcase page. Note the nine-slice → wedge mapping (top = outer arc), the constants and where they live, and that other button styles (`slot_button`, `tool_button`, …) can be added as further `RadialWheels` properties by listing them in `wedges.rs`.
- [ ] **Step 2: Commit** (only with the user's OK): `git add docs/handoff.md && git commit -m "Docs: radial wheel"`

---

Not in this plan (follow-ups once the widget exists): wiring a hold-key radial
menu into mmo's `GameUi` (it needs the modal pointer-ownership flag the spec
describes) and other source styles beyond `button`.
