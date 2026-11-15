# BUI (Blender's UI as a library) — investigation

2026-11-06. Repo: https://github.com/ppmpreetham/BUI (crate `blenderui` 0.4.0, GPL-2.0-or-later).
Question: could it serve as a game-development interface here, and what would it cost to compile?

## What it is

- A C "kit" (~450k lines of C/C++ in `kit/` + `gpu/` + `core/`, incl. vendored FreeType and image
  decoders) re-porting Blender's widget drawing and interaction rules; retained-mode node tree,
  damage-tracked software raster, optional GPU renderer (Direct3D 11 / OpenGL 3.3 only).
- Rust: `blenderui-sys` (build script runs CMake on the C sources, always `Release`, links three
  static libs + libstdc++) and `blenderui` (gpui-style builders, `rsx!` markup with compile-time
  checked Tailwind, CSS engine, layout by `taffy` 0.15, components + hooks, 45 Blender components
  such as `NumberSlider`, colour ramp, curve and node editors).
- Hosts: native X11 / Win32 windows (Cocoa experimental), or **headless**: `App::draw(window)` returns
  `(w, h, Vec<u32>)` and `simulate_mouse_move/click/drag/scroll/key` inject input (`BK_press`,
  `BK_release`, `BK_key`, `BK_key_release` exist underneath for finer control).
- No WebAssembly yet (listed as planned).

## Compile-time cost (measured)

Probe crate `bui-probe` (Debug-tab-sized `rsx!` panel; our dev profile — opt-level 1, deps
opt-level 3, line-tables debuginfo; nightly-2026-10-04; pinned to 16 cores like `just`):

| | |
|---|---|
| Cold build, everything | **23.7 s** wall, 1.07 GB peak RSS, 393 MB target dir |
| — `blenderui-sys` build script (CMake C build) | 11.6 s |
| — `blenderui` crate | 7.7 s |
| — tailwind / taffy / table / macros / cc | ~1–3 s each, in parallel |
| Edit a string in the panel's `rsx!` → rebuild + relink (17.5 MB bin) | **0.2 s** |

In this workspace the C build is a one-time ~12 s per target dir / profile / `target/variants`
copy (build-script output is cached; it does not depend on our region/chunk dials). `taffy` 0.15
would be a second copy beside Slint's 0.10. Edits to the UI cost whatever the crate holding them
costs: ~0.2 s in its own small crate, ~15–18 s if the code lives in `client` (its relink).
For comparison, a `.slint` edit today is ~35–42 s (generated-crate rebuild) + ~3 s client relink.

Runtime (probe, CPU raster, 360×480): first frame 5.9 ms, steady frames 0.2–0.55 ms.

## Fit for this project

**Blockers for in-game / shipped UI:**

1. **Licence: GPL-2.0-or-later.** Linking it into `client` makes the distributed binary (native
   and the public web build) GPL. Fine for tools that are never distributed.
2. **No wasm32.** The client builds for the browser; BUI code would have to be native-only.
3. **Opaque frames.** The raster is `0x00RRGGBB`, its alpha byte unused (probe: never set), so it
   cannot be laid over the 3D view like the Slint layers (straight-alpha textures in
   `slint_composite`). Workarounds: rectangular opaque panels only (mask by element bounds),
   render twice on black and white to recover alpha (2× raster), or patch `gpu_raster.c`.
4. **Closure-scoped `App` + process-global C kit.** `Application::run(|cx| …)` owns the app for
   the closure only and holds a process-wide mutex (one kit per process). Embedding in Bevy means a
   dedicated UI thread fed by channels (the probe does this; it works), state crossing as messages.
5. **GPU path not shareable** (D3D11 / GL 3.3, no wgpu/Vulkan) — CPU raster + texture upload, the
   same shape as the current Slint path.
6. **Look is Blender's**, not the Fortress theme; and CLAUDE.md / the Slint decision put every
   screen, the F3 dev page included, in Slint. Adopting BUI in-client reverses that decision.
7. Young (0.4.0, very active, few maintainers) — API churn likely.

**Where it fits:** standalone, non-distributed **developer tools** as their own binaries (native
X11 window, no Bevy embedding, GPL irrelevant since not shipped): e.g. a native replacement for
`just forge-ui` (today an HTML page on 127.0.0.1:8787), a world / history / legends inspector, or
a creature-asset workbench — Blender's number fields, curve and colour-ramp editors suit knob
tuning. Cost there: ~24 s once per cold target, sub-second edits.

**Not recommended:** player UI or the in-client F3 page (licence, wasm, alpha, duplicate UI stack).
