//! Client-side worldgen LOD rings — the beyond-subscription view distance.
//!
//! Regions past the streamed far ring render coarse terrain generated locally
//! from the deterministic `worldgen` crate at zero bandwidth: factor 4 for the
//! middle ring, factor 16 (block scale) for the outer ring, per
//! `WORLDGEN_RINGS`. Assignment keys off the viewer region with a sticky band
//! so ring boundaries don't thrash while the viewer wanders near an edge.
//! Regions owned by the near window (`Regions`) or the streamed far ring
//! (`FarRegionRoots`) are excluded — those render authoritative state. A ring
//! that loses its region (to them, or to a factor reassignment) keeps its old
//! root rendering until the replacement is visibly meshed (`retire_roots`),
//! so transitions never pop the horizon to empty. Outputs are
//! `LodVoxelData` children under per-region roots, meshed by `meshing::lod` —
//! nothing here touches the full-res pipeline.

use std::collections::{BTreeMap, BTreeSet};

use bevy::prelude::*;
use bevy::tasks::{block_on, futures_lite::future, AsyncComputeTaskPool, Task};
use game::lod::{block_scale_cells, downsample_cells, ring_factor_in};
use game::{Chunk, ChunkCoords, RegionCoords};
use worldgen::material::Materials;

use game::REGION_CHUNKS;

use super::bridge::{FarRegionRoots, Regions};
use super::hud::HudStatus;
use super::meshing::lod::{self, LodCell};
use super::meshing::queue::ChunkKey;

/// The view distance in regions — the ring table's last bound (kept in sync
/// by constructing the table from it). Regions beyond this render nothing.
pub const WORLDGEN_VIEW_RADIUS: i32 = 8;
/// Worldgen ring table: `(chebyshev_bound, lod_factor)` with ascending bounds.
/// A region at Chebyshev distance `d` from the viewer region renders at the
/// factor of the first entry with `d <= bound` — unless the near window or the
/// streamed far ring owns it (exclusion is by `Regions`/`FarRegionRoots`
/// membership, not by distance, so the innermost worldgen ring's inner edge is
/// wherever streamed coverage actually ends).
pub const WORLDGEN_RINGS: &[(i32, u32)] = &[(4, 4), (WORLDGEN_VIEW_RADIUS, 16)];
/// Hysteresis: an assigned region keeps its ring until its distance leaves the
/// ring's bounds by more than this, in either direction (promote and demote).
pub const RING_STICKY_BAND: i32 = 1;
/// Concurrent client-side generation budget.
pub const WORLDGEN_TASKS_MAX_IN_FLIGHT: usize = 2;
/// Deadline for retiring/retired roots: if the replacement hasn't shown a
/// mesh within this many frames (~10s at 60fps), despawn the old root anyway
/// — a late pop beats leaking an entity tree forever (e.g. a replacement
/// whose cells are all air never produces a mesh at all).
///
/// The counter resets on each carry/adopt (a flip rebuilds the slot at
/// frame 0), so a viewer ping-ponging across a boundary under budget
/// starvation can extend one old root's life beyond this total — accepted:
/// each flip spawns a fresh replacement and the sticky band damps the
/// oscillation.
pub const RETIREMENT_MAX_FRAMES: u32 = 600;

/// Ring assignment with the production table and band. See `ring_factor_in`
/// (moved to `game::lod`), which the systems call directly (the table is a
/// resource field so tests can shrink it); this fixed-constant face pins the
/// old client-only 2-entry table and is currently unused — kept until Task 11
/// deletes it along with `WORLDGEN_RINGS`.
#[allow(dead_code)]
pub fn ring_factor(dist: i32, current: Option<u32>) -> Option<u32> {
    ring_factor_in(WORLDGEN_RINGS, RING_STICKY_BAND, dist, current)
}

/// One region's worldgen-ring state: the factor it should render at, and the
/// spawned region root once generation lands (`None` while its `WorldgenTask`
/// is pending or still awaiting budget).
pub struct RingSlot {
    pub factor: u32,
    pub root: Option<Entity>,
    /// The superseded assignment's root, kept alive (still rendering the old
    /// terrain) so a factor flip never pops the horizon to empty. Despawned
    /// by `retire_roots` once `root`'s children carry meshes, or on the
    /// `retirement_max_frames` deadline. Ownership when the slot itself goes
    /// away is `manage_worldgen_rings`' (see the reconcile loop): the value
    /// carries into the replacement slot, moves to the takeover retirement
    /// list, or despawns on radius exit — cancelled `WorldgenTask`s never
    /// own it, so task-drop paths can't leak it.
    pub retiring: Option<Entity>,
    /// Frames since `retiring` was set — the deadline clock.
    pub retiring_frames: u32,
}

impl RingSlot {
    /// A freshly assigned slot: generation pending, nothing retiring.
    pub fn pending(factor: u32) -> Self {
        Self { factor, root: None, retiring: None, retiring_frames: 0 }
    }
}

/// A worldgen root whose region was taken over by the near window or the
/// streamed far ring, kept rendering until that owner's replacement is
/// actually visible (or the deadline passes) — see `retire_roots`.
pub struct RetiredRoot {
    pub coords: RegionCoords,
    pub entity: Entity,
    /// Frames since retirement — the deadline clock.
    pub frames: u32,
}

#[derive(Resource)]
pub struct WorldgenRings {
    /// Desired + realized assignments. Root `None` = generation pending.
    pub assigned: BTreeMap<RegionCoords, RingSlot>,
    /// Ring table — a field (defaulting to `WORLDGEN_RINGS`) so tests can
    /// shrink it; the effective view radius is the last entry's bound.
    pub table: &'static [(i32, u32)],
    /// Sticky band — a field (defaulting to `RING_STICKY_BAND`) so tests can
    /// zero it, consistent with `table`/`max_in_flight`.
    pub band: i32,
    /// Roots retired by a near/far takeover, pending a visible replacement.
    pub retired: Vec<RetiredRoot>,
    /// Deadline (in frames) for retiring/retired roots — a field (defaulting
    /// to `RETIREMENT_MAX_FRAMES`) so tests can shrink it.
    pub retirement_max_frames: u32,
    /// Generation budget — a field (defaulting to
    /// `WORLDGEN_TASKS_MAX_IN_FLIGHT`) so tests can pin it. The live in-flight
    /// count is derived from `WorldgenTask` entities, never counted separately
    /// (a counter could leak if a task entity died without a decrement).
    pub max_in_flight: usize,
    /// Terrain materials, resolved once from the block registry (mirrors
    /// `local_server.rs`).
    pub materials: Materials,
    /// Worldgen seed. ASSUMPTION (plan deviation 2): the client generates
    /// rings with its own compiled-in `worldgen::WORLD_SEED`, which must match
    /// the server's world — true for the bundled server today. A foreign-seed
    /// server would render wrong terrain beyond the subscribed ring; the named
    /// follow-up is carrying the seed in the join handshake.
    pub seed: u64,
}

impl WorldgenRings {
    pub fn new(materials: Materials) -> Self {
        Self {
            assigned: BTreeMap::new(),
            table: WORLDGEN_RINGS,
            band: RING_STICKY_BAND,
            retired: Vec::new(),
            retirement_max_frames: RETIREMENT_MAX_FRAMES,
            max_in_flight: WORLDGEN_TASKS_MAX_IN_FLIGHT,
            materials,
            seed: worldgen::WORLD_SEED,
        }
    }
}

/// In-flight client-side generation for one region's ring. A standalone
/// entity, despawned when the result lands (or is dropped as stale) — and by
/// `manage_worldgen_rings` when the assignment changes mid-flight, which drops
/// (= cancels) the task.
#[derive(Component)]
pub struct WorldgenTask {
    pub coords: RegionCoords,
    pub factor: u32,
    pub task: Task<Vec<LodCell>>,
}

/// Reconcile ring assignments with the viewer region and the near/far
/// exclusion sets, then top up generation tasks nearest-first under the
/// budget. Runs in `Update`, decoupled from the meshing chain.
pub fn manage_worldgen_rings(
    mut commands: Commands,
    mut rings: ResMut<WorldgenRings>,
    hud: Res<HudStatus>,
    near: Res<Regions>,
    far: Res<FarRegionRoots>,
    tasks: Query<(Entity, &WorldgenTask)>,
) {
    let Some(viewer) = hud.viewer_region else { return };
    let chebyshev = |c: RegionCoords| (c.x - viewer.x).abs().max((c.z - viewer.z).abs());
    let radius = rings.table.last().map_or(0, |&(bound, _)| bound);

    // Candidates: everything inside the radius, plus everything currently
    // assigned — an assigned region can sit outside the radius (held by the
    // sticky band, or left behind by a viewer jump) and must still be
    // reconciled so it despawns once truly out.
    let mut candidates: BTreeSet<RegionCoords> = rings.assigned.keys().copied().collect();
    for dx in -radius..=radius {
        for dz in -radius..=radius {
            candidates.insert(RegionCoords::new(viewer.x + dx, viewer.z + dz));
        }
    }

    for coords in candidates {
        let current = rings.assigned.get(&coords).map(|slot| slot.factor);
        // A region the near window or the streamed far ring owns never gets a
        // worldgen ring; if one takes over later, the ring's root retires
        // (below) rather than despawning into a hole.
        let excluded = near.0.contains_key(&coords) || far.0.contains_key(&coords);
        let desired = if excluded {
            None
        } else {
            ring_factor_in(rings.table, rings.band, chebyshev(coords), current)
        };
        if desired == current {
            continue;
        }
        // The removed slot's live root: prefer the current root; a previous
        // retiring occupant is despawned rather than stacked — at most one
        // old root ever covers a region.
        let live = rings.assigned.remove(&coords).and_then(|slot| match (slot.root, slot.retiring) {
            (Some(root), prev) => {
                if let Some(prev) = prev {
                    commands.entity(prev).despawn();
                }
                Some(root)
            }
            (None, prev) => prev,
        });
        match desired {
            // Factor transition: the old root keeps rendering as `retiring`
            // until the replacement's children mesh (`retire_roots`). A fresh
            // assignment adopts a still-retired takeover root the same way
            // (the region flipped back before its old root despawned).
            Some(factor) => {
                let retiring =
                    live.or_else(|| take_retired(&mut rings.retired, coords, &mut commands));
                rings.assigned.insert(coords, RingSlot { retiring, ..RingSlot::pending(factor) });
            }
            // Takeover by the near window / far ring: retire the root until
            // the new owner's render is visible. KNOWN REMAINING POP (out of
            // scope): the reverse handoff — a region leaving the far ring has
            // its far root despawned immediately by the bridge while the
            // worldgen replacement lags; fixing that needs bridge
            // cooperation. Noted for a future pass.
            None if excluded => {
                if let Some(entity) = live {
                    rings.retired.push(RetiredRoot { coords, entity, frames: 0 });
                }
            }
            // Radius exit: no replacement is coming (the region is beyond
            // view) — despawn immediately, as before.
            None => {
                if let Some(entity) = live {
                    commands.entity(entity).despawn();
                }
            }
        }
    }

    // Cancel tasks whose assignment moved on while they generated: despawning
    // the entity drops the `Task`, which cancels the underlying job and frees
    // its budget slot now instead of at completion. (`finish_worldgen_tasks`'
    // stale check remains the backstop for a result that completes in the
    // same frame the assignment changes.)
    let mut in_flight = 0usize;
    for (task_entity, task) in &tasks {
        let stale = rings
            .assigned
            .get(&task.coords)
            .is_none_or(|slot| slot.factor != task.factor || slot.root.is_some());
        if stale {
            commands.entity(task_entity).despawn();
        } else {
            in_flight += 1;
        }
    }

    // Top up generation for pending slots, nearest-first (ties in BTree key
    // order — deterministic), under the budget.
    let mut pending: Vec<(i32, RegionCoords, u32)> = rings
        .assigned
        .iter()
        .filter(|(_, slot)| slot.root.is_none())
        .filter(|(coords, slot)| {
            !tasks.iter().any(|(_, t)| t.coords == **coords && t.factor == slot.factor)
        })
        .map(|(coords, slot)| (chebyshev(*coords), *coords, slot.factor))
        .collect();
    pending.sort_unstable();
    let budget = rings.max_in_flight.saturating_sub(in_flight);
    let pool = AsyncComputeTaskPool::get();
    for (_, coords, factor) in pending.into_iter().take(budget) {
        let (seed, materials) = (rings.seed, rings.materials);
        let task =
            pool.spawn(async move { build_worldgen_lod_cells(coords, seed, materials, factor) });
        commands.spawn(WorldgenTask { coords, factor, task });
    }
}

/// Pull the takeover-retired root for `coords` back out of the retirement
/// list — the region returned to worldgen ownership before its old root
/// despawned, so the old root becomes the new slot's `retiring` instead.
/// More than one entry per region can't normally exist (a slot insert always
/// drains the list for its coords, a push always follows a slot removal);
/// extras are despawned defensively rather than stacked.
fn take_retired(
    retired: &mut Vec<RetiredRoot>,
    coords: RegionCoords,
    commands: &mut Commands,
) -> Option<Entity> {
    let mut found = None;
    retired.retain(|r| {
        if r.coords != coords {
            return true;
        }
        if let Some(prev) = found.replace(r.entity) {
            commands.entity(prev).despawn();
        }
        false
    });
    found
}

/// Despawn retiring/retired worldgen roots once their replacement is
/// actually VISIBLE — the point of retirement is that a ring transition
/// never pops the horizon to empty (under-draw, the plan's named failure
/// mode). Until then the old root keeps rendering stale terrain on top of
/// (or instead of) the new — over-draw, the guard rails' accepted trade.
///
/// Visibility means meshes, not entities: a replacement's `LodVoxelData`
/// children render nothing until the budgeted LOD mesh chain reaches them,
/// so despawning the old root when the children merely spawn would still
/// hole the horizon for several frames (worse under mesh-queue load). Hence:
/// - factor transition: the slot's new `root` has a child carrying `Mesh3d`;
/// - far takeover: the region's `FarRegionRoots` root has a meshed child;
/// - near takeover: `Regions` holds the region and some `ChunkKey`ed meshed
///   entity's key lies in the region's chunk bounds
///   (`r.x*REGION_CHUNKS..(r.x+1)*REGION_CHUNKS`, z likewise).
///
/// "≥1 meshed child" (not all children) is deliberate: all-air cells never
/// receive a `Mesh3d`, so an all-children predicate would always fall through
/// to the deadline; the trade is at worst a few budget-frames of partial fill
/// instead of a guaranteed full hole.
///
/// Each root also carries a frame deadline (`retirement_max_frames`): a
/// replacement that never meshes (all-air cells, a far region removed again
/// while beyond the worldgen radius) must not pin the old root forever.
pub fn retire_roots(
    mut commands: Commands,
    mut rings: ResMut<WorldgenRings>,
    near: Res<Regions>,
    far: Res<FarRegionRoots>,
    children: Query<&Children>,
    meshed: Query<(), With<Mesh3d>>,
    keyed_meshes: Query<&ChunkKey, With<Mesh3d>>,
) {
    if rings.retired.is_empty() && rings.assigned.values().all(|s| s.retiring.is_none()) {
        return;
    }
    let max = rings.retirement_max_frames;
    let has_meshed_child = |root: Entity| {
        children.get(root).is_ok_and(|ch| ch.iter().any(|c| meshed.contains(c)))
    };

    // Factor transitions: the replacement is the slot's own new root.
    for slot in rings.assigned.values_mut() {
        let Some(old) = slot.retiring else { continue };
        slot.retiring_frames += 1;
        if slot.root.is_some_and(has_meshed_child) || slot.retiring_frames >= max {
            commands.entity(old).despawn();
            slot.retiring = None;
        }
    }

    // Takeovers: the replacement is the new owner's render for that region.
    let retired = std::mem::take(&mut rings.retired);
    rings.retired = retired
        .into_iter()
        .filter_map(|mut r| {
            r.frames += 1;
            let far_visible = far.0.get(&r.coords).copied().is_some_and(has_meshed_child);
            let near_visible = near.0.contains_key(&r.coords) && {
                let x0 = r.coords.x * REGION_CHUNKS as i32;
                let z0 = r.coords.z * REGION_CHUNKS as i32;
                let span = REGION_CHUNKS as i32;
                keyed_meshes.iter().any(|k| {
                    (x0..x0 + span).contains(&k.0[0]) && (z0..z0 + span).contains(&k.0[2])
                })
            };
            if far_visible || near_visible || r.frames >= max {
                commands.entity(r.entity).despawn();
                None
            } else {
                Some(r)
            }
        })
        .collect();
}

/// Poll `WorldgenTask`s; on completion spawn the region root at the region's
/// world offset plus one LOD child per cell (Task 3's spawner contract:
/// translation `group * 32 * factor` with `-factor` on y, scale
/// `splat(factor)`), wire same-batch `LodNeighbors`, and record the root.
/// Results whose `(coords, factor)` no longer matches the pending assignment
/// are dropped — the assignment moved on while the task was generating.
pub fn finish_worldgen_tasks(
    mut commands: Commands,
    mut rings: ResMut<WorldgenRings>,
    mut tasks: Query<(Entity, &mut WorldgenTask)>,
) {
    for (task_entity, mut task) in &mut tasks {
        let Some(cells) = block_on(future::poll_once(&mut task.task)) else { continue };
        commands.entity(task_entity).despawn();
        let (coords, factor) = (task.coords, task.factor);
        // Stale-result drop: only land if this (region, factor) is still the
        // pending assignment. `manage_worldgen_rings` cancels mismatched
        // tasks, but a task can complete in the very frame the assignment
        // changes — this check is the backstop.
        let Some(slot) = rings.assigned.get_mut(&coords) else { continue };
        if slot.factor != factor || slot.root.is_some() {
            continue;
        }

        let offset = coords.world_offset();
        let root = commands
            .spawn((
                Transform::from_translation(Vec3::new(offset[0], offset[1], offset[2])),
                Visibility::default(),
                Name::new(format!("worldgen ring {:?} x{}", coords, factor)),
            ))
            .id();

        // Spawner contract (lod.rs, Task 3): cell origin `group * 32 * factor`
        // (a group key is in units of its own grid's world span), `-factor` on
        // y to compensate the skirt bias, scale = factor.
        let span = 32 * factor as i32;
        let mut spawned: BTreeMap<[i32; 3], Entity> = BTreeMap::new();
        for LodCell { group, grid } in cells {
            let translation = Vec3::new(
                (group[0] * span) as f32,
                (group[1] * span - factor as i32) as f32,
                (group[2] * span) as f32,
            );
            let child = commands
                .spawn((
                    Transform { translation, scale: Vec3::splat(factor as f32), ..default() },
                    Visibility::default(),
                    lod::LodChunk { factor },
                    lod::LodVoxelData(grid),
                    ChildOf(root),
                ))
                .id();
            spawned.insert(group, child);
        }

        // Wire same-batch neighbors for padding gather, exactly like
        // `bridge::finish_far_lod_builds`: a region's whole terrain resolves
        // in one task, so every same-region neighbor is already in `spawned`.
        // (Cross-region neighbors are not wired — border side-faces render
        // unculled walls instead, over-draw the guard rails accept.)
        for (&g, &e) in &spawned {
            let mut neighbors = BTreeMap::new();
            for dx in -1..=1i32 {
                for dy in -1..=1i32 {
                    for dz in -1..=1i32 {
                        if dx == 0 && dy == 0 && dz == 0 {
                            continue;
                        }
                        if let Some(&ne) = spawned.get(&[g[0] + dx, g[1] + dy, g[2] + dz]) {
                            neighbors.insert([dx, dy, dz], ne);
                        }
                    }
                }
            }
            if !neighbors.is_empty() {
                commands.entity(e).insert(lod::LodNeighbors(neighbors));
            }
        }

        slot.root = Some(root);
    }
}

/// Generate one region's terrain and downsample it to `factor`'s LOD cells.
/// Runs inside the async task — pure, owns everything it needs. Chunk coords
/// come out region-local in x/z (column indices 0..REGION_CHUNKS) with `y`
/// the absolute (signed) sub-chunk index from
/// `Column::materialized_sub_chunks`.
pub fn build_worldgen_lod_cells(
    coords: RegionCoords,
    seed: u64,
    materials: Materials,
    factor: u32,
) -> Vec<LodCell> {
    let columns = worldgen::generate_region(coords, seed, materials);
    let mut chunks: Vec<(ChunkCoords, Chunk)> = Vec::new();
    for (cc, column) in columns {
        for (sub_y, chunk) in column.materialized_sub_chunks() {
            chunks.push((ChunkCoords::new(cc.x, sub_y, cc.z), chunk));
        }
    }
    match factor {
        2 => lod::build_far_lod_cells(chunks),
        4 => downsample_cells(lod::build_far_lod_cells(chunks)),
        16 => block_scale_cells(chunks),
        _ => panic!("unsupported worldgen ring factor {factor} (the table must use 2, 4, or 16)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::{BlockId, GameDataUpdate, Voxel, CHUNK_VOXEL_COUNT};

    use super::super::meshing::lod::{LodChunk, LodVoxelData};
    use super::super::meshing::queue::{self, MeshBudgets};
    use super::super::meshing::queue::tests::{pipeline_app, wait_until};

    // --- app harness --------------------------------------------------------

    fn test_materials() -> Materials {
        Materials {
            stone: BlockId(1),
            dirt: BlockId(2),
            grass: BlockId(3),
            sand: BlockId(4),
            water: BlockId(5),
        }
    }

    /// The full meshing/LOD pipeline (`pipeline_app`) plus the worldgen-ring
    /// systems and their exclusion-set resources.
    fn rings_app(table: &'static [(i32, u32)], max_in_flight: usize) -> App {
        let mut app = pipeline_app(MeshBudgets::default());
        let mut rings = WorldgenRings::new(test_materials());
        rings.table = table;
        rings.max_in_flight = max_in_flight;
        app.init_resource::<Regions>()
            .init_resource::<FarRegionRoots>()
            .init_resource::<HudStatus>()
            .insert_resource(rings)
            .add_systems(
                Update,
                (manage_worldgen_rings, finish_worldgen_tasks, retire_roots).chain(),
            );
        app
    }

    fn set_viewer(app: &mut App, x: i32, z: i32) {
        app.world_mut().resource_mut::<HudStatus>().viewer_region =
            Some(RegionCoords::new(x, z));
    }

    fn task_list(app: &mut App) -> Vec<(RegionCoords, u32)> {
        let world = app.world_mut();
        let mut q = world.query::<&WorldgenTask>();
        q.iter(world).map(|t| (t.coords, t.factor)).collect()
    }

    fn lod_child_count(app: &mut App) -> usize {
        let world = app.world_mut();
        let mut q = world.query::<&LodVoxelData>();
        q.iter(world).count()
    }

    // --- (b) exclusion -------------------------------------------------------

    #[test]
    fn near_and_far_owned_regions_are_never_assigned_and_conflicts_despawn() {
        // Budget 0: assignment bookkeeping only, no real generation.
        let mut app = rings_app(&[(1, 4)], 0);
        set_viewer(&mut app, 0, 0);
        let (_keep_send, recv) = crossbeam::channel::unbounded::<GameDataUpdate>();
        app.world_mut().resource_mut::<Regions>().0.insert(RegionCoords::new(0, 0), recv);
        let far_dummy = app.world_mut().spawn_empty().id();
        app.world_mut()
            .resource_mut::<FarRegionRoots>()
            .0
            .insert(RegionCoords::new(0, 1), far_dummy);
        app.update();

        {
            let rings = app.world().resource::<WorldgenRings>();
            assert!(
                !rings.assigned.contains_key(&RegionCoords::new(0, 0)),
                "near-owned region never gets a worldgen ring"
            );
            assert!(
                !rings.assigned.contains_key(&RegionCoords::new(0, 1)),
                "far-owned region never gets a worldgen ring"
            );
            assert_eq!(rings.assigned.len(), 7, "the other 7 regions of the 3x3 are assigned");
            assert!(rings.assigned.values().all(|s| s.factor == 4 && s.root.is_none()));
        }

        // A region the far ring takes over later loses its ASSIGNMENT
        // immediately, but the root retires (keeps rendering) until the far
        // replacement is visible — see `far_takeover_retires_the_root_until_
        // the_replacement_meshes` for the full retirement lifecycle.
        let root = app.world_mut().spawn_empty().id();
        app.world_mut()
            .resource_mut::<WorldgenRings>()
            .assigned
            .get_mut(&RegionCoords::new(1, 1))
            .expect("assigned above")
            .root = Some(root);
        let far_dummy2 = app.world_mut().spawn_empty().id();
        app.world_mut()
            .resource_mut::<FarRegionRoots>()
            .0
            .insert(RegionCoords::new(1, 1), far_dummy2);
        app.update();
        assert!(
            !app.world()
                .resource::<WorldgenRings>()
                .assigned
                .contains_key(&RegionCoords::new(1, 1)),
            "far takeover evicts the assignment"
        );
        assert!(
            app.world().get_entity(root).is_ok(),
            "the conflicting worldgen root retires instead of popping to empty"
        );
        let rings = app.world().resource::<WorldgenRings>();
        assert_eq!(rings.retired.len(), 1, "the root moved to the takeover retirement list");
        assert_eq!(rings.retired[0].coords, RegionCoords::new(1, 1));
        assert_eq!(rings.retired[0].entity, root);
    }

    #[test]
    fn assignments_are_sticky_at_region_granularity_and_leave_past_the_band() {
        let mut app = rings_app(&[(1, 4)], 0);
        set_viewer(&mut app, 0, 0);
        app.update();
        assert_eq!(app.world().resource::<WorldgenRings>().assigned.len(), 9);

        // Viewer steps +1x: regions at x=-1 sit at distance 2 = bound + band —
        // sticky keeps them even though a fresh assignment would not.
        set_viewer(&mut app, 1, 0);
        app.update();
        {
            let rings = app.world().resource::<WorldgenRings>();
            assert!(
                rings.assigned.contains_key(&RegionCoords::new(-1, 0)),
                "distance 2 = bound+band: held by the sticky band"
            );
            assert_eq!(rings.assigned.len(), 12, "9 held + the new x=2 column");
        }

        // Viewer steps again: x=-1 is now distance 3, past the band — gone.
        set_viewer(&mut app, 2, 0);
        app.update();
        let rings = app.world().resource::<WorldgenRings>();
        assert!(!rings.assigned.contains_key(&RegionCoords::new(-1, 0)), "past the band: evicted");
        assert!(
            rings.assigned.contains_key(&RegionCoords::new(0, 0)),
            "distance 2: still held by the band"
        );
    }

    // --- retirement: ring transitions never pop to empty ----------------------

    use bevy::asset::RenderAssetUsages;
    use bevy::mesh::{Mesh, PrimitiveTopology};

    fn slot_root(app: &App, coords: RegionCoords) -> Option<Entity> {
        app.world().resource::<WorldgenRings>().assigned.get(&coords).and_then(|s| s.root)
    }

    fn root_meshed(app: &mut App, root: Entity) -> bool {
        let world = app.world_mut();
        let mut q = world.query_filtered::<&ChildOf, With<Mesh3d>>();
        q.iter(world).any(|c| c.parent() == root)
    }

    /// Hand-plant a live root on an already-assigned slot, for
    /// bookkeeping-only tests (budget 0, no real generation).
    fn plant_root(app: &mut App, coords: RegionCoords) -> Entity {
        let root = app.world_mut().spawn_empty().id();
        app.world_mut()
            .resource_mut::<WorldgenRings>()
            .assigned
            .get_mut(&coords)
            .expect("slot assigned")
            .root = Some(root);
        root
    }

    /// An (empty) mesh handle — `retire_roots` only checks `Mesh3d` presence.
    fn meshed_marker(app: &mut App) -> Mesh3d {
        let mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD);
        Mesh3d(app.world_mut().resource_mut::<Assets<Mesh>>().add(mesh))
    }

    // Real generation twice (factor 4 settle, then the demoted factor-16
    // replacement) plus the meshing chain — well past the 100ms fast lane.
    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn factor_transition_keeps_the_old_root_until_the_replacement_meshes() {
        let mut app = rings_app(&[(0, 4), (1, 16)], 2);
        app.world_mut().resource_mut::<WorldgenRings>().band = 0; // flips act immediately
        // Headless updates spin ~2ms apart while the 32×32-chunk factor-16
        // generation runs for real, so the frame-count deadline would fire
        // long before the replacement meshes and force the very pop this
        // test forbids. The deadline has its own tests — disable it here.
        app.world_mut().resource_mut::<WorldgenRings>().retirement_max_frames = u32::MAX;
        set_viewer(&mut app, 0, 0);
        let coords = RegionCoords::new(0, 0);

        assert!(
            wait_until(&mut app, |app| {
                slot_root(app, coords).is_some_and(|r| root_meshed(app, r))
            }),
            "the factor-4 root must settle to meshed children"
        );
        let old_root = slot_root(&app, coords).unwrap();
        let old_children: Vec<Entity> = app
            .world()
            .entity(old_root)
            .get::<Children>()
            .map(|c| c.iter().collect())
            .unwrap_or_default();
        assert!(!old_children.is_empty(), "settled root has LOD children");

        // Viewer steps away: distance 1, band 0 — the region demotes to 16.
        // The old root must survive every frame until the NEW root has a
        // meshed child (the invariant is asserted inside the wait condition).
        set_viewer(&mut app, 1, 0);
        assert!(
            wait_until(&mut app, |app| {
                let new_meshed = slot_root(app, coords)
                    .is_some_and(|r| r != old_root && root_meshed(app, r));
                if !new_meshed {
                    assert!(
                        app.world().get_entity(old_root).is_ok(),
                        "old root despawned before the replacement meshed — horizon pop"
                    );
                }
                new_meshed
            }),
            "the factor-16 replacement must land and mesh"
        );

        // Once the replacement is visible, `retire_roots` despawns the old
        // root — recursively, and without leaving anything retiring.
        assert!(
            wait_until(&mut app, |app| app.world().get_entity(old_root).is_err()),
            "old root despawns once the replacement is visible"
        );
        for e in old_children {
            assert!(app.world().get_entity(e).is_err(), "old root's children go with it");
        }
        {
            let rings = app.world().resource::<WorldgenRings>();
            assert!(rings.assigned[&coords].retiring.is_none());
            assert!(rings.retired.is_empty());
        }
        // No orphans world-wide: every LOD child still has a live parent.
        let world = app.world_mut();
        let parents: Vec<Entity> = {
            let mut q = world.query_filtered::<&ChildOf, With<LodChunk>>();
            q.iter(world).map(|c| c.parent()).collect()
        };
        assert!(!parents.is_empty());
        for p in parents {
            assert!(world.get_entity(p).is_ok(), "no orphaned LOD children anywhere");
        }
    }

    #[test]
    fn far_takeover_retires_the_root_until_the_replacement_meshes() {
        // Budget 0 + a hand-planted root: retirement bookkeeping only.
        let mut app = rings_app(&[(0, 4)], 0);
        set_viewer(&mut app, 0, 0);
        app.update();
        let coords = RegionCoords::new(0, 0);
        let root = plant_root(&mut app, coords);

        let far_root = app.world_mut().spawn_empty().id();
        app.world_mut().resource_mut::<FarRegionRoots>().0.insert(coords, far_root);
        for _ in 0..5 {
            app.update();
        }
        assert!(
            app.world().get_entity(root).is_ok(),
            "no meshed far child yet: the retired root must keep rendering"
        );
        assert_eq!(app.world().resource::<WorldgenRings>().retired.len(), 1);

        // The far replacement shows a mesh: the retired root despawns.
        let marker = meshed_marker(&mut app);
        app.world_mut().spawn((marker, ChildOf(far_root)));
        assert!(
            wait_until(&mut app, |app| app.world().get_entity(root).is_err()),
            "retired root despawns once the far child is meshed"
        );
        assert!(app.world().resource::<WorldgenRings>().retired.is_empty());
    }

    #[test]
    fn near_takeover_waits_for_a_meshed_chunk_inside_the_region_bounds() {
        let mut app = rings_app(&[(0, 4)], 0);
        set_viewer(&mut app, 1, -1);
        app.update();
        let coords = RegionCoords::new(1, -1); // chunk keys x 8..16, z -8..0
        let root = plant_root(&mut app, coords);

        let (_keep_send, recv) = crossbeam::channel::unbounded::<GameDataUpdate>();
        app.world_mut().resource_mut::<Regions>().0.insert(coords, recv);
        app.update();
        assert!(app.world().get_entity(root).is_ok(), "near takeover retires, not despawns");

        // A meshed chunk OUTSIDE the region's bounds must not release it.
        let marker = meshed_marker(&mut app);
        app.world_mut().spawn((ChunkKey([0, 0, 0]), marker));
        for _ in 0..3 {
            app.update();
        }
        assert!(app.world().get_entity(root).is_ok(), "out-of-region mesh must not count");

        // One inside the bounds does.
        let marker = meshed_marker(&mut app);
        app.world_mut().spawn((ChunkKey([8, 0, -8]), marker));
        assert!(
            wait_until(&mut app, |app| app.world().get_entity(root).is_err()),
            "retired root despawns once a region chunk is meshed"
        );
        assert!(app.world().resource::<WorldgenRings>().retired.is_empty());
    }

    #[test]
    fn radius_exit_despawns_immediately_without_retirement() {
        let mut app = rings_app(&[(0, 4)], 0);
        set_viewer(&mut app, 0, 0);
        app.update();
        let root = plant_root(&mut app, RegionCoords::new(0, 0));

        // Beyond radius + band: no replacement is coming — despawn now.
        set_viewer(&mut app, 10, 10);
        app.update();
        assert!(app.world().get_entity(root).is_err(), "radius exit still despawns immediately");
        let rings = app.world().resource::<WorldgenRings>();
        assert!(rings.retired.is_empty());
        assert!(!rings.assigned.contains_key(&RegionCoords::new(0, 0)));
    }

    #[test]
    fn retiring_root_despawns_at_the_deadline_when_no_replacement_ever_meshes() {
        // Budget 0: the demoted slot's replacement never generates, so only
        // the (shrunk) deadline can release the retiring root.
        let mut app = rings_app(&[(0, 4), (1, 16)], 0);
        {
            let mut rings = app.world_mut().resource_mut::<WorldgenRings>();
            rings.band = 0;
            rings.retirement_max_frames = 3;
        }
        set_viewer(&mut app, 0, 0);
        app.update();
        let coords = RegionCoords::new(0, 0);
        let root = plant_root(&mut app, coords);

        set_viewer(&mut app, 1, 0); // demotes (0,0) to 16, root starts retiring
        app.update();
        assert_eq!(
            app.world().resource::<WorldgenRings>().assigned[&coords].retiring,
            Some(root),
            "demotion made the old root retiring"
        );
        assert!(app.world().get_entity(root).is_ok(), "deadline not reached yet");
        assert!(
            wait_until(&mut app, |app| app.world().get_entity(root).is_err()),
            "deadline must despawn the retiring root"
        );
        assert!(app.world().resource::<WorldgenRings>().assigned[&coords].retiring.is_none());
    }

    #[test]
    fn retired_takeover_root_despawns_at_the_deadline() {
        let mut app = rings_app(&[(0, 4)], 0);
        app.world_mut().resource_mut::<WorldgenRings>().retirement_max_frames = 3;
        set_viewer(&mut app, 0, 0);
        app.update();
        let coords = RegionCoords::new(0, 0);
        let root = plant_root(&mut app, coords);

        // Far takeover whose root never meshes anything.
        let far_root = app.world_mut().spawn_empty().id();
        app.world_mut().resource_mut::<FarRegionRoots>().0.insert(coords, far_root);
        assert!(
            wait_until(&mut app, |app| app.world().get_entity(root).is_err()),
            "deadline must despawn the retired root"
        );
        assert!(app.world().resource::<WorldgenRings>().retired.is_empty());
    }

    #[test]
    fn flip_back_before_landing_carries_the_retiring_root() {
        // 4 -> 16 -> 4 with budget 0: the 16 replacement never lands, so the
        // original root must ride along as `retiring` through both flips —
        // never despawned, never duplicated.
        let mut app = rings_app(&[(0, 4), (1, 16)], 0);
        app.world_mut().resource_mut::<WorldgenRings>().band = 0;
        set_viewer(&mut app, 0, 0);
        app.update();
        let coords = RegionCoords::new(0, 0);
        let root = plant_root(&mut app, coords);

        set_viewer(&mut app, 1, 0);
        app.update();
        {
            let rings = app.world().resource::<WorldgenRings>();
            assert_eq!(rings.assigned[&coords].factor, 16);
            assert_eq!(rings.assigned[&coords].retiring, Some(root));
        }
        set_viewer(&mut app, 0, 0);
        app.update();
        let rings = app.world().resource::<WorldgenRings>();
        assert_eq!(rings.assigned[&coords].factor, 4);
        assert_eq!(rings.assigned[&coords].retiring, Some(root), "old root carries over");
        assert!(app.world().get_entity(root).is_ok());
    }

    #[test]
    fn takeover_of_a_pending_transition_retires_the_old_root() {
        // A slot mid-factor-transition (root None, retiring Some) is taken
        // over: the retiring root must move to the retirement list — not
        // leak, not despawn into a hole.
        let mut app = rings_app(&[(0, 4), (1, 16)], 0);
        app.world_mut().resource_mut::<WorldgenRings>().band = 0;
        set_viewer(&mut app, 0, 0);
        app.update();
        let coords = RegionCoords::new(0, 0);
        let root = plant_root(&mut app, coords);
        set_viewer(&mut app, 1, 0);
        app.update();
        assert_eq!(app.world().resource::<WorldgenRings>().assigned[&coords].retiring, Some(root));

        let far_root = app.world_mut().spawn_empty().id();
        app.world_mut().resource_mut::<FarRegionRoots>().0.insert(coords, far_root);
        app.update();
        let rings = app.world().resource::<WorldgenRings>();
        assert!(!rings.assigned.contains_key(&coords));
        assert_eq!(rings.retired.len(), 1);
        assert_eq!(rings.retired[0].entity, root);
        assert!(app.world().get_entity(root).is_ok());
    }

    #[test]
    fn reassignment_adopts_a_retired_takeover_root_as_retiring() {
        // Takeover, then the owner leaves again before the old root died:
        // the fresh worldgen assignment adopts it as `retiring` so the
        // region keeps rendering seamlessly through the round trip.
        let mut app = rings_app(&[(0, 4)], 0);
        set_viewer(&mut app, 0, 0);
        app.update();
        let coords = RegionCoords::new(0, 0);
        let root = plant_root(&mut app, coords);
        let far_root = app.world_mut().spawn_empty().id();
        app.world_mut().resource_mut::<FarRegionRoots>().0.insert(coords, far_root);
        app.update();
        assert_eq!(app.world().resource::<WorldgenRings>().retired.len(), 1);

        app.world_mut().resource_mut::<FarRegionRoots>().0.remove(&coords);
        app.update();
        let rings = app.world().resource::<WorldgenRings>();
        assert!(rings.retired.is_empty(), "retired entry pulled back out of the list");
        assert_eq!(rings.assigned[&coords].retiring, Some(root), "adopted as retiring");
        assert!(app.world().get_entity(root).is_ok());
    }

    // --- budget / nearest-first ----------------------------------------------

    #[test]
    fn generation_is_budgeted_and_nearest_first() {
        let mut app = rings_app(&[(1, 4)], 1);
        set_viewer(&mut app, 0, 0);
        app.update();
        // 9 regions want generation; budget 1 admits exactly the nearest
        // (the viewer region itself, distance 0).
        assert_eq!(task_list(&mut app), vec![(RegionCoords::new(0, 0), 4)]);
    }

    #[test]
    fn in_flight_task_is_cancelled_when_the_region_is_taken_over() {
        let mut app = rings_app(&[(0, 16)], 2);
        set_viewer(&mut app, 0, 0);
        app.update();
        assert_eq!(task_list(&mut app).len(), 1, "generation started for the one region");

        let far_dummy = app.world_mut().spawn_empty().id();
        app.world_mut()
            .resource_mut::<FarRegionRoots>()
            .0
            .insert(RegionCoords::new(0, 0), far_dummy);
        app.update();
        assert!(task_list(&mut app).is_empty(), "takeover cancels the in-flight task");
        assert!(app.world().resource::<WorldgenRings>().assigned.is_empty());
        // Nothing may land afterwards, whichever side of completion the
        // cancellation raced with.
        for _ in 0..5 {
            app.update();
        }
        assert_eq!(lod_child_count(&mut app), 0, "no LOD children may ever land");
    }

    // --- finish: stale drop + spawner contract --------------------------------

    /// Minimal app running ONLY `finish_worldgen_tasks`, so tests can feed it
    /// hand-crafted completed tasks without `manage_worldgen_rings`
    /// interfering (it would cancel a mismatched task before finish saw it).
    fn finish_only_app() -> App {
        let mut app = App::new();
        let mut rings = WorldgenRings::new(test_materials());
        rings.max_in_flight = 0;
        app.add_plugins(MinimalPlugins)
            .insert_resource(rings)
            .add_systems(Update, finish_worldgen_tasks);
        app
    }

    fn ready_task(app: &mut App, coords: RegionCoords, factor: u32, cells: Vec<LodCell>) {
        let task = AsyncComputeTaskPool::get().spawn(async move { cells });
        app.world_mut().spawn(WorldgenTask { coords, factor, task });
    }

    fn one_cell(group: [i32; 3]) -> Vec<LodCell> {
        let mut grid = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        grid[0] = Voxel::new(BlockId(1));
        vec![LodCell { group, grid }]
    }

    #[test]
    fn finish_drops_stale_results_and_spawns_current_ones_per_contract() {
        let mut app = finish_only_app();
        let coords = RegionCoords::new(1, -1);
        app.world_mut()
            .resource_mut::<WorldgenRings>()
            .assigned
            .insert(coords, RingSlot::pending(16));

        // Stale: the assignment says factor 16, this result was built for 4.
        ready_task(&mut app, coords, 4, one_cell([0, 0, 0]));
        // And one for a region with no assignment at all.
        ready_task(&mut app, RegionCoords::new(9, 9), 4, one_cell([0, 0, 0]));
        assert!(
            wait_until(&mut app, |app| {
                let world = app.world_mut();
                world.query::<&WorldgenTask>().iter(world).next().is_none()
            }),
            "completed tasks are consumed"
        );
        assert_eq!(lod_child_count(&mut app), 0, "stale results are dropped, not spawned");
        assert!(
            app.world().resource::<WorldgenRings>().assigned[&coords].root.is_none(),
            "the pending slot stays pending"
        );

        // Current: matching factor lands root + children per the spawner
        // contract: translation group * 32 * factor with -factor on y, scaled.
        ready_task(&mut app, coords, 16, one_cell([0, 2, 0]));
        assert!(
            wait_until(&mut app, |app| {
                app.world().resource::<WorldgenRings>().assigned[&coords].root.is_some()
            }),
            "matching result lands"
        );
        let root = app.world().resource::<WorldgenRings>().assigned[&coords].root.unwrap();
        let offset = coords.world_offset();
        assert_eq!(
            app.world().entity(root).get::<Transform>().unwrap().translation,
            Vec3::new(offset[0], offset[1], offset[2]),
            "root sits at the region's world offset"
        );
        let world = app.world_mut();
        let children: Vec<(Entity, &Transform, &LodChunk)> = {
            let mut q = world.query::<(Entity, &Transform, &LodChunk, &LodVoxelData)>();
            q.iter(world).map(|(e, tf, lc, _)| (e, tf, lc)).collect()
        };
        assert_eq!(children.len(), 1);
        let (child, tf, lod_chunk) = children[0];
        assert_eq!(lod_chunk.factor, 16);
        assert_eq!(
            tf.translation,
            Vec3::new(0.0, (2 * 32 * 16 - 16) as f32, 0.0),
            "cell origin group*32*factor, y-bias -factor"
        );
        assert_eq!(tf.scale, Vec3::splat(16.0));
        assert_eq!(
            world.entity(child).get::<ChildOf>().map(|c| c.parent()),
            Some(root),
            "children hang off the region root"
        );
    }

    // --- (c) full pipeline: real worldgen for one region ----------------------

    // Runs REAL worldgen for one region — was ~60ms in debug at 8×8 chunks;
    // at REGION_CHUNKS = 32 a region is 16× the columns, well past the 100ms
    // fast-lane threshold, so it's tagged slow.
    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn one_region_generates_and_settles_to_meshed_lod_children() {
        let mut app = rings_app(&[(0, 16)], 2);
        set_viewer(&mut app, 1, -1);
        let coords = RegionCoords::new(1, -1);

        assert!(
            wait_until(&mut app, |app| {
                app.world()
                    .resource::<WorldgenRings>()
                    .assigned
                    .get(&coords)
                    .is_some_and(|s| s.root.is_some())
            }),
            "real generation must land a root"
        );
        let root = app.world().resource::<WorldgenRings>().assigned[&coords].root.unwrap();
        assert_eq!(
            app.world().entity(root).get::<Transform>().unwrap().translation,
            Vec3::new(game::REGION_SIZE, 0.0, -game::REGION_SIZE),
            "root at the region's world offset"
        );
        assert!(task_list(&mut app).is_empty(), "in-flight drains to zero");

        // Children follow the factor-16 packing: one grid per 16×16-chunk
        // tile and vertical band, translation (tile_x*512, band*512 - 16,
        // tile_z*512), scale 16.
        {
            let world = app.world_mut();
            let mut q = world.query::<(&Transform, &LodChunk, &LodVoxelData, &ChildOf)>();
            let children: Vec<_> = q.iter(world).filter(|(.., c)| c.parent() == root).collect();
            assert!(!children.is_empty(), "ocean-floor terrain must materialize cells");
            for (tf, lod_chunk, data, _) in children {
                assert_eq!(lod_chunk.factor, 16);
                assert_eq!(tf.scale, Vec3::splat(16.0));
                assert_eq!((tf.translation.x as i32).rem_euclid(512), 0, "x = tile_x*512");
                assert_eq!((tf.translation.z as i32).rem_euclid(512), 0, "z = tile_z*512");
                assert_eq!(
                    (tf.translation.y as i32 + 16).rem_euclid(512),
                    0,
                    "y = band*512 - 16"
                );
                assert!(data.0.iter().any(|v| !v.is_air()), "cells carry real terrain");
            }
        }

        // The LOD meshing chain (registered by pipeline_app) picks them up.
        assert!(
            wait_until(&mut app, |app| {
                let world = app.world_mut();
                let mut q = world.query_filtered::<&ChildOf, (With<Mesh3d>, With<LodChunk>)>();
                q.iter(world).any(|c| c.parent() == root)
            }),
            "worldgen LOD children must mesh"
        );

        // Isolation: nothing entered the full-res pipeline.
        assert!(app.world().resource::<queue::ChunkGrid>().0.is_empty());
        let world = app.world_mut();
        assert_eq!(
            world.query::<&super::super::bridge::VoxelData>().iter(world).count(),
            0,
            "worldgen rings never produce full-res VoxelData"
        );
    }

    // --- (d) view-distance integration smoke ---------------------------------

    // Whole-view-distance version of (c): real worldgen across every region a
    // shrunk ring table assigns around the viewer (a 3×3 block, all factor 4),
    // not just one. Measured ~500ms in debug (9 regions, budget 2) — well past
    // the 100ms fast-lane threshold, so tagged slow like the rest of this
    // suite's real-worldgen tests.
    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn view_distance_smoke_settles_lod_roots_and_meshes_across_every_region_in_view() {
        let mut app = rings_app(&[(1, 4)], WORLDGEN_TASKS_MAX_IN_FLIGHT);
        set_viewer(&mut app, 0, 0);

        assert!(
            wait_until(&mut app, |app| {
                let rings = app.world().resource::<WorldgenRings>();
                rings.assigned.len() == 9 && rings.assigned.values().all(|s| s.root.is_some())
            }),
            "every region within the (shrunk) view distance must settle to a spawned LOD root"
        );
        assert!(task_list(&mut app).is_empty(), "generation budget drains to zero once settled");

        assert!(
            wait_until(&mut app, |app| {
                let world = app.world_mut();
                let mut q = world.query_filtered::<&LodChunk, With<Mesh3d>>();
                q.iter(world).count() > 0
            }),
            "the LOD meshing chain must produce at least one mesh across the view distance"
        );
    }
}
