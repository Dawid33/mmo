//! Server-streamed far terrain — the beyond-subscription view distance.
//!
//! The instance layer holds `RequestLod` subscriptions by ring assignment
//! (`instance::update_lod_window`); the bridge routes the resulting
//! `ClientUpdateEvent::StudLodData` / `RemoveLod` events into
//! `LodRings.pending`. `apply_lod_events` spawns one root per `LodKey`, whose
//! far tile is meshed by `stud::far` (terrain and water children). A key that
//! is refreshed (re-receipt) or removed keeps its old root rendering until a
//! replacement is visibly meshed (`retire_lod_roots`), so transitions never
//! pop the horizon to empty. A key handed over to the near window stays until
//! that region's authoritative chunks have ALL meshed — the switch is one
//! swap, not a far tile that vanishes while the near terrain fills in chunk
//! by chunk.

use std::collections::{BTreeMap, BTreeSet};

use bevy::prelude::*;
use game::lod::LodKey;
use game::RegionCoords;

use super::bridge::RegionRoots;

/// Deadline for retiring/retired roots: if the replacement hasn't shown a
/// mesh within this many frames (~10s at 60fps), despawn the old root anyway
/// — a late pop beats leaking an entity tree forever (e.g. a replacement
/// whose tile does not decode never produces a mesh at all).
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
    /// The far tile for one key (`game::stud_lod`) — initial send or refresh
    /// (replace-on-re-receipt) — drawn by `stud::far`.
    StudData(LodKey, game::StudLodTile),
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

/// Far tiles `apply_lod_events` may hand to the mesher per frame before
/// deferring the remaining messages: spawning a root and moving its tile into
/// a compute job is cheap, this only stops a burst from stacking into one frame.
pub const LOD_APPLY_TILES_PER_FRAME: usize = 64;

/// Drain `LodRings.pending`: spawn a root per `StudData` tile (its mesh job
/// drops a tile that does not decode — wire data is untrusted), retire the old
/// root on re-receipt, and move `Remove`d keys' roots to the retirement list.
pub fn apply_lod_events(
    mut commands: Commands,
    mut rings: ResMut<LodRings>,
    mut stud_tasks: Option<ResMut<super::stud::far::StudLodTasks>>,
) {
    let mut pending = std::mem::take(&mut rings.pending);
    // Messages stay in arrival order; whatever exceeds the budget waits in
    // `pending` for the next frame, and at least one message always makes
    // progress.
    let mut tiles = 0usize;
    let take = pending
        .iter()
        .position(|msg| {
            let over = tiles >= LOD_APPLY_TILES_PER_FRAME;
            if matches!(msg, LodEventMsg::StudData(..)) {
                tiles += 1;
            }
            over
        })
        .unwrap_or(pending.len());
    rings.pending = pending.split_off(take);
    let mut stall = game::perf::StallGuard::new("apply_lod_events");
    stall.add_work(pending.len() as u32);
    for msg in pending {
        match msg {
            LodEventMsg::StudData(key, tile) => {
                // Without the stud mesher (an app that draws no terrain) there
                // is nothing to show the tile with.
                let Some(tasks) = stud_tasks.as_mut() else { continue };
                let root = super::stud::far::spawn_stud_lod_root(&mut commands, tasks, key, tile);
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
                        v.insert(LodSlot { root, retiring: None, retiring_frames: 0 });
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
                rings.retired.push(RetiredLod { key, entity: slot.root, frames: 0, near_takeover });
            }
        }
    }
}

/// A far root's water surface child (`stud::far`).
#[derive(Component)]
pub struct LodWaterSurface;

/// Despawn retiring/retired LOD roots once their replacement is actually
/// VISIBLE — the point of retirement is that a transition never pops the
/// horizon to empty (under-draw, the design's named failure mode). Until
/// then the old root keeps rendering stale terrain on top of (or instead
/// of) the new — over-draw, the guard rails' accepted trade.
///
/// Visibility means meshes, not entities: a replacement root draws nothing
/// until its far tile's mesh job has landed (`stud::far`). Hence:
/// - re-receipt: the slot's new `root` has a child carrying `Mesh3d`;
/// - removal: some live slot with a meshed child covers one of the retired
///   key's regions, OR the near window holds one of those regions and it is
///   fully meshed ([`near_regions_meshed`]) — for a near-window takeover,
///   every region of the key must be replaced one of those two ways.
///
/// Each root also carries a frame deadline (`retirement_max_frames`): a
/// replacement that never meshes (a tile that does not decode, a key released
/// with no successor in view) must not pin the old root forever. Near-window
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
    // flying, so skip it when nobody needs the answer.
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

/// A meshed far tile's terrain. The water surface child has a `Mesh3d` too,
/// but it is not terrain: a root showing only water has not replaced anything.
type ReadyLodCellFilter = (With<Mesh3d>, Without<LodWaterSurface>);
/// A near chunk not yet on screen: a stud chunk whose first mesh has not
/// landed (`stud::systems::apply_stud_meshes` gives every meshed chunk its
/// children, empty ones included).
type UnmeshedNearChunkFilter = (With<super::stud::SimStudChunk>, Without<super::stud::systems::StudMeshChildren>);

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

    use super::super::stud::far::{apply_stud_lod_meshes, StudLodRoot, StudLodTasks, StudLodTerrain};
    use super::super::stud::material::{StudMaterials, StudTerrainMaterial};
    use super::super::stud::systems::StudMeshChildren;
    use super::super::stud::SimStudChunk;
    use super::super::water::material::{WaterMaterial, WaterMaterialHandle};
    use game::{StudLodSample, StudLodTile};

    /// The two LOD-ring systems and the near-window resource, with no mesher:
    /// roots, slots and retirement only.
    fn rings_app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_resource::<RegionRoots>()
            .init_resource::<LodRings>()
            .add_systems(Update, (apply_lod_events, retire_lod_roots).chain());
        app
    }

    /// `rings_app` plus what the far tiles are drawn with.
    fn stud_rings_app() -> App {
        bevy::tasks::AsyncComputeTaskPool::get_or_init(bevy::tasks::TaskPool::new);
        let mut app = rings_app();
        app.init_asset::<WaterMaterial>()
            .init_resource::<WaterMaterialHandle>()
            .init_asset::<StudTerrainMaterial>()
            .init_resource::<StudMaterials>()
            .init_resource::<StudLodTasks>()
            .add_systems(Update, apply_stud_lod_meshes.after(apply_lod_events));
        app
    }

    fn wait_until(app: &mut App, mut cond: impl FnMut(&mut App) -> bool) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            app.update();
            if cond(app) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        false
    }

    fn push_event(app: &mut App, msg: LodEventMsg) {
        app.world_mut().resource_mut::<LodRings>().pending.push(msg);
    }

    fn slot_root(app: &App, key: LodKey) -> Option<Entity> {
        app.world().resource::<LodRings>().roots.get(&key).map(|s| s.root)
    }

    /// A terrain-meshed child for `root`, as a landed far tile leaves it.
    fn mesh_root(app: &mut App, root: Entity) {
        app.world_mut().spawn((Mesh3d(Handle::default()), ChildOf(root)));
    }

    /// A slot for `key` whose root is already meshed (no job involved).
    fn meshed_slot(app: &mut App, key: LodKey) -> Entity {
        let root = app.world_mut().spawn_empty().id();
        mesh_root(app, root);
        app.world_mut().resource_mut::<LodRings>().roots.insert(key, LodSlot { root, retiring: None, retiring_frames: 0 });
        root
    }

    /// A near region as the bridge spawns it from a snapshot: a root and `n`
    /// stud chunks under it, none meshed yet.
    fn spawn_near_region(app: &mut App, rc: RegionCoords, n: usize) -> Vec<Entity> {
        let offset = rc.world_offset();
        let root = app.world_mut().spawn((Transform::from_translation(Vec3::from_array(offset)), Visibility::default())).id();
        app.world_mut().resource_mut::<RegionRoots>().0.insert(rc, root);
        (0..n)
            .map(|x| {
                let coords = game::StudChunkCoords { x, y: 0, z: 0 };
                app.world_mut().spawn((SimStudChunk { coords, chunk: game::StudChunk::default() }, ChildOf(root))).id()
            })
            .collect()
    }

    /// The chunk's first mesh lands, as `stud::systems::apply_stud_meshes` does it.
    fn land(app: &mut App, chunk: Entity) {
        let child = app.world_mut().spawn(ChildOf(chunk)).id();
        app.world_mut().entity_mut(chunk).insert(StudMeshChildren { body: child, top: child, glass: child });
    }

    /// A stud key one region wide at this build's region size.
    fn stud_key(rc: RegionCoords) -> LodKey {
        let key = LodKey::for_region(game::stud_ring_table(game::lod::LOD_RING_TABLE)[0].1, rc);
        assert_eq!(key.region_span(), 1);
        key
    }

    /// A sloping tile for `key`; with `sea`, water stands over its low half.
    fn stud_tile(key: &LodKey, sea: bool) -> StudLodTile {
        let side = StudLodTile::side_for(key);
        let n = usize::from(side) + 1;
        let samples: Vec<StudLodSample> = (0..n * n)
            .map(|i| {
                let top = (i % n) as i32 - 8;
                StudLodSample::of_column(top, game::MAT_GRASS, (sea && top < 0).then_some(0))
            })
            .collect();
        StudLodTile::encode(side, &samples)
    }

    fn stud_children(app: &mut App, root: Entity) -> (usize, usize) {
        let world = app.world_mut();
        let mut terrain = world.query_filtered::<&ChildOf, (With<StudLodTerrain>, With<Mesh3d>)>();
        let t = terrain.iter(world).filter(|c| c.parent() == root).count();
        let mut water = world.query_filtered::<&ChildOf, (With<LodWaterSurface>, With<Mesh3d>)>();
        (t, water.iter(world).filter(|c| c.parent() == root).count())
    }

    fn meshed_stud_root(app: &mut App, key: LodKey) -> Entity {
        push_event(app, LodEventMsg::StudData(key, stud_tile(&key, false)));
        assert!(wait_until(app, |app| slot_root(app, key).is_some_and(|r| stud_children(app, r).0 == 1)));
        slot_root(app, key).unwrap()
    }

    /// A tile takes its key's slot: a root at the key's
    /// world offset whose terrain (and water) land from the mesh job, and a
    /// re-receipt keeps the old root until the replacement's terrain is meshed.
    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn stud_tiles_take_the_ring_slots_and_replace_on_re_receipt() {
        let mut app = stud_rings_app();
        let key = stud_key(RegionCoords::new(3, -2));
        push_event(&mut app, LodEventMsg::StudData(key, stud_tile(&key, true)));
        app.update();
        let root = slot_root(&app, key).expect("the tile's root is the key's slot");
        assert_eq!(app.world().get::<StudLodRoot>(root).map(|r| r.0), Some(key));
        assert_eq!(app.world().get::<Transform>(root).unwrap().translation, Vec3::from_array(key.world_offset()));
        assert!(wait_until(&mut app, |app| stud_children(app, root) == (1, 1)), "one terrain and one water child");
        assert!(app.world().resource::<StudLodTasks>().is_empty());

        // Refresh, dry this time: the old root (with its water) stays until the
        // new terrain is on screen, then goes whole.
        push_event(&mut app, LodEventMsg::StudData(key, stud_tile(&key, false)));
        app.update();
        let fresh = slot_root(&app, key).unwrap();
        assert_ne!(fresh, root);
        assert_eq!(app.world().resource::<LodRings>().roots[&key].retiring, Some(root));
        assert!(app.world().get_entity(root).is_ok());
        assert!(wait_until(&mut app, |app| app.world().get_entity(root).is_err()), "retired once the replacement meshed");
        assert_eq!(stud_children(&mut app, fresh), (1, 0));
        assert_eq!(app.world().resource::<LodRings>().roots.len(), 1);
    }

    /// The hand-over to the near window on stud terrain: the far tile of a region
    /// that joined the near window keeps drawing until every stud chunk of the
    /// region has its first mesh, then goes — no hole, no lasting double draw.
    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn a_loaded_stud_region_releases_the_far_tile_it_covers() {
        let mut app = stud_rings_app();
        let rc = RegionCoords::new(1, -1);
        let key = stud_key(rc);
        let far_root = meshed_stud_root(&mut app, key);

        push_event(&mut app, LodEventMsg::Remove { key, near_takeover: true });
        // The region as the bridge spawns it: a root and two stud chunks, unmeshed.
        let chunks = spawn_near_region(&mut app, rc, 2);
        for _ in 0..NEAR_HANDOFF_HOLD_FRAMES + 5 {
            app.update();
        }
        assert!(app.world().get_entity(far_root).is_ok(), "an unmeshed stud region keeps its far tile");
        land(&mut app, chunks[0]);
        for _ in 0..NEAR_HANDOFF_HOLD_FRAMES + 5 {
            app.update();
        }
        assert!(app.world().get_entity(far_root).is_ok(), "a partly meshed one too, past any deadline");
        land(&mut app, chunks[1]);
        assert!(wait_until(&mut app, |app| app.world().get_entity(far_root).is_err()), "released once every chunk is meshed");
        assert!(app.world().resource::<LodRings>().retired.is_empty());
        assert!(slot_root(&app, key).is_none());
    }

    /// Untrusted wire data: a tile sized for another key, or one that does not
    /// decode, leaves an empty root (no mesh job result to show); and an app
    /// without the stud mesher ignores stud payloads.
    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn bad_stud_tiles_draw_nothing() {
        let mut app = stud_rings_app();
        let key = stud_key(RegionCoords::new(0, 4));
        let mut wrong_side = stud_tile(&key, true);
        wrong_side.side -= 1;
        push_event(&mut app, LodEventMsg::StudData(key, wrong_side));
        app.update();
        let root = slot_root(&app, key).expect("the root still spawns");
        assert!(app.world().resource::<StudLodTasks>().is_empty(), "no job for it");

        let other = stud_key(RegionCoords::new(0, 5));
        let mut cut = stud_tile(&other, true);
        cut.heights.pop();
        push_event(&mut app, LodEventMsg::StudData(other, cut));
        assert!(wait_until(&mut app, |app| slot_root(app, other).is_some() && app.world().resource::<StudLodTasks>().is_empty()));
        for r in [root, slot_root(&app, other).unwrap()] {
            assert_eq!(stud_children(&mut app, r), (0, 0));
        }

        let mut plain = rings_app();
        push_event(&mut plain, LodEventMsg::StudData(key, stud_tile(&key, false)));
        plain.update();
        assert!(slot_root(&plain, key).is_none());
        assert!(plain.world().resource::<LodRings>().pending.is_empty());
    }

    /// A burst of tiles must not stack into one frame: the budget defers the
    /// excess, in arrival order, and every key still gets its root.
    #[test]
    fn a_tile_burst_is_spread_over_frames_in_arrival_order() {
        let mut app = stud_rings_app();
        let n = LOD_APPLY_TILES_PER_FRAME + 3;
        let keys: Vec<LodKey> = (0..n as i32).map(|x| stud_key(RegionCoords::new(10 + x, 0))).collect();
        for key in &keys {
            // Undecodable on purpose: the slots are the subject, not the meshes.
            push_event(&mut app, LodEventMsg::StudData(*key, StudLodTile::default()));
        }
        app.update();
        assert!(slot_root(&app, keys[LOD_APPLY_TILES_PER_FRAME - 1]).is_some(), "a frame's worth lands");
        assert!(slot_root(&app, keys[LOD_APPLY_TILES_PER_FRAME]).is_none(), "a full budget defers the rest");
        assert_eq!(app.world().resource::<LodRings>().pending.len(), 3);
        app.update();
        assert!(keys.iter().all(|k| slot_root(&app, *k).is_some()), "deferred tiles keep arrival order and all land");
        assert!(app.world().resource::<LodRings>().pending.is_empty());
    }

    #[test]
    fn re_receipt_retires_the_old_root_once_the_replacement_is_meshed() {
        let mut app = rings_app();
        let key = LodKey { factor: 32, coords: [0, 0] };
        let old_root = app.world_mut().spawn_empty().id();
        let new_root = app.world_mut().spawn_empty().id();
        app.world_mut().resource_mut::<LodRings>().roots.insert(key, LodSlot { root: new_root, retiring: Some(old_root), retiring_frames: 0 });
        for _ in 0..5 {
            app.update();
        }
        assert!(app.world().get_entity(old_root).is_ok(), "old root despawned before the replacement meshed — horizon pop");
        mesh_root(&mut app, new_root);
        app.update();
        assert!(app.world().get_entity(old_root).is_err(), "the replacement is renderable");
        assert!(app.world().resource::<LodRings>().roots[&key].retiring.is_none());
    }

    #[test]
    fn remove_lod_retires_until_deadline_or_coverage() {
        let mut app = rings_app();
        app.world_mut().resource_mut::<LodRings>().retirement_max_frames = 3;
        let key = LodKey { factor: 64, coords: [0, 0] };
        let root = meshed_slot(&mut app, key);

        push_event(&mut app, LodEventMsg::Remove { key, near_takeover: false });
        app.update();
        assert!(slot_root(&app, key).is_none(), "slot gone after Remove");
        assert!(
            app.world().get_entity(root).is_ok(),
            "no replacement ever comes: the retired root keeps rendering until the deadline"
        );
        assert!(wait_until(&mut app, |app| app.world().get_entity(root).is_err()), "deadline must despawn the retired root");
        assert!(app.world().resource::<LodRings>().retired.is_empty());
    }

    #[test]
    fn removed_key_releases_when_another_meshed_key_covers_its_region() {
        let mut app = rings_app();
        app.world_mut().resource_mut::<LodRings>().retirement_max_frames = u32::MAX;
        let fine = LodKey::for_region(32, RegionCoords::new(0, 0));
        let old_root = meshed_slot(&mut app, fine);

        push_event(&mut app, LodEventMsg::Remove { key: fine, near_takeover: false });
        for _ in 0..5 {
            app.update();
        }
        assert!(app.world().get_entity(old_root).is_ok(), "no coverage yet: the retired root must keep rendering");

        // The ring reassignment's replacement: a coarser key over the same region,
        // not meshed at first.
        let coarse = LodKey::for_region(64, RegionCoords::new(0, 0));
        assert!(coarse.regions().contains(&RegionCoords::new(0, 0)));
        let new_root = app.world_mut().spawn_empty().id();
        app.world_mut().resource_mut::<LodRings>().roots.insert(coarse, LodSlot { root: new_root, retiring: None, retiring_frames: 0 });
        for _ in 0..5 {
            app.update();
        }
        assert!(app.world().get_entity(old_root).is_ok(), "an unmeshed cover replaces nothing");
        mesh_root(&mut app, new_root);
        app.update();
        assert!(app.world().get_entity(old_root).is_err(), "retired root despawns once a covering key is meshed");
        assert!(app.world().resource::<LodRings>().retired.is_empty());
    }

    /// Server genesis can take seconds: a takeover waits for the region
    /// however long that is, instead of popping on the frame deadline.
    #[test]
    fn near_takeover_ignores_the_retirement_deadline() {
        let mut app = rings_app();
        app.world_mut().resource_mut::<LodRings>().retirement_max_frames = 3;
        let rc = RegionCoords::new(0, 0);
        let key = LodKey::for_region(32, rc);
        let lod_root = meshed_slot(&mut app, key);

        push_event(&mut app, LodEventMsg::Remove { key, near_takeover: true });
        for _ in 0..20 {
            app.update();
        }
        assert!(app.world().get_entity(lod_root).is_ok(), "region not even arrived yet: the far tile must keep rendering");

        let chunks = spawn_near_region(&mut app, rc, 1);
        land(&mut app, chunks[0]);
        assert!(wait_until(&mut app, |app| app.world().get_entity(lod_root).is_err()));
    }

    /// A takeover whose region leaves the near window again before it
    /// arrives is released by the key re-requested for it instead.
    #[test]
    fn near_takeover_releases_on_returning_lod_cover() {
        let mut app = rings_app();
        let key = LodKey::for_region(32, RegionCoords::new(3, 3));
        let lod_root = meshed_slot(&mut app, key);
        push_event(&mut app, LodEventMsg::Remove { key, near_takeover: true });
        app.update();
        let new_root = meshed_slot(&mut app, key);
        assert_ne!(new_root, lod_root);
        assert!(wait_until(&mut app, |app| app.world().get_entity(lod_root).is_err()));
        assert!(app.world().resource::<LodRings>().retired.is_empty());
    }
}
