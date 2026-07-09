# Voxel Rendering — Post-Program Optimization Research (voxel.wiki survey)

**Date:** 2026-07-09
**Status:** Research notes (no decisions taken)
**Context:** Companion to `2026-07-08-voxel-rendering-optimization-design.md`. That program's
phases 1–2 (binary mesher + packed 8-byte vertices) are merged; phases 3–4 (visibility
culling, LOD) are planned. This survey mined https://voxel.wiki/categories/rendering/ (plus
primary sources its articles cite) for techniques applicable *after* the program completes.
All claims below survived 3-vote adversarial verification against primary sources (WebGPU
spec text, gpuweb issues, reference-implementation C++ source, godot_voxel issue tracker);
two stronger phrasings of the vertex-pulling claims were refuted and are noted.

## Ranked candidates

### 1. Per-quad SSBO vertex pulling ("face pulling") — top candidate

Replace the current 8-byte-per-vertex + index-buffer format with **one bit-packed record per
greedy quad** in a read-only storage buffer; the vertex shader reconstructs each corner from
the quad record plus `@builtin(vertex_index)`.

- **Win:** memory/bandwidth, ~4–5× past phase 2. Today: ~32 B/quad (4 × 8-byte vertices) +
  index data. Target: ~8 B/quad — our payload (x/y/z/face/w/h/layer/4×AO) is ≈57 bits.
  The cgerikj v2 reference does exactly this (8 B/quad: 6-bit x/y/z/w/h + 8-bit type) at
  50–200 µs/chunk mesh time, proving it composes with binary greedy meshing at
  editable-terrain remesh budgets.
- **Speed:** footprint win only, *not* shading speed. SSBO fetch is "roughly as fast" as
  fixed-function VBO fetch (Kitware 2025, RTX A5000: indexed pulling 0.89–0.98× classic-VBO
  GPU time; **non-indexed pulling measured slower** — 0.31 ms vs 0.19 ms — from lost vertex
  reuse). The stronger claim "no inherent speed penalty" was refuted 0–3. Keep a
  vertex-reuse-aware layout in mind.
- **Browser/WebGPU:** ✅ core WebGPU explicitly permits read-only storage buffers in vertex
  shaders (`maxStorageBuffersInVertexStage` defaults to 8; gpuweb#4132 settled the WGSL
  prose). ⚠️ WebGPU **Compatibility Mode** defaults that limit to **0** (older Mali/GLES 3.1
  hardware). Compat mode is opt-in (`featureLevel: "compatibility"`), so this only binds if
  we ever chase that hardware — then we need a fallback or a hard core-mode requirement.
- **Cost/complexity:** shader-side unpacking; stepping outside Bevy's fixed vertex-attribute
  pipeline (custom vertex stage with no vertex buffers, including redoing the shipped custom
  prepass vertex path). **Open question:** whether Bevy 0.18's Material pipeline can express
  a no-vertex-buffer draw cleanly or this needs a bespoke `RenderCommand`/render node
  (superdump/bevy-vertex-pulling is the reference to re-check against 0.18). This is the
  same trade the phase-2 design deferred deliberately — the spec's "documented future
  option if profiling demands" now has verified numbers behind it.

### 2. Pooled chunk mesh memory → indirect multidraw (native-only track)

Self-managed pooled GPU buffer for all chunk meshes (instead of one allocation per Bevy
`Mesh` asset), feeding a GPU-resident buffer of draw commands executed in one
multi-draw-indirect call.

- **Win:** CPU/driver draw-submission overhead — "nearly eliminates" per-draw validation
  cost (wiki, citing NVIDIA AZDO). Only pays off when draw-call-bound, not fill-bound.
- **Browser/WebGPU:** ❌ `MULTI_DRAW_INDIRECT` is native-only in wgpu; browser WebGPU has no
  baseline equivalent (only Chrome's `chromium-experimental-multi-draw-indirect` behind an
  unsafe flag; no Firefox/Safari as of 2026-07). wasm32 fallback: per-bucket instancing or
  multiple `drawIndirect` calls.
- **Caveats (why rated medium):** Bevy 0.18 already batches and (native, Vulkan) has
  GPU-driven rendering — the marginal win vs the wiki's raw-GL framing is smaller. Pooling
  also forfeits the mesh-allocator benefits (PR #17793 no-relocation) that phase 2 chose to
  keep. Natural pairing: the pooled SSBO of quad records from candidate 1 serves both.

### 3. Orientation-bucket backface culling (named future work — now concretized)

The cgerikj demo implements it: six per-face-direction index ranges per chunk
(`faceDrawCommands[6]`); a bucket's draw command is appended to the indirect buffer only
when the camera can see that orientation (e.g. top faces only when camera y ≥ chunk y).

- **Win is bounded:** hardware already back-face culls before raster — this saves only
  upstream vertex fetch/shading (up to ~50% of quad vertices) and submission, never raster
  cost. zeux's "Fine-grained Backface Culling" is the deep reference (the wiki's own
  backface-culling page is an explicit stub).
- **Coupling:** in the reference it rides on multidraw (candidate 2), inheriting the browser
  gap; a browser variant issues up to 3–6 ordinary draws per chunk instead of 1.
  Verdict unchanged from the design spec: stays deferred unless we go down the
  pooled-buffer/multidraw road natively.

### 4. Voxel light engine (named future work — scope now bounded)

The wiki's baseline is **BFS flood-fill lighting** (Minecraft-class: propagate light values
from emitting voxels through the grid; 0fps voxel-lighting article is the lineage we already
use for AO). Fits us well: engine-agnostic per-voxel data, incremental on edits, composes
with the baked-AO vertex path.

- **Explicit bound (wiki, verbatim-verified):** flood-fill "doesn't actually give you
  shadows. Nor does it allow for diagonal/moving sunlight." Escalation ladder if we want
  more: grid-based visibility → LPV → radiance cascades → voxel cone tracing → path tracing.
- **Composition problem to solve at design time:** the packed quad record is near-full
  (~57 bits); per-vertex light forces a wider record, a separate light buffer/texture, or
  per-face light sampling in the fragment shader.
- **Rollback constraint:** light values stay out of hashed sim state only if lighting is
  derived client-side — same rule as AO/visibility.

### 5. GPU compute culling — mostly redundant here

Wiki: culling "can also (and perhaps should)" move to compute (vkguide: >1M objects
<0.5 ms). For us the residue is narrow: Bevy's HZB occlusion culling (planned, phase 3) is
already GPU-compute two-phase; the remaining delta — GPU frustum/coarse culling of chunk
draw commands — only pays with GPU-driven submission, i.e. it belongs to the native
pooled-buffer track (candidate 2). The Tommo cave-culling BFS is **not** a compute
candidate: inherently sequential CPU graph traversal.

## Cross-cutting warning for phase 4 (LOD)

**Skirts are an imperfect crack-hider.** godot_voxel's maintainer reports holes at
differing-LOD borders "can still show up" despite skirts (too-short skirts, adjacent octrees
subdividing differently); the issue (Zylann/godot_voxel#63) was only closed after replacing
skirts with Transvoxel transition meshes. That evidence is from *smooth isosurface* terrain
(2019–2020) and partly traced to a separate LOD-data bug — axis-aligned block faces may make
skirts watertight in practice — but the phase-4 design should budget for a transition-mesh
or overdraw fallback if holes appear. (Matches existing project posture: defer until
observed, per the multi-scale-collider seams precedent.)

## Not-candidates / stubs encountered

- voxel.wiki **backface-culling** and **level-of-detail** pages are explicit stubs — no
  techniques, numbers, or API guidance beyond pointers (zeux for backface culling).
- **POP buffers** (0fps blocky-LOD): single mesh buffer serving every LOD level via
  power-of-two vertex rounding + coarse-to-fine primitive ordering. Noted as an alternative
  to multi-mesh LOD tiers, but it constrains the vertex format and doesn't obviously beat
  the planned downsample-and-remesh for our 3-tier design. Parked.
- **Exile-style per-quad instancing** (thenumb.at): a 4-vertex triangle strip instanced once
  per face — same destination as candidate 1 by a different API route; instancing small
  meshes has known per-instance overhead, so the SSBO pull formulation is preferred.

## Suggested sequencing (post-program)

1. **Face pulling (candidate 1)** — biggest verified win, browser-compatible, and the
   Bevy-0.18 pipeline question is the only real unknown → prototype that first.
2. **Flood-fill light engine (candidate 4)** — gameplay-visible; needs its own design doc
   (record-format decision + rollback boundary), as the program spec already says.
3. **Pooled buffers + multidraw + orientation buckets (candidates 2/3/5)** — one composed
   native-only track; take it up only if profiling shows draw-submission bound after
   phases 3–4.

## Primary sources

voxel.wiki: /wiki/vertex-pulling · /wiki/face-pulling · /wiki/multidraw · /wiki/rendering ·
/wiki/backface-culling (stub) · /wiki/level-of-detail (stub) — github.com/cgerikj/binary-greedy-meshing
(v2 demo source verified) — gpuweb#4132 + compatibility-mode proposal + spec text —
webgpufundamentals compat-mode lesson — Kitware vertex-pulling benchmark (2025) —
ktstephano programmable-vertex-pulling — 0fps voxel-lighting / blocky-LOD / AO —
seedofandromeda flood-fill lighting — thenumb.at Voxel-Meshing-in-Exile —
nickmcd.me high-performance-voxel-engine — Zylann/godot_voxel#63 — zeux.io fine-grained
backface culling — vkguide compute culling.
