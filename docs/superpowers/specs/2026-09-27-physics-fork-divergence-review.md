# Physics fork divergence review (rapier 0.36.0 / parry 0.31.1)

Upgrade plan `docs/superpowers/plans/2026-09-22-rapier-035-upgrade.md`, Task 6.3.
Measured 2026-09-27 at `60393db` (develop). Raw data (diffs, per-hunk TSV,
scripts) was produced in the job scratch dir and is not kept; everything
below can be regenerated with the commands in "Method".

## Totals

Two independent baselines, which agree exactly (same files, same numstat per
file):

- **A**: the pristine import commit `bb7d5b5` (`crates/rapier-next`,
  `crates/parry-next`) vs HEAD `crates/rapier`, `crates/parry`.
- **B**: fresh shallow clones of upstream `v0.36.0` (rapier) and `v0.31.1`
  (parry) vs HEAD. The import was the upstream repo root trimmed to the 3D f32
  crate (top-level docs, `Cargo.toml`, `src/`, `crates/<x>3d/`, parry's
  `assets/`); every kept file was byte-identical to the tag, so A = B.

No newer release exists: `v0.36.0` / `v0.31.1` are the newest tags of rapier /
parry, so nothing upstream overlaps the files we patch.

| | files | lines | of which `src/` | markers (`mmo-fork` lines) |
|---|---|---|---|---|
| rapier (new fork) | 51 | +1373 / −71 | 49 files, +1330 / −40 | 213 |
| parry (new fork) | 44 | +658 / −208 | 43 files, +590 / −28 | 97 |
| rapier (old fork, vs 0.31 release commit) | 71 | +3621 / −1639 | 70 files, +3612 / −1631 | 0 |
| parry (old fork, vs v0.25.3) | 218 | +2042 / −1285 | 166 files, +1876 / −1118 | 0 |

The `src/` divergence shrank to **37 %** (rapier, added lines) and **31 %**
(parry), and deletions almost vanished (−1631 → −40, −1118 → −28): the new
forks add, they barely rewrite. Every touched file carries at least one marker.

### By category (273 hunks, +2031 / −279)

| category | hunks | lines | where |
|---|---:|---|---|
| `StateHash` derives, skips, hand impls | 196 | +468 / −28 | 115 rapier, 81 parry; 158 derive lines |
| Rollback journal + arena inverses + memo invalidation | 41 | +844 / −0 | rapier only; `step_journal.rs` alone +369, the set accessors +228, `physics_pipeline/mod.rs` +130 |
| Shape casts + KCC | 26 | +581 / −35 | voxels cast +277/−25, continuous surfaces +195, support-map grazing +66, KCC +38/−9 |
| Fixed-body mass-props skip | 3 | +24 / −4 | `rigid_body_components.rs`, `user_changes.rs` |
| Kinematic `can_sleep(false)` gate | 1 | +3 / −1 | `rigid_body_components.rs` |
| Packaging | 6 | +111 / −211 | two self-contained `Cargo.toml`s, the re-minted `snapshot_portability` golden |
| Other / unexplained | 0 | | |

`StateHash` is 72 % of the hunks but 23 % of the lines; the journal is the
largest body of code; the physics behaviour patches are ~30 % of the lines.

### Markers

- 260 hunks carry a marker in their own lines.
- 6 are *weakly* marked (marker elsewhere in the same call/function): the five
  `cast_shape(` → `cast_shape_on_continuous_surfaces(` call sites in
  `character_controller.rs` (marker on an argument 10–13 lines below), and the
  voxels trailing-row search-domain rewrite
  (`shape_cast_voxels_shape.rs:326-355`, covered by the file-header marker).
- 7 have **no** marker nearby — all mechanical consequences of marked
  changes: `depenetrate: true` in `KinematicCharacterController::default`
  (`character_controller.rs:225`), `capture: None` in the four set
  constructors (`rigid_body_set.rs:99,119`, `collider_set.rs:71,83`),
  `seen_journal_reverts: 0` (`physics_pipeline/mod.rs:129`), and one removed
  line in the voxels rewrite (`shape_cast_voxels_shape.rs:370`).

### Dead / leftover

None: no debug prints, `dbg!`, TODO/FIXME, commented-out code or
`#[allow(dead_code)]` among the added lines (the one added allow is
`clippy::too_many_arguments` on `step_journaled`). Every fork-added `pub` item
has a caller outside its file. Test-only public API (called from physics-next
or game tests, never production): `Arena::revert_remove`,
`StepJournal::{saved_body_count, saved_collider_count, broad_captured,
joints_captured, live_digest, is_empty}`. They must stay `pub` because
physics-next is a separate crate.

## Hunk table (per file, by category)

`StateHash`-only files are grouped; the pinning tests are in
`crates/physics-next/tests/` unless a path says otherwise.

| file | category | hunks | lines | pinned by |
|---|---|---:|---|---|
| rapier `pipeline/step_journal.rs` (new) | journal | 1 | +369 | `step_journal.rs`, `journal_fuzz.rs`, `journal_parallel.rs`; game `rollback_restore`, `random_ops`, `hash_mode` |
| rapier `geometry/collider_set.rs` | journal/arena | 14 | +113 | `arena_revert.rs`, `step_journal.rs`; game `hash_restore.rs` |
| rapier `dynamics/rigid_body_set.rs` | journal/arena | 13 | +115 | same |
| rapier `pipeline/physics_pipeline/mod.rs` | journal + memo invalidation | 4 | +130 | `pipeline_memo.rs`, `step_journal.rs` |
| rapier `data/arena.rs`, `data/modified_objects.rs` | arena inverses | 2 | +58 | `arena_revert.rs`; game `hash_restore.rs` |
| rapier `island_manager/manager.rs` | `journal_reverts` epoch | 1 | +9 | `pipeline_memo.rs` |
| rapier `impulse_joint_set.rs`, `multibody_joint_set.rs` | journal quiescence probes | 2 | +29 | `step_journal.rs` |
| rapier `staged_island_solver/{mod,worker}.rs` | journal debug assert (Review Focus 1) | 2 | +16 | `journal_fuzz.rs` (debug builds) |
| rapier `pipeline/mod.rs` | module wiring | 2 | +5 | — |
| rapier `pipeline/query_pipeline.rs` | continuous-surfaces cast + TOI tie pass | 2 | +195 | `continuous_surfaces.rs` (`a_touching_tie_resolves_the_same_in_every_bvh_layout`, `the_tie_pass_casts_no_collider_twice`); game `physics_goldens` terrace, `shape_cast_grazing` |
| rapier `control/character_controller.rs` | KCC snap gate, flush seams, `depenetrate` (D7) | 16 | +38 / −9 | `upstream_movement_probe.rs`, `walking_smoothness.rs`, `sunk_body.rs`; game `walking_smoothness`, `sunk_body`, `jump_arc` |
| parry `shape_cast/shape_cast_voxels_shape.rs` | voxels cast: search domain, internal edges, resting contact, D8 | 4 | +277 / −25 | `voxels_cast.rs`, `voxels_upstream.rs` |
| parry `shape_cast/shape_cast_support_map_support_map.rs` | grazing-start policy | 3 | +66 | `voxels_cast.rs`, `upstream_movement_probe.rs`; game `shape_cast_grazing` |
| parry `shape_cast/mod.rs` | probe-taking cast export | 1 | +5 / −1 | (used by the continuous-surfaces cast) |
| rapier `rigid_body_components.rs` + `user_changes.rs` | fixed-body mass-props skip | 3 | +24 / −4 | `fixed_mass_props.rs`; upstream `snapshot_portability` (re-minted golden); `just determinism-wasm` |
| rapier `rigid_body_components.rs` | kinematic `can_sleep(false)` | 1 | +3 / −1 | `kinematic_sleep.rs` |
| parry `shape/voxels/voxels.rs` | `StateHash` + cached content hash | 6 | +93 | `voxels_hash_cache.rs`, `state_hash_parity.rs` |
| rapier `broad_phase_bvh/mod.rs`, `island_manager/persistent.rs` | `StateHash` (sorted `pairs` / `joint_link_locs`) | 16 | +77 / −26 | `state_hash_parity.rs` (serde round trip) |
| parry `bvh_tree.rs` | `StateHash` (whole tree) | 6 | +42 | `state_hash_parity.rs` |
| 78 other files | `StateHash` derive / skip lines | ~160 | ~+250 | `state_hash_parity.rs`, `state_hash_wasm.rs` |
| rapier / parry `Cargo.toml`s | packaging | 5 | +107 / −210 | the build |
| rapier `tests/snapshot_portability.rs` | re-minted golden (Task 3.1) | 1 | +4 / −1 | itself |

No patch is **superseded** by upstream (no newer release; the pair filter that
the old fork carried is already upstream in 0.36 and was never re-ported).

## Specific reviews

### H10 TOI tie pass (`QueryPipeline::cast_shape_on_continuous_surfaces`)

Necessary: without it the hit among colliders touching at TOI 0 is whichever
BVH leaf traversal meets first, and `length_unit` (D6) reshapes the tree, so
the terrace golden moved (−0.61 units in one tick). Ranking is total and
deterministic (blocking before separating, least-opposing, lowest collider
index) and reuses `find_best`'s casts (review I1). Pinned twice.

Open question (H10 deferred minor, **decision for the user**): "least
opposing" depends on the motion. A grounded body whose only motion is gravity,
touching both the floor and a 2–6-voxel ledge edge (≥ 45°, a wall for the
KCC), picks the edge → one ~0.67-unit (~4 cm) horizontal nudge when the player
stops; `handle_stairs`' ramp check likewise prefers the steeper normal.
Ranking blocking ties by `normal · up` first would be KCC-specific (needs an
`up`) and changes a pinned golden — recommend leaving it until the nudge is
seen in play, then add a gravity-only ledge pin with the change.

### Kinematic sleep gate

Correct and minimal. Upstream 0.36 lets kinematic bodies sleep at exactly-zero
velocity even with `can_sleep(false)`. The gate checks only
`normalized_linear_threshold >= 0`; `RigidBodyBuilder::can_sleep(false)` and
`cannot_sleep()` set both thresholds to −1, so it is harmless today, but the
dynamic branch also requires `angular_threshold >= 0` — mirror it (cleanup C2).
Clean upstream bug report; worth a PR.

### Stale upstream docstring

`IslandManager::wake_up` (`island_manager/sleep.rs:30`) says "Only affects
dynamic bodies (kinematic and fixed bodies don't sleep)"; the code wakes every
non-fixed body, and kinematic bodies do sleep (the bug above). Upstream text,
not ours; fix it in the upstream PR, or as a doc-only marked hunk (C3).

### physics-next's Walker

`crates/physics-next/tests/common/mod.rs` keeps glam copies of
`game::movement::{depenetration_lift, character_controller, sweep_character}`
(written before the switch, when game still ran on the old fork). Since the
switch both crates compile against the same `crates/rapier`, so the copies are
pure duplication and can silently drift from the game. The Walker can call
`game::movement::sweep_character(&w.broad_phase, &w.narrow_phase, &w.bodies,
&w.colliders, handle, motion, desired)` directly (~60 lines deleted; C4).

### `StateHasher` cost (`crates/game/src/lib.rs:107`)

A 4 KiB stack buffer (`STATE_HASHER_CHUNK`). `new()` zero-fills it for every
per-entry Delta digest (the macro creates one per entry), and
`Hasher::finish()` clones the whole struct to finalize a copy. Production runs
`HASH_MODE=off`, so this is test/debug cost. Cheaper: `finish()` = clone of the
`crc32fast::Hasher` + `update(&buf[..len])`; `new()` could use a smaller
buffer for digests. **Unmeasured** (benchmarks need the user's go-ahead) — C6
is "measure first".

### Outside the forks

- **physics-next** still earns its keep: it is the only place the fork
  patches are pinned in isolation (18 test files), and the forks are path
  dependencies, not workspace members, so their tests have no other home. The
  name is stale since the switch (nothing is "next" any more); renaming it
  (e.g. `physics-tests`) touches the justfile, CLAUDE.md and memory — optional
  (C7). Its `Cargo.toml` still cites the deleted `just determinism-wasm-next`
  (C5).
- **state-hash / state-hash-derive**: no leftovers; `no_std`, used by game,
  macros, slotmapd and both forks.
- **slotmapd** (Uriopass/slotmapd v1.0.11, itself a fork of orlp/slotmap):
  `src/` +283 / −79 over 7 files; adds `revert_insert`, `revert_remove`,
  `alloc_fingerprint`, `alloc_fingerprint_head` (all used by `crates/macros`
  and `game/tests/hash_restore.rs`) plus `StateHash` impls; serde made
  non-optional. Necessary as long as the undo log reverts slot-map inserts.
- **Game-side workarounds**: `movement::depenetration_lift` is kept by
  decision D7 (upstream #485 is inert on `Voxels` and pushes a deeply buried
  body out; the lift deliberately leaves it) — still needed, pinned by
  `sunk_body`. The old fixed-fixed pair filter and its `set_body_type` /
  `set_parent` guards are gone (upstream filters at creation and re-pairs;
  `pair_filter.rs` parity pins).

## Upstreaming candidates (would shrink the fork)

1. Kinematic `can_sleep(false)` ignored (+ the `wake_up` docstring) — a clear
   bug, 3 lines.
2. Voxels shape cast: search domain misses the voxel under a hovering shape,
   internal-edge normals inside flat floors, exact single-axis normals (the
   upstream KCC's `decompose_hit` drops a tick on a `(0, 0.99999994, 0)`
   normal). Generic, pinned by `voxels_cast.rs` / `voxels_upstream.rs`.
3. Support-map grazing starts (cast starting on/near contact).
4. `cast_shape_on_continuous_surfaces` with the deterministic tie ranking —
   larger, KCC-oriented; offer the tie ranking as an issue first.
5. KCC `depenetrate` toggle — small, but encodes a game policy; an issue
   asking for an opt-out of #485.

Not upstreamable: `StateHash` (ours, D2), the `StepJournal` and arena
inverses (rollback-netcode specific), the mass-props skip (a cross-target
determinism workaround for the eigen solver; upstream would rather fix
`SymmetricEigen3`).

## Ranked cleanup list (nothing applied without the user's OK)

| # | cleanup | risk | proof |
|---|---|---|---|
| C1 | Mark the 7 unmarked hunks and the 6 weakly marked ones (`// mmo-fork` on the initialisers and the five renamed call sites; a marker at the voxels trailing-row rewrite) | none (comments) | build + `just smoke`; `grep -c mmo-fork` |
| C2 | Kinematic sleep gate also requires `angular_threshold >= 0`; add the `kinematic_velocity_based().can_sleep(false)` case to `kinematic_sleep.rs` | none today (both thresholds set together) | `kinematic_sleep.rs` |
| C3 | Fix the `wake_up` docstring (doc-only marked hunk) | none | build |
| C4 | physics-next Walker calls `game::movement::{sweep_character, depenetration_lift, character_controller}`; delete the copies | low: the ports' numbers may shift only if the copies already drifted — which is what the change is meant to expose | physics-next `walking_smoothness`, `sunk_body`, `upstream_movement_probe` |
| C5 | physics-next `Cargo.toml`: `determinism-wasm-next` → `determinism-wasm` | none | — |
| C6 | `StateHasher::finish()` without the 4 KiB clone; cheaper `new()` | low; **measure first** (timing run needs OK) | game `hash_mode`, `state_hash`, `determinism-wasm` (hash values must not change) |
| C7 | Rename `crates/physics-next` → e.g. `crates/physics-tests` | churn only (justfile, CLAUDE.md, memory, docs) | build + physics-next suite |
| D1 | (decision) Keep the tie ranking, or rank blocking ties by `normal · up` | changes KCC behaviour on ledges; a golden may move | `physics_goldens`, a new gravity-only ledge pin |

Must stay, and why: the journal + arena inverses (rollback bar), `StateHash`
(D2; cross-target hash parity), the mass-props skip (cross-target
determinism), the voxels / support-map / continuous-surfaces casts and the KCC
gates (walking on voxel terrain; goldens), the kinematic sleep gate (players
and slimes would sleep mid-rollback), the packaging manifests.

## Forward risks (record, not cleanups)

- Kinematic bodies now never sleep and are island members, so anything a
  player or slime touches cannot sleep, and piles merge through them — the
  dynamic-bodies plan must decide whether kinematics leave islands.
- Solver tuning at `dt` = 0.05 s: contact natural frequency 30/60 Hz is above
  the step's Nyquist (dt·ω ≈ 9.4); stable (implicit) but stacking/falling will
  feel unlike rapier's 60 Hz tuning — substeps or softer contacts for dynamic
  bodies.

## Method

```sh
git diff --numstat bb7d5b5:crates/rapier-next HEAD:crates/rapier
git diff --numstat bb7d5b5:crates/parry-next HEAD:crates/parry
git clone --depth 1 --branch v0.36.0 https://github.com/dimforge/rapier
git clone --depth 1 --branch v0.31.1 https://github.com/dimforge/parry
git diff --no-index --numstat <clone>/{src,crates/<x>3d} crates/<x>/{src,crates/<x>3d}
git ls-remote --tags https://github.com/dimforge/{rapier,parry}
grep -rn mmo-fork crates/rapier crates/parry | wc -l
```

Hunks were classified by their added lines (derive/skip → `StateHash`;
`journal_capture`/`StepJournal`/`revert_*`/`alloc_state`/`restore_raw`/
`pop_modified`/`journal_reverts` → journal; the cast/KCC files → shape-cast),
then checked by hand for the ambiguous ones (four journal hunks also carry
`#[state_hash(skip)]` attributes).

## Cleanups applied (2026-09-27, H12)

The user approved C1–C5 and C7 at the Step 6 gate, then chose to skip C7
when asked again. Each item is one commit, verified with its pinning test
and `just smoke`.

| # | commit | result | proof |
|---|---|---|---|
| C1 | `a2c2d36` | markers on the six `cast_shape_on_continuous_surfaces(` call lines, `depenetrate: true` in the KCC `Default`, the four `capture: None` initialisers, `seen_journal_reverts: 0`, and the voxels trailing-row block — including **two** removed upstream lines (the count above said one) | build + smoke; markers rapier 213 → 225, parry 97 → 100 |
| C2 | `eb053ad` | kinematic sleep gate also requires `angular_threshold >= 0` | `kinematic_sleep.rs`: `a_velocity_based_kinematic_body_that_cannot_sleep_stays_awake_at_rest` (green before and after: `can_sleep(false)` sets both thresholds) and `a_negative_angular_threshold_alone_keeps_a_kinematic_body_awake` (red → green) |
| C3 | `a89a507` | `IslandManager::wake_up` docstring matches the code (doc-only, marked) | build |
| C4 | `0b499fa` | the physics-next Walker calls `game::movement::sweep_character`; `sunk_body` imports `character_controller` / `depenetration_lift` from the game; the glam copies are gone | `walking_smoothness` 14, `sunk_body` 3, `upstream_movement_probe` 11 (`--include-ignored --nocapture`): byte-identical output before and after — the copies had not drifted |
| C5 | `ec89417` | physics-next `Cargo.toml` names `just determinism-wasm` | the recipe runs `state_hash_wasm` |
| C6 | — | not applied (user: leave listed; needs a timing run first) | — |
| C7 | — | skipped (user, H12): `crates/physics-next` keeps its name | — |

**D1** (user): keep the TOI tie ranking as is (blocking → least-opposing →
lowest collider index). Revisit only if the ~4 cm stop nudge at a ledge
edge is seen in play, together with a gravity-only ledge pin.
