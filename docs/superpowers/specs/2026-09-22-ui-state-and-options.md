# UI: current state, gaps, and library options

**Status:** Decided 2026-09-22: keep `bevy_ui`. Bevy was upgraded 0.18 → 0.19.1 and an egui F10 dev panel was added the same day. The architecture summary is in §0.
**Date:** 2026-09-22
**Related:**
- `2026-07-07-in-engine-ui-design.md` (the existing engine-UI design)
- `2026-07-06-hud-crosshair-debug-design.md`
- `2026-09-22-chat-and-commands-investigation.md` (the first screen that needs text input)
- `2026-09-20-modding-wasm-research.md` (mod-defined UI)

**Question:** The user is leaning towards running taffy in the engine and having Bevy or another library render the result. Research the options.

## 0. Architecture: where UI logic runs (decided 2026-09-22)

User context: taffy-in-engine was originally wanted for **UI rendered on meshes in the
world and clicked through the world**; that was ruled out earlier to keep the complexity
out of the engine. It stays out — the in-world case is solvable entirely client-side (below).

**Four kinds of UI, one rule: the sim decides *what*, the client decides *where on screen*.**

| Kind | Structure / logic | Layout + paint | Pointer → effect |
|---|---|---|---|
| **Sim-state screens** (hotbar; next: inventory, containers, entity status, mod UI) | `game::ui::build_player_ui(GameData)` — a pure projection, run on the **client's game thread** once per settled tick (`instance.rs::project_ui`) → `diff_ui` → `UiUpdate` deltas over a channel. Never on the server, never hashed. | Bevy main thread: `renderer/ui_bridge.rs` maps deltas to `bevy_ui` `Node`s; Bevy's taffy (0.10 in 0.19) lays out against the real viewport; `bevy_ui_render` paints. | `bevy_picking` UI backend → `ui_input.rs` observer maps the clicked entity's `WidgetAction` to a **logical** `GameEventKind::UiAction` (never pixels) → predicted locally in the client sim **and** sent to the server → the authoritative sim mutates state → the projection re-derives → deltas. Rollback needs nothing special: a mispredict re-projects and re-diffs. |
| **Client-local UI** (HUD/crosshair, F3 overlay, loading screen, menus, settings, chat box) | Plain Bevy systems on the main thread; no sim involvement unless it emits an event (chat line, `/command`). | `bevy_ui` (+ 0.19 `EditableText` for chat input). | `bevy_picking` observers / Bevy UI widgets on the main thread. |
| **Dev tooling** (F3 panel, `dev_ui` feature) | egui immediate mode, main thread (`renderer/dev_ui.rs`). | `bevy_egui` render pass on the local player's camera. | egui's own input; while open, `CursorPolicy.overlay_open` frees the cursor and `forward_input` stops sending clicks/mouse motion to the sim. |
| **In-world (diegetic) panels** — signs, terminals, shop boards (planned) | Same as sim-state screens: a projection of the panel's owning sim data (block entity / NPC) into a widget tree. | Client: the widget tree is laid out by bevy_ui into an **image render target** (a second UI camera with `RenderTarget::Image`), used as the texture of the panel's mesh — Bevy 0.19's `examples/ui/render_ui_to_texture.rs`. | Client: a mesh raycast from the cursor/crosshair hits the panel, the hit **UV × texture size** drives a virtual `PointerId::Custom` (`PointerInput` events), so ordinary bevy_ui picking/hover works on the texture; the clicked widget's `UiAction` goes to the sim as above. |

Why the in-world case does not need taffy in the engine: the server never needs pixel
geometry, because what crosses the wire is the *logical* action (`UiAction::Press { panel,
button }`), validated by the sim against authoritative state (is the player in reach of the
panel? is the button enabled?). Reach is a world-space check the sim already does for block
editing. Only if the server had to verify *which* pixel was hit (it doesn't) would layout —
and deterministic text measurement — have to move into `game`.

Threads: the projection runs on the client's game (sim) thread; layout, paint, picking and
egui run on Bevy's main/render threads; the two sides meet only through the `UiUpdate`
channel (sim → UI) and the `GameEvents` channel (UI → sim).

## TL;DR

- **We already have "engine-owned UI, client renders it."**
  - `game::ui` is a Bevy-free widget vocabulary, plus a pure `build_player_ui(GameData)` projection and a `diff_ui` that emits `UiUpdate` deltas.
  - The client's `ui_bridge.rs` maps those deltas onto `bevy_ui` nodes, and Bevy's bundled taffy (0.9.2 in our lockfile) lays them out.
  - The missing piece is not the architecture. It is **vocabulary breadth** (the style is a toy subset) and **widgets**: text input, scrolling, icons, and drag and drop.
- **`bevy_ui` is taffy plus a renderer.** Replacing the renderer means rebuilding text shaping, IME text input, clipping and scroll, picking, focus and gamepad navigation, and accessibility. We gain nothing from that.
  - Vello, the obvious alternative painter, needs compute shaders, which kills the wasm WebGL2 fallback.
- **Putting a real `TaffyTree` in `game`** only pays off where the *sim* needs UI geometry. That means diegetic, world-space panels that are hit-tested on the server, which the 07-07 spec already defers.
  - Doing it for the screen-space HUD would force viewport, fonts and DPI into the engine, and they are client-local facts.
  - It would also compile a second taffy version, because Bevy 0.18 embeds 0.9 and the latest is 0.14.
- **Recommendation:**
  1. Keep `bevy_ui` as layout and paint.
  2. Grow `game::ui` into a versioned, serde, CSS-like schema. That schema is the mod-facing UI format.
  3. Put `bevy_egui` behind a feature flag for dev and debug panels only.
  4. Plan the Bevy 0.18→0.19 migration for upstream `EditableText` (chat) and parley text. Until then, use `bevy_ui_text_input`.

## 1. Current state (code survey)

| Piece | Where | What it does |
|---|---|---|
| Widget vocabulary | `crates/game/src/ui.rs` | `WidgetId(u64)` with stable ids chosen by role. `Style` is only direction / width / height / gap / padding / justify / align / background. `Content` is None / Text(String) / Icon(IconId). `UiAction` is MoveItem / ToggleInventory. |
| Projection | `game::build_player_ui` | Builds a 9-slot hotbar from `Client::inventory`. It is the only screen. |
| Diff | `game::diff_ui` | Emits deltas ordered create → set → wire children → remove. |
| Driver | `crates/client/src/instance.rs` `project_ui` (~:723) | Once per settled tick, rebuilds the whole tree, diffs it, sends the deltas, and toggles cursor grab from `inventory_open`. |
| Bridge | `crates/client/src/renderer/ui_bridge.rs` | Maps `WidgetId → Entity`, and `Style → bevy::ui::Node` via `style_to_node`. **Icons are placeholder tinted backgrounds.** |
| Input | `crates/client/src/renderer/ui_input.rs` | An observer turns a `Pointer<Click>` on a widget with a `WidgetAction` into a `GameEventKind::UiAction` sim event (predicted, then sent to the server). |
| HUD | `crates/client/src/renderer/hud.rs` | Hand-written `bevy_ui`: the crosshair and the F3 debug text panel. |
| Debug | `grid_debug.rs`, `collider_debug.rs`, `diag.rs` | Gizmo overlays and log-based diagnostics. There are no in-game debug panels. |
| Loading screen | wasm `crates/client/index.html` only | Download progress bar. Native has nothing; the `[perf] load_progress` values are logged, not shown on screen. |

Cargo features: `bevy_ui`, `bevy_ui_render`, `bevy_text`, `bevy_picking`, `ui_picking`, `default_font`. Bevy is pinned to 0.18.

### Gaps and small defects found

- **The hotbar doesn't drive gameplay.** `game/src/edit.rs:34` hardcodes the placed block ("Dirt until the hotbar drives it"). There is no selected-slot state, no scroll-wheel or 1–9 selection, and no highlight.
- **No inventory screen.** `ToggleInventory` flips `inventory_open` and releases the cursor, but `build_player_ui` draws the same hotbar either way. Drag and drop (the `MoveItem` UX) is unbuilt.
- **Icons are placeholders.** `Content::Icon` → a brown tint. Real icons could reuse `block_textures.rs` (render block faces into an atlas, or draw isometric block renders into an icon atlas).
- **The `Style` vocabulary is too small for real screens.** It has no margin, position (absolute/relative + insets), min/max size, flex grow/shrink/wrap, grid, border (width, radius, colour), overflow/scroll, z-index, text style (size, colour, alignment) or image slicing. There is also no serde, so it can't be a mod format yet.
- **Content is single-valued.** A slot can't have both an icon *and* a text style; the count label is modelled as a child widget, which is fine. But text has no styling at all.
- **No text input and no scrolling.** Chat, the first real need, is designed in the chat spec as "plain `bevy_ui` + `KeyboardInput`" with a hand-rolled line editor.
- **The whole tree is rebuilt every tick.** That's fine at ~20 nodes. Once there are more screens, rebuild only the screens whose source data changed, or keep a per-screen dirty bit.
- **Stale comment.** `hud.rs` `setup_hud` says "the client does not enable the `bevy_picking` feature". It does now (`bevy_picking` + `ui_picking` for `ui_input`). The full-viewport crosshair container is pickable, but it is hidden whenever the cursor is free, so it can't swallow widget clicks today. Add `Pickable::IGNORE` when touching it.
- **UI split across two systems.** Engine-projected UI (hotbar) and client-local UI (HUD, F3, and soon chat) are both fine. Say explicitly which side a new screen belongs on:
  - **Engine projection** when the screen is a function of sim state: inventory, containers, entity status, mod UI.
  - **Client-local `bevy_ui`** when it is local, presentation or connection state: chat, menus, settings, loading, debug.

## 2. The "taffy in the engine" idea, evaluated

Three different things could be meant:

**A. Engine-owned UI *description*, laid out and painted by the client.** This is the current design and the right one.
- It is Bevy-free and serialisable, so a wasm mod or the server can emit it.
- Layout stays client-side because layout = f(data, viewport, fonts, DPI), and three of those four are client-local.

**B. A real `taffy::TaffyTree` inside `game`, with the client only painting the computed boxes.**
- Gains:
  - One headless layout implementation, for tests or server-side validation of mod UI.
  - Geometry the sim can reason about.
- Costs:
  - Text measurement becomes an engine concern. Taffy needs measure callbacks, and to be deterministic it needs a vendored font and shaper at a fixed reference resolution.
  - Viewport and DPI have to be passed in.
  - Bevy 0.18 still lays its own `Node`s out with its internal taffy 0.9. You would have to either:
    - paint absolute boxes yourself, bypassing `bevy_ui` layout (possible with absolute-positioned `Node`s, but you lose `bevy_ui`'s text-driven sizing), or
    - pin `game`'s taffy to Bevy's version and upgrade both in lockstep.
  - Only worth it for **diegetic world-space panels** (signs, terminals, shop boards the server hit-tests). The 07-07 spec already calls this deferred. Build it then, with pinned taffy, and paint into a world-space texture.

**C. Your own taffy with a custom renderer (vello, a custom 2D batcher, …) replacing `bevy_ui` entirely.**
- You own paint (9-slice, borders, radii), glyph atlas and shaping (parley), clipping and scroll, z-order, hit testing, focus and navigation, IME text editing, and accessibility. `bevy_ui` 0.19 already ships every one of those.
- Vello (bevy_vello 0.14 → Bevy 0.19) needs compute shaders, so it is **WebGPU only**, and our wasm build currently falls back to WebGL2. `vello_hybrid` has a WebGL2 backend but is new.
- Not recommended.

## 3. Library landscape (as of 2026-09; versions from crates.io/GitHub)

**Bevy UI itself:**
- **0.18.1** (ours): taffy 0.9.2, cosmic-text. It has variable fonts, pickable text sections, `AutoDirectionalNavigation`, `IgnoreScroll`, and experimental headless `bevy_ui_widgets` (button, checkbox, radio, slider, scrollbar, menu, popover).
- **0.19** (2026-06, 0.19.1 on 08-13): taffy 0.10, **parley** text, and **`EditableText`**. `EditableText` covers:
  - cursor, word navigation and selection
  - IME/CJK and bidi
  - multiline and scrolling
  - per-character filters
  0.19 also brings `bsn!` scenes, and Feathers widgets (text and number input, dropdown, list view), though Feathers looks like an editor, not a game.
- **0.20** (rc.1 on 09-15): runtime `.bsn` asset loading with hot reload. An open PR (#25653) replaces the separate taffy tree with incremental ECS-backed layout.

**Layers on top of `bevy_ui`** (they keep our renderer):

| Crate | Version | Bevy | Why interesting |
|---|---|---|---|
| bevy_flair | 0.8.1 | 0.19 | Real CSS for bevy_ui: selectors, :hover, transitions, keyframes, var(), hot reload. A natural theme or mod-skin layer. |
| bevy_hui | 0.7 | 0.19 | HTML/XML templates → bevy_ui, hot reload. Precedent for data-file screens. |
| haalka | 0.7.1 | 0.18 | Reactive (signals) layer. Single maintainer. |
| bevy_immediate | 0.8 | 0.19 | egui-like API that produces bevy_ui entities. Young. |
| bevy_ui_text_input / bevy_simple_text_input | — | 0.18 | Text input for chat until we reach 0.19. |

**Separate renderers:**

| Option | Verdict |
|---|---|
| **bevy_egui** 0.42 (0.39.x for 0.18) | Immediate mode, very active, works on WebGL2. **Best for dev, debug, perf and admin panels** (plus bevy-inspector-egui). The tool look and poor gamepad story make it wrong for the player-facing UI. Veloren ships egui only in debug builds; do the same (feature flag). |
| bevy_lunex 0.7 | Its own layout on `Transform`s. Good for stylised or world-space UI. A second layout system; keep in mind for diegetic panels. |
| Slint 1.18 | Healthy toolkit, but its Bevy integration reports blank screens and init-order panics (2026-02). It adds a second event loop and brings licensing questions. No. |
| Blitz / Dioxus Native (beta) | A full Stylo CSS + taffy + parley + vello engine. The most "web" option, but not embeddable in Bevy yet and WebGPU-only via vello. No, for now. |
| Xilem/Masonry, iced (no maintained Bevy bridge), Clay | Not embeddable, or nothing gained over taffy. |
| kayak_ui, sickle_ui, quill, bevy_cobweb_ui (archived 2026-01) | Dead. Avoid. |
| Ultralight / Noesis | Native C++ SDKs with no wasm32 support. No. |
| DOM overlay (wasm only) | Native text input, IME and CSS for free, but it is a second implementation (native has no DOM) and fights pointer lock. Only for web-only pre-join chrome. |

**Comparable projects:**
- **Veloren** chose third-party GUIs (conrod for the HUD, iced for menus) and now maintains **forks** of both. Their conrod → iced HUD migration was called "a fairly massive undertaking" and still hasn't happened.
- Minecraft Bedrock (JSON UI) and Hytale (markup files) use engine-native, data-driven screens.
- Lesson: stay on the engine's first-party UI and make the *screens* data.

## 4. Mod-defined UI

Mods are deterministic, stateless wasm guests with host-owned, undo-logged state (modding spec). That fits the current design exactly:
- A mod's UI export is a function from host state to a widget tree, the same shape as `build_player_ui`.
- It runs client-side in the projection, never in hashed state.
- Clicks come back as a logical `UiAction::Mod { mod_id, action_id, payload }` routed into the sim like any input.
- Guests never see pixels, layout or fonts.

Schema choice for the tree:
- **Own, versioned `game::ui` schema with serde (recommended).**
  - Grow it towards a CSS subset: grid, borders and radius, images and 9-slice, text runs, overflow, and position.
  - Translate it to `bevy::ui::Node` in the bridge.
  - We control breakage. Taffy changed its alignment types in 0.11, and we don't want that leaking into mod ABI.
- `taffy::Style` + serde directly: complete, but it couples the mod format to taffy's breaking releases.
- Bevy `.bsn` (0.20): reflection can instantiate *any* registered component, which is a sandboxing hazard for untrusted content. Fine for first-party screens only.
- Optional styling layer: class names in the schema + a stylesheet (bevy_flair-style) = mod "themes".

## 5. Recommended roadmap

Ordered by value per effort. Each step stands alone.

1. **Make the hotbar real.**
   - Selected slot in `Client`, driven by 1–9 and the scroll wheel.
   - `edit.rs` places the selected item's block.
   - Highlighted slot.
   - Real icons from the block texture atlas (`ImageNode` with an atlas rect).
2. **Grow the `game::ui` vocabulary** as the next screens need it: margin, position + insets, min/max size, flex grow/wrap, border + radius, overflow scroll, text style, image. Add `serde` + a schema version now, while it is cheap.
3. **Inventory screen + drag and drop.**
   - A client-side drag gesture (picking `DragStart`/`DragDrop`) resolves to `UiAction::MoveItem`.
   - `build_player_ui` gains the `inventory_open` arm.
4. **Chat (client-local UI).**
   - On 0.18: `bevy_ui_text_input`, or the hand-rolled line editor from the chat spec.
   - Better: do step 5 first and use `EditableText`.
5. ~~**Bevy 0.18 → 0.19 migration**~~ DONE 2026-09-22 (0.19.1). Render graph → `Core3d` systems; pipelines created in `RenderStartup`; view layout key from `ViewKeyCache`; fog behind `DISTANCE_FOG`; NVK cooperative-matrix workaround. `EditableText` is now available for chat.
6. ~~**`bevy_egui` dev panels**~~ DONE 2026-09-22: F10 panel (rebound to F3 2026-09-23, replacing the old F3 overlay) in `renderer/dev_ui.rs` behind the default-on `dev_ui` feature (stats plus grid/culling toggles), with `CursorPolicy` and an input gate for overlays. Add region/LOD/hash-mode panels as needed.
7. **Native loading screen** driven by the existing `load_progress` / `world_ready` join timeline.
8. **Deferred:** diegetic world-space panels (real taffy in `game`, pinned to Bevy's version, painted to a texture), mod UI export, and CSS theming.

## Open questions for the user

- Is the "taffy in the engine" motivation mainly **mods emitting UI**, **server-side knowledge of UI**, or **independence from Bevy**?
  - The first two are served by the description layer (plus B only for the diegetic case).
  - The third is already true for `game`; the client is Bevy by design.
- Appetite for the Bevy 0.19 migration now vs. a text-input crate on 0.18 for chat.
- Is `bevy_egui` acceptable as a dev-only dependency?

## Sources

- https://bevy.org/news/bevy-0-18/
- https://bevy.org/news/bevy-0-19/
- https://github.com/bevyengine/bevy/pull/23576 (runtime .bsn)
- https://github.com/bevyengine/bevy/pull/25653 (incremental layout)
- https://github.com/DioxusLabs/taffy (CHANGELOG)
- https://github.com/vladbat00/bevy_egui
- https://github.com/bytestring-net/bevy_lunex
- https://github.com/eckz/bevy_flair
- https://github.com/Lommix/bevy_hui
- https://github.com/databasedav/haalka
- https://github.com/PPakalns/bevy_immediate
- https://github.com/linebender/bevy_vello
- https://github.com/dioxuslabs/blitz
- https://github.com/carlosdp/dioxus-in-bevy
- https://github.com/slint-ui/slint/discussions/10681
- https://veloren.net/blog/devblog-94/
- https://gitlab.com/veloren/veloren/-/raw/master/voxygen/Cargo.toml
