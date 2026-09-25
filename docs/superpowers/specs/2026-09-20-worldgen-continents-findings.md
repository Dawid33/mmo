# Worldgen: from "ocean with specks" to continents with mountains — findings + prototype

2026-09-20. Measured with `just world-map` (`crates/worldgen/examples/world_map.rs`) and a
standalone prototype (`2026-09-20-worldgen-continents-prototype/proto.rs`, which `#[path]`-includes
the real `noise.rs`/`spline.rs`, so nothing in the world output changed). Renders next to it;
full-size ones in `target/world-map/proto/`.

> **IMPLEMENTED 2026-09-20** (uncommitted on develop) with continent scale ×3, ~30% land, +190 m ranges.
> What landed, and where it differs from the prototype below:
>
> - `noise.rs`: `OctaveNoise` = Minecraft weighting (`amp_i·2^-i`), second stack, value factor;
>   `pipeline_noises_have_minecraft_like_spread` pins sd ∈ 0.3..0.8.
> - `density.rs`: new `WarpedNoise` node; `Min` is now used (peaks-and-valleys).
> - `pipeline.rs`: splines + mountain term + warp in the fused `column()` AND the `#[cfg(test)]` graph
>   oracle (bit-exact tests pass). Named knobs: `CONTINENT_SCALE` 3, `MOUNTAIN_OFFSET` 1.75,
>   `RIDGE_WARP_M` 1400, `EROSION_BASE_FLOOR`, `PV_CREST`/`PV_BACK`.
>   Beyond the prototype: (a) **asymmetric pv fold** — vanilla's symmetric fold sank a crater into every
>   wide ridge; the back slope is now gentle (massifs); (b) erosion-gate floor 0.12 → inland lowlands roll
>   (~20 m ridge lines) instead of lying dead flat; (c) a dedicated **jagged noise** (64 m + 32 m) feeds the
>   jaggedness slot → a few meters of roughness on mountain flanks; (d) an **offshore drop-off** in the
>   base spline — any-solid LOD rounds the sea floor up a cell, so water shallower than 16 m reads as sand
>   in the factor-256 ring; gradual shores made that band ~1 km wide, the drop-off makes it ~150 m;
>   (e) the ocean floor is erosion-modulated like the land (no special case).
> - `WORLD_SEED = 0x00D5_C0DE_0206` from the extended `seed_scan` (flat dry spawn y=71 + a peak ≥ sea+110
>   and open sea within ±1.5 km): bay ~300 m west, range with a y=259 peak ~1 km east.
> - Tests: `world_is_ocean_with_continents_and_mountains` replaces `ocean_is_the_majority` (its 1.9 km
>   patch is all-land or all-sea now); coast tests moved to region (−5, 2). All 37 worldgen lib tests
>   incl. ignored pass, `tests/player_walk.rs` passes, `just smoke` ok.
> - Measured: 256 km span 32.8% land, p99 164 m, max 264 m. `column()` 377 ns (0.2% of a 64 ms
>   `generate_region`). Mountain region vs spawn region: same genesis wall time (0.40 s) and RSS;
>   wire snapshot 1.8–2.6 MiB vs 1.15 MiB. Slopes are steep in the ranges (p99 ≈ 2 rise/run at 4 m).
> - Not run: client test binaries (`keepup_budget` uses the spawn region), live client/server session.

## What the world was before

With `WORLD_SEED`: **0.3% land** over 64 km and 512 km spans, highest surface 80 m (sea 64), spawn is
a smooth ~600 m island that `seed_scan` had to hunt for. No structure above ~1 km.

## Why (two defects in `OctaveNoise`, not in the splines)

The octave tables were copied from Minecraft/Tectonic (`[1.75, 1, 2, 3, 2, 2, 1]` at firstOctave −10),
but Minecraft's `NormalNoise` evaluates them differently from `noise.rs`:

| | Minecraft / Tectonic | ours |
|---|---|---|
| octave `i` weight | `amp_i · 2^-i` (persistence) | `amp_i` |
| scaling | × value factor ≈ 1.5, two decorrelated stacks summed | ÷ Σ amp (12.75) |
| resulting spread | sd ≈ 0.5, reaches ±1.2 | sd ≈ 0.1, \|cont\| p99 0.27, max 0.47 |
| dominant wavelength | the first octave: 1024/0.13 ≈ 7.9 km | the amp-3/2/2 octaves: 1 km … 250 m |

So (1) the splines' land point (\|cont\| ≳ 0.33) is a 1-in-1000 event and the mountain points (≳ 0.9)
are unreachable; erosion (±0.26) barely moves `erosion_mod`; jaggedness is gated on offset > 0 and so
never fires. And (2) even with a gain the field is short-wavelength speckle — no coherent landmass.

## Prototype results (64 km span, same seed)

| variant | change | land | land height above sea p50 / p90 / p99 / max |
|---|---|---|---|
| v0 | today (reproduces the real pipeline) | 0.3% | 1 / 4 / 8 / 11 m |
| v1 | **A:** Minecraft-style octave weighting only, splines untouched | 54.8% | 18 / 64 / 99 / 107 m |
| v2 | A + **B:** retuned continents/base splines + mountain term | 29.1% | 11 / 46 / 143 / 192 m |
| v3 | A + B + **C:** domain-warped ridge noise | 29.1% | 11 / 47 / 143 / 192 m |
| v3, `CS=3` | v3 with continents+erosion wavelength ×3 | 23–31% | 10 / 38 / 116 / 192 m |

- **A** (`McNoise` in the prototype): weight `amp_i · 2^(n-1-i)/(2^n-1)`, second stack sampled at
  ×1.0181268882175227, result × `(1/6)/(0.1·(1+1/n))`. This alone yields coherent 5–10 km landmasses.
- **B**: `raw = spline(|cont|)` laid out on the *measured* distribution (coast at |cont| ≈ 0.55 →
  ~30% land); `base = spline(raw)` for shelf/coast/plains/plateau (≤ +58 m); mountains
  `= inland_gate(raw) · low_erosion_gate(erosion) · peaks(pv) · 1.75`, where
  `pv = −3·(||ridges| − ⅔| − ⅓)` is vanilla's peaks-and-valleys fold — it turns ridge-noise contour
  lines into ridge *lines* with valleys between. `offset = base·(0.55+0.45·egate) + mountains`,
  × the existing `HEIGHT_SCALE` 96 m → peaks at +192 m (y = 256; the band tops out at 320).
- **C**: ridge noise sampled at `(x,z) + 1400 m · (warp1, warp2)` (two low-frequency noises) —
  Tectonic's long-range trick; bends round ridge loops into sinuous ranges. Ridge xz scale 0.22
  (~1.2 km) instead of 0.5.
- **Scale**: Tectonic's own 0.13 gives 5–10 km "continents" (large islands when the LOD horizon is
  2 km). `CS=3` (xz 0.043 / 0.083) gives 15–25 km landmasses with mountain belts — reads as continental.

## Implementation path (as planned — done, see IMPLEMENTED above)

1. `noise.rs`: Minecraft weighting + value factor + second stack in `OctaveNoise` (keeps powers-of-two
   frequencies; all plain f64 add/mul, so the determinism story is unchanged). Update its unit tests.
2. `pipeline.rs`: new splines + mountain term + warp in BOTH the fused `column()` and the
   `#[cfg(test)]` `DensityFn` graph oracle (the bit-exact tests compare the two). `ColumnProfile`
   keeps its three fields; the mountain term lives in `offset`. Expose the scale/height knobs as named
   constants (`CONTINENT_SCALE`, `MOUNTAIN_HEIGHT`, `RIDGE_WARP_M`, …).
3. Cost: `column()` goes from 16 Perlin samples to ~2×(7+6+3)+2×4 ≈ 40 → ~2.5× per profile. It is
   sampled on the 4 m lattice (289 per region), so region generation moves little; `coarse_tile` and
   `world_map` scale with it. Re-run `just bench-column`.
4. Fallout to handle: re-run `seed_scan` for a new `WORLD_SEED`/spawn (criteria still fine);
   tests pinned to specific terrain (`coarse_*` tests use region (−4,−4) as "coast",
   `tests/player_walk.rs`, `ocean_is_the_majority` — 70% ocean still passes); taller terrain means more
   sub-chunks per column near mountains (collider genesis + snapshot size — measure one mountain region
   with `just collider-bench` / `examples/snapshot_size.rs`); worldgen disk cache self-invalidates.
5. Still separate: sub-100 m surface detail (the old "flat" complaint — ridge tops in the close-up are
   smooth), rock/snow materials by slope and altitude, Tectonic's island/continent split and card-suit
   regions, caves.

Choices made (user, 2026-09-20): continent scale ×3, ~30% land, +192 m mountain term.
