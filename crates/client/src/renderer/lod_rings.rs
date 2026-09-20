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
//! empty.

use std::collections::{BTreeMap, BTreeSet};

use bevy::prelude::*;
use game::lod::LodKey;
use game::{LodPayload, RegionCoords, REGION_CHUNKS};

use super::bridge::Regions;
use super::far_voxel::FarVoxelReady;
use super::meshing::lod;
use super::meshing::lod::LodRasterReady;
use super::meshing::queue::ChunkKey;
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

/// A LOD event routed from the bridge (`drain_client_updates`), applied by
/// `apply_lod_events` the same frame.
pub enum LodEventMsg {
    /// Streamed cells for one key — initial send or refresh
    /// (replace-on-re-receipt).
    Data(LodKey, LodPayload),
    /// The ring assignment moved on: retire this key's render root.
    Remove(LodKey),
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
            LodEventMsg::Remove(key) => {
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
///   key's regions, OR the near window holds one of those regions and some
///   `ChunkKey`ed meshed entity's key lies in its chunk bounds.
///
/// "≥1 meshed child" (not all children) is deliberate: all-air cells never
/// receive a `Mesh3d`, so an all-children predicate would always fall
/// through to the deadline; the trade is at worst a few budget-frames of
/// partial fill instead of a guaranteed full hole.
///
/// Each root also carries a frame deadline (`retirement_max_frames`): a
/// replacement that never meshes (all-air cells, a key released with no
/// successor in view) must not pin the old root forever.
pub fn retire_lod_roots(
    mut commands: Commands,
    mut rings: ResMut<LodRings>,
    near: Res<Regions>,
    children: Query<&Children>,
    meshed: Query<(), ReadyLodCellFilter>,
    keyed_meshes: Query<&ChunkKey, ReadyNearChunkFilter>,
) {
    if rings.retired.is_empty() && rings.roots.values().all(|s| s.retiring.is_none()) {
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
    let near_meshed = |rc: &RegionCoords| {
        near.0.contains_key(rc) && {
            let x0 = rc.x * REGION_CHUNKS as i32;
            let z0 = rc.z * REGION_CHUNKS as i32;
            let span = REGION_CHUNKS as i32;
            keyed_meshes
                .iter()
                .any(|k| (x0..x0 + span).contains(&k.0[0]) && (z0..z0 + span).contains(&k.0[2]))
        }
    };

    let retired = std::mem::take(&mut rings.retired);
    rings.retired = retired
        .into_iter()
        .filter_map(|mut r| {
            r.frames += 1;
            let regions = r.key.regions();
            let replaced =
                regions.iter().any(|rc| covered.contains(rc)) || regions.iter().any(near_meshed);
            if replaced || r.frames >= max {
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
type ReadyNearChunkFilter = Or<(With<Mesh3d>, With<ChunkQuads>)>;

#[cfg(test)]
mod tests {
    use super::*;
    use game::lod::PackedLodGrid;
    use game::{BlockId, GameDataUpdate, Voxel, CHUNK_VOXEL_COUNT};

    use super::super::far_voxel::FarVoxelReady;
    use super::super::meshing::lod::{LodChunk, LodVoxelData};
    use super::super::meshing::queue::tests::{pipeline_app, wait_until};
    use super::super::meshing::queue::MeshBudgets;
    use super::super::meshing::records::QuadRecord;
    use super::super::voxel_pull::ChunkQuads;

    use bevy::asset::RenderAssetUsages;
    use bevy::mesh::{Mesh, PrimitiveTopology};

    /// The full meshing/LOD pipeline (`pipeline_app`) plus the two LOD-ring
    /// systems and the near-window exclusion resource.
    fn rings_app() -> App {
        let mut app = pipeline_app(MeshBudgets::default());
        app.init_resource::<Regions>()
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
        vec![Voxel::new(BlockId(1)); CHUNK_VOXEL_COUNT]
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

    /// An (empty) mesh handle — `retire_lod_roots` only checks `Mesh3d`
    /// presence.
    fn meshed_marker(app: &mut App) -> Mesh3d {
        let mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::RENDER_WORLD,
        );
        Mesh3d(app.world_mut().resource_mut::<Assets<Mesh>>().add(mesh))
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
    fn removed_key_releases_when_pulled_near_chunk_covers_its_region() {
        let mut app = rings_app();
        let key = LodKey {
            factor: 4,
            coords: [1, -1],
        };
        let old_root = app.world_mut().spawn_empty().id();
        app.world_mut()
            .resource_mut::<LodRings>()
            .retired
            .push(RetiredLod {
                key,
                entity: old_root,
                frames: 0,
            });
        let (_keep_send, recv) = crossbeam::channel::unbounded::<GameDataUpdate>();
        app.world_mut()
            .resource_mut::<Regions>()
            .0
            .insert(RegionCoords::new(1, -1), recv);
        app.world_mut()
            .spawn((ChunkKey([32, 0, -32]), pulled_marker()));

        app.update();

        assert!(
            app.world().get_entity(old_root).is_err(),
            "pulled near chunk provides coverage"
        );
        assert!(app.world().resource::<LodRings>().retired.is_empty());
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

        push_event(&mut app, LodEventMsg::Remove(key));
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

        push_event(&mut app, LodEventMsg::Remove(key4));
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

    #[test]
    fn removed_key_releases_when_a_near_chunk_meshes_inside_its_region() {
        let mut app = rings_app();
        app.world_mut()
            .resource_mut::<LodRings>()
            .retirement_max_frames = u32::MAX;
        let key = LodKey {
            factor: 4,
            coords: [1, -1],
        }; // chunk keys x 32..64, z -32..0
        push_event(&mut app, LodEventMsg::Data(key, solid_payload([0, 0, 0])));
        assert!(wait_until(&mut app, |app| slot_root(app, key)
            .is_some_and(|r| root_meshed(app, r))));
        let root = slot_root(&app, key).unwrap();

        // Near-window takeover of the region.
        push_event(&mut app, LodEventMsg::Remove(key));
        let (_keep_send, recv) = crossbeam::channel::unbounded::<GameDataUpdate>();
        app.world_mut()
            .resource_mut::<Regions>()
            .0
            .insert(RegionCoords::new(1, -1), recv);

        // A meshed chunk OUTSIDE the region's bounds must not release it.
        let marker = meshed_marker(&mut app);
        app.world_mut().spawn((ChunkKey([0, 0, 0]), marker));
        for _ in 0..3 {
            app.update();
        }
        assert!(
            app.world().get_entity(root).is_ok(),
            "out-of-region mesh must not count"
        );

        // One inside the bounds does.
        let marker = meshed_marker(&mut app);
        app.world_mut().spawn((ChunkKey([32, 0, -32]), marker));
        assert!(
            wait_until(&mut app, |app| app.world().get_entity(root).is_err()),
            "retired root despawns once a region chunk is meshed"
        );
        assert!(app.world().resource::<LodRings>().retired.is_empty());
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
