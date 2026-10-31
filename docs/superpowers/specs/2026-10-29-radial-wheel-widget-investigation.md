# Radial wheel menu in the Factorio-style widget library — investigation

2026-10-29. Question: how would we add a radial (pie) wheel menu to `~/Projects/ui`'s
`widgets` crate, in the same style as the rest of the library? Nothing is built yet.

## What the library is, and why that matters here

`widgets` is not a free-form theme. Its rule (`~/Projects/ui/docs/handoff.md`) is
that every element is **generated from Factorio's own style data**
(`codegen/style.rs` → `ui/generated/styles.slint`), drawn by a hand-written
`…Face` + interactive pair in `ui/<type>.slint`, shown in `showcase`, and
**pixel-diffed against in-game captures** by `reftest` (max Δ ≤ 3, mean Δ ≤ 0.3).
`docs/inventory.md` tracks all 582 Factorio styles.

**Factorio has no radial menu.** There is no `radial_style` to generate and nothing
in the game to capture. A wheel therefore has to be a **composite widget** made
only of styles that already exist, the way `Dropdown` is a `button` plus a
`list_box`. It can't be a new generated style type.

## What the renderer can draw (Slint 1.18.1 software renderer)

The library is verified only on the software renderer, which is also the one the
mmo client uses (`crates/slint-ui`, headless `MinimalSoftwareWindow`). Its source
(`i-slint-renderer-software-1.18.1/lib.rs`) settles the drawing options:

| Capability | Status | Consequence |
|---|---|---|
| `Path` fill / stroke | **yes**: anti-aliased via zeno (the `path` feature comes with `std`, which we enable) | Annular wedges and arcs can be drawn as vectors |
| Gradient on a `Path` | **no**: `draw_path` uses `fill_brush.color()` | Wedges are flat colour only |
| Rotated or transformed images | **no**: `supports_transformations() → false` | Factorio nine-slice sprites can't be bent or rotated into wedges at runtime |
| Clipping to a circle or radius | **no**: `combine_clip` ignores the radius ("TODO") | Can't mask a textured panel to a ring |
| `drop-shadow` | **no**: `draw_box_shadow` is a TODO | Shadows only through the generated shadow layers |
| `atan2`, `sin`, `cos`, `sqrt` in `.slint` | **yes** (compiler builtins) | Sector picking and layout can stay in pure Slint |
| Property animations | yes; `Overlay::render` already calls `update_timers_and_animations` | An open/close tween is possible, though Factorio's GUI doesn't animate |

The upshot: **anything Factorio-textured stays axis-aligned rectangles**. The only
round shapes we can draw are flat-colour `Path`s or pre-baked images.

## Three ways to draw it

**A. A ring of slot buttons (recommended).** Lay out N `ButtonFace`s with an
existing generated slot style (`slot-button`, `inventory-slot`, `big-slot-button`,
or `tool-button` for text) on a circle using `cos`/`sin`. Show the hovered sector
with the style's own `hovered` / `selected` states. Put a small `FrameFace` in the
middle with a `LabelFace` naming the hovered item. Every pixel is then a
reftest-verified Factorio sprite, so it matches the rest of the UI exactly. Cost:
the "wheel" is implied by placement and there are no drawn wedges. That is also
how Factorio-like games usually do radial menus: icons on a ring.

**B. Vector wedges.** Annular sectors as `Path`s (`MoveTo`/`ArcTo`/`LineTo`), filled
with colours from Factorio's palette: the frame body grey, and the slot-hover
orange for the active sector. This reads clearly as a wheel and is cheap, but it is
flat and has no bevel, so it doesn't match the textured frames around it. The
colours would be hand-picked constants, which breaks the "nothing hand-tuned" rule.
The one place a vector fits is a **thin highlight arc or pointer** on top of A
(stroke only, 2–3 px), to show the aim direction between slots.

**C. Wedges baked at build time.** Extend `codegen/bake.rs` to render one RGBA
image per (segment count, segment index, state), polar-mapping
`gui-new.png`'s frame tiles (body fill plus bevel edges) onto the annulus. They
draw unrotated as ordinary `Image`s, so the software renderer copes. This gives
the most "Factorio-made" wheel, but it is new invented artwork the game never
draws: there's nothing to reftest it against, it costs about 77 images × 3 states
for N = 2..12, and it adds a lot of codegen for one widget. Only worth it if A
looks too sparse in practice.

**Recommendation was A. DECIDED 2026-10-29 (Dawid): C** — rework the existing
Factorio artwork into wedges at build time. Plan:
`docs/superpowers/plans/2026-10-29-radial-wheel-widget.md`. The sketch below is
for A; the component API (items, hovered, pick by angle, `activated`) carries
over to C unchanged.

## Sketch (A)

Unverified Slint, to show the shape; it follows the `…Face` + interactive split
in `button.slint` / `slider.slint`:

```slint
// crates/widgets/ui/radial-menu.slint
export struct RadialItem { text: string, icon: image, enabled: bool }

// Fixed state: which sector is hovered. Showcase + snapshot tests use this.
export component RadialMenuFace {
    in property <[RadialItem]> items;
    in property <int> hovered: -1;
    in property <ButtonStyle> slot-style: Styles.slot-button;
    in property <length> radius: 88px;
    in property <angle> start: -90deg;          // item 0 straight up
    property <angle> step: 360deg / max(1, items.length);

    for item[i] in items: ButtonFace {
        property <angle> a: root.start + i * root.step;
        style: root.slot-style;
        // Whole pixels, as the slider snaps its thumb: sprites must not land on
        // half pixels or the halved tiles resample.
        x: round((root.width / 2 + root.radius * cos(a) - self.preferred-width / 2) / 1phx) * 1phx;
        y: round((root.height / 2 + root.radius * sin(a) - self.preferred-height / 2) / 1phx) * 1phx;
        state: !item.enabled ? root.slot-style.disabled
            : i == root.hovered ? root.slot-style.hovered : root.slot-style.default;
        // icon: an Image centred on top (sprite-button style), as slot buttons show items
    }
    // Centre: FrameFace (e.g. Styles.frame / inside-shallow-frame) + LabelFace with
    // items[hovered].text; hidden when hovered < 0.
}

// Interactive: picks by ANGLE, not by hit box, so the gaps between slots count.
export component RadialMenu {
    in property <[RadialItem]> items;
    in property <length> dead-zone: 24px;       // centre = cancel
    out property <int> hovered: ...;            // from touch.mouse-x/-y:
    //   dx, dy = mouse - centre; |d| < dead-zone → -1;
    //   else mod(round((atan2(dy/1px, dx/1px) - start) / step), items.length),
    //   -1 if that item is disabled
    callback activated(int);
    face := RadialMenuFace { hovered: root.hovered; ... }
    touch := TouchArea { clicked => { if root.hovered >= 0 { root.activated(root.hovered); } } }
}
```

Things to settle while building it:

- **Gamepad / stick aim.** Expose an optional `in property <angle> aim` (plus
  `aiming: bool`) so `hovered` can come from a stick instead of the pointer.
- **Hold-to-open, release-to-pick** is the usual radial idiom. The key is a Bevy
  key, so the client does it: it opens the wheel at the cursor on press, then on
  release reads `hovered` and fires the action. Slint only provides `hovered` and
  `activated` (click-to-pick still works for mouse-only use).
- **Pointer ownership.** The overlay gives the pointer to the UI only over
  **opaque** pixels (`Overlay::opaque_at`, `CursorPolicy::ui_pointer`), so the
  transparent gaps between slots would leak clicks to the commander camera. While
  the wheel is open it must be modal: export an `open` property that
  `slint_overlay.rs` reads to own the pointer over the whole window. Don't paint a
  1-alpha veil to fake it.
- **Item count.** Six to eight reads well. Above about 12, slot buttons on one ring
  overlap at a sane radius, so grow the radius with N (keep the gap between slots
  at least one slot width) or page.

## Fitting the library's process

- **Inventory.** `docs/inventory.md` lists Factorio *styles*, so a composite has no
  row there; it's like Dropdown/ListBox built from rows that are already done.
- **Showcase.** Add a section with every hovered index for N = 4/6/8, disabled
  items, the empty centre, and an interactive row.
- **Verification.** reftest can't compare against the game. Use a pixel
  **self-snapshot** of `RadialMenuFace`, which proves only that it is unchanged,
  plus a headless pointer test in the style of mmo's
  `crates/slint-ui/tests/asset_viewer.rs`: move the pointer to angle θ and assert
  `hovered`, including the gap between slots, the dead zone and a disabled item.
  Every sprite inside it is already reftest-verified.
- **Unused data worth noting.** `button_style.pie_progress_color` is generated
  but ignored today (it appears in every button row's ignored list). It is
  Factorio's own colour for pie-shaped overlays on slots, so it's the obvious
  in-data source for option B's highlight arc instead of a hand-picked colour.

## Open decision

**Where the widget lives.** In `~/Projects/ui` (`widgets`), it is a reusable
composite next to Dropdown, but it is the library's first widget with no Factorio
original, which bends the "every element is checked against the game" rule. In
mmo's `crates/slint-ui/ui/`, it is a game-side component built from `@widgets`, and
the library stays a pure Factorio reproduction. Recommendation: put it in `widgets`
(that's what "a new widget" asks for, and nothing in it is hand-drawn), with a
handoff note saying its verification is a self-snapshot, not a game capture.
