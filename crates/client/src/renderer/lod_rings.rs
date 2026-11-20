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
    super::meshing::coarse::AwaitingTwinFilter,
)>;

/// Near regions whose every chunk has meshed, stably for
/// [`NEAR_HANDOFF_HOLD_FRAMES`]. `hold` tracks each region's streak against
/// its root entity, so a replaced region (new root) starts over and a region
/// that regresses (a chunk re-inserted before the LOD was released) resets.
/// Remeshes of already-meshed chunks don't count against it: they keep their
/// previous quads on screen until the new ones attach. Answered as the
/// copies the regions are drawn at (`RegionRoots::alias`), the frame LOD
/// keys are in: a far second image of a near region is not replaced by it.
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
        // A hidden tree (the frame moved away from it) replaces nothing.
        if streak.1 >= NEAR_HANDOFF_HOLD_FRAMES && near.drawn(rc) {
            ready.extend(near.alias(rc));
        }
    }
    ready
}

