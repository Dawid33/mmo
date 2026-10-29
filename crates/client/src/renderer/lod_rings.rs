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
//! empty. A key handed over to the near window stays until that region's
//! authoritative chunks have ALL meshed — the switch is one swap, not a LOD
//! that vanishes while the near terrain fills in chunk by chunk.

use std::collections::{BTreeMap, BTreeSet};

use bevy::prelude::*;
use game::lod::LodKey;
use game::{LodPayload, RegionCoords};

use super::bricks::{BrickOf, ChunkBricks};
use super::bridge::{PendingSnapshotVoxels, RegionRoots, VoxelData};
use super::far_voxel::FarVoxelReady;
use super::meshing::lod;
use super::meshing::lod::LodRasterReady;
use super::meshing::visibility::ChunkConnectivity;
use super::voxel_pull::ChunkQuads;

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
    /// (replace-on-re-receipt).
    Data(LodKey, LodPayload),
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

/// Cell mode: the home region's active cells, from
/// `ClientUpdateEvent::ActiveCells`. Home-region LOD cells under `cells` are
/// hidden — those cells render at full detail from the sim. An active cell
/// joins `cells` only once it is fully meshed ([`reveal_meshed_cells`]), so
/// the LOD keeps covering it until then.
#[derive(Resource, Debug)]
pub struct HomeCellMask {
    pub region: Option<RegionCoords>,
    /// Masked: active AND fully meshed (or past `CELL_REVEAL_MAX_FRAMES`).
    pub cells: BTreeSet<game::CellCoords>,
    /// Every active cell with its terrain chunk count at activation.
    pub active: BTreeMap<game::CellCoords, u32>,
    /// Cell edge in chunks: `game::CELL_CHUNKS`, a field only so tests can
    /// exercise masking in a build without cell mode.
    pub cell_chunks: usize,
}

impl Default for HomeCellMask {
    fn default() -> Self {
        Self {
            region: None,
            cells: BTreeSet::new(),
            active: BTreeMap::new(),
            cell_chunks: game::CELL_CHUNKS,
        }
    }
}

/// Frames an activated cell may take to mesh before its LOD is hidden
/// anyway: a cell whose chunk count never adds up (a chunk removed before
/// it meshed) must not leave LOD and near terrain overlapping for good.
pub const CELL_REVEAL_MAX_FRAMES: u32 = 600;

/// Promote active cells into the LOD mask once every terrain chunk the sim
/// reported for them has landed its bricks and every one of those bricks has
/// meshed (`ChunkConnectivity`, set for every finished mesh, all-air ones
/// included), stably for [`NEAR_HANDOFF_HOLD_FRAMES`] — the render sub-app
/// uploads a frame later. The expected brick count is what the chunks
/// actually spawned (`ChunkBricks`): all-air and buried bricks never exist.
/// Scans the home region's chunks and bricks only while some cell is pending.
pub fn reveal_meshed_cells(
    mut mask: ResMut<HomeCellMask>,
    roots: Res<RegionRoots>,
    chunks: Query<(&Transform, &ChildOf, &ChunkBricks), Without<PendingSnapshotVoxels>>,
    meshed: Query<(&Transform, &ChildOf), (With<BrickOf>, With<ChunkConnectivity>)>,
    mut state: Local<BTreeMap<game::CellCoords, (u32, u32)>>,
) {
    let pending: BTreeMap<game::CellCoords, u32> = mask
        .active
        .iter()
        .filter(|(c, _)| !mask.cells.contains(*c))
        .map(|(c, n)| (*c, *n))
        .collect();
    // (frames pending, frames complete) per pending cell.
    state.retain(|c, _| pending.contains_key(c));
    let (Some(home), cc) = (mask.region, mask.cell_chunks) else { return };
    if pending.is_empty() || cc == 0 {
        return;
    }
    let Some(&root) = roots.0.get(&home) else { return };
    // Per pending cell: (landed chunks, their bricks, meshed bricks).
    let mut counts: BTreeMap<game::CellCoords, (u32, u32, u32)> = BTreeMap::new();
    // Chunk and brick transforms are region-local minimum corners; a brick
    // lies inside its chunk, so both resolve to the same cell.
    let cell = |tf: &Transform| {
        game::cell_of_pos(tf.translation.x, tf.translation.z, cc).filter(|c| pending.contains_key(c))
    };
    for (tf, parent, bricks) in &chunks {
        if parent.parent() != root {
            continue;
        }
        if let Some(c) = cell(tf) {
            let entry = counts.entry(c).or_default();
            entry.0 += 1;
            entry.1 += bricks.0.len() as u32;
        }
    }
    for (tf, parent) in &meshed {
        if parent.parent() != root {
            continue;
        }
        if let Some(c) = cell(tf) {
            counts.entry(c).or_default().2 += 1;
        }
    }
    let mut ready = Vec::new();
    for (cell, want) in pending {
        let (age, streak) = state.entry(cell).or_default();
        *age += 1;
        let (landed, bricks, meshed) = counts.get(&cell).copied().unwrap_or_default();
        if landed >= want && meshed >= bricks {
            *streak += 1;
        } else {
            *streak = 0;
        }
        if *streak >= NEAR_HANDOFF_HOLD_FRAMES || *age >= CELL_REVEAL_MAX_FRAMES {
            ready.push(cell);
        }
    }
    if !ready.is_empty() {
        mask.cells.extend(ready);
    }
}

/// A masked LOD cell's grid, parked off `LodVoxelData` so both LOD paths
/// drop it (brick residency reacts to the removal; raster also hides it).
#[derive(Component)]
pub struct LodMaskStash(pub game::lod::PackedLodGrid);

/// Hide/unhide LOD cells under the home region's active cells (cell mode).
/// Re-evaluates everything when the mask changed, else only new LOD cells.
///
/// A flip also re-meshes the flipped cell's same-key neighbours: raster LOD
/// meshing culls faces against neighbour grids and only re-runs on
/// `Changed<LodVoxelData>`, so a neighbour meshed while this cell was present
/// would keep its wall toward it culled — and that inward wall is exactly
/// what closes the seam between near terrain and LOD (sky showed through).
/// The touch goes through `commands`, so it lands in the same flush as the
/// removal and the re-mesh sees the masked cell as air.
pub fn mask_home_lod(
    mut commands: Commands,
    mask: Res<HomeCellMask>,
    cells: Query<(
        Entity,
        &lod::LodChunk,
        &Transform,
        &ChildOf,
        Option<&lod::LodVoxelData>,
        Option<&LodMaskStash>,
        Option<&lod::LodNeighbors>,
        Ref<lod::LodChunk>,
    )>,
    roots: Query<&Transform, Without<lod::LodChunk>>,
) {
    let Some(home) = mask.region else { return };
    let all = mask.is_changed();
    let mut flipped: BTreeSet<Entity> = BTreeSet::new();
    let mut touch: BTreeSet<Entity> = BTreeSet::new();
    for (e, chunk, tf, parent, data, stash, neighbors, added) in &cells {
        if !all && !added.is_added() {
            continue;
        }
        let Ok(root) = roots.get(parent.parent()) else { continue };
        let min = root.translation + tf.translation;
        let size = (32 * chunk.factor) as f32;
        let masked =
            game::lod_cell_masked(home, &mask.cells, mask.cell_chunks, [min.x, min.z], size);
        match (masked, data, stash) {
            (true, Some(d), None) => {
                commands
                    .entity(e)
                    .insert((LodMaskStash(d.0.clone()), Visibility::Hidden))
                    .remove::<lod::LodVoxelData>();
            }
            (false, None, Some(st)) => {
                commands
                    .entity(e)
                    .insert((lod::LodVoxelData(st.0.clone()), Visibility::Inherited))
                    .remove::<LodMaskStash>();
            }
            _ => continue,
        }
        flipped.insert(e);
        if let Some(n) = neighbors {
            touch.extend(n.0.values().copied());
        }
    }
    // Re-insert (not mutate) so the `Changed` lands with the flip's flush.
    // Flipped cells are skipped: they already get fresh data or none.
    for ne in touch.difference(&flipped) {
        if let Ok((.., Some(d), None, _, _)) = cells.get(*ne) {
            commands.entity(*ne).insert(lod::LodVoxelData(d.0.clone()));
        }
    }
}

/// Drain `LodRings.pending`: spawn a root per `Data` payload (decoding the
/// RLE grids; undecodable cells drop with a warning — wire data is
/// untrusted), retire the old root on re-receipt, and move `Remove`d keys'
/// roots to the retirement list.
pub fn apply_lod_events(mut commands: Commands, mut rings: ResMut<LodRings>) {
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
            if let LodEventMsg::Data(_, payload) = msg {
                cells += payload.len();
            }
            over
        })
        .unwrap_or(pending.len());
    rings.pending = pending.split_off(take);
    let mut stall = game::perf::StallGuard::new("apply_lod_events");
    stall.add_work(pending.len() as u32);
    for msg in pending {
        match msg {
            LodEventMsg::Data(key, payload) => {
                let root = spawn_lod_root(&mut commands, key, &payload);
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
    mut hold: Local<BTreeMap<RegionCoords, (Entity, u32)>>,
) {
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
        near_regions_meshed(&near, &unmeshed_near, &mut hold)
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

type ReadyLodCellFilter = Or<(
    With<Mesh3d>,
    With<ChunkQuads>,
    With<FarVoxelReady>,
    With<LodRasterReady>,
)>;
/// A near chunk not yet on screen: its snapshot voxels are still being
/// derived, or it has voxels but its first mesh has not attached
/// (`apply_meshed_chunks` inserts `ChunkConnectivity` for every finished
/// mesh, all-air ones included, and strips it when the voxels go away).
type UnmeshedNearChunkFilter = Or<(
    With<PendingSnapshotVoxels>,
    (With<VoxelData>, Without<ChunkConnectivity>),
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

    use super::super::far_voxel::FarVoxelReady;
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
            .init_resource::<HomeCellMask>()
            .add_systems(Update, (apply_lod_events, mask_home_lod, retire_lod_roots).chain());
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

    fn mesh_vertices(app: &App, e: Entity) -> Option<usize> {
        let h = &app.world().get::<Mesh3d>(e)?.0;
        app.world().resource::<Assets<Mesh>>().get(h).map(|m| m.count_vertices())
    }

    /// An activated cell's LOD stays until every chunk the sim reported for
    /// it has landed and all their bricks have meshed, held for
    /// NEAR_HANDOFF_HOLD_FRAMES — no hole while the near chunks are still
    /// being built.
    #[test]
    fn an_active_cell_is_masked_only_once_its_chunks_have_meshed() {
        let mut app = App::new();
        app.init_resource::<HomeCellMask>()
            .init_resource::<RegionRoots>()
            .add_systems(Update, reveal_meshed_cells);
        let home = RegionCoords::new(0, 0);
        let cell = game::CellCoords::new(1, 0);
        let root = app.world_mut().spawn_empty().id();
        app.world_mut().resource_mut::<RegionRoots>().0.insert(home, root);
        {
            let mut mask = app.world_mut().resource_mut::<HomeCellMask>();
            mask.region = Some(home);
            mask.cell_chunks = 4; // 128-unit cells: x in [128, 256) is cell (1, 0)
            mask.active.insert(cell, 3);
        }
        // A landed chunk with one brick (the bridge's shape: both children
        // of the region root, the brick at the chunk's origin).
        let mut chunk = |x: f32| {
            let world = app.world_mut();
            let chunk = world.spawn((Transform::from_xyz(x, 0.0, 0.0), ChildOf(root))).id();
            let brick = world
                .spawn((BrickOf { chunk, local: [0; 3] }, Transform::from_xyz(x, 0.0, 0.0), ChildOf(root)))
                .id();
            world.entity_mut(chunk).insert(ChunkBricks(vec![brick]));
            brick
        };
        let (a, b) = (chunk(128.0), chunk(160.0));
        // A third chunk, still deriving its bricks.
        let c = app
            .world_mut()
            .spawn((
                Transform::from_xyz(192.0, 0.0, 0.0),
                ChildOf(root),
                ChunkBricks::default(),
                PendingSnapshotVoxels,
            ))
            .id();
        let masked = |app: &App| app.world().resource::<HomeCellMask>().cells.contains(&cell);
        for _ in 0..NEAR_HANDOFF_HOLD_FRAMES + 2 {
            app.update();
        }
        assert!(!masked(&app), "nothing meshed yet: the LOD stays");
        app.world_mut().entity_mut(a).insert(ChunkConnectivity(0));
        for _ in 0..NEAR_HANDOFF_HOLD_FRAMES + 2 {
            app.update();
        }
        assert!(!masked(&app), "one of two chunks meshed: the LOD stays");
        app.world_mut().entity_mut(b).insert(ChunkConnectivity(0));
        for _ in 0..NEAR_HANDOFF_HOLD_FRAMES + 2 {
            app.update();
        }
        assert!(!masked(&app), "a chunk whose bricks have not landed: the LOD stays");
        // It lands with no bricks at all (all air): nothing more to mesh.
        app.world_mut().entity_mut(c).remove::<PendingSnapshotVoxels>();
        app.update();
        assert!(!masked(&app), "held for NEAR_HANDOFF_HOLD_FRAMES first");
        for _ in 0..NEAR_HANDOFF_HOLD_FRAMES {
            app.update();
        }
        assert!(masked(&app), "fully meshed: the near cell takes over");
    }

    /// Cell mode: activating a cell masks the home LOD cell under it, and the
    /// UNMASKED neighbour must re-mesh with that cell as air — its wall toward
    /// the masked cell is what closes the near/LOD seam from inside the
    /// active cell. Before the fix the neighbour kept its stale mesh (wall
    /// culled against the now-hidden cell) and sky showed through.
    #[test]
    fn masking_a_cell_remeshes_its_neighbours_wall_toward_it() {
        let mut app = rings_app();
        let home = RegionCoords::new(0, 0);
        {
            let mut mask = app.world_mut().resource_mut::<HomeCellMask>();
            mask.region = Some(home);
            // One factor-4 LOD cell (32 * 4 units) == one 4-chunk game cell.
            mask.cell_chunks = 4;
        }
        let key = LodKey::for_region(4, home);
        let packed = PackedLodGrid::encode(&one_solid_grid());
        push_event(&mut app, LodEventMsg::Data(key, vec![([0, 0, 0], packed.clone()), ([1, 0, 0], packed)]));
        assert!(wait_until(&mut app, |app| slot_root(app, key).is_some()));
        let root = slot_root(&app, key).unwrap();
        let child_at = |app: &mut App, x: f32| {
            let world = app.world_mut();
            let mut q = world.query::<(Entity, &Transform, &ChildOf)>();
            q.iter(world)
                .find(|(_, tf, c)| c.parent() == root && tf.translation.x == x)
                .map(|(e, ..)| e)
                .unwrap()
        };
        let (kept, masked) = (child_at(&mut app, 0.0), child_at(&mut app, 128.0));
        assert!(wait_until(&mut app, |app| mesh_vertices(app, kept).is_some()
            && mesh_vertices(app, masked).is_some()));
        let before = mesh_vertices(&app, kept).unwrap();

        app.world_mut()
            .resource_mut::<HomeCellMask>()
            .cells
            .insert(game::CellCoords::new(1, 0));
        assert!(
            wait_until(&mut app, |app| app.world().get::<LodVoxelData>(masked).is_none()),
            "the cell under the active game cell is masked"
        );
        assert!(
            wait_until(&mut app, |app| mesh_vertices(app, kept).is_some_and(|v| v > before)),
            "the neighbour must re-mesh and gain its +X wall toward the masked cell"
        );
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
            push_event(&mut app, LodEventMsg::Data(*key, big.clone()));
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
    fn re_receipt_retires_old_root_when_brick_replacement_is_ready() {
        let mut app = rings_app();
        let key = LodKey {
            factor: 16,
            coords: [0, 0],
        };
        let old_root = app.world_mut().spawn_empty().id();
        let new_root = app.world_mut().spawn_empty().id();
        app.world_mut().spawn((FarVoxelReady, ChildOf(new_root)));
        app.world_mut().resource_mut::<LodRings>().roots.insert(
            key,
            LodSlot {
                root: new_root,
                retiring: Some(old_root),
                retiring_frames: 0,
            },
        );

        app.update();

        assert!(app.world().get_entity(old_root).is_err());
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
        push_event(&mut app, LodEventMsg::Data(key, solid_payload([0, 2, 0])));
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
        push_event(&mut app, LodEventMsg::Data(key, solid_payload([0, 0, 0])));
        assert!(
            wait_until(&mut app, |app| slot_root(app, key)
                .is_some_and(|r| root_meshed(app, r))),
            "the first root must settle to meshed children"
        );
        let old_root = slot_root(&app, key).unwrap();

        // Refresh: the old root must survive every frame until the NEW root
        // has a meshed child (the invariant is asserted inside the wait).
        push_event(&mut app, LodEventMsg::Data(key, solid_payload([0, 1, 0])));
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
        push_event(&mut app, LodEventMsg::Data(key, solid_payload([0, 0, 0])));
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
        push_event(&mut app, LodEventMsg::Data(key4, solid_payload([0, 0, 0])));
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
        push_event(&mut app, LodEventMsg::Data(key16, solid_payload([0, 0, 0])));
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
        push_event(app, LodEventMsg::Data(key, solid_payload([0, 0, 0])));
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
        push_event(&mut app, LodEventMsg::Data(key, bad));
        app.update();
        let root = slot_root(&app, key).expect("root spawns even if all cells drop");
        assert_eq!(
            lod_children(&mut app, root).len(),
            0,
            "bad cell dropped with a warning"
        );
    }
}
