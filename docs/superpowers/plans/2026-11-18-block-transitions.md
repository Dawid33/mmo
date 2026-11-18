# Block Transitions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Break up the block grid and soften material boundaries in three steps, each visible on its own.
- **Part A:** random texture variants, 90° rotations on top faces, and low-frequency macro shading.
- **Part B:** climate tint from the world map.
- **Part C:** overlay fringe quads where a higher-priority ground material meets a lower one, plus "connected sides" for grass.

**Architecture:** the locked quad record never changes.
- **Variants, rotation and macro noise** are chosen in the fragment shader from the integer block cell. Render space is absolute world voxels (`RegionCoords::world_offset` = `x * REGION_SIZE`, `game/src/protocol.rs:127`), so the cell is `floor((world_position − ½·n) / 16)`, as `overlay_bits` does.
- **A per-layer `LayerInfo` table** replaces today's `top_layers` lookup in both terrain paths.
- **Tint** is a small per-region texture built from `WorldMap` tiles.
- **Fringes** are extra cut-out records emitted by the mesher. Their `layer` is ≥ `FRINGE_LAYER_BASE` (1024) and addresses a **second** texture array, and the vertex shader lifts them a sixteenth of a voxel.

**Tech Stack:** Rust (game, client/Bevy 0.19.1), WGSL (`voxel_pull.wgsl`, `voxel_pull_depth.wgsl`, `voxel_texture.wgsl`), Python + Pillow (`tools/textures`).

**Spec:** `docs/superpowers/specs/2026-11-18-block-visuals-research.md` §2 (and §4's table rows "variants", "tint", "transition quads").

**Starts after:** `docs/superpowers/plans/2026-11-18-oriented-blocks.md` (plan 1). Plan 1 makes the following true, and this plan consumes it and redefines none of it:
- `BlockTextureLayers(pub BTreeMap<BlockId, FaceLayers>)` with `pub struct FaceLayers(pub [u32; 6])` indexed by `Face::index()`.
- `pack::layer_for_face(faces: FaceLayers, face: Face) -> u32`.
- `build_layers` appends rotated copies of side layers and still returns `(Vec<RgbaImage>, BlockTextureLayers)`.

## Global Constraints

- The record layout is LOCKED (`meshing/records.rs:4-5`). This plan changes no bit. Fringe records are ordinary cut-out records whose 11-bit `layer` is ≥ 1024.
- **WebGL2:**
  - It caps every texture array at **256 layers** (wgpu `Limits::downlevel_webgl2_defaults().max_texture_array_layers`). The main block array and the fringe array must each stay ≤ `WEBGL2_MAX_ARRAY_LAYERS = 256`; a test pins both for the shipped manifest.
  - It has no vertex-stage storage buffers. The per-chunk path (`voxel_texture.wgsl`) reads tables from textures (`textureLoad`); the pull path reads storage buffers.
- **Identical shader blocks:** every WGSL helper that must agree between shaders is wrapped in `// BEGIN <name>` / `// END <name>` and pinned identical by a test, as `texture_position` is (`voxel_pull/pipeline.rs:729`).
- **Testing (CLAUDE.md):**
  - Game tests: `just test-one game "--lib <filter>"`.
  - **Client tests are a slow opt-in: ask the owner before each client test run** (`just test-one client "--lib <filter>"`).
  - Every task ends with `just smoke` (60 s; type-checks the client), and WGSL or wasm-touching tasks also run `just web-check`.
  - The visual check is the owner's: never launch `just run` / `just play` without asking.
- **Art:** every texture comes from deterministic Pillow scripts in `tools/textures/`. No paid generation. Every new PNG in `assets/blocks/` is added to `EMBEDDED_BLOCK_TEXTURES` (`renderer/mod.rs:373`); the test `embedded_block_textures_match_assets_dir` enforces it.
- `game` stays Bevy-free, and `game` registry types stay serde (they travel to compute workers in mesh jobs).
- Keep the justfile in sync: the new script gets a recipe.

## Review Focus

1. **The WebGL2 layer cap.** The shipped manifest's main array (182 today, plus plan 1's rotated layers, plus variants and masks) and its fringe array must each be ≤ 256. A browser would otherwise fail to create the texture. Test: `shipped_layers_fit_webgl2` (Task A2, extended in C2).
2. **Chiselled tops never get fringes.** Worldgen slopes are chiselled grass (`worldgen/src/slopes.rs`). A fringe quad over a staircase top would float. Test: `overlay_skips_partial_tops` (Task C3).
3. **Neighbours across a brick edge or corner** are read from the padded shell. A fringe that disappears at every 2-block brick boundary is the likely bug. Test: `overlay_reads_shell_neighbours` (Task C3).
4. **A world with no map** (procedural, offline) must look exactly as today: tint multiplier 1. Test: `tint_view_without_map_is_identity` (Task B2).
5. **The variant cell at large coordinates and on merged quads.** The cell comes from the face-inward point, never from a point on the boundary. Test: `block_cell_is_inside_the_face` (Task A3), mirrored by the WGSL `block_cell` block.

---

## File map

| File | Responsibility |
|---|---|
| `crates/game/src/registry.rs` | `TextureProps`, `TintClass` (A1); `BlendDef`, `blend_group`, `connect_sides`, `BlendTable` (C1) |
| `crates/game/src/block_props.rs` | `connect_sides` set (C5) |
| `crates/client/src/renderer/block_textures.rs` | `LayerInfo`, `BuiltLayers`, variant groups (A2); colormap lookup, `tint_texture_data` (B2); fringe compositing, `FringeTable`, `fringe_case` (C2) |
| `crates/client/src/renderer/block_variants.rs` (new) | Rust mirror of the WGSL `block_hash` / `block_cell` / variant and rotation choice (A3) |
| `crates/client/src/renderer/tint.rs` (new) | `TintViewUniform`, the tint texture resource and its rebuild from `WorldMapState` (B2, B3) |
| `crates/client/src/renderer/meshing/overlay.rs` (new) | `OverlayQuad`, `overlay_quads` (C3) |
| `crates/client/src/renderer/meshing/{mod.rs,records.rs,emit.rs,queue.rs,binary.rs}` | carry overlays into records and meshes (C3); connected sides (C5) |
| `crates/client/src/renderer/voxel_pull/{pipeline.rs,prepare.rs,arena.rs,mod.rs}` | bindings 4 (now `layer_info`) and 8–10 (A3, B3, C4) |
| `crates/client/src/renderer/voxel_material.rs`, `renderer/mod.rs` | per-chunk bindings 103 (now `layer_info`) and 104–106 (A4, B3, C4) |
| `assets/shaders/voxel_pull.wgsl`, `voxel_pull_depth.wgsl`, `voxel_texture.wgsl` | the shader side of every part |
| `tools/textures/transitions.py` (new), `justfile` | variants, tint masks, colormaps, fringe masks |
| `assets/blocks/blocks.ron` | texture props (A5, B4), blend declarations (C5) |

---

## Part A — variants, rotations, macro shading

### Task A1: Texture properties in the manifest

**Files:**
- Modify: `crates/game/src/registry.rs` (`BlockManifest`, `BlockRegistry`, `RegistryError`)
- Test: `crates/game/src/registry.rs` `mod tests`

**Interfaces:**
- Produces:
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)] #[repr(u8)] pub enum TintClass { #[default] None = 0, Grass = 1, Foliage = 2 }`
  - `#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] pub struct TextureProps { pub path: String, #[serde(default)] pub variants: Vec<String>, #[serde(default)] pub rotate: bool, #[serde(default)] pub tint: TintClass, #[serde(default)] pub tint_mask: Option<String> }`
  - `BlockManifest` gains `#[serde(default)] textures: Vec<TextureProps>`.
  - `BlockRegistry` methods: `pub fn texture_props(&self, path: &str) -> Option<&TextureProps>` and `pub fn texture_props_iter(&self) -> impl Iterator<Item = &TextureProps>` (in manifest order).
  - `RegistryError` gains `DuplicateTextureProps(String)` and `TooManyVariants(String)`. More than `MAX_EXTRA_VARIANTS = 15` variants is refused (4 bits in `LayerInfo`).
- Properties belong to a texture **path**, not a block: a path shared by two blocks varies the same way in both.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn parses_texture_props() {
    let src = r#"(blocks:[(id:0,name:"air",textures:Untextured),(id:1,name:"grass",textures:All("g.png"))],
                 textures:[(path:"g.png",variants:["g1.png","g2.png"],rotate:true,tint:Grass,tint_mask:"gm.png")])"#;
    let reg = BlockRegistry::from_ron(src).unwrap();
    let p = reg.texture_props("g.png").unwrap();
    assert_eq!(p.variants, vec!["g1.png".to_string(), "g2.png".to_string()]);
    assert!(p.rotate);
    assert_eq!(p.tint, TintClass::Grass);
    assert_eq!(p.tint_mask.as_deref(), Some("gm.png"));
    assert!(reg.texture_props("other.png").is_none());
}
#[test]
fn texture_props_default_when_absent() { /* manifest with no `textures:` parses; texture_props_iter() is empty */ }
#[test]
fn rejects_duplicate_texture_props() { /* two entries with path "g.png" → Err(DuplicateTextureProps("g.png")) */ }
#[test]
fn rejects_too_many_variants() { /* 16 variants → Err(TooManyVariants("g.png")) */ }
```

- [ ] **Step 2:** `just test-one game "--lib registry::"`. Expected: FAIL (no `texture_props`).
- [ ] **Step 3:** Implement as in Interfaces. Add `Display` arms for the two errors.
- [ ] **Step 4:** `just test-one game "--lib registry::"`. Expected: PASS, including `parses_shipped_manifest`.
- [ ] **Step 5: Commit** `feat(registry): per-texture variants, rotation and tint in the block manifest`.

### Task A2: Layer info table and contiguous variant groups

**Files:**
- Modify: `crates/client/src/renderer/block_textures.rs` (`build_layers`, `side_to_top`, new `LayerInfo`, `BuiltLayers`)
- Modify: `crates/client/src/renderer/mod.rs` (the `build_layers` call around `:1105-1190`)
- Modify: `crates/client/src/renderer/voxel_pull/mod.rs:191` (rename the `BlockTopLayers` resource to `BlockLayerInfo`; holds `Vec<u32>`)
- Test: `block_textures.rs` tests

**Interfaces:**
- Consumes: `TextureProps`, `TintClass` (A1); plan 1's `FaceLayers`.
- Produces:
  - `pub const FRINGE_LAYER_BASE: u32 = 1024;`
  - `pub const WEBGL2_MAX_ARRAY_LAYERS: usize = 256;`
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)] pub struct LayerInfo { pub extra_variants: u8, pub rotatable: bool, pub tint: TintClass, pub tint_mask: u32, pub top: u32 }` with `pub fn pack(self) -> u32` and `pub fn unpack(v: u32) -> Self`. Bit layout (LOCKED to the WGSL `layer_info` block):

    | Bits | Field |
    |---|---|
    | 0..4 | `extra_variants` (0..=15) |
    | 4 | `rotatable` |
    | 5..7 | `tint` |
    | 7..18 | `tint_mask` layer (0 = whole texture tinted) |
    | 18..29 | `top` (the `side_to_top` value; identity by default) |

  - `pub struct BuiltLayers { pub images: Vec<RgbaImage>, pub layers: BlockTextureLayers, pub info: Vec<u32> }`. `info` is indexed by **record layer** and has length `FRINGE_LAYER_BASE as usize`; Part C extends it. Entries with no layer are `LayerInfo { top: i, ..Default::default() }.pack()`.
  - `build_layers(registry, load) -> BuiltLayers`.
- **Variant groups:** before blocks are interned, each `TextureProps` with variants interns `path` and then every variant path **contiguously** (base layer `b`, variants `b+1..=b+n`). Blocks interned afterwards dedup to `b`. A path shared with a non-varied texture gets the same group (properties are per path).
- `side_to_top()` stays the source for `top`. It now iterates `FaceLayers` (plan 1) and maps every side face layer to that block's +Y layer.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn layer_info_round_trips_and_bits_are_locked() {
    let i = LayerInfo { extra_variants: 3, rotatable: true, tint: TintClass::Foliage, tint_mask: 7, top: 1000 };
    assert_eq!(LayerInfo::unpack(i.pack()), i);
    assert_eq!(i.pack(), 3 | 1 << 4 | 2 << 5 | 7 << 7 | 1000 << 18);
}
#[test]
fn variant_groups_are_contiguous() {
    // registry: block "g" All("g.png"); textures: [(path:"g.png", variants:["g1.png","g2.png"], rotate:true)]
    let built = build_layers(&reg, |_| Some(RgbaImage::new(16, 16)));
    let base = built.layers.0[&id_g].0[Face::PosY.index()];
    let info = LayerInfo::unpack(built.info[base as usize]);
    assert_eq!((info.extra_variants, info.rotatable), (2, true));
    assert_eq!(built.images.len() as u32, base + 3); // base, g1, g2 are the last three, in order
}
#[test]
fn info_has_identity_top_for_untouched_layers() { /* built.info[0] unpacks to top == 0; len == FRINGE_LAYER_BASE */ }
#[test]
fn shipped_layers_fit_webgl2() {
    // the real blocks.ron + the embedded PNG loader: images.len() <= WEBGL2_MAX_ARRAY_LAYERS
}
```

- [ ] **Step 2: Ask the owner, then run** `just test-one client "--lib block_textures::"`. Expected: FAIL to compile.
- [ ] **Step 3:** Implement. In `renderer/mod.rs`, build the material's `top_layers` image (binding 103) from `built.info` with the existing `top_layers_image` (rename it to `layer_info_image`; same R32Uint `len × 1` format). The pull path's `BlockLayerInfo` gets `built.info`. Nothing reads the new bits yet.
- [ ] **Step 4: Ask the owner, then run** `just test-one client "--lib block_textures::"`. Then run `just smoke`. Expected: PASS.
- [ ] **Step 5: Commit** `feat(client): per-layer info table and contiguous variant groups`.

### Task A3: Variants, rotation and macro shading in the pull shaders

**Files:**
- Create: `crates/client/src/renderer/block_variants.rs` (Rust mirror + tests)
- Modify: `assets/shaders/voxel_pull.wgsl`, `assets/shaders/voxel_pull_depth.wgsl`
- Modify: `crates/client/src/renderer/voxel_pull/pipeline.rs` (binding 4 comment + tests), `voxel_pull/prepare.rs:255-278` and `voxel_pull/arena.rs:157-205` (the `top_layers` buffer becomes `layer_info`, filled from `BlockLayerInfo`)

**Interfaces:**
- Consumes: `LayerInfo` bits (A2).
- Produces:
  - Rust: `pub const SALT_VARIANT: u32 = 0x9E37_79B9; pub const SALT_ROTATE: u32 = 0x85EB_CA6B; pub const SALT_MACRO: u32 = 0xC2B2_AE35;`
  - Rust: `pub fn block_hash(cell: [i32; 3], salt: u32) -> u32`
  - Rust: `pub fn block_cell(world_position: [f32; 3], normal: [f32; 3]) -> [i32; 3]`
  - Rust: `pub fn variant_offset(cell: [i32; 3], extra: u32) -> u32` (0 when `extra == 0`, else `block_hash(cell, SALT_VARIANT) % (extra + 1)`)
  - Rust: `pub fn rotation(cell: [i32; 3]) -> u32` (`block_hash(cell, SALT_ROTATE) & 3`)
  - WGSL blocks, identical in all three terrain shaders: `block_hash`, `block_cell`, `layer_info`, `rotate_uv`. The helper `pick_texture` is identical in the two pull shaders; A4 copies it into `voxel_texture.wgsl`.
- `block_hash` (all arithmetic wrapping u32; the WGSL is the same text with `bitcast<u32>`):

```rust
pub fn block_hash(c: [i32; 3], salt: u32) -> u32 {
    let mut h = (c[0] as u32).wrapping_mul(0x8DA6_B343)
        ^ (c[1] as u32).wrapping_mul(0xD816_3841)
        ^ (c[2] as u32).wrapping_mul(0xCB1A_B31F)
        ^ salt;
    h ^= h >> 16; h = h.wrapping_mul(0x7FEB_352D);
    h ^= h >> 15; h = h.wrapping_mul(0x846C_A68B);
    h ^ (h >> 16)
}
```

- `block_cell` is `floor((p − 0.5·n) / BLOCK_VOXELS)` per axis. It uses the **geometric** normal (before the CUTOUT double-sided flip) or, under the cutaway cap, the cap position with n = +Y.
- `rotate_uv(uv, k)` rotates the in-block part about the block centre:
  - local = uv − floor(uv); k = 0: (u, v); 1: (1−v, u); 2: (1−u, 1−v); 3: (v, 1−u).
  - The gradients `ddx`, `ddy` turn by the same rotation without the offset: 1: (−g.v, g.u); 2: −g; 3: (g.v, −g.u).
- `pick_texture(base_layer, cell, n, uv, ddx, ddy) -> (layer, uv, ddx, ddy)`:
  - layer = base + `variant_offset`.
  - Rotation only when `rotatable` and `abs(n.y) > 0.5`.
  - Applied in the fragment stage after the cap choice and before `crisp_uv`. The riser fade's `top_layer(base)` reads `info.top` of the **base** layer and takes no variant.
  - Sampling stays `textureSampleGrad` with the rotated gradients.
- Macro shading (forward shader only; not the depth shader):
  - `const MACRO_CELL_BLOCKS: f32 = 8.0; const MACRO_AMPLITUDE: f32 = 0.06;`
  - `macro_shade(xz_blocks)` is bilinear value noise with smoothstep weights over lattice points `block_hash(vec3(ix, 0, iz), SALT_MACRO)`, mapped to `1 + MACRO_AMPLITUDE * (2·v − 1)` with `v = h / 2^32`.
  - It multiplies `tex.rgb` before lighting.
- The depth shader's CUTOUT variant calls the same `pick_texture`, so a leaf's alpha matches its colour.
- Binding 4 keeps its slot, type `array<u32>` and fragment-only visibility. `top_layer(l)` becomes `(layer_info[l] >> 18u) & 2047u`, with identity when `l` is out of range.

- [ ] **Step 1: Write the failing tests**

```rust
// block_variants.rs
#[test] fn variants_spread_evenly() { /* over cells x,z in 0..64: each of variant_offset(.., 3)'s 4 outcomes in 900..=1150 of 4096 */ }
#[test] fn rotation_spreads_evenly() { /* same bounds for rotation() */ }
#[test] fn block_cell_is_inside_the_face() {
    // a +Y face at y = 32.0 voxels of the block whose cell y is 1: block_cell([5.0, 32.0, 5.0], [0.,1.,0.]) == [0, 1, 0]
    // a -X face at x = -16.0: block_cell([-16.0, 3.0, 3.0], [-1.,0.,0.]) == [-1, 0, 0]
    // large coordinates: block_cell([16.0 * 900_000.0 + 0.25, 1.0, 1.0], [0.,1.,0.])[0] == 900_000
}
// voxel_pull/pipeline.rs
#[test] fn block_helpers_are_identical_in_all_terrain_shaders() {
    // for each name in ["block_hash", "block_cell", "layer_info", "rotate_uv"]: the BEGIN..END text is equal in
    // voxel_pull.wgsl, voxel_pull_depth.wgsl and voxel_texture.wgsl (A4 adds the third; until then assert the two pull shaders)
}
#[test] fn pick_texture_is_identical_in_both_pull_shaders() { /* BEGIN pick_texture block equal */ }
#[test] fn wgsl_block_hash_constants_match_rust() {
    // the BEGIN block_hash text contains "0x8da6b343u", "0xd8163841u", "0xcb1ab31fu", "0x7feb352du", "0x846ca68bu";
    // and each SALT_* constant appears in voxel_pull.wgsl as `{:#x}u`
}
```

- [ ] **Step 2: Ask the owner, then run** `just test-one client "--lib block_variants:: voxel_pull::pipeline::"`. Expected: FAIL.
- [ ] **Step 3:** Implement the Rust mirror and both shaders. The existing naga validation tests (`forward_shader_is_valid_wgsl_*`, `depth_shader_is_valid_wgsl_*`, `pipeline.rs:529-711`) must still pass in every variant (CUTAWAY, CUTOUT, XRAY).
- [ ] **Step 4: Ask the owner, then run** the same filter. Then run `just smoke`. Expected: PASS.
- [ ] **Step 5: Commit** `feat(client): per-block texture variants, top rotation and macro shading on the pulled path`.

### Task A4: The same on the per-chunk (WebGL2) path

**Files:**
- Modify: `assets/shaders/voxel_texture.wgsl` (binding 103 `top_layers` → `layer_info`, same `texture_2d<u32>`; fragment at `:285-310`)
- Modify: `crates/client/src/renderer/voxel_material.rs` (field `top_layers` → `layer_info`; doc comments)
- Test: `voxel_material.rs` tests; `voxel_pull/pipeline.rs` identity test (now covers three shaders)

**Interfaces:**
- Consumes: A3's blocks.
- `layer_info(l)` here is `textureLoad(layer_info, vec2<u32>(l, 0u), 0).r` (identity outside the width). The shared `layer_info` BEGIN block holds only the bit **decoding** (`fn info_extra(v: u32) -> u32` and so on), so it is identical across shaders; the fetch helper `fetch_layer_info(l)` is per shader.

- [ ] **Step 1: Write the failing tests**
  - `voxel_texture_shader_is_valid_wgsl()` in `voxel_material.rs`, following `pipeline.rs`'s harness (`:400-440`, naga parse + validate after imports). If `voxel_texture.wgsl`'s Bevy PBR imports cannot be composed there, report it and keep the identity test only.
  - Extend `block_helpers_are_identical_in_all_terrain_shaders` to include `voxel_texture.wgsl`.
- [ ] **Step 2: Ask the owner, then run** `just test-one client "--lib voxel_material:: voxel_pull::pipeline::"`. Expected: FAIL.
- [ ] **Step 3:** Implement: copy `pick_texture` and `macro_shade` into `voxel_texture.wgsl`'s fragment, with the cell from its `world_position` and geometric normal.
- [ ] **Step 4: Ask the owner, then run** the same filter. Then run `just smoke` and `just web-check`. Expected: PASS.
- [ ] **Step 5: Commit** `feat(client): texture variants and macro shading on the per-chunk path`.

### Task A5: Variant art and declarations

**Files:**
- Create: `tools/textures/transitions.py` (subcommand `variants`)
- Modify: `justfile` (recipe `transition-textures *args: python3 tools/textures/transitions.py {{args}}`, next to `block-textures`, with a comment like its neighbours'), `assets/blocks/blocks.ron` (`textures:` list), `renderer/mod.rs` `EMBEDDED_BLOCK_TEXTURES`
- Create: `assets/blocks/{grass_top,dirt,stone,sand,gravel}_v{1,2}.png`, `grass_top_v3.png`

**Interfaces:**
- `transitions.py variants` derives each variant from the base PNG deterministically, seeded by `(name, n)`:
  - re-scatter the speckle texels (swap ~30 % of texel pairs whose luminance differs by < 12 %);
  - ±4 % luminance jitter per texel.
- The silhouette and average colour stay the same, so mips match the base.
- Declarations:
  - `(path:"grass_top.png", variants:[…3], rotate:true)`
  - `dirt.png`, `stone.png`, `sand.png`, `gravel.png`: 2 variants each, `rotate:true`
  - every `*_leaves.png`: `rotate:true` (no variants)
  - Directional textures (bark, rings, `grass_side`, `hay_bale_top`) are **not** declared rotatable.

- [ ] **Step 1:** Run `just transition-textures variants`. Expected: 11 PNGs, all 16×16 RGBA; a second run leaves `git status` clean (deterministic).
- [ ] **Step 2:** Declare them in `blocks.ron` and `EMBEDDED_BLOCK_TEXTURES`.
- [ ] **Step 3: Ask the owner, then run** `just test-one client "--lib block_textures:: embedded_block_textures"` (includes `shipped_layers_fit_webgl2`) and `just test-one game "--lib registry::"`. Then run `just smoke`. Expected: PASS.
- [ ] **Step 4:** Ask the owner for a look (`just run`): ground no longer tiles visibly; leaves rotate; bark and grass sides unchanged.
- [ ] **Step 5: Commit** `feat(assets): ground texture variants and rotatable leaves`.

---

## Part B — climate tint

### Task B1: Colormaps and the grass-side tint mask

**Files:**
- Modify: `tools/textures/transitions.py` (subcommands `colormaps`, `tint-masks`), `EMBEDDED_BLOCK_TEXTURES`
- Create: `assets/blocks/colormap_grass.png`, `assets/blocks/colormap_foliage.png` (64×64 RGB), `assets/blocks/grass_side_tint.png` (16×16, red = tint weight)

**Interfaces:**
- **Colormap axes:**
  - x = temperature from `T_MIN = -20 °C` (column 0) to `T_MAX = 40 °C` (column 63);
  - y = precipitation from 0 mm/yr (row 0) to `P_MAX = 3000` mm/yr (row 63).
- **Values are relative multipliers:** the reference cell nearest `(T_REF = 12 °C, P_REF = 800 mm)` is exactly `(200, 200, 200)`, and 200 means ×1. The script also writes those constants into the PNG's text chunk for review. Gradients:
  - cold and dry → desaturated yellow-brown;
  - hot and wet → saturated deeper green;
  - foliage is darker and more blue-green than grass.
- `grass_side_tint.png` has red 255 on the rows `grass_side.png` paints green and 0 below, read from `grass_side.png` by hue.

- [ ] **Step 1:** Run `just transition-textures colormaps tint-masks` twice. Expected: the files exist, the second run is clean, and the reference texel is `(200,200,200)`. The script asserts the reference texel itself.
- [ ] **Step 2:** Run `just smoke` (embedded list compiles).
- [ ] **Step 3: Commit** `feat(assets): grass and foliage colormaps, grass-side tint mask`.

### Task B2: Tint data from the world map

**Files:**
- Create: `crates/client/src/renderer/tint.rs`
- Modify: `crates/client/src/renderer/block_textures.rs` (`colormap_lookup`), `renderer/mod.rs` (register the plugin and systems)
- Test: `tint.rs`

**Interfaces:**
- Consumes: `WorldMapState { map: Option<WorldMap> }` (`renderer/world_map.rs:14`); `TileSummary::{temperature_c, precip_mm}`; `WorldMap::{origin, width, height, tile}`.
- Produces:
  - `pub const T_MIN: f32 = -20.0; pub const T_MAX: f32 = 40.0; pub const P_MAX: f32 = 3000.0; pub const TINT_UNIT: f32 = 200.0 / 255.0;`
  - `pub fn colormap_lookup(map: &RgbImage, temperature_c: f32, precip_mm: f32) -> [u8; 3]` (nearest texel, clamped)
  - `pub fn tint_texture_data(map: &WorldMap, grass: &RgbImage, foliage: &RgbImage) -> (u32, u32, Vec<u8>)`: width = `map.width`, height = `2 * map.height`, RGBA8, row-major. Rows `0..h` hold grass and `h..2h` foliage; texel `(i, j)` is tile `(origin.x + i, origin.z + j)`. A missing tile (ocean outside the map) gets `[200, 200, 200, 255]`.
  - `#[derive(ShaderType, Clone, Copy, Default, PartialEq)] pub struct TintViewUniform { pub origin_size: IVec4 /* origin.x, origin.z, width, height */, pub region: Vec4 /* x = REGION_SIZE voxels, y = enabled 0.0|1.0 */ }`
  - `pub fn tint_view(map: Option<&WorldMap>) -> TintViewUniform`
  - Resources `TintTexture(Handle<Image>)` (Rgba8Unorm, not sRGB; a 1×2 white stand-in until a map arrives) and `TintView(TintViewUniform)`, rebuilt by `fn rebuild_tint(state: Res<WorldMapState>, …)` when `state` changes.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn tint_view_without_map_is_identity() { assert_eq!(tint_view(None).region.y, 0.0); }
#[test] fn reference_climate_is_unit() {
    let g = image::open("../../assets/blocks/colormap_grass.png").unwrap().to_rgb8();
    assert_eq!(colormap_lookup(&g, 12.0, 800.0), [200, 200, 200]);
}
#[test] fn tint_texture_layout() {
    // 2×1 map with tiles (t=12,p=800) and (t=-20,p=0): data len == 2*2*4;
    // texel (0,0) == [200,200,200,255]; texel (1,0) == colormap(-20,0)+[255]; texel (0,1) is foliage of tile 0
}
```

- [ ] **Step 2: Ask the owner, then run** `just test-one client "--lib renderer::tint::"`. Expected: FAIL.
- [ ] **Step 3:** Implement. Load the colormaps through the same loader as block PNGs (embedded on wasm).
- [ ] **Step 4: Ask the owner, then run** the same filter. Then run `just smoke`. Expected: PASS.
- [ ] **Step 5: Commit** `feat(client): climate tint texture from the world map`.

### Task B3: Tint in both shaders (near and far)

**Files:**
- Modify: `voxel_pull.wgsl` (bindings 9 `tint_map: texture_2d<f32>` and 10 `tint_view: TintView`, both fragment-only), `voxel_pull/pipeline.rs` (layout entries and the LOCKED binding comment), `voxel_pull/prepare.rs` (bind group: `TintTexture` and a uniform buffer of `TintView`)
- Modify: `voxel_texture.wgsl` (bindings 105 `tint_map`, 106 `tint_view`), `voxel_material.rs` (`#[texture(105, sample_type = "float", filterable = false)] pub tint_map`, `#[uniform(106)] pub tint_view: TintViewUniform`, plus a sync system like `sync_chunk_material_cut`)
- Binding 8 is reserved for Part C's fringe array, so the pull numbering matches the per-chunk 104 = fringe.

**Interfaces:**
- WGSL block `tint` (identical in both forward shaders; the depth shader has none):

```wgsl
// tint_at(p_world_voxels, class) -> vec3<f32>: 1.0 when disabled or class 0.
// f = p.xz / region.x - origin - 0.5 (tile centres); i0 = floor(f), w = fract(f);
// four textureLoads at clamp(i0 + {0,1}, 0, size-1), row + class_row (grass 0, foliage height);
// bilinear mix; result / TINT_UNIT.
```

- Applied as `tex.rgb *= mix(vec3(1.0), tint_at(world_position, class), mask)`, where:
  - `class` is the base layer's `info.tint`;
  - `mask` = 1, or the red channel of `textureSampleGrad(mat_array_texture, …, uv, info.tint_mask, …)` when `tint_mask ≠ 0`.
- This runs on every terrain chunk the forward shaders draw, so the LOD rings (same shaders, `lod.rs`) are tinted too.

- [ ] **Step 1: Write the failing test.** Extend `block_helpers_are_identical_in_all_terrain_shaders` with a `tint` block shared by `voxel_pull.wgsl` and `voxel_texture.wgsl`. The naga tests cover the new bindings.
- [ ] **Step 2: Ask the owner, then run** `just test-one client "--lib voxel_pull::pipeline:: voxel_material::"`. Expected: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4: Ask the owner, then run** the same filter. Then run `just smoke` and `just web-check`. Expected: PASS.
- [ ] **Step 5: Commit** `feat(client): climate tint on near and far terrain`.

### Task B4: Declare tinted textures

- [ ] **Step 1:** In `blocks.ron` `textures:` set:
  - `tint:Grass` on `grass_top.png` (and its variants' group);
  - `tint:Grass, tint_mask:"grass_side_tint.png"` on `grass_side.png`;
  - `tint:Foliage` on every `*_leaves.png`.
  Leave `dry_grass_*` and `podzol_*` untinted (their colour is already the climate's).
- [ ] **Step 2:** Run `just test-one game "--lib registry::"` and `just smoke`. Expected: PASS.
- [ ] **Step 3:** Ask the owner for a look (`just play` on a forged world, across climates; a `world=tiny` procedural run must look as before).
- [ ] **Step 4: Commit** `feat(assets): grass and leaves take the climate tint`.

---

## Part C — fringe overlays and connected sides

### Task C1: Blend declarations in the registry

**Files:**
- Modify: `crates/game/src/registry.rs`
- Test: same file

**Interfaces:**
- Produces:
  - `BlockDef` gains `#[serde(default)] pub blend: Option<BlendDef>`, `#[serde(default)] pub blend_group: Option<String>` and `#[serde(default)] pub connect_sides: bool`.
  - `#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] pub struct BlendDef { pub priority: u8, pub onto: Vec<String> }`
  - `RegistryError::UnknownBlendGroup { block: String, group: String }`: an `onto` group no block declares.
  - `#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)] pub struct BlendTable { … }` with:
    - `pub fn priority_onto(&self, source: BlockId, target: BlockId) -> Option<u8>`: `Some(p)` iff `source` has a `blend` with priority `p`, `target`'s `blend_group` is in its `onto`, and `target`'s own blend priority (0 if none) is `< p`;
    - `pub fn sources(&self) -> impl Iterator<Item = BlockId>` (ascending).
  - `BlockRegistry::blend_table(&self) -> BlendTable`.

- [ ] **Step 1: Write the failing tests:**
  - `blend_onto_lower_group_only`: grass `(30, ["soil"])` onto dirt `blend_group:"soil"` gives `Some(30)`; onto stone (no group) gives `None`.
  - `higher_target_priority_wins`: sand `(20, ["turf"])` with grass in group `turf` and grass priority 30 gives `priority_onto(sand, grass) == None` and `priority_onto(grass, sand)` per grass's own `onto`.
  - `rejects_unknown_blend_group`.
  - `sources_are_ascending`.
- [ ] **Step 2:** `just test-one game "--lib registry::"`. Expected: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Same command. Expected: PASS.
- [ ] **Step 5: Commit** `feat(registry): blend priorities and groups for fringe overlays`.

### Task C2: Fringe masks and the fringe texture array

**Files:**
- Modify: `tools/textures/transitions.py` (subcommand `masks`), `EMBEDDED_BLOCK_TEXTURES`
- Create: `assets/blocks/fringe_edge.png`, `assets/blocks/fringe_corner.png` (16×16, alpha = coverage)
- Modify: `crates/client/src/renderer/block_textures.rs`, `renderer/mod.rs` (a second `texture_2d_array` image with its mip chain, `mip_chain` reused)

**Interfaces:**
- **Masks:** both are drawn in the **north / north-east orientation** of a +Y face, where image row 0 = −z (north) and column 15 = +x (east).
  - Edge: alpha 255 on rows 0..3, with a ragged hash-noise boundary between rows 3 and 6.
  - Corner: a ragged quarter-disc of radius about 5 texels at the top-right.
  - East, south and west are the north mask rotated 90°, 180° and 270° clockwise.
- Produces:
  - `pub fn fringe_case(edges: u8, corners: u8) -> Option<u8>`:
    - edge bits N = 1, E = 2, S = 4, W = 8; corner bits NE = 1, SE = 2, SW = 4, NW = 8;
    - a corner counts only when both its edges are absent;
    - returns a dense index in 0..46 over the canonical `(edges, effective_corners)` pairs in ascending `edges << 4 | corners` order;
    - `None` when nothing is set.
  - `pub fn compose_fringe(source: &RgbaImage, edge: &RgbaImage, corner: &RgbaImage, edges: u8, corners: u8) -> RgbaImage`: rgb from `source`; alpha = max of the present parts' rotated mask alphas.
  - `#[derive(Resource, Clone, Default, Serialize, Deserialize)] pub struct FringeTable { pub base: BTreeMap<BlockId, u32>, pub blend: game::BlendTable }`, where `base[s]` is the **record layer** of `s`'s case 0, `FRINGE_LAYER_BASE + 46 * k` for the k-th source.
  - `BuiltLayers` gains `pub fringe_images: Vec<RgbaImage>` and `pub fringe: FringeTable`.
  - `info[FRINGE_LAYER_BASE + i]` gets the source's top-texture tint class (so a grass fringe tints like grass), with `tint_mask` 0 and `top` identity.
  - The source image is the block's +Y face layer image (plan 1's `FaceLayers`).

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn forty_six_fringe_cases() {
    let set: BTreeSet<u8> = (0..16u8).flat_map(|e| (0..16u8).map(move |c| (e, c))).filter_map(|(e, c)| fringe_case(e, c)).collect();
    assert_eq!(set, (0..46).collect());
    assert_eq!(fringe_case(0, 0), None);
    assert_eq!(fringe_case(1, 1), fringe_case(1, 0)); // NE ignored beside the N edge
}
#[test] fn compose_rotates_the_edge() {
    // edge = alpha 255 on row 0 only; E case (edges=2): column 15 alpha 255, column 0 alpha 0, rgb == source rgb
}
#[test] fn fringe_layers_follow_sources() { /* two sources → base values FRINGE_LAYER_BASE, +46; fringe_images.len() == 92 */ }
// extend shipped_layers_fit_webgl2: fringe_images.len() <= WEBGL2_MAX_ARRAY_LAYERS
```

- [ ] **Step 2:** Run `just transition-textures masks` twice (clean second run). **Ask the owner, then run** `just test-one client "--lib block_textures::"`. Expected: FAIL.
- [ ] **Step 3:** Implement. Insert `FringeTable` as a resource.
- [ ] **Step 4: Ask the owner, then run** the same filter. Then run `just smoke`. Expected: PASS.
- [ ] **Step 5: Commit** `feat(client): composited fringe layers in their own texture array`.

### Task C3: The mesher emits fringe records

**Files:**
- Create: `crates/client/src/renderer/meshing/overlay.rs`
- Modify:
  - `meshing/mod.rs`: `mesh_chunk`, `mesh_chunk_classified`, `mesh_chunk_classified_wire` take `fringe: &FringeTable`
  - `meshing/records.rs`: `pub fn append_records(records: &mut Vec<QuadRecord>, extra: impl IntoIterator<Item = QuadRecord>)`, which pushes and re-sorts by `record_order`
  - `meshing/emit.rs`: `pub fn layered_quads_to_mesh_data(items: &[(Quad, u32)]) -> Option<MeshData>`; `quads_to_mesh_data` becomes a wrapper over it
  - `meshing/queue.rs`: `NearMeshJob` gains `pub fringe: FringeTable`, filled from `Res<FringeTable>`
  - `renderer/compute.rs` tests (job constructors)
- `lod.rs` passes **no** fringe: overlays only exist at scale 1.

**Interfaces:**
- Produces:
  - `pub struct OverlayQuad { pub quad: Quad, pub layer: u32 }`
  - `pub fn overlay_quads(padded: &PaddedVoxels, fringe: &FringeTable) -> Vec<OverlayQuad>`
  - `mesh_chunk_classified_wire(padded, layers, fringe, pull, interior)`: for pull, `append_records(&mut r, overlays.map(|o| pack_record(&o.quad, o.layer)))`; for meshes, the overlays join the `(Quad, layer)` list.
- Algorithm, per block cell (8 per brick; cell origin `o = 1 + 16·(bx, by, bz)` in padded coords; top row `y = o.y + 15`):
  1. **Target:** a full top. All 16×16 voxels at `y` are one block `t`, and every voxel at `y + 1` is `BlockId::AIR`. Otherwise skip (chiselled, slopes, dug).
  2. **Neighbours at row `y`:**
     - edges N `(o.x+8, y, o.z−1)`, E `(o.x+16, y, o.z+8)`, S `(o.x+8, y, o.z+16)`, W `(o.x−1, y, o.z+8)`;
     - corners NE `(o.x+16, y, o.z−1)`, SE `(o.x+16, y, o.z+16)`, SW `(o.x−1, y, o.z+16)`, NW `(o.x−1, y, o.z−1)`.
     - Indices 0 and 33 are the padded shell.
  3. **Source:** the neighbour block `s` with the highest `fringe.blend.priority_onto(s, t)`, ties to the lowest id. If none, skip.
  4. **Bits:** set for each neighbour equal to `s`; `case = fringe_case(edges, corners)`.
  5. **Quad:** `Quad { min: [o.x, y, o.z] (the same convention as binary.rs's +Y quads), w: 16, h: 16, face: PosY, block: s, ao, cutout: true, interior: false }`, `layer = fringe.base[s] + case`.
     - `ao[i]` comes from `ao::face_ao(padded, &CutoutTable::installed(), 1, true, corner_voxel_i)[i]` at the four corner voxels of the top, in corner order (0,0), (1,0), (0,1), (1,1).

- [ ] **Step 1: Write the failing tests (`overlay.rs`)**
  - `overlay_on_dirt_beside_grass`: a padded grid of dirt cells with one grass cell to the north. Exactly one overlay, `layer == base[grass] + fringe_case(1, 0)`, `cutout`, `face == PosY`, `w == h == 16`.
  - `overlay_skips_partial_tops`: the same with one top voxel of the dirt cell air, or a non-air voxel above. No overlay.
  - `overlay_reads_shell_neighbours`: the grass neighbour only in the shell (padded x = 33 beside the brick's +x cells). The E edge is set.
  - `no_overlay_without_rule`: stone beside grass with no `onto` for stone. Empty.
  - `records_keep_cutout_order`: after `append_records`, `face_ends(&r)[7]` is still the start of a range that holds every cutout record (`records.rs` ordering).
  - Use `common`'s installed block props as the meshing tests do.
- [ ] **Step 2: Ask the owner, then run** `just test-one client "--lib meshing::overlay:: meshing::records:: compute::"`. Expected: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4: Ask the owner, then run** the same filter. Then run `just smoke` and `just web-check` (the job struct is serialised to workers). Expected: PASS.
- [ ] **Step 5: Commit** `feat(client): the mesher emits fringe overlay records on blended tops`.

### Task C4: Fringe sampling and the lift in all three shaders

**Files:**
- Modify:
  - `voxel_pull.wgsl` and `voxel_pull_depth.wgsl`: binding 8 `fringe_texture: texture_2d_array<f32>`, fragment-only, sampled with the binding-3 sampler
  - `voxel_pull/pipeline.rs`: layout entry and LOCKED comment
  - `voxel_pull/prepare.rs`: bind the fringe image
  - `voxel_texture.wgsl` and `voxel_material.rs`: `#[texture(104, dimension = "2d_array")] pub fringe_texture`, sharing sampler 101

**Interfaces:**
- WGSL block `fringe` (identical in the three shaders):
  - `const FRINGE_LAYER_BASE: u32 = 1024u; const FRINGE_LIFT_VOXELS: f32 = 0.0625;`
  - `fn is_fringe(layer: u32) -> bool { return layer >= FRINGE_LAYER_BASE; }`
- **Vertex stage** (all three): `if is_fringe(layer) { local_pos.y += FRINGE_LIFT_VOXELS; }` before the transform. Fringes are only ever +Y records at scale 1.
- **Fragment** (and the depth CUTOUT fragment): when `is_fringe(layer)`, sample `fringe_texture` at `layer − FRINGE_LAYER_BASE` with the same `textureSampleGrad` gradients. There is no variant or rotation (`pick_texture` is skipped). Tint uses `info[layer]`.
- Fringes land in the cutout range, so they use the alpha-tested pipelines and the existing CUTOUT shadow and depth variants. No new pipeline. `face_ends` is unchanged.
- On the per-chunk path the single `AlphaMode::Mask(0.5)` material (`renderer/mod.rs:1179-1183`) already alpha-tests them.
- Add a `rust` constant test: `block_textures::FRINGE_LAYER_BASE` equals the WGSL constant text.

- [ ] **Step 1: Write the failing tests.** Extend the identity test with `fringe`, plus `fringe_base_matches_wgsl`. The naga tests cover the new binding in every variant.
- [ ] **Step 2: Ask the owner, then run** `just test-one client "--lib voxel_pull::pipeline:: voxel_material:: block_textures::"`. Expected: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4: Ask the owner, then run** the same filter. Then run `just smoke` and `just web-check`. Expected: PASS.
- [ ] **Step 5: Commit** `feat(client): draw fringe overlays from their own array, lifted off the face`.

### Task C5: Declarations, and connected sides

**Files:**
- Modify: `assets/blocks/blocks.ron`, `crates/game/src/block_props.rs` (`connect_sides: BTreeSet<u16>`, filled in `registry.rs::props`), `meshing/binary.rs` (merge key, `Quad`), `meshing/records.rs` + `emit.rs` (layer choice)
- Test: `binary.rs` tests; `game` registry test

**Interfaces:**
- **Declarations** (check each name exists in `blocks.ron`; at most **5 sources**, `5 × 46 = 230 ≤ 256`):
  - Sources:
    - `grass` `blend:(priority:30, onto:["soil","stone","sand"])`, `blend_group:"turf"`
    - `dry_grass` `(28, ["soil","stone","sand"])`, `blend_group:"turf"`
    - `sand` `(20, ["soil","stone"])`, `blend_group:"sand"` (`red_sand` too)
    - `gravel` `(15, ["soil","stone"])`, `blend_group:"gravel"`
    - `snow` `(40, ["turf","soil","stone","sand","gravel"])`
  - Receivers only: `dirt`, `mud`, `frozen_dirt`, `podzol` get `blend_group:"soil"`; `stone`, `andesite`, `granite`, `diorite`, `basalt` get `blend_group:"stone"`.
  - `connect_sides: true` on `grass`, `dry_grass`, `podzol`.
- **Connected sides** (the OptiFine "better grass" behaviour, https://optifine.readthedocs.io/better_grass.html):
  - **When:** a horizontal side face of a `connect_sides` block shows the block's **top** layer when (a) and (b) both hold:
    - (a) the face lies on a block boundary (`(p[axis] − 1 + positive as usize) % 16 == 0` in padded coords);
    - (b) the block one cell out along the face and one cell down has the same id. Read it at padded `(p + d·1)` with `y = block_bottom_y − 1`, where `block_bottom_y = 1 + 16·((p.y − 1) / 16)`.
  - **Quad:** gains `pub top_texture: bool`.
  - **Merge key:** gains `TOP_TEXTURE_KEY_BIT = 1 << 50`.
  - **Layer choice:** `quads_to_records` and `quads_to_mesh_data` use `layer_for_face(faces, if q.top_texture { Face::PosY } else { q.face })`.
  - `CutoutTable`-style `ConnectSidesTable::installed()` from `game::block_props()`.

- [ ] **Step 1: Write the failing tests:**
  - `binary.rs` `connected_side_uses_top`: a grass block on a 1-block step down onto grass. Its exposed side quad has `top_texture == true`. Against dirt it is `false`.
  - `connected_sides_do_not_merge_with_plain`: the `top_texture` and plain parts of a side are separate quads.
  - `game` `parses_shipped_manifest` still passes.
  - The client test `shipped_layers_fit_webgl2` covers 230 fringe layers.
- [ ] **Step 2: Ask the owner, then run** `just test-one client "--lib meshing::binary:: block_textures::"` and `just test-one game "--lib registry::"`. Expected: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4: Ask the owner, then run** the same filters. Then run `just smoke` and `just web-check`. Expected: PASS.
- [ ] **Step 5:** Ask the owner for a look (`just run`):
  - grass edges creep onto dirt, sand and stone;
  - snow fringes everything;
  - slopes (chiselled) show no floating fringes;
  - grassy steps read as continuous grass;
  - the web build (`just web`) renders the same, which is the WebGL2 per-chunk path when forced.
- [ ] **Step 6: Commit** `feat(assets): ground blends — grass, dry grass, sand, gravel and snow fringes; connected grass sides`.

---

## Follow-ups (not in this plan)

- Several fringe sources on one top: today only the highest priority wins.
- Fringes on side faces (grass hanging over a cliff edge).
- Weighted variants (`dirt_path`-style rare variants).
- A finer climate grid (a per-column byte in `ColumnGround`) if 256 m tiles look too coarse.
- On WebGL2, a fragment texture-unit overflow would only show at runtime in a browser. If it does, fold `tint_map` into the `layer_info` texture as extra rows.
