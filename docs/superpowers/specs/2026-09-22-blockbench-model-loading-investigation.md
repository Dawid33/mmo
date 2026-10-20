# Loading Blockbench models — investigation (2026-09-22)

Status: **Option B BUILT 2026-10-20** — `crates/bbmodel` (parser + mesher, engine-free,
tested against MIT Blockbench files from hephaestus-engine) and the client's
`renderer/model.rs` (`.bbmodel` Bevy asset loader + `BbModel` spawn component),
`just model-view` to look at one. Built differently from the sketch below: its own crate
(not in `game`), Bevy's asset server (hot reload; works on wasm) instead of
`include_bytes!`, all faces double-sided, mesh elements supported. Not built yet:
animations, models→`ChiselData`. Original status: investigation only. The worked example is the slime
(`crates/game/src/slime.rs`, spec `2026-09-22-slime-entity-investigation.md`).
The slime entity itself is being built in parallel, so this doc only covers
getting a Blockbench model on screen. It deliberately does not touch
`slime.rs` or `renderer/avatar.rs`.

Blockbench facts were checked against its source: JannisX11/blockbench at
commit e2ede08 (v5.2.1, 2026-09-21). File paths below are in that repo unless
they start with `crates/`.

## TL;DR

- **Units line up for free.** Blockbench works in pixels, 16 per block. Here
  1 block = `BLOCK_VOXELS` = 16 voxels = 16 world units = 1 m
  (`crates/game/src/block.rs:15`, `movement::UNITS_PER_METER`). So
  **1 Blockbench pixel = 1 world unit = 1 voxel**, and model coordinates carry
  over with no scaling.
- **Nothing can load a model today.**
  - Bevy is built without `bevy_gltf`, `bevy_scene`, `bevy_animation` and
    `png`, so the `.glb` files already in `crates/client/assets/` are dead
    weight.
  - Block textures bypass the asset server entirely: `include_bytes!` plus
    the `image` crate (`crates/client/src/renderer/mod.rs:208-232`).
  - Avatars are one hard-coded capsule per `EntityKind`
    (`crates/client/src/renderer/avatar.rs`).
- **Recommendation: a small native `.bbmodel` loader**, not the glTF route.
  - It reads the Blockbench project file directly: cube elements, the group
    hierarchy, and the embedded PNG.
  - It builds one Bevy `Mesh` per group with a nearest-sampled
    `StandardMaterial`.
  - It follows the house `include_bytes!` idiom.
  - It needs no new Bevy features, and every dependency is already in the
    lockfile (`serde_json`, `base64`, `image` with PNG).
  - Estimate: about 400–600 lines for static models; animation is v2.
- **The glTF route works but fights us in three places.** Each would need a
  workaround on every model:
  - The export is 1/16 scale.
  - Translucency is forced to alpha *cutout*, which breaks the slime's
    see-through outer shell.
  - It needs about four new Bevy features across native and wasm.
- **Bonus: models as blocks.** Blockbench cubes on integer pixels are exactly
  the axis-aligned voxel boxes `ChiselData` already stores
  (`crates/game/src/block.rs:112`). The same parser could turn a model into
  placeable chiseled blocks such as furniture or decorations. That part can
  live in `game`, because it needs no Bevy.

## The `.bbmodel` format (what a loader must read)

Blockbench 5.x writes `meta.format_version = "5.0"` (`js/formats/bbmodel.js:6`).

- **Top level:**
  - `meta{format_version, model_format, box_uv}`
  - `resolution{width,height}`, the UV space when textures don't set their own
  - `elements[]`, `groups[]`, `outliner`, `textures[]`, `animations[]`
- **Groups (5.0+):** a flat `groups[]` of `{name, uuid, origin, rotation, …}`.
  - `outliner` is a tree of `{uuid, children:[…]}`, and elements appear in it
    as bare uuid strings.
  - **4.x files** put full group objects inline in `outliner` and have no
    `groups[]`. Blockbench still reads those (an outliner item with a `name`
    is a legacy group), so a loader should accept both.
- **Cube:**
  - `from`, `to` (pixels, absolute model space)
  - `origin`: the pivot, absolute
  - `rotation`: Euler degrees, omitted when zero
  - `inflate`, `box_uv`, `uv_offset`, `mirror_uv`
  - `faces{north,south,east,west,up,down}`, each
    `{uv:[x1,y1,x2,y2], texture:<index|null>, rotation:0|90|180|270}`
  - `texture: null` means the face is absent.
- **Box UV is not stored per face.** It is computed from `uv_offset` and the
  cube size (x,y,z) (`js/outliner/cube.js:1302-1345`):

  | face  | uv origin     | size   |
  |-------|---------------|--------|
  | east  | (0, z)        | (z, y) |
  | north | (z, z)        | (x, y) |
  | west  | (z+x, z)      | (z, y) |
  | south | (2z+x, z)     | (x, y) |
  | up    | (z+x, z) − … | (−x, −z), i.e. flipped |
  | down  | (z+2x, 0)     | (−x, z) |

  `mirror_uv` flips the faces and swaps east and west. Inflate grows the
  geometry but not the UVs. In practice Blockbench also writes the resolved
  per-face `uv` into `faces` for box-UV cubes. Reading `faces[*].uv` first
  and falling back to the table above covers both cases. That the resolved
  UVs are always present is not verified, so the test should cover it.
- **Axes:**
  - +Y is up. `north` is −Z (the face at `from.z`), south is +Z, east is +X.
  - Rotation order is three.js `'ZYX'` for every format, so X is applied
    first (`js/formats/format.ts:717`).
  - Pivots are absolute; child groups become parent-relative at runtime.
- **Textures:**
  - `source` is a `data:image/png;base64,…` URL when embedded
    (`internal:true`).
  - `uv_width`/`uv_height` apply only for per-texture UV size; otherwise
    `resolution` is the UV space.
- **Animations:**
  - `animators{<group uuid>: {keyframes:[{channel: rotation|position|scale,
    time, interpolation: linear|catmullrom|bezier|step,
    data_points:[{x,y,z}]}]}}`.
  - Data points are **molang**: a number *or* an expression string.
  - Two data points on one keyframe mark a discontinuity (pre/post values).
  - **5.0 breaking change:** files older than 5.0 get position.x, rotation.x
    and rotation.y negated when loaded (`bbmodel.js:72-100`). A loader that
    reads animations must do the same.

## Option A — Blockbench → glTF → Bevy's glTF loader

Mechanics (`js/formats/standards/gltf.js` and the Blockbench-forked
`js/lib/GLTFExporter.js`):

- **Scale:** positions are divided by `model_export_scale`, which defaults to
  16, so 1 glTF unit = 1 block. Here that is 1/16 of a world unit.
  - Workaround: export with scale 1, or scale every scene root by 16.
- **Textures:** embedded PNGs, padded up to a power of two with UVs rescaled.
  The sampler is NEAREST/NEAREST, which is right for pixel art.
- **Materials:** Lambert with `alphaTest 0.05`, exported as
  **`alphaMode: MASK`, cutoff 0.05**. Every texel with any alpha becomes
  fully opaque. `doubleSided` is on by default.
  - The Minecraft slime's outer cube is a *translucent* shell over an opaque
    core, so it would render as a solid green box.
  - Workaround: patch the material after load
    (`AlphaMode::Blend` on a named material). Blend also brings sorting
    against the inner cube.
- **Hierarchy:** groups become nodes at their pivot; cubes become child nodes.
- **Animation:**
  - Rotation is written as quaternions.
  - Molang, catmullrom and bezier tracks are **resampled at 24 fps** with
    molang evaluated at export time, so any runtime-driven molang is baked
    to constants.
  - Output is LINEAR or STEP only.
- **Bevy cost:**
  - Features: `bevy_gltf`, which pulls in `bevy_scene`; `gltf_animation`,
    which pulls in `bevy_animation`; and `png`.
  - All of them compile for wasm, but each adds compile time and wasm bytes.
  - The runtime is `SceneRoot` plus `AnimationPlayer`/`AnimationGraph`.
- **Workflow cost:** there are two files per model, the `.bbmodel` source and
  the exported `.glb`. An exported file that lags behind its source is a bug
  that nothing catches.

Pros: no geometry code, and rigged animation for free.
Cons: the three workarounds above, plus the two-file workflow.

## Option B — native `.bbmodel` loader (recommended)

Shape (sketch):

```
crates/game/src/bbmodel.rs          Bevy-free: serde types, outliner walk (4.x + 5.0),
                                    box-UV resolution, cube → triangles in model px,
                                    cube → ChiselData (when every cube is on integer pixels
                                    and unrotated), bounds() for tests
crates/client/src/renderer/model.rs Bevy side: one Mesh per group (positions, normals, UVs),
                                    Transform hierarchy from pivots/rotations, texture via
                                    base64 → image → Image with ImageSampler::nearest(),
                                    StandardMaterial { perceptual_roughness: 1, metallic: 0 },
                                    AlphaMode per texture (Blend if any 0<a<255 texel, else Mask/Opaque)
```

- **Loading:**
  - `include_bytes!("../../../../assets/models/slime.bbmodel")`, as the
    block textures do, so native and wasm behave the same with no asset
    fetch.
  - Parse once into a `Resource` of shared `Handle<Mesh>` and
    `Handle<StandardMaterial>` per model. That reuses the
    `AvatarAssets`-style sharing that avatar.rs already tests.
- **Dependencies:**
  - `serde_json` is currently native-only in `crates/client/Cargo.toml`, so
    move it to the common table. It is pure Rust and fine on wasm.
  - Add `base64` (already in `Cargo.lock` at 0.22) and `image` (workspace,
    PNG feature on).
  - If the parser sits in `game`, `game` gains `serde_json`. Otherwise the
    client does the JSON parsing and hands `game` plain structs.
- **Geometry:**
  - Each cube gives 6 quads. Apply `inflate`, then the cube's own rotation
    about its `origin`, then express it relative to its group's pivot.
  - Skip `texture: null` faces. Honour face `rotation` by rotating the 4 UV
    corners.
  - Mesh elements (`type:"mesh"`) are v2. Log and skip them.
- **Rotations:** Euler degrees in 'ZYX' order become
  `Quat::from_euler(EulerRot::ZYX, z, y, x)`. Pin this with a test on a
  model that has a known rotated cube.
- **Animation (v2):**
  - Numeric keyframes with linear or step interpolation give a small sampler
    over each group's `Transform`. Catmull-Rom is about 10 lines.
  - Molang strings: reject them with a warning in v1, or evaluate a tiny
    subset later (`query.anim_time`, `math.sin`, and so on).
  - The slime needs **no authored animation**. Its squish is the
    Minecraft-style spring already specified as render-only
    (`slime-entity-investigation.md`), which is a non-uniform scale on the
    model root.

Pros:
- One source file.
- Exact pixel = unit mapping.
- Per-texture alpha mode chosen by us.
- Zero new Bevy features.
- The same parse feeds `ChiselData` and test assertions.

Cons:
- The code is ours to maintain: cube triangulation, box UV, and animation
  later.
- The format moves: 4.x→5.0 changed groups and animation axes. Mitigate by
  pinning fixture files in tests, one 4.x and one 5.0.

Existing crates: `bbmodel-rs` 0.1.0, published 2026-07, 31 downloads, 0
stars, too new to depend on. `bevy-animator` targets Bevy 0.13 and is
abandoned. No Bedrock geometry crate exists. Writing our own is the realistic
option either way.

## Option C — Bedrock `.geo.json`

Bones with `pivot` and `rotation`; cubes with `origin`, `size`, and `uv`
given either as a box offset or per face. Coordinates are absolute pixels.
**X is mirrored relative to Blockbench**, which negates pivot.x, sets
`from.x = −(origin.x+size.x)`, and negates rotation X and Y
(`js/formats/bedrock/bedrock.js:684-700`).

It is simpler than `.bbmodel` and covers the huge corpus of Bedrock entity
models, but it is an export rather than the editable source. **Not
recommended as primary.** It is a cheap second front end to the same mesher
if we ever want Bedrock assets.

## The slime as the worked example

Mojang's Bedrock geometry (`bedrock-samples`: `models/entity/slime.geo.json`
and `slime_armor.geo.json`) uses a 64×32 texture, box UV, and all pivots at
[0,24,0]:

| part       | origin (px)    | size | uv     |
|------------|----------------|------|--------|
| outer cube | [-4, 0, -4]    | 8    | [0,0]  |
| inner cube | [-3, 1, -3]    | 6    | [0,16] |
| eye 0      | [-3.3, 4, -3.5]| 2    | [32,0] |
| eye 1      | [1.3, 4, -3.5] | 2    | [32,4] |
| mouth      | [0, 2, -3.5]   | 1    | [32,8] |

What that means here:

- **Size.**
  - The model is 8 px = 8 world units = 0.5 m.
  - The sim body is `SLIME_EDGE_M = 0.52` m ≈ 8.3 units, Minecraft's hitbox,
    which is slightly larger than its model.
  - Keep the sim constant authoritative. Add a client test asserting the
    model's `bounds()` stays within a small tolerance of it. The sim must
    **never** read the model file, because a re-export would then change
    physics and determinism.
  - Size N renders as a uniform ×N scale, as in Minecraft.
- **Two alpha modes in one model.**
  - The outer shell is translucent (`Blend`); the core, eyes and mouth are
    opaque.
  - With one texture that means either two materials split by face, or
    per-texel alpha with `Blend` on the shell group only. Option B does this
    naturally by giving each group its own material. glTF would need the
    post-load patch.
- **Eyes at x = −3.3 and 1.3.**
  - These are the one off-grid feature. They are fine for a render mesh, but
    this model is **not** a `ChiselData` candidate.
  - Models meant to become blocks should stay on integer pixels. The
    converter should refuse, rather than round, any cube that isn't.
- **Facing.** The face is on north (−Z). Check which way `Slime.yaw = 0`
  points in the sim, and fold any correction into the model's root
  rotation. Don't touch the sim.
- **Where it plugs in.**
  - `attach_avatars` gets an `EntityKind::Slime` arm that spawns the model as
    a **child entity**. The squish scale then goes on the child, never on
    the interpolated sim `Transform`.
  - That arm belongs to whoever is building the slime; the loader is
    independent of it.
- **Texture licensing.** Mojang's slime texture can't ship. Author an
  original 64×32 texture in Blockbench. The geometry proportions are generic
  and fine to use.

## Test corpus: complex mobs, and what they will break

`target/test-models/` holds 53 real files: 31 `.bbmodel`, 13 `.geo.json` and
9 `.animation.json`. It is **local-only and gitignored**, because most
sources are All Rights Reserved or have no stated licence. `MANIFEST.tsv`
records each file's repo, ref, path, licence and why it was picked. Only
MIT, Apache or CC0 files could ever become checked-in fixtures:
hephaestus-engine, WorldSeedEntityEngine, microsoft/minecraft-samples,
Changed-Minecraft-Mod and piratenation-art.

Run `just model-census [paths]` (`scripts/bbmodel_census.py`). It prints
per-file counts (groups, depth, cubes, meshes, textures, animations,
keyframes, molang expressions, texture sizes) and, for each feature, the
files that use it.

Picks worth knowing:

| file | why |
|---|---|
| `vengi/loy_s_goodies_female_template{,_5_0_3}.bbmodel` | **the same 69-group, depth-9 model saved in 4.5 and in 5.0**: a direct 4.x↔5.0 equivalence test (inline outliner vs `groups[]`, animation axis flip) |
| `hephaestus-engine/correct_{x,y,z}_axis_rotation_test.bbmodel` | one rotation axis each, with animations: pins the Euler order and sign |
| `CrittersAndCompanions/jumping_spider.bbmodel` | 8 textures (skin variants), 459 keyframes, v5 |
| `CrittersAndCompanions/red_panda.bbmodel` | depth 7, molang in keyframes |
| `CrittersAndCompanions/baby_roly_poly.bbmodel` | animated texture, key rotations >180° apart |
| `Malum-Mod/evangelist.bbmodel` | 14 animations, 1152 keyframes: bezier + catmullrom + step, effect tracks, multi-axis and odd-angle cube rotation |
| `Malum-Mod/{magehand,wands}.bbmodel`, `vengi/blockbench_meshtypes.bbmodel`, `piratenation-art/trophy_pvp.bbmodel` | mesh elements (tris and quads) |
| `Clockwork/coal_burner_compiled.bbmodel` | 16×320 animated texture, emissive texture, absent faces |
| `FiguraBlueArchiveCrafters/main.bbmodel` | a Figura avatar: 5 textures, one of them 34×17 (not a power of two), face UV rotation, bezier/step |
| `Galacticraft/moon_villager`, `undergarden/muncher` | 3.x-era mobs |
| `bedrock-samples/{warden,ender_dragon,sniffer,breeze,camel,frog}` | complex Bedrock mobs + animations. The dragon has 130 molang expressions, breeze has several geometries, camel/sniffer have pre/post keys, locators |

What the census says a loader will meet, with counts out of the 53 files:

- **Off-grid geometry is the norm for mobs.** 33 files have fractional cube
  bounds, 11 inflate, 8 odd-angle rotations and 5 multi-axis rotations.
  - **Model → `ChiselData` is a props/furniture feature, never a mob
    feature.** The converter must refuse off-grid input rather than round it.
  - The render mesher must take arbitrary floats and full 3-axis rotations,
    not the Java-block single-axis 22.5° steps.
- **Zero-thickness cubes: 21 files.** Mob modellers use planes for ears,
  whiskers, wings and fins. Faces must render **double-sided**, and the two
  faces of a plane share a position, so emit one quad per direction and don't
  cull by winding.
- **Both UV modes, even within one project.** Box UV appears in 29 files and
  per-face UV in 12.
  - `box_uv` is per cube; the `meta.box_uv` value is only the default.
  - Mirror appears in 15 files.
  - `uv_width`/`uv_height` ≠ pixel size in 2 files. These are
    high-resolution textures, so UVs must be scaled by the UV size, not the
    image size.
- **Multiple textures: 12 files.** Some are variants (the jumping spider's 8
  skins, where only one is used at a time), and some are per-face (one file
  mixes textures inside one cube).
  - The loader needs a texture-index → material map per face, not one
    material per model.
  - Variants suggest "model + skin" as the asset unit.
- **Textures:**
  - 4 are not a power of two; the sampler must clamp and not assume mips.
  - 2 are animated, with frames stacked vertically; show frame 0 in v1.
  - 2 are emissive.
  - Only 1 has partial alpha. Most "translucent" looks are really cutout,
    so `Mask` is a fine default, with `Blend` only when partial alpha is
    detected.
  - None use path-only textures, but 4.x can reference external files.
    Treat that as an error.
- **Hidden or non-exported elements: 3 files.** Skip anything with
  `visibility:false` or `export:false`, or reference geometry renders in game.
- **Hierarchy depth up to 9.** Transforms must be composed in a real parent
  chain, not flattened by name.
- **Animation (30 of 53 files):**
  - Catmull-Rom in 11 files, bezier in 4, step in 2.
  - Molang in 8, most heavily in the Bedrock files. Counting identifiers
    across the corpus:
    - `math.cos` 109 and `query.anim_time` 107, which a pure evaluator
      handles.
    - About 130 reads of **entity-driven `variable.*` values**: the dragon's
      `tail_*`/`neck_*`, `flap_time`, `attack_time`,
      `rotation_factor_translate`. The game sets these every frame.
    - A molang evaluator is therefore also an interface: the entity's
      renderer must feed named variables into the animation. That can be
      the hook for the slime's squish, if slime squish ever moves from code
      into data.
    - `math.random` appears 4 times; it is nondeterministic, but render-only
      so harmless.
  - Pre/post keys in 2, and effect tracks (sound, particle, timeline) in 5.
    Ignore effect tracks in v1.
  - Rotation keys more than 180° apart in 3. **Interpolate Euler
    components, then convert**, as Blockbench does. Slerping between key
    quaternions takes the short way round and spins bones backwards.
  - Conclusion: a v1 that supports only numeric linear keys would load but
    mis-play about half the animated files. The minimum useful set is
    linear + step + Catmull-Rom + numeric pre/post. Bezier and molang are a
    separate, larger step; molang needs an expression evaluator.
- **Not seen anywhere:** faces with more than 4 vertices (Blockbench mesh
  faces are tris or quads), path-only textures, dangling animators, Bedrock
  `binding`, Bedrock `poly_mesh`.
- **Mesh elements (4 files) are props, not mobs.** No mob in the corpus uses
  them, which supports skipping them in v1 with a warning.

## Open questions

1. Is Option B (the native loader) the direction, or is the glTF pipeline
   preferred for its free rigging and animation?
2. Should the pure parser live in `game`? That would enable `ChiselData`
   import and sim-side bounds checks, but `game` gains `serde_json`. The
   alternative keeps it all client-side.
3. Asset home: `assets/models/*.bbmodel` embedded via `include_bytes!`,
   matching block textures? Or start using Bevy's asset server now, which
   brings hot reload but async loading on wasm?
4. Animation scope for v1: none (squish is procedural), or numeric keyframes
   for future creatures?

## Suggested first slice (after answers)

1. `bbmodel.rs` parse plus cube mesher for the 5.0 format with box and
   per-face UV. Fixtures: an original slime `.bbmodel`, and a one-cube
   rotated model to pin the Euler order.
2. The client `ModelAssets` resource and a spawn helper that returns a child
   entity with the group hierarchy.
3. Tests, headless under `MinimalPlugins`: the parse round trip; a UV
   snapshot for a known cube; `bounds()` against `SLIME_EDGE_M`.
4. Hand over to the slime work: the `EntityKind::Slime { size }` arm in avatar.rs (which already owns the
   render-only squish) swaps its placeholder mesh for the model child.
