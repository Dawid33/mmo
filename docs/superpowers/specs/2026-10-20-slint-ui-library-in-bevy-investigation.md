# Using `~/Projects/ui` (Slint) in the Bevy client — investigation

Date: 2026-10-20. Status: **option A implemented** the same day (`crates/slint-ui`
+ `crates/client/src/renderer/slint_overlay.rs`, commander mode; one "Fortress"
panel with region, connection, selection summary and a working "Clear
selection" button). The §5 spike measurements (render timing, self-screenshot
check, wasm size) were deliberately not run. Implemented differently from §3:
the upload goes through `Assets<Image>` (whole-texture re-upload on any
change), not a dirty-rect `write_texture` yet.

Question: use the Slint widget library in `~/Projects/ui` for the client, or
recreate it pixel for pixel in Bevy.

## 1. What `~/Projects/ui` actually is

A recreation of **Factorio 2.0's GUI** in Slint 1.18.1. Its pixel target is
Factorio, and the Slint code is only how it gets there.

- **`crates/widgets/build.rs` + `codegen/` (~2.5k lines).** Evaluates
  Factorio's `core/prototypes/style.lua` with mlua and parses all 582 styles
  strictly. It cuts and tints the sprite pieces out of `gui-new.png` and
  friends, and bakes `@half` tiles with the game's 2×2 round-down average.
  Tints are baked into the PNGs, because Slint can't tint. Output:
  - `ui/generated/styles.slint`: every style as a Slint struct literal, in the
    `Styles` / `Fonts` / `Sprites` / `StyleLists` globals.
  - 5,572 PNGs and the Titillium/Noto fonts, 34 MB.
- **`ui/*.slint` (~2k lines, hand-written).**
  - `graphical-set.slint`, the core: Factorio's `ElementImageSet` as shadow,
    base and glow layers. Each layer is pieces stretched or tiled per edge
    code, with outer borders, shifts, overall tilings, monolith growth, and
    halved pieces drawn texel for pixel and clipped.
  - Thin widgets on top: a `…Face` (stateless, explicit state) plus an
    interactive component per type. Types: button, frame, label, checkbox,
    flow, table, textbox, progressbar, slider, scroll pane, list box,
    dropdown, tabbed pane, switch, image, line.
- **`crates/reftest`.** Drives a real Factorio with a mod that freezes widget
  states, grabs the window, and renders the same cases with **Slint's software
  renderer**, then diffs them.
  - Status: 420 of 582 styles done, the rest out of scope with reasons
    (`docs/inventory.md`).
  - Graphics: max Δ ≤ 3, mean Δ ≤ 0.3 at 1× and 2× UI scale.
  - Text is approximate: 56% (1×) and 72% (2×) of pixels identical.
    `docs/text-rendering.md` has a fitted stb_truetype model as the way to
    exact text.

**Consequence:** the fidelity numbers belong to the **software renderer**
specifically. Slint's FemtoVG and Skia renderers, and any Bevy port, are
unmeasured until reftested.

## 2. Constraints from this repo

- Bevy 0.19.1 is on **wgpu 29.0.4**. Slint 1.18.1 has `unstable-wgpu-29`
  *and* `unstable-wgpu-30`; the `ui` lockfile resolved 30. GPU sharing is
  version-compatible only with `-29`.
- Slint's `default` features pull `backend-default` (winit) and
  `renderer-femtovg`. Embedding must use
  `default-features = false, features = ["std", "compat-1-2", "renderer-software"]`,
  or the client gets a second winit and wgpu.
- The 2026-09-22 UI spec (`2026-09-22-ui-state-and-options.md` §3) rejected
  Slint over three things:
  - its Bevy GPU integration (blank screens, init-order panics),
  - a second event loop,
  - licensing.

  The software path below has neither of the first two. Licensing still
  stands (§6).
- That spec's other decision does constrain this one: **sim/mod UI is a
  data-driven `game::ui` widget tree** (`build_player_ui` → `diff_ui` →
  `UiUpdate` → `renderer/ui_bridge.rs`). Mods describe UI as a tree. Slint
  components are compiled, and **Slint cannot express recursive components**,
  so a compiled Slint UI can't render an arbitrary `game::ui` tree. Only
  fixed, first-party screens fit.
- Input gating: `CursorPolicy.overlay_open` + `forward_input` already drop
  sim clicks while an overlay holds the pointer.
- `widgets/build.rs` **panics without a Factorio install** (`FACTORIO_DATA`
  or a Steam path), even though `ui/generated/` is now committed. This has to
  be fixed before anything else builds it: skip codegen when `generated/`
  exists and no data dir is found.

## 3. Option A — embed Slint, software-rendered into a Bevy texture

Slint runs headless inside the Bevy main thread. A custom
`slint::platform::Platform` returns one `MinimalSoftwareWindow`, so there is
no Slint event loop and no shared GPU device.

Per frame, in one main-thread system (the component handles are `!Send`;
keep them in a `NonSend` resource, which 0.19 still has):

1. `slint::platform::update_timers_and_animations()`.
2. Forward Bevy input as `WindowEvent`s:
   - `CursorMoved` → `PointerMoved`
   - `MouseButtonInput` → `PointerPressed` / `PointerReleased`
   - `MouseWheel` → `PointerScrolled`
   - `KeyboardInput` → `KeyPressed` / `KeyReleased` with text
   - window resize → `set_size` and `ScaleFactorChanged` (the scale is
     window scale × UI scale)
3. `window.draw_if_needed(|r| r.render(&mut buf, stride))`, with
   `RepaintBufferType::ReusedBuffer` so only the dirty region is
   re-rasterised.
4. Upload into a full-screen `Rgba8UnormSrgb` `Image` shown by an `ImageNode`
   on top of the HUD.
   - Slint writes **premultiplied** colour and `ImageNode` blends straight
     alpha. Either un-premultiply the dirty rows, or implement Slint's
     `TargetPixel` for a straight-alpha type.
   - Touching `Assets<Image>` re-uploads the whole texture: 8 MB at 1080p,
     33 MB at 4K. That's fine for static screens. For hover and animation, a
     render-world system doing `queue.write_texture` of the dirty rect only is
     the real fix.
5. Hit-test: the UI owns the pointer where `buf[cursor].alpha > 0`. Factorio
   panels are opaque, so this is exact. Feed it into `CursorPolicy` next to
   `overlay_open`.

Data in and out:

- Sim/projection data goes in via Slint properties and `VecModel`s, set from
  the existing `UiUpdate` stream or straight from client state.
- Slint callbacks map to `GameEventKind::UiAction`, the way
  `renderer/ui_input.rs` does today.

Packaging: a new workspace crate (`crates/ui-slint`) depends on `widgets` by
path and compiles the game's own `.slint` screens in its `build.rs`, the way
`showcase` does. Slint compilation then never reruns on client-only edits.

**Pros**

- Pixel output is *exactly* what reftest measured. The graphics evidence and
  the text numbers carry over unchanged, and future reftest work in
  `~/Projects/ui` lands for free.
- Small: roughly a 400–600 line adapter crate, no widget rewrite.
- Works on wasm (the software renderer is pure Rust: swash/zeno/skrifa) and
  on WebGL2. It doesn't care about the per-chunk fallback.

**Cons and risks**

- **CPU cost.** Rasterising is on the main thread, and a full-screen dirty
  region at 4K is millions of pixels.
  - Static Factorio-style panels repaint rarely, and the dirty rects are
    small.
  - Scrolling a large list or a full-screen fade is the worst case.
  - Not measured. Spike step 1 below measures it.
- **Two UI systems.** Mod and data-driven screens stay on
  `game::ui` → `bevy_ui` and won't share the Factorio look unless they're
  also ported.
- **Binary and wasm size.** Every referenced image is embedded:
  `@image-url` → bytes in the binary, up to the 34 MB of generated PNGs. The
  `EmbedForSoftwareRenderer` mode pre-decodes them and is bigger still. Slim
  it by pruning unused styles before generation.
- **Compositing over the world** is linear-space (Bevy). UI-on-UI blending is
  Slint's own gamma-space blending, so it stays exact. Only translucent
  shadows over the 3D scene differ from Factorio, and reftest has no
  reference for that case anyway.
- **IME and clipboard:** forward `Ime` events by hand. Slint's clipboard
  needs a platform hook.

## 4. Option B — pixel-perfect recreation in Bevy UI

Keep the generator and replace the Slint layer.

1. **Codegen.**
   - Split `codegen/` into the reusable parse/bake part (`data`, `style`,
     `graphics`, `bake`, `sprites`, most of `elements`) and the emitters.
     Only `emit.rs` and `lit.rs` (~190 lines) are Slint-specific.
   - Add a second emitter writing RON or Rust consts for a
     `factorio_style`-like crate.
   - Bake pieces into one atlas: `TextureAtlas` + `ImageNode` indices, not
     5.5k separate assets.
2. **Layer engine.**
   - Port `graphical-set.slint` to a Bevy system that spawns
     absolutely-positioned `ImageNode` children per piece: edge codes, tiling
     of whole tiles only, outer borders, shifts, overall tilings, clipping via
     `Overflow::clip`, halved-piece selection by scale factor.
   - Don't use `NodeImageMode::Sliced`. The Slint code avoided nine-slice for
     the same reasons: its own scale and tile semantics.
3. **Widgets.** Port ~1.5k lines of `.slint` to Bevy components and systems:
   - behaviour from `bevy_ui_widgets` (button, checkbox, radio, slider,
     scrollbar);
   - text input from 0.19's `EditableText` / `bevy_ui_widgets::text_input`;
   - scrolling via `ScrollPosition`.

   Factorio flow/table layout semantics have to be mapped onto taffy flexbox
   and grid. Slint's layout was already a mapping, so this is a re-fit, not a
   copy.
4. **Reftest.** Reuse the oracle: render the same `cases.rs` with a headless
   Bevy render-to-image, like the repo's self-screenshot probes (`pull-smoke`,
   `model-view --shot`). Diff against the stored Factorio captures, **or
   against the Slint software output**, which is already within Δ ≤ 3 of
   Factorio. That way Bevy iterations need no Factorio runs.

**Known fidelity traps (each needs its own reftest loop)**

- **Blending space.** Bevy blends UI in linear space on an sRGB target, while
  Factorio and Slint blend in gamma space. Shadows (`…-0f070364`, about 39%
  alpha) and glows will miss Δ ≤ 3.
  - Fix: render the UI with a dedicated camera into an offscreen
    **`Rgba8Unorm`** (non-sRGB) target, with sprites loaded `is_srgb = false`,
    so that maths runs on encoded values.
  - Then composite that texture onto the screen as sRGB.
- **Sampling.** Pieces must land texel for pixel. Bevy UI rounds layout to
  physical pixels, which is good. Use a nearest sampler for 1:1 pieces.
  Non-integer scales need a decision; Slint uses `image-rendering: smooth`.
- **Text.** Bevy 0.19 is parley + swash, not Slint's swash path. The glyph
  pixels and advances will differ, so the 56%/72% numbers restart from
  scratch. The stb_truetype route in `docs/text-rendering.md` fits both
  options equally: rasterise text to an image yourself.

**Pros**

- One UI system. `game::ui` trees, including mod UI, can use the Factorio
  styles directly: give the schema a `style: "frame"` and let the bridge spawn
  the layer nodes.
- First-party renderer, no second toolkit, no Slint licence.
- GPU-drawn, no CPU raster.

**Cons**

- Large. It's the same per-element-type generate/show/capture/diff/fix loop
  that took ~20 commits in `~/Projects/ui`, plus the three traps above.
- Bevy's UI API churns every release; 0.20 will bring BSN and incremental
  layout.

## 5. Recommendation

**A first, B only if one UI system becomes a requirement.**

- A is the only way to get the measured, pixel-exact Factorio look into the
  client soon. It's small, and it leaves `~/Projects/ui` as the single source
  of truth that keeps improving.
- Its real unknown is CPU cost, which is cheap to measure.
- B is the right end state only if Factorio-styled mod or data-driven UI
  becomes a must. If it does, A's Slint render becomes B's reference image.
  Both share the generator, so A is not wasted work.

Suggested spike, in order, stopping at the first failure:

1. In `~/Projects/ui`, make `widgets/build.rs` skip codegen when `generated/`
   exists and no Factorio is found.
2. Time a headless full-screen software render (1080p and 4K, at 1× and 2×
   scale) of the showcase's heaviest section, full repaint vs a hover-sized
   dirty rect. Use `showcase --screenshot` plus a timer (release, pinned
   cores).
3. `crates/ui-slint`:
   - the platform adapter, input forwarding, upload and alpha hit-test;
   - one Factorio `frame` with a button and a label as an overlay in the
     commander view;
   - verify against `showcase` output with a self-screenshot.
4. Check the size cost: the wasm build's binary growth with all styles vs
   pruned styles.

## 6. Blocking non-technical issues (both options)

- **Factorio's art and style data are Wube's.** `~/Projects/ui`'s own handoff
  calls them non-redistributable, and `generated/` was committed in `fe7515a`.
  - Fine for a private prototype. **Not shippable** in a public native or web
    build: the web build serves the files to anyone.
  - The generator + layer engine are generic. Shipping means feeding them
    original sheets in the same `style.lua` shape, or authoring a
    `style.lua`-shaped table for our own art.
  - Titillium Web and Noto are OFL, which is fine.
- **Slint licence (A only):** GPL-3.0, or the Royalty-free 2.0 licence.
  - Royalty-free 2.0 covers desktop, web and mobile apps (not embedded
    devices) but requires visible attribution: an "About Slint" notice or a
    badge on the download page.
  - Or a commercial licence.
  - Decide before shipping.
