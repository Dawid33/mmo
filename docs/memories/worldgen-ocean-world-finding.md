---
name: worldgen-ocean-world-finding
description: "MEASURED 2026-09-20 — generated world is 99.7% ocean, max 16 m above sea; cause = normalized octave noise never reaches the spline control points; `just world-map` renders PNG maps"
metadata: 
  node_type: memory
  type: project
  originSessionId: f27286e3-00c5-41d6-9df2-4537564d6790
  modified: 2026-09-20T18:12:15.404Z
---

**Tool (added 2026-09-20, uncommitted):** `just world-map [--span m --size px --center x,z --out stem --seed hex --relief f]`
→ `crates/worldgen/examples/world_map.rs` writes `target/world-map/<stem>-color.png` (hypsometric +
hillshade) and `-height.png` (16-bit gray, −64..320 m → 0..65535) plus land %/height/slope stats.
One `TerrainPipeline::column` per pixel, ~60 ms per 1024² — never materializes a region.

**Finding:** with `WORLD_SEED`, land is 0.3% of columns over 64 km and 512 km spans (0.8% in 8 km,
24.6% in the 1 km around spawn); highest surface anywhere ≈ 80 m (sea = 64). Spawn is a ~600 m smooth
island — `seed_scan` had to search for it; its "mostly-land ±256 m" comment oversells it.

**Root cause (pipeline.rs tuning, not a code bug):** `OctaveNoise` divides by the amplitude sum, so
|continents| has p50 0.07 / p99 0.27 / max 0.47, but `continents_curve`/`offset_base` splines are laid
out for inputs up to 1.0–1.2: land needs |cont| ≳ 0.33 (p99.9), mountains (offset ≥ 0.9) need ≳ 0.9 —
unreachable. Erosion spans only ±0.26 so `erosion_mod` barely acts; jaggedness is gated on offset > 0 so
it ~never fires. Also no continent-scale structure: longest wavelength is 1024/0.13 ≈ 7.9 km.

**What-if (noise × gain before the splines, clamped ±1):** gain 2.5 → 24% land, p99 +53 m, max +90 m;
gain 3 → 33% land, max +100 m; gain 4 → 46% land. Not applied — changes world output (re-run
seed_scan, worldgen cache self-invalidates, `ocean_is_the_majority` test still holds ≤ gain 4), and it
is the user's design call; see [[design-questions-need-tradeoffs]]. Related: [[flat-world-ocean-spawn]]
(the "flat" complaint is this same tuning gap plus no <100 m detail), [[tectonic-worldgen-research]].

**Deeper cause + prototype (same day):** the real defect is `OctaveNoise` itself — no per-octave
persistence (Minecraft weights octave i by amp_i·2^-i; ours uses amp_i, so 250 m–1 km octaves dominate
and nothing is coherent above ~1 km) AND ÷Σamp normalization (sd 0.1 vs Minecraft's ~0.5). A plain gain
would NOT give continents. Prototype `docs/superpowers/specs/2026-09-20-worldgen-continents-prototype/proto.rs`
(#[path]-includes real noise.rs/spline.rs; findings doc alongside: `…-worldgen-continents-findings.md`):
MC-style weighting + retuned splines + peaks-and-valleys mountain term gated by inland/low-erosion +
1400 m domain-warped ridges → ~30% land, peaks +192 m, 5–10 km landmasses (15–25 km with continent
scale ×3). NOT applied to the crate; open user choices: continent scale, land %, peak height.

**IMPLEMENTED same day (user approved my recommended values; uncommitted on develop):** `noise.rs`
Minecraft weighting; `density.rs` `WarpedNoise` node; `pipeline.rs` new splines + mountain term
(`CONTINENT_SCALE` 3, `MOUNTAIN_OFFSET` 1.75, `RIDGE_WARP_M` 1400, asymmetric `PV_*` fold so wide ridges
are massifs not craters, erosion-gate floor for rolling lowlands, jagged 32–64 m noise, offshore drop-off
so shallow water doesn't read as sand in the factor-256 LOD ring). `WORLD_SEED = 0x00D5_C0DE_0206`
(extended `seed_scan`: flat spawn y=71, bay ~300 m W, y=259 peak ~1 km E). 256 km: 32.8% land, max 264 m.
All 37 worldgen lib tests incl. ignored + player_walk + `just smoke` pass; mountain region genesis cost ≈
spawn region, wire snapshot 1.8–2.6 vs 1.15 MiB. NOT done: client test binaries, live session, rock/snow
materials by slope/altitude, caves, Tectonic island split/regions. Gotcha hit: `cargo run --example X`
rebuilds only X — a stale `world_map` binary rendered pre-change terrain; use `just world-map`.

**2026-09-22 FEATURE_SCALE = 0.5** (user: "after increasing region size, everything got more spread out, I want world gen tighter"): terrain is region-size independent in meters — bigger regions only made the spread noticeable. New `FEATURE_SCALE` in pipeline.rs multiplies continent/erosion/ridge/warp wavelengths + RIDGE_WARP_M; heights and jagged detail untouched, so slopes ~×2 (4 km p50 0.12→0.32, p99 2.1→4.7). WORLD_SEED re-picked → 0x00D5_C0DE_007B (spawn y=77, sea N, peak 261 S). Sea fixture coast moved to world (5632, -2560). If user finds it too cliffy: scale HEIGHT_SCALE/MOUNTAIN_OFFSET down too.
