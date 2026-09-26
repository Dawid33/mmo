# Vendored forks: can we update to latest upstream? (investigation, 2026-09-22)

Read-only investigation; nothing changed. Pristine upstream sources used for diffs
were unpacked in a scratch dir (re-fetch with
`curl -sL https://static.crates.io/crates/<n>/<n>-<v>.crate | tar xz`).

## Versions

| Fork | Vendored | Latest upstream | Local patch (whitespace-insensitive) |
|---|---|---|---|
| rapier | git snapshot between 0.30.1 and 0.31.0 (imported 346659f, 2025-12-26) | 0.35.3 | 70 files, +3.6k/−1.6k |
| parry | 0.25.3 | 0.31.1 (rapier 0.35.3 pins parry 0.30.2) | 166 files, +1.7k/−1.1k |
| nalgebra | 0.34.1 | 0.35.0 | ~40 real lines (+~290 doc-link rewrites) |
| simba | 0.9.1 | 0.10.2 | +634/−43 |
| approx | 0.5.1 | 0.5.1 | +23/−5 (the ~2.1k raw diff is CRLF→LF) |
| ordered-float | 5.1.0 | 5.5.0 | +9 |
| slotmapd | 1.0.11 | 1.0.11 (dead; slotmap is 1.1.1) | +193/−67 |
| block-mesh | 0.2.0 | 0.2.0 (unmaintained since 2022) | +52/−11 |

## The deciding fact

Our determinism design is `parry::Real = OrderedFloat<f32>` (`crates/parry/src/lib.rs`).
The nalgebra/simba/approx/ordered-float forks exist mainly so nalgebra accepts that
scalar with all math routed through `libm`.

**Upstream parry 0.26 and rapier 0.32 moved from nalgebra to glam (`glamx`).** parry 0.31
is `pub use f32 as Real; Vector = glamx::Vec3`, so there is no scalar slot for
OrderedFloat. Upstream determinism is now `enhanced-determinism` =
`simba/libm_force` + `glamx/libm` + `glamx/scalar-math`. The scalar-math part (0.29) fixes
non-determinism from NEON/SSE2 dot products. rapier 0.35 also makes `parallel` combined with
`enhanced-determinism` bit-identical across thread counts. The OrderedFloat design cannot
be carried forward.

rapier 0.35 also rewrote exactly what StepJournal patches:
- narrow phase: now a persistent contact graph;
- broad phase;
- island manager: persistent islands, with a new serde format;
- solver: staged and multithreaded;
- CCD: sweep-based;
- NaN quarantine.

`narrow_phase.rs`, `physics_pipeline.rs`, `broad_phase_bvh.rs` and `island_manager.rs`
no longer exist as files. Only `arena`, `coarena`, `collider_set` and `rigid_body_set`
are nearly unchanged.

## Local patch themes (rapier + parry)

| Theme | ~Lines | Conflict with upstream latest |
|---|---|---|
| OrderedFloat scalar (determinism) | 2.2k across ~220 files | total: incompatible with glam |
| StepJournal / rollback | 2.0k | total: redesign against the new narrow phase, islands and solver |
| Hashing / digests (pairs_hash, BVH bytes, voxels content hash) | 400 | high |
| Bug fixes (3 shape-cast defects, KCC snap gate, voxels manifolds) | 650 | medium (parry query files are mostly glam syntax churn) |
| Perf (fixed-body mass-props skip, fixed-fixed pair filter, voxel neighbourhoods) | 250 | medium |
| sentinel_usize serde (wasm32) | 35 | high (snapshot formats changed) |

Upstream covers our shape-cast fixes only partly:
- The grazing fallback in parry 0.28 applies only when GJK hits.
- `FIX_INTERNAL_EDGES` in parry 0.31 handles compounds only, not flush voxels or separate colliders.
- The voxels search domain has no upstream equivalent.

Game-side migration if the math types change: ~16 files import rapier/parry directly. Rough call sites:
- `Isometry`: ~66
- `OrderedFloat`: ~59
- `na::`: ~30
- `vector!`/`point!`/`Real::from`: ~190

## Small forks

- **slotmapd:** carries the `clone_from` memory leak that slotmap 1.1 fixed (occupied slot cloned from a vacant one), plus its panic guard. **Take it now** (~15 lines). Do not rebase onto slotmap: that would lose slotmapd's deterministic serialization.
- **nalgebra:** lacks the `SymmetricEigen` fix #1210 (0.34.2). parry uses it in mass props, OBB, voxel_set and convex_hull3. It is a ~15-line cherry-pick, but it changes sim numerics, so fixtures and hashes need re-baselining. nalgebra 0.35 needs simba 0.10, and simba 0.10 dropped `paste`, so the OrderedFloat impls would need re-porting by hand (1–2 days, little value).
- **simba:** `libm_force` is upstream (0.9.1 and 0.10.2); the fork only turns it on by default. Its real content is the OrderedFloat impls and the `MinMax` trait split.
- **ordered-float 5.5:** removed `#[repr(transparent)]`. parry's BVH byte-hash (`bvh_tree.rs`) relies on that layout. Stay on 5.1, or re-add the attribute when bumping.
- **approx:** only exists for the OrderedFloat impls. Leave it.
- **block-mesh:** at runtime it only supplies the `ndshape::ConstShape` re-export and the `Voxel`/`MergeVoxel` traits. `greedy_quads` survives only in tests (`client/tests/mesh_parity.rs`, `worldgen/tests/region_load_cost.rs`). Possible cleanup: depend on `ndshape` directly and keep the fork as a test-only oracle.

Lockfile duplicates are harmless:
- approx 0.5.1 appears twice: the fork, plus the registry copy for bevy_math.
- ordered-float 2.x/4.x come from serde-value and wgpu-hal.
- The 16 glam versions under nalgebra are optional and never compiled.

## Recommendation

1. **Stay on the nalgebra-era forks.** rapier 0.31.0 / parry 0.25.3 is the end of that line. Moving to exact rapier 0.31.0 adds only contact softness, the pin-slot joint (2D) and `contact_id`; not worth it.
2. **Cheap fixes now:** the slotmapd `clone_from` fix; optionally nalgebra #1210, with a re-baseline.
3. **Cherry-pick upstream fixes by hand onto the nalgebra code**, each pinned by the existing seam/walking tests (~1–3 days). Candidates:
   - parry 0.30.2 GJK/EPA fixes: #396, #193, #180, #429, #106, and the large-coordinate EPA fixes;
   - Voxels #404 and #373 (diff them against our manifold patch first);
   - parry 0.28: `Voxels::scaled` BVH fix and `optimize_incremental` panic fix; also #403 (non-deterministic `optimize_incremental` after deserialize), relevant only if the optimizer is re-enabled;
   - rapier 0.35: signed-zero impulse canonicalisation, same-body/group pair skip.
4. **A full re-port to rapier 0.35.3 + parry 0.30.2 on glam is its own project (~4–8 weeks).** It means:
   - redesigning StepJournal;
   - re-porting the shape-cast fixes;
   - re-proving cross-machine and wasm32 determinism on glam scalar-math;
   - redoing state hashing on f32 bit patterns;
   - migrating the game code;
   - breaking the wire and snapshot format.

   Worth it only for the new solver/islands/CCD, `parallel` with determinism, or to stop maintaining four math forks.

## Landed (2026-09-26, plan `docs/superpowers/plans/2026-09-22-physics-fork-fixes.md`)

Hand-ported upstream fixes in the nalgebra-era forks. Each one is pinned by a test in `crates/game/tests/` that becomes a port-parity check for the rapier 0.35 upgrade. No hard-coded expected value moved: the movement rig (`shape_cast_grazing`, `walking_smoothness` incl. slow, `kinematic`, `step_journal`, `sunk_body`, `jump_arc`, `slime`, `random_ops`, `hash_restore`) and `just test-threads` stayed green.

- `47e4ebe` **slotmapd**: `Slot::clone_from` drops the overwritten value (slotmap #137), plus the `PanicOnDrop` guard on map-level `clone_from`. Test: `hash_restore::slotmap_clone_from_drops_overwritten_values`.
- `3b276a8` **nalgebra**: stable 2×2 deflation basis in `SymmetricEigen` (#1210). Test: `nalgebra_fixes.rs`, with upstream's per-element 1e-5 bound; the recompose error was 11.6 before.
- `6d295d9` **parry voxels**: the #404 and #373 fixes were already local. They are now pinned by `voxels_upstream.rs`; each test was checked to fail with its fix reverted.
- `fa08f24` **parry GJK/EPA**:
  - GJK stalls are classified by the best lower bound (separated / touching / penetrating), with deterministic direction perturbation (#193, #396);
  - EPA bootstraps a 0-simplex from an axis support point (#315).
  - Tests: `gjk_upstream::issue_{193,396,70,315}_*`.
- `80ff452` **parry**: the last-chance shape-cast TOI is refined with the simplex witnesses (#429). Test: `issue_429_*`, upstream's sweep, ≤ 0.2 mm.
- `c25d6ae` **parry**:
  - the GJK ray-cast containment tolerance (#180) and the EPA degenerate-face tolerance (#415 / PR #420) now scale with coordinate magnitude;
  - test: `issue_415_capsule_sunk_into_wide_cuboid_gets_a_contact`;
  - #180 test: `issue_180_grazing_cast_is_not_ulp_sensitive`, upstream's 2D MRE embedded at z = 0. Two x values (bits `0x417b0843`/`0x417b0844`) miss with the scaled tolerance reverted. Added in the review follow-up. c25d6ae's message says `issue_70` guards #180; it doesn't, since `intersection_test` never reaches `minkowski_ray_cast`. `issue_70` is fixed by the stall classification in fa08f24.

Deliberately not ported: C4/#106, the 1e-4 small-TOI fallback, BVH #419/#409/#403, and signed-zero impulses (see the plan).

**Known open item: EPA seeding.** When GJK hands EPA a 1-simplex passing through the origin, EPA can only expand toward one side. Faces whose origin projection falls outside them never enter the heap. Example: a capsule sunk 1 mm into a 16-unit cuboid at (0.1, −0.1) gets normal −Y and depth 2.4 from `query::contact`.
- **Cause:** a pre-existing weakness, now reached more often because the GJK stall classification hands stalls over to EPA. Upstream's EPA has the same code.
- **Net effect:** in a 2000-case sunk ball/capsule sweep, wrong-direction normals dropped from 128 to 30 (46 → 1 at our 8/16-unit sizes) and bad contacts from 609 to 372.
- **Game impact:** the path is live. The KCC's floor-snap and penetration casts (in the rapier fork's `control/character_controller.rs`, ~:323/447/744-815, with `compute_impact_geometry_on_penetration: true`; `movement::depenetration_lift` starts from a pose it has checked is clear, so it reaches this path only through the skin, via `target_distance`) fall back to `contact_support_map_support_map`, and from there to GJK and EPA, whenever the capsule starts penetrating or within `target_distance` (`shape_cast_support_map_support_map.rs`). The net effect of the ports on that path is an improvement: the final review's probe of a capsule sunk 0.5–10 mm into 8/16-unit cuboids moved horizontal normals in 9 of 40 cases to ±Y in all 40. The flipped-normal configuration above is still possible there.
- **Found in review follow-up (pre-existing, fixed by the upgrade):** the player capsule centred exactly over a block and touching it exactly (distance 0) gets normal (-0.707, 0, 0.707) from `query::contact`. The fork's GJK stops at distance exactly 0 and returns a stale direction. Pristine parry 0.25.3 gives identical output, so this is not a Plan 1 regression; parry 0.31.1 gets +Y. Rarely reached in play, since the KCC keeps a skin gap. Pinned for the switch: upgrade plan Task 5.4. The off-centre case is pinned against upstream's exact values in `gjk_upstream::capsule_touching_cuboid_matches_upstream`.
- The old `issue_315_ball_exactly_touching_cuboid` never reached GJK/EPA (a ball against a convex shape uses point projection). It was replaced by that capsule parity pin. #315's bootstrap is covered by the two cuboid tests, which were probed to reach it.
- **Also left as upstream behaviour:** GJK's "touching" band (`best_min_bound >= -eps_rel * support_scale`, about 1 cm wide at 16-unit scale) returns the previous simplex's imprecise witnesses for very shallow contacts. Example: a capsule sunk 0.5 mm reports dist +2.7e-4, the wrong sign. The same happens at base.
