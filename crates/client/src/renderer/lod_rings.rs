//! Server-streamed LOD rings — the beyond-subscription view distance.
//!
//! The instance layer holds `RequestLod` subscriptions by ring assignment
//! (`instance::update_lod_window`); the bridge routes the resulting
//! `ClientUpdateEvent::LodData`/`RemoveLod` events into `LodRings.pending`.
//! `apply_lod_events` decodes each RLE payload and spawns one root per
//! `LodKey` with `LodVoxelData` children, meshed by `meshing::lod` — nothing
//! here touches the full-res pipeline. A key that is refreshed (re-receipt)
//! or removed keeps its old root rendering until a replacement is visibly
//! meshed (`retire_lod_roots`), so transitions never pop the horizon to
//! empty. Each root with water also gets one [`LodWaterSurface`] child: the
//! key's `LodWater` (the grids hold no liquid) as greedy-merged top quads in
//! the blended water material (water plan Task 8), retired with its root. A key handed over to the near window stays until that region's
//! authoritative chunks have ALL meshed — the switch is one swap, not a LOD
//! that vanishes while the near terrain fills in chunk by chunk.

use std::collections::{BTreeMap, BTreeSet};

use bevy::prelude::*;
use bevy::light::NotShadowCaster;
use game::lod::{LodKey, LodWater};
use game::{LodPayload, RegionCoords};

use super::bridge::{PendingSnapshotVoxels, RegionRoots, VoxelData};
use super::meshing::lod;
use super::meshing::lod::LodRasterReady;
use super::meshing::visibility::ChunkConnectivity;
use super::voxel_pull::ChunkQuads;
use super::water::material::{WaterMaterial, WaterMaterialHandle};
use super::water::mesh::WaterMeshData;

/// Deadline for retiring/retired roots: if the replacement hasn't shown a
/// mesh within this many frames (~10s at 60fps), despawn the old root anyway
/// — a late pop beats leaking an entity tree forever (e.g. a replacement
/// whose cells are all air never produces a mesh at all).
///
/// The counter resets on each re-receipt (a refresh rebuilds the slot at
/// frame 0), so a key refreshed faster than its replacements mesh can extend
/// one old root's life beyond this total — accepted: each refresh spawns a
/// fresh replacement and terrain-change pushes are cadence-limited.
pub const RETIREMENT_MAX_FRAMES: u32 = 600;

/// Consecutive frames a near region must stay fully meshed before it
/// replaces a LOD key: a chunk's quads are extracted and uploaded on the
/// render sub-app a frame after they land (pipelined rendering), so despawning
/// the LOD on the very frame the last mesh attached would flash that chunk.
pub const NEAR_HANDOFF_HOLD_FRAMES: u32 = 3;

/// A LOD event routed from the bridge (`drain_client_updates`), applied by
/// `apply_lod_events` the same frame.
pub enum LodEventMsg {
    /// Streamed cells for one key — initial send or refresh
    /// (replace-on-re-receipt) — and its water surface layer.
    Data(LodKey, LodPayload, LodWater),
    /// The ring assignment moved on: retire this key's render root.
    /// `near_takeover` as on `ClientUpdateEvent::RemoveLod`.
    Remove { key: LodKey, near_takeover: bool },
}

/// One key's render state: the live root, plus the superseded root while a
/// refresh's replacement meshes.
pub struct LodSlot {
    pub root: Entity,
    /// The superseded root after a re-receipt, kept alive (still rendering
    /// the old terrain) so a refresh never pops the horizon to empty.
    /// Despawned by `retire_lod_roots` once `root` carries a meshed child,
    /// or on the `retirement_max_frames` deadline.
    pub retiring: Option<Entity>,
    /// Frames since `retiring` was set — the deadline clock.
    pub retiring_frames: u32,
}

/// A root whose key was removed (ring reassignment / near-window takeover),
/// kept rendering until replacement coverage is actually visible (or the
/// deadline passes) — see `retire_lod_roots`.
pub struct RetiredLod {
    pub key: LodKey,
    pub entity: Entity,
    /// Frames since retirement — the deadline clock.
    pub frames: u32,
    /// Retired because its region joined the near window: released only
    /// once every region of the key is replaced (fully meshed near region or
    /// meshed LOD cover), and exempt from the deadline — the server may take
    /// seconds to generate the region, and a hole is the failure mode this
    /// exists to prevent. Cannot leak: a region that leaves the near window
    /// again is re-requested as LOD, and that key's mesh releases this one.
    pub near_takeover: bool,
}

#[derive(Resource)]
pub struct LodRings {
    /// Live render roots per subscribed key.
    pub roots: BTreeMap<LodKey, LodSlot>,
    /// Roots retired by `Remove`, pending a visible replacement.
    pub retired: Vec<RetiredLod>,
    /// Events queued by the bridge, drained by `apply_lod_events`.
    pub pending: Vec<LodEventMsg>,
    /// Deadline (in frames) for retiring/retired roots — a field (defaulting
    /// to `RETIREMENT_MAX_FRAMES`) so tests can shrink it.
    pub retirement_max_frames: u32,
}

impl Default for LodRings {
    fn default() -> Self {
        Self {
            roots: BTreeMap::new(),
            retired: Vec::new(),
            pending: Vec::new(),
            retirement_max_frames: RETIREMENT_MAX_FRAMES,
        }
    }
}

/// Decoded LOD cells `apply_lod_events` may spawn per frame before deferring
/// the remaining messages: about one factor-2 region payload, ~100 ms worst
/// case. The real fix is to keep cells packed and decode in the LOD mesh task;
/// this only stops several payloads from stacking into one frame.
pub const LOD_APPLY_CELLS_PER_FRAME: usize = 512;

/// Drain `LodRings.pending`: spawn a root per `Data` payload (decoding the
/// RLE grids; undecodable cells drop with a warning — wire data is
/// untrusted), retire the old root on re-receipt, and move `Remove`d keys'
/// roots to the retirement list.
pub fn apply_lod_events(
    mut commands: Commands,
    mut rings: ResMut<LodRings>,
    mut water_assets: LodWaterAssets,
) {
    let mut pending = std::mem::take(&mut rings.pending);
    // Budgeted by decoded cells: `spawn_lod_root` expands every cell of a
    // payload to a dense 32^3 grid on this thread (~0.2 ms per cell, a
    // factor-2 region is ~480 cells). Unbudgeted, a burst of payloads was one
    // 0.7-0.9 s frame (2026-09-19 captures). Messages stay in arrival order;
    // whatever exceeds the budget waits in `pending` for the next frame, and
    // at least one message always makes progress.
    let mut cells = 0usize;
    let take = pending
        .iter()
        .position(|msg| {
            let over = cells >= LOD_APPLY_CELLS_PER_FRAME;
            if let LodEventMsg::Data(_, payload, water) = msg {
                cells += payload.len() + lod_water_cost(water);
            }
            over
        })
        .unwrap_or(pending.len());
    rings.pending = pending.split_off(take);
    let mut stall = game::perf::StallGuard::new("apply_lod_events");
    stall.add_work(pending.len() as u32);
    for msg in pending {
        match msg {
            LodEventMsg::Data(key, payload, water) => {
                let root = spawn_lod_root(&mut commands, key, &payload);
                spawn_lod_water(&mut commands, &mut water_assets, root, &key, &water);
                match rings.roots.entry(key) {
                    std::collections::btree_map::Entry::Occupied(mut e) => {
                        let slot = e.get_mut();
                        // At most one old root per key: a previous retiring
                        // occupant is despawned rather than stacked.
                        if let Some(prev) = slot.retiring.take() {
                            commands.entity(prev).despawn();
                        }
                        slot.retiring = Some(slot.root);
                        slot.retiring_frames = 0;
                        slot.root = root;
                    }
                    std::collections::btree_map::Entry::Vacant(v) => {
                        v.insert(LodSlot {
                            root,
                            retiring: None,
                            retiring_frames: 0,
                        });
                    }
                }
            }
            LodEventMsg::Remove { key, near_takeover } => {
                let Some(slot) = rings.roots.remove(&key) else {
                    continue;
                };
                if let Some(prev) = slot.retiring {
                    commands.entity(prev).despawn();
                }
                rings.retired.push(RetiredLod {
                    key,
                    entity: slot.root,
                    frames: 0,
                    near_takeover,
                });
            }
        }
    }
}

/// Spawn one key's render tree: a root at the key's world offset plus one
/// LOD child per decodable cell, per the `meshing::lod` spawner contract
/// (translation `group * 32 * factor` with `-factor` on y to compensate the
/// skirt bias, scale `splat(factor)`), with same-batch `LodNeighbors` wiring
/// for padding gather. A key's whole payload arrives in one message, so
/// every same-key neighbor is already in `spawned` by the time wiring runs.
/// (Cross-key neighbors are not wired — border side-faces render unculled
/// walls instead, over-draw the guard rails accept.)
fn spawn_lod_root(commands: &mut Commands, key: LodKey, payload: &LodPayload) -> Entity {
    let factor = key.factor;
    let offset = key.world_offset();
    let root = commands
        .spawn((
            Transform::from_translation(Vec3::new(offset[0], offset[1], offset[2])),
            Visibility::default(),
            Name::new(format!("lod {:?} x{}", key.coords, factor)),
            crate::session::SessionScoped,
            super::meshing::lod_tiles::LodTiles::default(),
        ))
        .id();

    let span = 32 * factor as i32;
    let mut spawned: BTreeMap<[i32; 3], Entity> = BTreeMap::new();
    for (group, packed) in payload {
        // Validate, don't decode: the run total is O(runs); the 64 KiB dense
        // expansion is left to the mesh task (`LodVoxelData` stays packed).
        if !packed.is_well_formed() {
            warn!("lod: dropping undecodable grid {group:?} for {key:?}");
            continue;
        }
        let translation = Vec3::new(
            (group[0] * span) as f32,
            (group[1] * span - factor as i32) as f32,
            (group[2] * span) as f32,
        );
        let child = commands
            .spawn((
                Transform {
                    translation,
                    scale: Vec3::splat(factor as f32),
                    ..default()
                },
                Visibility::default(),
                lod::LodChunk { factor },
                lod::LodVoxelData(packed.clone()),
                super::meshing::lod_tiles::LodGroup(*group),
                ChildOf(root),
            ))
            .id();
        spawned.insert(*group, child);
    }

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

    root
}

/// A LOD root's water surface child (see the module doc).
#[derive(Component)]
pub struct LodWaterSurface;

/// What [`spawn_lod_water`] draws with; absent in headless apps (no water
/// child then).
#[derive(bevy::ecs::system::SystemParam)]
pub struct LodWaterAssets<'w> {
    meshes: Option<ResMut<'w, Assets<Mesh>>>,
    materials: Option<ResMut<'w, Assets<WaterMaterial>>>,
    handle: Option<ResMut<'w, WaterMaterialHandle>>,
    fortress: Option<Res<'w, super::fortress_view::FortressViewUniform>>,
}

/// A key's water layer in [`LOD_APPLY_CELLS_PER_FRAME`] terms: decoding and
/// merging a cell's worth (32³) of LOD columns costs about one cell.
fn lod_water_cost(water: &LodWater) -> usize {
    if !water.has_water() {
        return 0;
    }
    let side = usize::from(water.side);
    (side * side).div_ceil(game::BRICK_VOXEL_COUNT)
}

/// The water surface of one LOD key: greedy-merged top quads, one per run of
/// equal water top and equal alpha within a row, extended across the rows
/// below it. Alpha from depth is the near pass's ([`super::water::water_alpha`]),
/// and it saturates within a few metres, so open water still merges into a
/// few quads. Positions are root-local world units: LOD column `(x, z)`
/// spans `factor` units like the cell children, the surface sits on top of
/// block `top` (`(top + 1)` blocks up). `None` when the layer is dry or
/// malformed.
pub fn lod_water_mesh(key: &LodKey, water: &LodWater) -> Option<WaterMeshData> {
    let tops = water.decode()?;
    let n = usize::from(water.side);
    let keys: Vec<Option<(i16, u32)>> = tops
        .iter()
        .map(|&(top, depth)| {
            (top != game::NO_WATER).then(|| (top, super::water::water_alpha(f32::from(depth)).to_bits()))
        })
        .collect();
    let cell = key.factor as f32;
    let block = game::BLOCK_VOXELS as f32;
    let mut used = vec![false; n * n];
    let mut data = WaterMeshData::default();
    for z in 0..n {
        let mut x = 0;
        while x < n {
            let i = z * n + x;
            let Some(k) = keys[i].filter(|_| !used[i]) else {
                x += 1;
                continue;
            };
            let mut w = 1;
            while x + w < n && !used[i + w] && keys[i + w] == Some(k) {
                w += 1;
            }
            let mut h = 1;
            while z + h < n {
                let row = (z + h) * n + x;
                if (row..row + w).any(|j| used[j] || keys[j] != Some(k)) {
                    break;
                }
                h += 1;
            }
            for dz in 0..h {
                let row = (z + dz) * n + x;
                used[row..row + w].fill(true);
            }
            let y = (f32::from(k.0) + 1.0) * block;
            data.quad(
                [x as f32 * cell, y, z as f32 * cell],
                [0.0, 0.0, h as f32 * cell],
                [w as f32 * cell, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                f32::from_bits(k.1),
            );
            x += w;
        }
    }
    (!data.is_empty()).then_some(data)
}

/// Spawn `root`'s water surface child when the key has water (a
/// `SessionScoped` child, despawned with its root).
fn spawn_lod_water(
    commands: &mut Commands,
    assets: &mut LodWaterAssets,
    root: Entity,
    key: &LodKey,
    water: &LodWater,
) {
    if !water.has_water() {
        return;
    }
    // Wire data is untrusted: a layer sized for another key would mesh past
    // (or short of) its root, so check its side before decoding anything.
    if water.side != LodWater::side_for(key) {
        warn!(
            "lod: dropping water layer of side {} for {key:?} (expected {})",
            water.side,
            LodWater::side_for(key)
        );
        return;
    }
    if !water.is_well_formed() {
        warn!("lod: dropping undecodable water layer for {key:?}");
        return;
    }
    let (Some(meshes), Some(materials), Some(handle)) =
        (assets.meshes.as_mut(), assets.materials.as_mut(), assets.handle.as_mut())
    else {
        return;
    };
    let Some(mesh) = lod_water_mesh(key, water).and_then(super::water::tile_mesh) else {
        return;
    };
    let material = handle.get_or_add(materials, assets.fortress.as_deref().copied().unwrap_or_default());
    commands.spawn((
        Name::new(format!("lod water {:?} x{}", key.coords, key.factor)),
        Mesh3d(meshes.add(mesh)),
        MeshMaterial3d(material),
        Transform::default(),
        Visibility::default(),
        NotShadowCaster,
        LodWaterSurface,
        crate::session::SessionScoped,
        ChildOf(root),
    ));
}

/// Despawn retiring/retired LOD roots once their replacement is actually
/// VISIBLE — the point of retirement is that a transition never pops the
/// horizon to empty (under-draw, the design's named failure mode). Until
/// then the old root keeps rendering stale terrain on top of (or instead
/// of) the new — over-draw, the guard rails' accepted trade.
///
/// Visibility means meshes, not entities: a replacement's `LodVoxelData`
/// children render nothing until the budgeted LOD mesh chain reaches them.
/// Hence:
/// - re-receipt: the slot's new `root` has a child carrying `Mesh3d`;
/// - removal: some live slot with a meshed child covers one of the retired
///   key's regions, OR the near window holds one of those regions and it is
///   fully meshed ([`near_regions_meshed`]) — for a near-window takeover,
///   every region of the key must be replaced one of those two ways.
///
/// "≥1 meshed child" (not all children) is deliberate: all-air cells never
/// receive a `Mesh3d`, so an all-children predicate would always fall
/// through to the deadline; the trade is at worst a few budget-frames of
/// partial fill instead of a guaranteed full hole.
///
/// Each root also carries a frame deadline (`retirement_max_frames`): a
/// replacement that never meshes (all-air cells, a key released with no
/// successor in view) must not pin the old root forever. Near-window
/// takeovers are exempt (see `RetiredLod::near_takeover`).
pub fn retire_lod_roots(
    mut commands: Commands,
    mut rings: ResMut<LodRings>,
    near: Res<RegionRoots>,
    children: Query<&Children>,
    meshed: Query<(), ReadyLodCellFilter>,
    unmeshed_near: Query<&ChildOf, UnmeshedNearChunkFilter>,
    mut hold: crate::session::SessionLocal<BTreeMap<RegionCoords, (Entity, u32)>>,
) {
    let hold = hold.get();
    if rings.retired.is_empty() && rings.roots.values().all(|s| s.retiring.is_none()) {
        hold.clear();
        return;
    }
    let max = rings.retirement_max_frames;
    let has_meshed_child = |root: Entity| {
        children
            .get(root)
            .is_ok_and(|ch| ch.iter().any(|c| meshed.contains(c)))
    };

    // Re-receipts: the replacement is the slot's own new root.
    for slot in rings.roots.values_mut() {
        let Some(old) = slot.retiring else { continue };
        slot.retiring_frames += 1;
        if has_meshed_child(slot.root) || slot.retiring_frames >= max {
            commands.entity(old).despawn();
            slot.retiring = None;
        }
    }

    // Removals: replacement coverage comes from whichever key (or the near
    // window) now owns the retired key's regions. Build the covered-region
    // set from live meshed slots once per run.
    let mut covered: BTreeSet<RegionCoords> = BTreeSet::new();
    for (key, slot) in &rings.roots {
        if has_meshed_child(slot.root) {
            covered.extend(key.regions());
        }
    }
    // Only a near-takeover retirement asks the near window; the scan behind
    // it walks every unmeshed near chunk, which is the whole backlog while
    // flying (~100k), so skip it when nobody needs the answer.
    let near_ready = if rings.retired.iter().any(|r| r.near_takeover) {
        near_regions_meshed(&near, &unmeshed_near, hold)
    } else {
        // Unwatched frames break a stability streak.
        hold.clear();
        BTreeSet::new()
    };
    let replaced_region = |rc: &RegionCoords| covered.contains(rc) || near_ready.contains(rc);

    let retired = std::mem::take(&mut rings.retired);
    rings.retired = retired
        .into_iter()
        .filter_map(|mut r| {
            r.frames += 1;
            let regions = r.key.regions();
            let (replaced, expired) = if r.near_takeover {
                (regions.iter().all(replaced_region), false)
            } else {
                (regions.iter().any(replaced_region), r.frames >= max)
            };
            if replaced || expired {
                commands.entity(r.entity).despawn();
                None
            } else {
                Some(r)
            }
        })
        .collect();
}

/// A meshed terrain cell. The water surface child has a `Mesh3d` too, but
/// it is not terrain: a root showing only water has not replaced anything.
type ReadyLodCellFilter = (
    Or<(
        With<Mesh3d>,
        With<ChunkQuads>,
        With<LodRasterReady>,
    )>,
    Without<LodWaterSurface>,
);
/// A near chunk not yet on screen: its snapshot voxels are still being
/// derived, or it has voxels but its first mesh has not attached
/// (`apply_meshed_chunks` inserts `ChunkConnectivity` for every finished
/// mesh, all-air ones included, and strips it when the voxels go away) — or,
/// on the per-chunk backend, a sim chunk whose merged mesh is not rebuilt yet.
type UnmeshedNearChunkFilter = Or<(
    With<PendingSnapshotVoxels>,
    (With<VoxelData>, Without<ChunkConnectivity>),
    With<super::meshing::chunk_mesh::ChunkMeshDirty>,
)>;

/// Near regions whose every chunk has meshed, stably for
/// [`NEAR_HANDOFF_HOLD_FRAMES`]. `hold` tracks each region's streak against
/// its root entity, so a replaced region (new root) starts over and a region
/// that regresses (a chunk re-inserted before the LOD was released) resets.
/// Remeshes of already-meshed chunks don't count against it: they keep their
/// previous quads on screen until the new ones attach.
fn near_regions_meshed(
    near: &RegionRoots,
    unmeshed: &Query<&ChildOf, UnmeshedNearChunkFilter>,
    hold: &mut BTreeMap<RegionCoords, (Entity, u32)>,
) -> BTreeSet<RegionCoords> {
    // A region's chunks mostly iterate together: dedupe runs before the set
    // insert (7.4 ms/frame over the flight backlog without it).
    let mut busy: BTreeSet<Entity> = BTreeSet::new();
    let mut last = None;
    for parent in unmeshed.iter().map(ChildOf::parent) {
        if last != Some(parent) {
            busy.insert(parent);
            last = Some(parent);
        }
    }
    hold.retain(|rc, _| near.0.contains_key(rc));
    let mut ready = BTreeSet::new();
    for (&rc, &root) in &near.0 {
        let streak = hold.entry(rc).or_insert((root, 0));
        if streak.0 != root || busy.contains(&root) {
            *streak = (root, 0);
            continue;
        }
        streak.1 = streak.1.saturating_add(1);
        if streak.1 >= NEAR_HANDOFF_HOLD_FRAMES {
            ready.insert(rc);
        }
    }
    ready
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::lod::PackedLodGrid;
    use game::{BlockId, Voxel, BRICK_VOXEL_COUNT};

    use super::super::meshing::lod::{LodChunk, LodVoxelData};
    use super::super::meshing::queue::tests::{pipeline_app, wait_until};
    use super::super::meshing::queue::MeshBudgets;
    use super::super::meshing::records::QuadRecord;
    use super::super::voxel_pull::ChunkQuads;


    /// The full meshing/LOD pipeline (`pipeline_app`) plus the two LOD-ring
    /// systems and the near-window exclusion resource.
    fn rings_app() -> App {
        let mut app = pipeline_app(MeshBudgets::default());
        app.init_resource::<RegionRoots>()
            .init_resource::<LodRings>()
            .add_systems(Update, (apply_lod_events, retire_lod_roots).chain());
        app
    }

    fn push_event(app: &mut App, msg: LodEventMsg) {
        app.world_mut().resource_mut::<LodRings>().pending.push(msg);
    }

    fn slot_root(app: &App, key: LodKey) -> Option<Entity> {
        app.world()
            .resource::<LodRings>()
            .roots
            .get(&key)
            .map(|s| s.root)
    }

    fn root_meshed(app: &mut App, root: Entity) -> bool {
        let world = app.world_mut();
        let mut q = world.query_filtered::<&ChildOf, With<Mesh3d>>();
        q.iter(world).any(|c| c.parent() == root)
    }

    fn lod_children(app: &mut App, root: Entity) -> Vec<(Transform, u32)> {
        let world = app.world_mut();
        let mut q = world.query::<(&Transform, &LodChunk, &LodVoxelData, &ChildOf)>();
        q.iter(world)
            .filter(|(.., c)| c.parent() == root)
            .map(|(tf, lc, ..)| (*tf, lc.factor))
            .collect()
    }

    fn one_solid_grid() -> Vec<Voxel> {
        vec![Voxel::new(BlockId(1)); BRICK_VOXEL_COUNT]
    }

    fn solid_payload(group: [i32; 3]) -> LodPayload {
        vec![(group, PackedLodGrid::encode(&one_solid_grid()))]
    }

    /// A burst of payloads must not stack into one frame: the cell budget
    /// defers the excess, in arrival order, and every key still gets its root.
    #[test]
    fn a_payload_burst_is_spread_over_frames_in_arrival_order() {
        let mut app = rings_app();
        let packed = PackedLodGrid::encode(&one_solid_grid());
        let big: LodPayload = (0..LOD_APPLY_CELLS_PER_FRAME as i32)
            .map(|x| ([x, 0, 0], packed.clone()))
            .collect();
        let keys: Vec<LodKey> = (0..3).map(|x| LodKey::for_region(2, game::RegionCoords::new(10 + x, 0))).collect();
        for key in &keys {
            push_event(&mut app, LodEventMsg::Data(*key, big.clone(), LodWater::default()));
        }

        app.update();
        assert!(slot_root(&app, keys[0]).is_some(), "the first payload always makes progress");
        assert!(slot_root(&app, keys[1]).is_none(), "a full budget defers the rest");
        assert_eq!(app.world().resource::<LodRings>().pending.len(), 2);

        app.update();
        assert!(slot_root(&app, keys[1]).is_some(), "deferred payloads keep arrival order");
        assert!(slot_root(&app, keys[2]).is_none());

        app.update();
        assert!(slot_root(&app, keys[2]).is_some());
        assert!(app.world().resource::<LodRings>().pending.is_empty());
    }

    fn pulled_marker() -> ChunkQuads {
        ChunkQuads(std::sync::Arc::new(vec![QuadRecord([0, 0])]))
    }

    #[test]
    fn re_receipt_retires_old_root_when_pulled_replacement_is_visible() {
        let mut app = rings_app();
        let key = LodKey {
            factor: 4,
            coords: [0, 0],
        };
        let old_root = app.world_mut().spawn_empty().id();
        let new_root = app.world_mut().spawn_empty().id();
        app.world_mut().spawn((pulled_marker(), ChildOf(new_root)));
        app.world_mut().resource_mut::<LodRings>().roots.insert(
            key,
            LodSlot {
                root: new_root,
                retiring: Some(old_root),
                retiring_frames: 0,
            },
        );

        app.update();

        assert!(
            app.world().get_entity(old_root).is_err(),
            "pulled replacement is renderable"
        );
        assert!(app.world().resource::<LodRings>().roots[&key]
            .retiring
            .is_none());
    }

    #[test]
    fn lod_data_spawns_root_and_children_per_spawner_contract() {
        let mut app = rings_app();
        let key = LodKey {
            factor: 64,
            coords: [1, -1],
        };
        push_event(&mut app, LodEventMsg::Data(key, solid_payload([0, 2, 0]), LodWater::default()));
        assert!(wait_until(&mut app, |app| slot_root(app, key).is_some()));
        let root = slot_root(&app, key).unwrap();
        let off = key.world_offset();
        assert_eq!(
            app.world()
                .entity(root)
                .get::<Transform>()
                .unwrap()
                .translation,
            Vec3::new(off[0], off[1], off[2]),
            "root sits at the key's world offset"
        );
        // Child per the meshing::lod spawner contract.
        let children = lod_children(&mut app, root);
        assert_eq!(children.len(), 1);
        let (tf, factor) = children[0];
        assert_eq!(factor, 64);
        assert_eq!(tf.scale, Vec3::splat(64.0));
        assert_eq!(
            tf.translation,
            Vec3::new(0.0, (2 * 32 * 64 - 64) as f32, 0.0),
            "cell origin group*32*factor, y-bias -factor"
        );
        // And it meshes via the existing LOD chain.
        assert!(wait_until(&mut app, |app| root_meshed(app, root)));
    }

    #[test]
    fn re_receipt_retires_the_old_root_until_the_replacement_meshes() {
        let mut app = rings_app();
        let key = LodKey {
            factor: 4,
            coords: [0, 0],
        };
        push_event(&mut app, LodEventMsg::Data(key, solid_payload([0, 0, 0]), LodWater::default()));
        assert!(
            wait_until(&mut app, |app| slot_root(app, key)
                .is_some_and(|r| root_meshed(app, r))),
            "the first root must settle to meshed children"
        );
        let old_root = slot_root(&app, key).unwrap();

        // Refresh: the old root must survive every frame until the NEW root
        // has a meshed child (the invariant is asserted inside the wait).
        push_event(&mut app, LodEventMsg::Data(key, solid_payload([0, 1, 0]), LodWater::default()));
        assert!(
            wait_until(&mut app, |app| {
                let new_meshed =
                    slot_root(app, key).is_some_and(|r| r != old_root && root_meshed(app, r));
                if !new_meshed {
                    assert!(
                        app.world().get_entity(old_root).is_ok(),
                        "old root despawned before the replacement meshed — horizon pop"
                    );
                }
                new_meshed
            }),
            "the replacement must land and mesh"
        );
        assert!(
            wait_until(&mut app, |app| app.world().get_entity(old_root).is_err()),
            "old root despawns once the replacement is visible"
        );
        assert!(app.world().resource::<LodRings>().roots[&key]
            .retiring
            .is_none());
    }

    #[test]
    fn remove_lod_retires_until_deadline_or_coverage() {
        let mut app = rings_app();
        app.world_mut()
            .resource_mut::<LodRings>()
            .retirement_max_frames = 3;
        let key = LodKey {
            factor: 16,
            coords: [0, 0],
        };
        push_event(&mut app, LodEventMsg::Data(key, solid_payload([0, 0, 0]), LodWater::default()));
        assert!(wait_until(&mut app, |app| slot_root(app, key)
            .is_some_and(|r| root_meshed(app, r))));
        let root = slot_root(&app, key).unwrap();

        push_event(&mut app, LodEventMsg::Remove { key, near_takeover: false });
        app.update();
        assert!(slot_root(&app, key).is_none(), "slot gone after Remove");
        assert!(
            app.world().get_entity(root).is_ok(),
            "no replacement ever comes: the retired root keeps rendering until the deadline"
        );
        assert!(
            wait_until(&mut app, |app| app.world().get_entity(root).is_err()),
            "deadline must despawn the retired root"
        );
        assert!(app.world().resource::<LodRings>().retired.is_empty());
    }

    #[test]
    fn removed_key_releases_when_another_meshed_key_covers_its_region() {
        let mut app = rings_app();
        app.world_mut()
            .resource_mut::<LodRings>()
            .retirement_max_frames = u32::MAX;
        let key4 = LodKey {
            factor: 4,
            coords: [0, 0],
        };
        push_event(&mut app, LodEventMsg::Data(key4, solid_payload([0, 0, 0]), LodWater::default()));
        assert!(wait_until(&mut app, |app| slot_root(app, key4)
            .is_some_and(|r| root_meshed(app, r))));
        let old_root = slot_root(&app, key4).unwrap();

        push_event(&mut app, LodEventMsg::Remove { key: key4, near_takeover: false });
        for _ in 0..5 {
            app.update();
        }
        assert!(
            app.world().get_entity(old_root).is_ok(),
            "no coverage yet: the retired root must keep rendering"
        );

        // The ring reassignment's replacement: factor 16 over the same region.
        let key16 = LodKey {
            factor: 16,
            coords: [0, 0],
        };
        push_event(&mut app, LodEventMsg::Data(key16, solid_payload([0, 0, 0]), LodWater::default()));
        assert!(
            wait_until(&mut app, |app| app.world().get_entity(old_root).is_err()),
            "retired root despawns once a covering key is meshed"
        );
        assert!(app.world().resource::<LodRings>().retired.is_empty());
    }

    /// A near region as the bridge spawns it from a snapshot: a root at the
    /// region's world offset and one chunk child per `chunk_origins` entry
    /// (region-local), each still waiting for its off-thread voxels.
    fn spawn_near_region(app: &mut App, rc: RegionCoords, chunk_origins: &[[f32; 3]]) -> Vec<Entity> {
        let offset = rc.world_offset();
        let root = app
            .world_mut()
            .spawn((
                Transform::from_translation(Vec3::new(offset[0], offset[1], offset[2])),
                Visibility::default(),
            ))
            .id();
        app.world_mut().resource_mut::<RegionRoots>().0.insert(rc, root);
        chunk_origins
            .iter()
            .map(|&origin| {
                app.world_mut()
                    .spawn((
                        Transform::from_translation(Vec3::from_array(origin)),
                        Visibility::default(),
                        PendingSnapshotVoxels,
                        ChildOf(root),
                    ))
                    .id()
            })
            .collect()
    }

    /// The snapshot voxels land, as `bridge::apply_snapshot_voxels` does it.
    fn land_voxels(app: &mut App, chunk: Entity, voxels: Vec<Voxel>) {
        app.world_mut()
            .entity_mut(chunk)
            .insert(VoxelData::new(voxels))
            .remove::<PendingSnapshotVoxels>();
    }

    fn meshed_lod_root(app: &mut App, key: LodKey) -> Entity {
        push_event(app, LodEventMsg::Data(key, solid_payload([0, 0, 0]), LodWater::default()));
        assert!(wait_until(app, |app| slot_root(app, key)
            .is_some_and(|r| root_meshed(app, r))));
        slot_root(app, key).unwrap()
    }

    /// The handoff this module exists for: a LOD key taken over by the near
    /// window keeps rendering while the region's chunks mesh one by one, and
    /// is released only once ALL of them (all-air ones included) are on
    /// screen — not when the first one lands.
    #[test]
    fn near_takeover_keeps_lod_until_the_whole_region_is_meshed() {
        let mut app = rings_app();
        let rc = RegionCoords::new(1, -1);
        let key = LodKey {
            factor: 4,
            coords: [rc.x, rc.z],
        };
        let lod_root = meshed_lod_root(&mut app, key);

        push_event(&mut app, LodEventMsg::Remove { key, near_takeover: true });
        let chunks = spawn_near_region(&mut app, rc, &[[0.0; 3], [32.0, 0.0, 0.0], [0.0, 32.0, 0.0]]);

        // Two of three chunks mesh (one solid, one all air): the third is
        // still waiting for voxels, so the LOD must stay.
        land_voxels(&mut app, chunks[0], one_solid_grid());
        land_voxels(&mut app, chunks[2], vec![Voxel::default(); BRICK_VOXEL_COUNT]);
        assert!(wait_until(&mut app, |app| {
            [chunks[0], chunks[2]]
                .iter()
                .all(|&c| app.world().entity(c).contains::<ChunkConnectivity>())
        }));
        for _ in 0..NEAR_HANDOFF_HOLD_FRAMES + 5 {
            app.update();
        }
        assert!(
            app.world().get_entity(lod_root).is_ok(),
            "a partly meshed region must not release its LOD"
        );

        // Voxels without a mesh yet still count as not rendered.
        land_voxels(&mut app, chunks[1], one_solid_grid());
        app.update();
        assert!(app.world().get_entity(lod_root).is_ok());

        assert!(
            wait_until(&mut app, |app| app.world().get_entity(lod_root).is_err()),
            "LOD released once every chunk of the region is meshed"
        );
        assert!(app.world().entity(chunks[1]).contains::<ChunkConnectivity>());
        assert!(app.world().resource::<LodRings>().retired.is_empty());
    }

    /// Server genesis can take seconds: a takeover waits for the region
    /// however long that is, instead of popping on the frame deadline.
    #[test]
    fn near_takeover_ignores_the_retirement_deadline() {
        let mut app = rings_app();
        app.world_mut()
            .resource_mut::<LodRings>()
            .retirement_max_frames = 3;
        let rc = RegionCoords::new(0, 0);
        let key = LodKey {
            factor: 2,
            coords: [0, 0],
        };
        let lod_root = meshed_lod_root(&mut app, key);

        push_event(&mut app, LodEventMsg::Remove { key, near_takeover: true });
        for _ in 0..20 {
            app.update();
        }
        assert!(
            app.world().get_entity(lod_root).is_ok(),
            "region not even arrived yet: the LOD must keep rendering"
        );

        let chunks = spawn_near_region(&mut app, rc, &[[0.0; 3]]);
        land_voxels(&mut app, chunks[0], one_solid_grid());
        assert!(wait_until(&mut app, |app| app.world().get_entity(lod_root).is_err()));
    }

    /// A takeover whose region leaves the near window again before it
    /// arrives is released by the LOD key re-requested for it instead.
    #[test]
    fn near_takeover_releases_on_returning_lod_cover() {
        let mut app = rings_app();
        let key = LodKey {
            factor: 2,
            coords: [3, 3],
        };
        let lod_root = meshed_lod_root(&mut app, key);
        push_event(&mut app, LodEventMsg::Remove { key, near_takeover: true });
        app.update();
        let new_root = meshed_lod_root(&mut app, key);
        assert_ne!(new_root, lod_root);
        assert!(wait_until(&mut app, |app| app.world().get_entity(lod_root).is_err()));
        assert!(app.world().resource::<LodRings>().retired.is_empty());
    }

    /// A ring reassignment (not a takeover) still releases on a meshed chunk
    /// of a complete near region — and never on an incomplete one.
    #[test]
    fn plain_removal_waits_for_a_fully_meshed_near_region() {
        let mut app = rings_app();
        app.world_mut()
            .resource_mut::<LodRings>()
            .retirement_max_frames = u32::MAX;
        let rc = RegionCoords::new(-2, 1);
        let key = LodKey {
            factor: 4,
            coords: [rc.x, rc.z],
        };
        let lod_root = meshed_lod_root(&mut app, key);
        push_event(&mut app, LodEventMsg::Remove { key, near_takeover: false });
        let chunks = spawn_near_region(&mut app, rc, &[[0.0; 3], [0.0, 0.0, 32.0]]);
        land_voxels(&mut app, chunks[0], one_solid_grid());
        for _ in 0..20 {
            app.update();
        }
        assert!(app.world().get_entity(lod_root).is_ok());
        land_voxels(&mut app, chunks[1], one_solid_grid());
        assert!(wait_until(&mut app, |app| app.world().get_entity(lod_root).is_err()));
    }

    #[test]
    fn undecodable_grids_are_dropped_not_spawned() {
        let mut app = rings_app();
        let key = LodKey {
            factor: 4,
            coords: [0, 0],
        };
        let bad = vec![([0, 0, 0], PackedLodGrid(vec![(3, BlockId(1))]))]; // wrong total
        push_event(&mut app, LodEventMsg::Data(key, bad, LodWater::default()));
        app.update();
        let root = slot_root(&app, key).expect("root spawns even if all cells drop");
        assert_eq!(
            lod_children(&mut app, root).len(),
            0,
            "bad cell dropped with a warning"
        );
    }

    // ---- LOD water surface (water plan Task 8) ----

    const DEEP: (i16, u8) = (63, 20);

    /// Runs of equal top and alpha merge across rows; alpha saturates, so
    /// deep columns of different depths share a quad; a shallow column gets
    /// its own lighter one; the surface sits on top of block `top`.
    #[test]
    fn lod_water_mesh_merges_runs_and_lifts_to_the_top() {
        let dry = (game::NO_WATER, 0);
        let shallow = (63, 1);
        #[rustfmt::skip]
        let tops = [
            DEEP, DEEP,     dry, dry,
            DEEP, (63, 30), dry, dry,
            shallow, dry,   dry, dry,
            dry, dry,       dry, dry,
        ];
        let key = LodKey { factor: 16, coords: [0, 0] };
        let mesh = lod_water_mesh(&key, &LodWater::encode(&tops, 4)).expect("wet");
        assert_eq!(mesh.indices.len(), 12, "two quads: {:?}", mesh.positions);
        let y = 64.0 * game::BLOCK_VOXELS as f32;
        let f = key.factor as f32;
        let deep: Vec<_> = mesh.positions[..4].to_vec();
        assert_eq!(deep, vec![[0.0, y, 0.0], [0.0, y, 2.0 * f], [2.0 * f, y, 2.0 * f], [2.0 * f, y, 0.0]]);
        assert!(mesh.normals.iter().all(|n| *n == [0.0, 1.0, 0.0]));
        assert_eq!(mesh.colors[0][3], super::super::water::WATER_MAX_ALPHA);
        assert_eq!(mesh.positions[4], [0.0, y, 2.0 * f]);
        assert_eq!(mesh.colors[4][3], super::super::water::water_alpha(1.0));
        assert!(mesh.colors[4][3] < mesh.colors[0][3], "the shallows are lighter");

        assert!(lod_water_mesh(&key, &LodWater::encode(&[dry; 16], 4)).is_none(), "dry");
        assert!(lod_water_mesh(&key, &LodWater { side: 4, runs: vec![(3, 63, 2)] }).is_none(), "malformed");
    }

    /// A root with water gets one water child — and that child is not a
    /// meshed terrain cell: a refresh whose terrain has not meshed keeps the
    /// old root.
    #[test]
    fn lod_water_child_spawns_but_is_not_terrain_readiness() {
        let mut app = rings_app();
        app.init_asset::<WaterMaterial>()
            .init_resource::<WaterMaterialHandle>();
        let key = LodKey::for_region(16, game::RegionCoords::new(3, 0));
        let side = LodWater::side_for(&key);
        let water = LodWater::encode(&vec![DEEP; usize::from(side) * usize::from(side)], side);
        push_event(&mut app, LodEventMsg::Data(key, Vec::new(), water.clone()));
        app.update();
        let root = slot_root(&app, key).expect("root");
        let world = app.world_mut();
        let mut q = world.query_filtered::<&ChildOf, (With<LodWaterSurface>, With<Mesh3d>)>();
        assert_eq!(q.iter(world).filter(|c| c.parent() == root).count(), 1, "one water child");

        push_event(&mut app, LodEventMsg::Data(key, Vec::new(), water));
        for _ in 0..5 {
            app.update();
        }
        let slot = &app.world().resource::<LodRings>().roots[&key];
        assert_eq!(slot.retiring, Some(root), "water alone does not retire the old root");
    }

    /// A water layer whose side is not its key's (untrusted wire data) is
    /// dropped with a warning: no water child, the root still spawns.
    #[test]
    fn lod_water_of_the_wrong_side_is_dropped() {
        let mut app = rings_app();
        app.init_asset::<WaterMaterial>()
            .init_resource::<WaterMaterialHandle>();
        let key = LodKey::for_region(16, game::RegionCoords::new(3, 0));
        let side = LodWater::side_for(&key) / 2;
        let water = LodWater::encode(&vec![DEEP; usize::from(side) * usize::from(side)], side);
        assert!(water.is_well_formed(), "well-formed for its own side");
        push_event(&mut app, LodEventMsg::Data(key, Vec::new(), water));
        app.update();
        let root = slot_root(&app, key).expect("root spawns without its water");
        let world = app.world_mut();
        let mut q = world.query_filtered::<&ChildOf, With<LodWaterSurface>>();
        assert_eq!(q.iter(world).filter(|c| c.parent() == root).count(), 0, "no water child");
    }
}
