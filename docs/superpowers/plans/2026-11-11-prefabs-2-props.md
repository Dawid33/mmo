# Prefabs 2 — Prop prefabs: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prop prefabs — rigid assemblies of baked LDraw parts with free per-part transforms — can be baked, imported from `.ldr`, placed as cheap instances (by stamp or by a worldgen seed), collide, block paths, draw, be picked, be edited in the prefab editor, and be detached into one falling body.

**Architecture:** `tools/ldraw-bake` gains a second part list and emits a prop catalog (hulls, bounds) into `game` and meshes into `assets/studs/props.bin`. `game::prefab` gains the resolved Prop form and pure geometry helpers; the region gains `props` rollback state with one fixed compound collider per instance, a tile mask, a halo mirror and a store baseline-plus-diff. The client draws instances from cached per-(prefab, colours) meshes and edits a prop as a client-side document shown through one instance.

**Tech Stack:** Rust (nightly pinned), vendored `rapier` 0.36 / `parry` 0.31 (`enhanced-determinism`), `weldr` 0.3.1 (bake tool only), `libm`, Bevy 0.19.1 and Slint (client only).

**Spec:** `docs/superpowers/specs/2026-11-11-prefab-system-design.md` (steps 5–9; section "Prop prefabs"). Requirements source: `docs/superpowers/specs/2026-11-11-lego-tree-prefabs-design.md` §"What trees need from the prefab mechanism".

**Depends on:** Plan 1 (`2026-11-11-prefabs-1-build-library-editor.md`) complete — this plan uses its `PrefabFile`, `PropFile`, `ResolvedPrefab`, `ResolvedKind`, `Problem`, `Trust`, `PrefabKey`, `PrefabSet` / `prefabs()`, `PrefabCommand`, `PrefabRef`, `RegisteredPrefab`, `Client::prefabs`, `stamp_prefab_order_safe`, `PrefabDir`, `EditorSession`, `editor_anchor()`, and the library screen.

## Global Constraints

- All of Plan 1's Global Constraints apply (no worktrees, engine-agnostic crates, `BTreeMap`, `libm`, `StateHash`, rollback bar, session registration, Slint rules, test commands, client tests are ask-first).
- `PROP_PARTS` is **append-only**: indices live in sim state. `prop_parts.txt` is only ever appended to.
- Re-baking hulls is a physics change: `just determinism-wasm` and `just test-one game "--test it physics_goldens::"` must pass after any bake change.
- Mass is always explicit (summed axis-aligned boxes through `mass_properties_of_boxes`); never let parry derive mass from a hull or compound (eigen-solve determinism).
- `ColliderSpec`: new variants go at the **end**.
- `carry_components` and `Client::arrive` destructure with no `..`: classify every new field.
- Store: never add a write that skips the fence; a format bump keeps the previous format decodable (`…Vn` decode-only struct + `impl From`), with a golden blob in `crates/world-store/tests/fixtures/`.
- wasm: new compute work is a `renderer::compute::JobKind` and carries everything it reads.
- **Ask the user before launching any benchmark or perf capture.**
- Units: 1 LDU = `LDU_UNITS` (0.4) sim units; a stud is 20 LDU; a plate is 8 LDU.
- Caps: `PROP_MAX_PARTS = 256`, `PROP_MAX_SLOTS = 8`, `PROP_SOLID_REACH_LDU = 640` (16 m), `PROP_HULL_MAX_POINTS = 32`.

### Deviations from the spec, decided while planning

1. **A prop holds at most 256 parts** (the spec's 4096 is the Build cap). One instance is one compound collider; 256 hulls is already generous.
2. **Part rotation is stored as a quantised quaternion `[i16; 4]`** in the resolved form (the file keeps `[f32; 4]`), so the resolved prefab stays `Eq` + `StateHash` and bit-identical across platforms.
3. **The tile mask is ordinary rollback state** maintained on add / remove (and rebuilt on load, never saved), rather than an unhashed derived index: it must undo exactly with the rest.
4. **The store baseline is a `prop_seed` blob written once per region**, mirroring how `stud_ground` is stored, plus a diff in `RegionMeta`. A loaded region has no genesis function to re-derive the baseline from.
5. **`RemoveProp` is a `PrefabCommand`**, with the other prefab commands, not an `Order`.

## Review Focus

1. **A prefab whose parts are all non-solid** (a bush of leaves) — expect an instance with no collider and no blocked tiles, still drawn and pickable. (Tasks 4, 5, 9)
2. **Removing a `Local` prop while another instance of the same prefab remains, then undoing** — expect the definition to stay until the last instance goes, and the undo to restore both the instance and the count. (Task 5)
3. **A region saved by this build, loaded with a `Builtin` prefab since deleted from `assets/prefabs/`** — expect the instance dropped with one log line, the region still loading. (Task 6)
4. **A prop standing 1 m inside a seam, removed while the neighbour sleeps** — expect the neighbour's mirror collider and blocked tiles gone once it wakes and re-seeds its halo. (Task 8)
5. **An `.ldr` naming a part that is not in `prop_parts.txt`** — expect the import to fail listing every missing part number, writing nothing. (Task 3)

---

## File Structure

| File | Responsibility |
|---|---|
| `tools/ldraw-bake/prop_parts.txt` | Append-only prop part list with flags |
| `tools/ldraw-bake/src/prop.rs` | Prop bake: bounds, hulls, meshes |
| `tools/ldraw-bake/src/import.rs` | `.ldr` / shortcut → `PrefabFile` |
| `crates/game/src/prop_catalog.rs` | Generated `PROP_PARTS` |
| `crates/game/src/prop_assets.rs` | `PropPartDef`, `BakedProps` |
| `assets/studs/props.bin` | Baked prop meshes |
| `crates/game/src/prefab/prop.rs` | `ResolvedProp`, resolve / validate for props |
| `crates/game/src/prefab/prop_geom.rs` | Pure geometry: poses, shape, mass boxes, bounds, tiles, ray |
| `crates/game/src/prop_ops.rs` | Region state mutations (`add_prop_safe`, …), tiles, colliders |
| `crates/game/src/prop_halo.rs` | Halo mirror of props |
| `crates/game/src/prop_body.rs` | Detach into a falling body |
| `crates/client/src/renderer/prop/{mod,mesh,pick}.rs` | Assets, mesh cache, instance entities, picking |
| `crates/client/src/prefab_editor/prop_doc.rs`, `prop_tools.rs` | Prop document, undo, tools |
| `crates/slint-ui/ui/prop-tools.slint` | Prop tool panel |

---

### Task 1: Prop part bake

**Files:**
- Create: `tools/ldraw-bake/prop_parts.txt`, `tools/ldraw-bake/src/prop.rs`, `crates/game/src/prop_assets.rs`, `crates/game/src/prop_catalog.rs` (generated), `assets/studs/props.bin` (generated)
- Modify: `tools/ldraw-bake/src/{main,emit}.rs`, `crates/game/src/lib.rs`, `justfile` (comment on `ldraw-bake`)
- Test: unit tests in `tools/ldraw-bake/src/prop.rs`; `crates/game/tests/prop_catalog.rs`

**Interfaces:**
- Consumes: `flatten(lib, part, detail) -> Result<Flat>`, `Library`, `emit::outputs`, `emit::write_or_check`, `BakedLod`, `game::encode_packet`.
- Produces:

```rust
// game::prop_assets
pub struct PropPartDef { pub number: &'static str, pub name: &'static str,
    pub min: [i16; 3], pub max: [i16; 3],                 // bounds in LDU, part origin = LDraw origin, Y up
    pub hulls: &'static [&'static [[i16; 3]]],            // convex pieces, LDU; one for a convex part
    pub solid: bool }                                     // default for new placements
pub const BAKED_PROPS_VERSION: u32 = 1;
pub struct BakedProps { pub version: u32, pub parts: Vec<BakedPart> }     // BakedPart reused; tris in sim units, part origin (not bbox-min)
// game::prop_catalog (generated, "@generated", APPEND-ONLY)
pub const PROP_PARTS: &[PropPartDef];
pub fn prop_part_index(number: &str) -> Option<u16>
// tools/ldraw-bake/src/prop.rs
pub struct PropFlags { pub solid: bool, pub concave: bool }
pub fn parse_prop_list(text: &str) -> Result<Vec<(String, PropFlags)>>
pub fn bake_prop(lib: &Library, number: &str, flags: PropFlags) -> Result<BakedProp>
pub fn hull_of(points: &[[f64; 3]], max_points: usize) -> Vec<[i16; 3]>
// emit.rs
pub const PROP_CATALOG_PATH: &str = "crates/game/src/prop_catalog.rs";
pub const PROPS_BIN_PATH: &str = "assets/studs/props.bin";
```

List format, one part per line: `<number> [nonsolid] [concave]`, `#` comments. Initial list, in this order (from the tree design's table):

```
3471
52211
2435
3470
3778
2563a
2536a
2566b
2518 nonsolid
6148 nonsolid
30338 concave
30339 nonsolid
2417 nonsolid
2423 nonsolid
6064b
30131 concave
3742 nonsolid
4032a
3062b
```

Axis conversion is the one `flatten` already applies for grid parts (LDraw −Y up → game Y up); the origin stays the LDraw part origin. `hull_of`: convex hull of **all** flattened vertices, simplified to at most `PROP_HULL_MAX_POINTS` points, rounded to integer LDU, then re-hulled so the stored points are exactly the hull's vertices. `concave` parts are split into convex pieces first (method left to the builder — parry's decomposition or a manual axis split), each piece then through `hull_of`. `just ldraw-bake --check` covers the two new outputs.

- [ ] **Step 1: Write the failing tests:**

```rust
// tools/ldraw-bake/src/prop.rs
#[test] fn list_parses_flags() { let l = parse_prop_list("3471\n# c\n2518 nonsolid\n30131 concave\n").unwrap();
    assert_eq!(l[1], ("2518".into(), PropFlags { solid: false, concave: false })); assert!(l[2].1.concave); }
#[test] fn list_rejects_unknown_flags() { assert!(parse_prop_list("3471 wobbly").is_err()); }
#[test] fn hull_of_a_cube_cloud_is_its_eight_corners() { /* 1000 points inside a 20 LDU cube incl. corners */ assert_eq!(h.len(), 8); }
#[test] fn hull_is_capped_and_still_contains_the_centroid() { /* 2000 points on a sphere r=100 */ assert!(h.len() <= 32); }
// crates/game/tests/prop_catalog.rs
#[test] fn every_hull_builds_a_polyhedron() { for p in PROP_PARTS { for h in p.hulls { let pts: Vec<Vector> = /* * LDU_UNITS */;
    assert!(ConvexPolyhedron::from_convex_hull(&pts).is_some(), "{}", p.number); assert!(h.len() <= 32); } } }
#[test] fn bounds_contain_hulls() { /* every hull point within min..=max */ }
#[test] fn catalog_order_matches_the_list() { assert_eq!(PROP_PARTS[0].number, "3471"); assert_eq!(prop_part_index("2518"), Some(8)); assert!(!PROP_PARTS[8].solid); }
```

- [ ] **Step 2:** Run `cargo test -p ldraw-bake prop::` (prefix with the `just` cap: `nice -n 10`) and `just test-one game "--test it prop_catalog::"` — expected: compile failures.
- [ ] **Step 3:** Implement; run `just ldraw-bake` (needs `assets/ldraw`, git-ignored, present on this box) to generate `prop_catalog.rs` and `props.bin`.
- [ ] **Step 4:** Run both test commands — expected: 4 + 3 passed. `just ldraw-bake --check` — expected: exit 0.
- [ ] **Step 5:** Commit: `feat(ldraw-bake,game): prop part catalog with convex hulls`.

---

### Task 2: Resolved props

**Files:**
- Create: `crates/game/src/prefab/prop.rs`
- Modify: `crates/game/src/prefab/resolve.rs` (`ResolvedKind`, `Problem`, `resolve`, `unresolve`, `validate`), `crates/game/src/prefab/library.rs` (size for prop entries)
- Test: `crates/game/tests/prefab_prop.rs`; update `prefab_resolve::prop_files_are_refused_for_now` → now resolves

**Interfaces:**
- Produces:

```rust
pub const PROP_MAX_PARTS: usize = 256;
pub const PROP_MAX_SLOTS: usize = 8;
pub const PROP_SOLID_REACH_LDU: i32 = 640;
pub struct ResolvedProp { pub parts: Vec<PropPart>, pub slots: Vec<SlotDef>, pub foot: [u8; 2], pub markers: Vec<Marker> }
pub struct PropPart { pub part: u16, pub pos: [i32; 3], pub rot: [i16; 4], pub colour: PropColour, pub solid: bool }   // rot: unit quaternion (x,y,z,w) * 32767
pub fn quantise_rot(q: [f32; 4]) -> Option<[i16; 4]>          // None for a zero / non-finite quaternion; normalises; w >= 0
pub fn rot_f32(q: [i16; 4]) -> [f32; 4]                       // re-normalised with libm::sqrtf
ResolvedKind::Prop(ResolvedProp)                              // appended after Build
Problem::{ BadPropRotation(usize), BadSlot(usize), TooManySlots(usize), SolidTooFar(usize), BadFoot }   // appended; PropsNotSupported removed
impl ResolvedPrefab { pub fn as_prop(&self) -> Option<&ResolvedProp>; pub fn as_build(&self) -> Option<&ResolvedBuild> }
```

Checks: part in `PROP_PARTS`; ≤ 256 parts; ≤ 8 slots; `Slot(n)` with `n < slots.len()`; `Fixed(c)` and slot defaults in `STUD_COLORS`; `foot` each in `1..=16`; for every **solid** part, its rotated bounds lie within `PROP_SOLID_REACH_LDU` of the anchor on x and z. `element_count` for a prop is its part count.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn palm_resolves_and_round_trips() { let f = palm_file(); let r = resolve(&f, Trust::Player).unwrap(); assert_eq!(r.as_prop().unwrap().parts.len(), 7);
    let back = unresolve(&r, f.meta.clone()); assert_eq!(resolve(&back, Trust::Player).unwrap(), r); }   // f32 → i16 → f32 is stable after one pass
#[test] fn quantise_normalises_and_fixes_sign() { assert_eq!(quantise_rot([0.0, 0.0, 0.0, -2.0]), Some([0, 0, 0, 32767])); assert_eq!(quantise_rot([0.0; 4]), None); assert_eq!(quantise_rot([f32::NAN, 0.0, 0.0, 1.0]), None); }
#[test] fn problems() { /* unknown part, Slot(3) with 2 slots, 9 slots, solid part at x = 700 LDU, foot [0, 2], zero quaternion, 257 parts */
    for p in [UnknownPart("0000".into()), BadSlot(1), TooManySlots(9), SolidTooFar(2), BadFoot, BadPropRotation(3), TooManyParts(257)] { assert!(e.contains(&p)); } }
#[test] fn a_far_nonsolid_part_is_allowed() { /* frond at x = 700 LDU, nonsolid */ assert!(resolve(&f, Trust::Player).is_ok()); }
#[test] fn key_is_platform_stable() { assert_eq!(resolve(&palm_file(), Trust::Player).unwrap().key(), PrefabKey(0x…)); }   // pin the value once computed; also asserted in wasm_determinism
```

- [ ] **Step 2:** Run `just test-one game "--test it prefab_prop::"` — expected: compile failure.
- [ ] **Step 3:** Implement. Add the pinned-key assertion to `crates/game/tests/wasm_determinism.rs`.
- [ ] **Step 4:** Run — expected: 5 passed; `just test-one game "--test it prefab_resolve:: prefab_library::"` passes with the updated test. `just determinism-wasm` — expected: pass.
- [ ] **Step 5:** Commit: `feat(game): resolved prop prefabs`.

---

### Task 3: `.ldr` import

**Files:**
- Create: `tools/ldraw-bake/src/import.rs`
- Modify: `tools/ldraw-bake/src/main.rs` (subcommand), `justfile` (`prefab-import-ldr`)
- Test: unit tests in `import.rs` with inline LDraw text

**Interfaces:**
- Produces:

```rust
pub fn import_ldr(lib: &Library, text: &str, name: &str) -> Result<PrefabFile, ImportError>
pub enum ImportError { MissingParts(Vec<String>), BadName, Parse(String), TooManyParts(usize) }
// CLI: ldraw-bake import-prefab <file.ldr|file.dat> <name> [--out assets/prefabs]
```

Rules: walk type-1 lines; a sub-file whose number is in `prop_parts.txt` becomes one `PropPartFile` with the accumulated matrix (converted with the bake's axis conversion; rotation → quaternion; a matrix with scale or shear is a `Parse` error naming the line); any other sub-file is recursed into (this is how `2518c01` yields its trunk segments and fronds). Colour code 16 (inherit) → `PropColour::Slot(0)`; any other code → `Fixed(code)`. `slots = [SlotDef { name: "main", default: 2 }]` when any part uses slot 0. `solid` from the list's flag. `foot = [2, 2]`. The lowest solid point is moved to y = 0 and the model centred on its lowest part in x,z, so the anchor is the foot. `just prefab-import-ldr file name` runs the subcommand with `--out assets/prefabs`.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn two_parts_keep_their_transforms() { let f = import_ldr(&lib(), "1 16 0 0 0 1 0 0 0 1 0 0 0 1 2563a.dat\n1 6 0 -32 0 1 0 0 0 1 0 0 0 1 2536a.dat\n", "t").unwrap();
    let PrefabKindFile::Prop(p) = f.kind else { panic!() }; assert_eq!(p.parts[0].colour, PropColour::Slot(0)); assert_eq!(p.parts[1].colour, PropColour::Fixed(6));
    assert_eq!(p.parts[1].pos[1] - p.parts[0].pos[1], 32); }                                   // LDraw -32 → +32 up
#[test] fn missing_parts_are_all_listed_and_nothing_is_returned() { /* 9999.dat and 8888.dat */ assert!(matches!(e, ImportError::MissingParts(v) if v == ["8888", "9999"])); }
#[test] fn scaled_matrix_is_refused() { assert!(matches!(import_ldr(&lib(), "1 16 0 0 0 2 0 0 0 1 0 0 0 1 3471.dat\n", "t"), Err(ImportError::Parse(_)))); }
#[test] #[ignore = "needs assets/ldraw"] fn official_palm_shortcut_imports() { /* 2518c01.dat */ assert!(p.parts.len() >= 12); resolve(&f, Trust::Builtin).unwrap(); }
```

- [ ] **Step 2:** Run `cargo test -p ldraw-bake import::` — expected: compile failure.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run — expected: 3 passed; with `-- --ignored` on this box, 1 more.
- [ ] **Step 5:** Run `just prefab-import-ldr assets/ldraw/parts/2518c01.dat dev-palm`; commit the resulting `assets/prefabs/dev-palm.prefab.ron` as the prop sample. Commit: `feat(ldraw-bake): import LDraw assemblies as prop prefabs`.

---

### Task 4: Prop geometry (pure)

**Files:**
- Create: `crates/game/src/prefab/prop_geom.rs`
- Test: `crates/game/tests/prop_geom.rs`; one case added to `wasm_determinism.rs`

**Interfaces:**
- Consumes: `ResolvedProp`, `PROP_PARTS`, `rot_f32`, `LDU_UNITS`, `mass_properties_of_boxes(boxes: &[(f64, [f64; 3], [f64; 3])]) -> MassProperties`, `ConvexPolyhedron::from_convex_hull`, `SharedShape::compound`, `Pose`.
- Produces:

```rust
pub fn part_pose(part: &PropPart, turn: u8) -> Pose                                  // in the prop's frame, sim units, after the instance quarter turn
pub fn prop_shape(prop: &ResolvedProp, turn: u8, solid_only: bool) -> Option<SharedShape>   // compound of hulls; None when no part qualifies
pub fn prop_mass_boxes(prop: &ResolvedProp, turn: u8) -> Vec<(f64, [f64; 3], [f64; 3])>      // (kg, min, max): each part's turned AABB
pub const PROP_DENSITY: f64 = 60.0;                                                  // kg/m³ of a part's bounding box; tune with felling
pub fn prop_bounds(prop: &ResolvedProp, turn: u8, solid_only: bool) -> Option<([f32; 3], [f32; 3])>
pub fn prop_tiles(prop: &ResolvedProp, turn: u8, at: [i32; 3]) -> BTreeSet<TilePos>  // tiles covered by solid parts' turned AABBs; `at` = region-local studs/plates/studs
pub struct PropRayHit { pub part: usize, pub t: f32 }
pub fn raycast_prop(prop: &ResolvedProp, turn: u8, origin: Vector, dir: Vector, reach: f32) -> Option<PropRayHit>   // all parts, solid or not; ray in the prop's frame
```

Tile conversion goes through `game::tiles` (a tile is 2 studs x 6 plates x 2 studs). All trig-free: rotation by quaternion; any square root through `libm`.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn shape_has_one_piece_per_solid_hull() { let s = prop_shape(&palm(), 0, true).unwrap(); assert_eq!(s.as_compound().unwrap().shapes().len(), solid_hull_count(&palm())); }
#[test] fn all_nonsolid_prop_has_no_solid_shape_but_a_full_one() { assert!(prop_shape(&leaf_bush(), 0, true).is_none()); assert!(prop_shape(&leaf_bush(), 0, false).is_some()); assert!(prop_tiles(&leaf_bush(), 0, [10, 90, 10]).is_empty()); }
#[test] fn turn_rotates_bounds_about_the_anchor() { let (lo, hi) = prop_bounds(&offset_block(), 0, true).unwrap(); let (lo1, hi1) = prop_bounds(&offset_block(), 1, true).unwrap();
    assert!((hi[0] - lo[0] - (hi1[2] - lo1[2])).abs() < 1e-3); }
#[test] fn tiles_cover_the_trunk_only() { let t = prop_tiles(&pine(), 0, [64, 90, 64]); assert!(t.contains(&tile_of([64, 90, 64]))); assert!(!t.contains(&tile_of([70, 90, 64]))); }
#[test] fn mass_is_finite_and_positive() { let m = mass_properties_of_boxes(&prop_mass_boxes(&palm(), 0)); assert!(m.mass() > 0.0 && m.mass().is_finite()); }
#[test] fn ray_hits_a_frond_before_the_trunk() { /* ray from outside toward the crown */ assert!(!palm().parts[hit.part].solid); }
#[test] fn ray_misses_beside_the_prop() { assert!(raycast_prop(&pine(), 0, far_origin, down, 100.0).is_none()); }
```

`wasm_determinism.rs`: hash the bit patterns of `prop_bounds(&palm(), t, false)` for `t in 0..4` and of every compound piece's pose; assert a pinned value.

- [ ] **Step 2:** Run `just test-one game "--test it prop_geom::"` — expected: compile failure.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run — expected: 7 passed. `just determinism-wasm` — expected: pass (native and wasm agree on the pinned value).
- [ ] **Step 5:** Commit: `feat(game): prop geometry helpers`.

---

### Task 5: Prop instances in the region

**Files:**
- Create: `crates/game/src/prop_ops.rs`
- Modify: `crates/game/src/state.rs` (`GameData`, `GameDataUpdateKind`), `crates/game/src/prefab/command.rs` (`PrefabCommand`, stamp), `crates/game/src/orders.rs` + `tiles.rs` (`tile_fill`), `crates/game/src/lib.rs`
- Test: `crates/game/tests/prop_ops.rs`

**Interfaces:**
- Produces:

```rust
pub struct PropId { pub region: RegionCoords, pub serial: u64 }      // Copy, Ord, StateHash, serde
pub const PROP_WORLDGEN_BIT: u64 = 1 << 63;                           // set on ids made by worldgen; runtime ids come from next_entity_serial
impl PropId { pub fn is_worldgen(self) -> bool }
pub enum PropRef { Builtin(u32), Local(PrefabKey) }                   // Copy
pub struct PropInstance { pub prefab: PropRef, pub at: [i32; 3], pub turn: u8, pub colours: Vec<u16> }   // at: region-local studs / plates / studs
pub struct LocalPropDef { pub def: Arc<ResolvedPrefab>, pub uses: u32 }                                  // StateHash: key + uses
pub const PROP_BODY_USER_DATA: u128 = u128::MAX - 1;
// GameData (all #[undo(map)])
props: BTreeMap<PropId, PropInstance>, prop_defs: BTreeMap<PrefabKey, LocalPropDef>,
prop_bodies: BTreeMap<PropId, RigidBodyHandle>, prop_tiles: BTreeMap<TilePos, u16>, props_removed: BTreeMap<PropId, ()>
// GameDataUpdateKind (appended)
SetProps(Vec<(PropId, Option<PropInstance>)>), SetPropDef(PrefabKey, Option<Box<ResolvedPrefab>>)
// PrefabCommand (appended)
RemoveProp(PropId)
// impl Rollback
pub fn prop_def(&self, r: PropRef) -> Option<Arc<ResolvedPrefab>>             // Builtin → prefabs(); Local → prop_defs
pub fn add_prop_safe(&mut self, id: PropId, inst: PropInstance, local_def: Option<Arc<ResolvedPrefab>>) -> bool
pub fn remove_prop_safe(&mut self, id: PropId) -> Option<PropInstance>
pub fn alloc_prop_id_safe(&mut self) -> PropId
pub fn prop_at_tile(&self, p: TilePos) -> bool
```

`add_prop_safe`: refuses (false, nothing changed) if the id exists, the definition is missing or not a Prop, `turn > 3`, `colours.len() != slots.len()` or a colour not in `STUD_COLORS`, or `at` is outside this region's stud bounds. Otherwise: for `Local`, insert the definition or bump `uses`; insert into `props`; if `prop_shape(.., solid_only = true)` is `Some`, `insert_body_safe(RigidBodyBuilder::fixed().pose(..).user_data(PROP_BODY_USER_DATA).build())` + `attach_collider_safe` and record in `prop_bodies`; add `prop_tiles` counts; emit `SetPropDef` (first use) and `SetProps`. `remove_prop_safe` is the exact inverse path and, for a worldgen id, records `props_removed`. `tile_fill` returns `TileFill::Solid` when `prop_tiles` holds the tile, before consulting bricks.

Stamp: in `stamp_prefab_order_safe`, a Prop prefab goes to `stamp_prop_safe`: `anchor` must be in this region (else `None` — a prop is placed by the region that owns its anchor; the client sends the command to that region); `colours` = slot defaults; id from `alloc_prop_id_safe`; `Registered` references are copied in as `Local`, `Builtin` stays `Builtin`. On success it returns `Some(StampOutcome { placed: 1, skipped_overlap: 0, skipped_split: 0, sent_to_neighbours: 0 })`. `anchor` (world studs / plates / studs) becomes `at` by subtracting the region's stud origin.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn add_then_undo_restores_the_hash() { let b = hash(&r); stamp(&mut r, PrefabRef::Builtin(prefab_name_key("dev-palm")), at, 0);
    assert_eq!(r.data().props().len(), 1); assert_ne!(b, hash(&r)); r.rollback_last_event(); assert_eq!(b, hash(&r)); }
#[test] fn remove_then_undo_restores_the_hash() { /* same shape with PrefabCommand::RemoveProp */ }
#[test] fn solid_prop_has_a_fixed_body_and_blocks_its_tile() { assert!(r.data().prop_at_tile(tile_of(at))); assert_eq!(r.data().tile_fill(tile_of(at)), TileFill::Solid);
    assert!(find_path(r.data(), a, b, 4096).unwrap().iter().all(|t| *t != tile_of(at))); }
#[test] fn nonsolid_prop_has_no_body_and_blocks_nothing() { /* leaf bush */ assert!(r.data().prop_bodies().is_empty()); assert!(!r.data().prop_at_tile(tile_of(at))); }
#[test] fn local_def_is_refcounted() { /* register palm, stamp twice, remove one */ assert_eq!(r.data().prop_defs()[&k].uses, 1); /* remove the other */ assert!(r.data().prop_defs().is_empty());
    r.rollback_last_event(); assert_eq!(r.data().prop_defs()[&k].uses, 1); }
#[test] fn registered_prop_outlives_deregistration() { /* stamp, then Deregister */ assert_eq!(r.data().props().len(), 1); }
#[test] fn refusals_change_nothing() { for bad in [turn_4(), wrong_colour_count(), colour_not_in_table(), unknown_builtin(), build_prefab_as_prop(), at_outside_region()] { assert_eq!(h, hash(&r)); } }
#[test] fn updates_are_emitted() { assert!(drain(&rx).iter().any(|k| matches!(k, GameDataUpdateKind::SetProps(v) if v.len() == 1))); }
#[test] fn two_overlapping_tile_props_keep_the_tile_until_both_go() { /* counts */ }
```

- [ ] **Step 2:** Run `just test-one game "--test it prop_ops::"` — expected: compile failure.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run — expected: 9 passed. `just test-one game "--test it stud_tiles:: orders::"` — unchanged. `just smoke`.
- [ ] **Step 5:** Commit: `feat(game): prop instances with colliders and a tile mask`.

---

### Task 6: Seeding and saving props

**Files:**
- Create: (in `crates/game/src/prefab/seed.rs`) `stamp_prop_into_seed`
- Modify: `crates/game/src/stud_ground.rs` (`StudRegionSeed`), `region.rs` (`seed_stud_world`), `portable.rs` (`RegionMeta`, `PortableRegion`, `StoredRegion`, `portable_snapshot`, `restore_portable`, `SaveTerms`), `crates/world-store/src/codec.rs` + the SQLite and Postgres schema / queries where `stud_ground` is stored, `crates/game/src/state.rs` (`prop_seed` field)
- Test: `crates/game/tests/prop_portable.rs`; `crates/world-store/tests/codec.rs` + a new golden fixture

**Interfaces:**
- Produces:

```rust
pub struct StudRegionSeed { …, pub props: Vec<(PropId, PropInstance)> }            // Builtin refs only
pub fn stamp_prop_into_seed(seed: &mut StudRegionSeed, region: RegionCoords, prefab_name: &str, at_world: [i64; 3], turn: u8, colours: Vec<u16>, serial: u64) -> bool
// GameData: #[undo(cell)] prop_seed: Arc<Vec<(PropId, PropInstance)>>            // the worldgen baseline, immutable after genesis / restore
pub struct RegionMeta { …, pub props_removed: BTreeMap<PropId, ()>, pub props_added: BTreeMap<PropId, PropInstance>, pub prop_defs: BTreeMap<PrefabKey, ResolvedPrefab> }
pub struct RegionMetaV3 { /* the previous fields */ }  impl From<RegionMetaV3> for RegionMeta
pub struct PortableRegion { …, pub prop_seed: Option<Vec<(PropId, PropInstance)>> }   // Some only until first saved, like stud_ground
pub struct StoredRegion { …, pub prop_seed: Vec<(PropId, PropInstance)> }
// world-store codec
pub const FORMAT_REGION_META: u8 = 4; pub const FORMAT_REGION_META_V3: u8 = 3; pub const FORMAT_PROP_SEED: u8 = 1;
```

`stamp_prop_into_seed` returns false (seed untouched) unless the anchor's region is `region` and the prefab is a built-in Prop; the id is `PropId { region, serial: serial | PROP_WORLDGEN_BIT }`. `seed_stud_world` stores `prop_seed` and adds every instance through the non-undo genesis path. Restore: live props = `prop_seed` − `props_removed` + `props_added`; bodies and `prop_tiles` are rebuilt, not loaded; an instance whose `Builtin` prefab no longer exists is dropped with one `log::warn!` per region (count), and the region still loads. The `prop_seed` blob is stored **exactly the way `stud_ground` is** (same table shape, same once-only write through `SaveTerms`, same fence) — mirror every site found by `rg stud_ground crates/world-store crates/server crates/game/src/portable.rs`. Every `StudRegionSeed { .. }` literal already has `..Default::default()` from Plan 1.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn seeded_props_exist_after_genesis() { /* seed with 3 palms */ assert_eq!(r.data().props().len(), 3); assert!(r.data().props().keys().all(|i| i.is_worldgen())); }
#[test] fn untouched_region_saves_no_prop_diff() { let (p, terms) = r.data().portable_snapshot(&SaveTerms::default()); assert!(p.meta.props_removed.is_empty() && p.meta.props_added.is_empty());
    assert_eq!(p.prop_seed.as_ref().unwrap().len(), 3); let (p2, _) = r.data().portable_snapshot(&terms); assert!(p2.prop_seed.is_none()); }   // written once
#[test] fn save_and_restore_round_trips_the_hash() { /* remove one seeded, add one Local, snapshot, restore_portable */ assert_eq!(hash(&restored), hash(&r));
    assert_eq!(restored.data().props().len(), 3); assert!(restored.data().prop_at_tile(tile_of(local_at))); }
#[test] fn a_deleted_builtin_is_dropped_on_load() { /* StoredRegion whose prop_seed names Builtin(0xDEAD) */ assert!(restored.data().props().is_empty()); }
#[test] fn stamp_prop_into_seed_only_takes_its_own_region() { assert!(!stamp_prop_into_seed(&mut s, RC, "dev-palm", in_east_region, 0, vec![2], 7)); assert!(s.props.is_empty()); }
// world-store tests/codec.rs
#[test] fn region_meta_v3_blob_still_decodes() { let m = decode_region_meta(include_bytes!("fixtures/region_meta_v3.bin")).unwrap(); assert!(m.props_added.is_empty()); }
#[test] fn prop_seed_round_trips() { … }
```

Record `fixtures/region_meta_v3.bin` from the **current** encoder before changing the format (the same way the V2 fixture was made).

- [ ] **Step 2:** Run `just test-one game "--test it prop_portable::"` and `just test-one world-store "--test codec"` — expected: compile failures.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run both — expected: 5 + 2 passed (plus the existing codec goldens). Run the store-backed server test that saves and reloads a region (`just test-one server "--test it store_world::"`) — expected: pass. `just smoke`.
- [ ] **Step 5:** Commit: `feat(game,world-store): seed and persist props as baseline plus diff`.

---

### Task 7: Join snapshot placeholders

**Files:**
- Modify: `crates/game/src/region_wire.rs`
- Test: `crates/game/tests/prop_wire.rs` (or beside the existing region-wire tests)

**Interfaces:**
- Consumes: `WireOut` / `WireIn`, `placeholder()`, `swap_shape`, `prop_shape`, `prop_bodies`.
- Produces: `WireOut.prop_shapes: Vec<(PropId, ColliderHandle)>` (and the matching `WireIn` field). Send: for each entry of `prop_bodies` (and of the halo prop bodies after Task 8), swap its collider's shape for `placeholder()` in the slim clone and list it. Receive: rebuild with `prop_shape(def, turn, true)` and swap back; a missing definition is a deserialize error.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn snapshot_round_trip_keeps_the_hash_with_props() { /* region with 50 palms */ let bytes = encode_packet(&Wire(r.data())).unwrap(); let back: Rollback = decode(...); assert_eq!(state_hash(&back), state_hash(r.data())); }
#[test] fn prop_hulls_are_not_in_the_snapshot() { let with = snapshot_len(region_with_palms(50)); let without = snapshot_len(region_with_palms(0)); assert!((with - without) / 50 < 200); }   // bytes per palm ≈ the instance, not ~12 hulls
#[test] fn restored_colliders_are_real() { /* a ray / shape-cast against the restored region hits the trunk */ }
```

- [ ] **Step 2:** Run `just test-one game "--test it prop_wire::"` — expected: the second test fails (hulls sent whole).
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run — expected: 3 passed. `just test-one game "--test it region_wire"` (existing) — unchanged.
- [ ] **Step 5:** Commit: `feat(game): prop colliders travel as placeholders in join snapshots`.

---

### Task 8: Halo mirror

**Files:**
- Create: `crates/game/src/prop_halo.rs`
- Modify: `crates/game/src/halo.rs` (`HaloUpdate`, `halo_seed_for`, `halo_updates_for`, `apply_halo_update_safe`), `region.rs` (`take_halo_updates` dirty tracking), `state.rs` (`halo_props`, `halo_prop_bodies`), `region_wire.rs` (halo prop bodies in Task 7's list), `stud_halo.rs` (`TerrainSnapshot` if the sleeping-region seed needs props)
- Test: `crates/game/tests/prop_halo.rs`; seam case in `handoff_schedule.rs`

**Interfaces:**
- Produces:

```rust
pub struct HaloProp { pub inst: PropInstance, pub def: Option<Box<ResolvedPrefab>> }     // inst.at in the SENDER's frame; def set for Local refs
pub struct HaloUpdate { …, pub props: Vec<(PropId, Option<HaloProp>)> }
// GameData (#[undo(map)])
halo_props: BTreeMap<PropId, HaloProp>, halo_prop_bodies: BTreeMap<PropId, RigidBodyHandle>
pub fn prop_reaches(prop: &ResolvedProp, turn: u8, at: [i32; 3]) -> Vec<(i32, i32)>      // neighbour offsets its SOLID bounds cross into
pub(crate) fn apply_prop_halo_safe(&mut self, from: RegionCoords, props: &[(PropId, Option<HaloProp>)], replace: bool)
```

Export: an instance is mirrored to each neighbour in `prop_reaches`. `add_prop_safe` / `remove_prop_safe` mark the id dirty so `take_halo_updates` carries the delta; `halo_seed_for` carries the full set (`replace = true` clears the receiver's mirror from that sender first — this is what fixes a removal made while the neighbour slept, Review Focus 4). Receive: translate `at` by the sender's offset, build the fixed body with `HALO_BODY_USER_DATA`, add the solid tiles to `prop_tiles` so `tile_fill` and pathfinding see them. Mirrored props are collision and tiles only: not in `props`, not removable, not saved.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn a_prop_near_the_seam_is_exported_to_that_neighbour_only() { /* trunk 1 m inside the east edge */ let ups = r.take_halo_updates(); assert_eq!(targets(&ups), vec![RC.east()]); }
#[test] fn a_prop_mid_region_is_not_exported() { assert!(r.take_halo_updates().iter().all(|(u, _)| u.props.is_empty())); }
#[test] fn the_mirror_collides_and_blocks_tiles() { apply(&mut east, &ups); assert!(east.data().prop_at_tile(translated_tile)); assert_eq!(east.data().halo_prop_bodies().len(), 1); }
#[test] fn removal_is_mirrored_and_undo_restores_the_hash() { … }
#[test] fn a_replace_seed_drops_stale_mirrors() { /* east holds a mirror; west removed it while east slept; halo_seed_for(east) with replace */ assert!(east.data().halo_props().is_empty()); }
#[test] fn local_defs_travel_with_the_mirror() { /* Local prop */ assert!(ups[0].0.props[0].1.as_ref().unwrap().def.is_some()); }
```

Seam case: `prop_at_a_seam_blocks_a_walker_from_the_other_side` — a person ordered to walk across the seam through the trunk's tile paths around it; `rig.fingerprint()` equal across schedules.

- [ ] **Step 2:** Run `just test-one game "--test it prop_halo::"` — expected: compile failure.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run — expected: 6 passed; `just test-one game "--test it handoff_schedule::prop_"` — 1 passed; `just test-one game "--test it stud_halo::"` — unchanged.
- [ ] **Step 5:** `just test-threads` — expected: pass.
- [ ] **Step 6:** Commit: `feat(game): mirror seam-crossing props through the halo`.

---

### Task 9: Drawing props

**Files:**
- Create: `crates/client/src/renderer/prop/{mod,mesh}.rs`
- Modify: `crates/client/src/renderer/compute.rs` (`JobKind`), `renderer/bridge.rs` (apply `SetProps` / `SetPropDef`), `renderer/mod.rs` (plugin)
- Test: unit tests in `prop/mesh.rs` (client tests — ask first)

**Interfaces:**
- Consumes: `BakedProps`, `StudAssets::brick_colors(code) -> ([f32; 4], [f32; 4])`, `TriColor`, `StudMeshData`, `StudMaterials::opaque`, `part_pose`, `world_chunk_origin`-style region offset.
- Produces:

```rust
pub struct PropAssets { pub parts: BakedProps }  pub fn prop_assets() -> &'static PropAssets          // include_bytes!("…/assets/studs/props.bin"), version + count asserted against PROP_PARTS
pub enum PropLod { Hi, Lo }
pub const PROP_LO_DISTANCE: f32 = 96.0 * 8.0;                                                       // sim units; beyond it the Lo mesh
pub fn mesh_prop(prop: &ResolvedProp, colours: &[u16], lod: PropLod, assets: &PropAssets, studs: &StudAssets) -> StudMeshData
JobKind::PropMesh = 10
#[derive(Component)] pub struct PropInstanceView { pub id: PropId, pub region: RegionId }           // spawned with SessionScoped
#[derive(Resource, Default)] pub struct PropMeshCache { /* (PrefabKey-or-name-key, Vec<u16> colours, PropLod) → Handle<Mesh> */ }   // add_session_reset
#[derive(Resource, Default)] pub struct PropDefs { /* PrefabKey → Arc<ResolvedPrefab>, from SetPropDef */ }                        // add_session_reset
```

Colour per triangle: `Own` → the part's colour (`Fixed(c)` or `colours[slot]`), `Edge` → its edge colour, `Fixed(c)` → `c`. The mesh is in the prop's frame at turn 0; the instance entity's `Transform` carries position (region offset + `at`) and the quarter turn. One Bevy entity per instance sharing the cached mesh handle and `StudMaterials::opaque`, so identical props batch. The mesh job runs through the compute pool (`JobKind::PropMesh`, payload: the resolved prop, colours, lod — workers have no ECS) on native and wasm.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn slot_colour_reaches_the_vertices() { let a = mesh_prop(&pine(), &[2], PropLod::Hi, pa, sa); let b = mesh_prop(&pine(), &[288], PropLod::Hi, pa, sa); assert_ne!(a.colors, b.colors); assert_eq!(a.positions, b.positions); }
#[test] fn fixed_colour_ignores_slots() { /* trunk Fixed(70) */ assert!(a.colors.contains(&sa.brick_colors(70).0)); }
#[test] fn lo_has_fewer_triangles() { assert!(lo.indices.len() < hi.indices.len()); }
#[test] fn every_listed_part_has_a_mesh() { assert_eq!(prop_assets().parts.parts.len(), game::PROP_PARTS.len()); }
```

- [ ] **Step 2:** Implement.
- [ ] **Step 3:** `just smoke`; ask the user, then `just test-one client "--lib renderer::prop"` — expected: 4 passed. `just web` builds (wasm path compiles).
- [ ] **Step 4: Verify by running:** `just prefab-editor --game dev-arch`, then from the F3 dev tab (or a temporary debug key) send `PrefabCommand::Stamp { prefab: Builtin(prefab_name_key("dev-palm")), anchor: editor_anchor(), turn: 0 }` — expected: a palm with tilted trunk joints and fanned fronds, correctly lit, the player collides with the trunk and walks through fronds.
- [ ] **Step 5: Measure (ask the user first):** a temporary genesis that seeds ~2000 palms in one region; `just diag` in release. Record entity count, frame time, CPU-vs-GPU verdict in the commit message. If entity count is the cost, switch to one merged mesh per sim chunk of props (same `mesh_prop` output, concatenated) and re-measure.
- [ ] **Step 6:** Commit: `feat(client): draw prop instances`.

---

### Task 10: Picking props

**Files:**
- Create: `crates/client/src/renderer/prop/pick.rs`
- Modify: `crates/game/src/prop_ops.rs` (`raycast_props_seen`), `crates/client/src/renderer/stud/build.rs` only where the cursor target is chosen
- Test: `crates/game/tests/prop_ops.rs` (ray cases)

**Interfaces:**
- Produces:

```rust
// impl Rollback
pub fn raycast_props_seen(&self, origin: Vector, dir: Vector, reach: f32) -> Option<(PropId, PropRayHit)>     // own props; bounds test first, then raycast_prop
// client
#[derive(Resource, Default)] pub struct PropTarget { pub hit: Option<(PropId, usize /*part*/, f32 /*t*/)> }  // add_session_reset
pub fn prop_pick(...)   // system: cursor ray in each loaded region's frame; nearest hit; cleared when a stud hit is nearer
```

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn nearest_of_two_props_is_picked() { … assert_eq!(id, near_id); }
#[test] fn a_frond_is_pickable_though_not_solid() { assert!(!def.parts[hit.part].solid); }
#[test] fn a_ray_past_all_props_hits_nothing() { assert!(r.data().raycast_props_seen(o, d, 500.0).is_none()); }
```

- [ ] **Step 2:** Run `just test-one game "--test it prop_ops::raycast"` — expected: compile failure.
- [ ] **Step 3:** Implement both sides.
- [ ] **Step 4:** Run — expected: 3 passed; `just smoke`.
- [ ] **Step 5:** Commit: `feat: pick props with the cursor`.

---

### Task 11: Editing props in the editor

**Files:**
- Create: `crates/client/src/prefab_editor/{prop_doc,prop_tools}.rs`, `crates/slint-ui/ui/prop-tools.slint`
- Modify: `crates/client/src/prefab_editor/{session,save,library_ui}.rs`, `crates/slint-ui/ui/{prefab-library,game}.slint`
- Test: unit tests in `prop_doc.rs` (pure; client tests — ask first)

**Interfaces:**
- Consumes: `PropFile`, `resolve`, `PrefabCommand::{Register, Stamp, RemoveProp, Deregister}`, `PropTarget`, `PROP_PARTS`, `EditorSession`.
- Produces:

```rust
pub struct PropDoc { pub file: PropFile, undo: Vec<PropFile>, redo: Vec<PropFile>, pub selected: Option<usize> }
pub const PROP_UNDO_DEPTH: usize = 200;
pub enum PropEdit { Add { part: String, pos: [i32; 3] }, Delete(usize), Move { part: usize, by: [i32; 3] }, Turn { part: usize, axis: u8, degrees: i16 },
                    SetColour { part: usize, colour: PropColour }, SetSolid { part: usize, solid: bool },
                    AddSlot { name: String, default: u16 }, SetSlot { slot: usize, name: String, default: u16 }, RemoveSlot(usize), SetFoot([u8; 2]) }
impl PropDoc { pub fn apply(&mut self, e: PropEdit) -> bool; pub fn undo(&mut self) -> bool; pub fn redo(&mut self) -> bool }
#[derive(Resource)] pub struct PropEditor { pub doc: PropDoc, pub shown: Option<PropId>, pub preview_colours: Vec<u16>, pub needs_refresh: bool }   // add_session_reset
```

`apply` returns false and records nothing when the edit is a no-op or would make the file fail `resolve` (e.g. a 257th part, `RemoveSlot` of a slot in use). `Turn` composes a rotation about a world axis (0 = x, 1 = y, 2 = z) using `libm`. Steps: move by a stud (20 LDU) / plate (8 LDU), or 1 LDU with **Shift**; turn by 90°, or 1° with **Shift**.

Showing the document: whenever `needs_refresh`, resolve the document, send `RemoveProp(shown)`, `Register`, `Stamp` at `editor_anchor()`, `Deregister`; `shown` becomes the new instance id (read back from the next `SetProps`). `preview_colours` overrides slot defaults for the shown instance only.

Tools and keys (shown in the panel): palette of `PROP_PARTS` (click = `Add` at the cursor's hit on the prop or baseplate, snapped to stud / plate); click a part = select (`PropTarget`); arrows / PgUp / PgDn = move; `X` `Y` `Z` = turn about that axis; `Del` = delete; `Ctrl+Z` / `Ctrl+Y` = undo / redo; panel fields for colour (fixed code or slot), solid toggle, slot list with defaults and preview colours, foot size. Stud build mode is **off** in a prop session.

Save (`save.rs`): a Prop session writes `PropEditor.doc.file` directly (no world capture). The library's **New prop** button is enabled; `Open` on a Prop file starts a prop session.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn add_move_turn_undo_redo() { d.apply(Add { part: "3471".into(), pos: [0, 0, 0] }); d.apply(Move { part: 0, by: [20, 0, 0] }); assert_eq!(d.file.parts[0].pos, [20, 0, 0]);
    d.apply(Turn { part: 0, axis: 2, degrees: 15 }); assert!(d.undo() && d.undo()); assert_eq!(d.file.parts[0].pos, [0, 0, 0]); assert!(d.redo()); assert_eq!(d.file.parts[0].pos, [20, 0, 0]); }
#[test] fn four_quarter_turns_return_to_the_start() { /* within 1e-4 per component */ }
#[test] fn invalid_edits_are_refused_and_not_recorded() { /* 257th part; RemoveSlot in use; Add unknown part */ assert!(!d.apply(e)); assert!(!d.undo()); }
#[test] fn a_new_edit_clears_redo() { … assert!(!d.redo()); }
#[test] fn undo_depth_is_bounded() { /* 250 edits */ assert_eq!(undo_count(&mut d), 200); }
```

- [ ] **Step 2:** Implement.
- [ ] **Step 3:** `just smoke`, `just slint-wasm`; ask the user, then `just test-one client "--lib prefab_editor::prop_doc"` — expected: 5 passed.
- [ ] **Step 4: Verify by running** `just play` → Prefab editor → New prop: add a base, three trunk segments each tilted 10°, a crown in slot "foliage"; change the preview colour; undo and redo; Save; Exit; reopen — identical. Then `just prefab-editor --game dev-palm`: move one frond, Save, `git diff` shows one part changed; restore the file.
- [ ] **Step 5:** Commit: `feat(client,ui): edit prop prefabs in the editor`.

---

### Task 12: Prop to falling body

**Files:**
- Create: `crates/game/src/prop_body.rs`
- Modify: `crates/game/src/state.rs` (`Ecs`, `GameDataUpdateKind`, `build_body_safe`), `protocol.rs` (`ColliderSpec`), `migrate.rs` (`Carried`, `CarriedV3`, `carry_components`), `stud_body.rs` (`tick_stud_bodies_safe` or a sibling tick), `crates/world-store/src/codec.rs` (`FORMAT_ENTITY_BUNDLE`), `crates/client/src/renderer/prop/mod.rs` + `bridge.rs`
- Test: `crates/game/tests/prop_body.rs`; world-store codec golden

**Interfaces:**
- Produces:

```rust
pub struct PropBody { pub def: Arc<ResolvedPrefab>, pub turn: u8, pub colours: Vec<u16>, pub policy: SettlePolicy, pub spawned_tick: u64, pub still_ticks: u16 }   // serde inline; StateHash: def key + the rest
Ecs { …, prop_body: Component<crate::prop_body::PropBody> }
Carried { …, pub prop_body: Option<PropBody> }      // + CarriedV3 (decode-only) → Carried; FORMAT_ENTITY_BUNDLE = 4
ColliderSpec::Prop                                  // appended LAST: "rebuild from the entity's carried prop_body"
GameDataUpdateKind::AddPropBody(EntityKey, Pose, Box<PropBody>)
// impl Rollback
pub fn detach_prop_safe(&mut self, id: PropId, policy: SettlePolicy, angvel: Vector, pivot: Option<Vector>) -> Option<EntityKey>
```

`detach_prop_safe`: `remove_prop_safe(id)`; then the `detach_studs_safe` spawn sequence with these differences — compound = `prop_shape(def, turn, solid_only = false)` (fronds belong to the falling shape, per the tree design); mass = `mass_properties_of_boxes(&prop_mass_boxes(..))`, `local_com` zeroed the same way; `EntityKind::Body`; `AddPropBody` instead of `AddStudBody`; `ecs.prop_body.set_safe`. `collider_spec_of` returns `ColliderSpec::Prop` for an entity with a `prop_body`; `build_body_safe` rebuilds the shape from the carried `PropBody` (never from `ColliderSpec::shared_shape`, which panics for `Prop`). Settling reuses the stud-body rule (10 still ticks or the 10 s timeout → `remove_entity_safe`; `DropItems` drops nothing yet — yield is the tree work's). The client draws the body with `mesh_prop` on the entity's pose.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn detach_then_undo_restores_the_hash() { let b = hash(&r); let e = r.data_mut().detach_prop_safe(id, SettlePolicy::Despawn, tip, None); /* via a test event */ assert!(r.data().props().is_empty()); … assert_eq!(b, hash(&r)); }
#[test] fn the_body_falls_and_settles_then_despawns() { for _ in 0..(12 * ticks_per_second) { tick(&mut r); } assert!(r.data().entity_alive(e) == false); }
#[test] fn the_tile_is_free_after_detach() { assert!(!r.data().prop_at_tile(tile_of(at))); }
#[test] fn bundle_round_trip_rebuilds_the_compound() { let bundle = bundle_of(&r, e); let bytes = encode(&bundle); let e2 = other.apply_arrival(decode(&bytes)).unwrap(); assert_eq!(compound_len(&other, e2), all_hull_count(&palm())); }
#[test] fn a_falling_prop_survives_save_and_restore() { /* portable_snapshot mid-fall → restore_portable */ assert_eq!(hash(&restored), hash(&r)); }
// world-store
#[test] fn entity_bundle_v3_blob_still_decodes() { … }
```

Seam case in `handoff_schedule.rs`: `falling_prop_crosses_a_seam` — detach with a velocity toward the seam; fingerprint equal across schedules; exactly one body exists afterwards.

- [ ] **Step 2:** Run `just test-one game "--test it prop_body::"` — expected: compile failure.
- [ ] **Step 3:** Implement. Record the `entity_bundle_v3.bin` golden before bumping the format.
- [ ] **Step 4:** Run — expected: 5 passed; world-store codec — pass; `handoff_schedule::falling_prop` — pass.
- [ ] **Step 5:** `just test-threads`, `just determinism-wasm`, `just test-one game "--test it physics_goldens::"` — expected: all pass, no golden re-recorded.
- [ ] **Step 6:** Commit: `feat(game): detach a prop into one falling body`.

---

## Done when

- `just test-one game "--test it prop"` and `"--test it prefab_prop::"` pass; `just test-threads`, `just determinism-wasm`, the physics goldens and `just smoke` pass; `just ldraw-bake --check` exits 0.
- `just prefab-editor --game dev-palm` shows, edits and saves the palm; a new prop can be authored from the menu.
- A region with seeded props saves, reloads and joins with the per-prop costs measured in Task 7.
- Update `CLAUDE.md` (Prefabs entry: prop catalog is append-only, `just prefab-import-ldr`, hull re-bake is a physics change, props are baseline-plus-diff in the store) and revisit `docs/superpowers/specs/2026-11-11-lego-tree-prefabs-design.md` against what now exists (its "Open when this is revisited" list).
