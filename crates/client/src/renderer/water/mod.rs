//! Static water, drawn as its own blended pass (water plan Task 6).
//!
//! The chunk mesher treats liquid as empty, so terrain shows the bed and the
//! banks; water is drawn here instead, per region in `WATER_TILE_BLOCKS`²
//! tiles, each one Bevy `Mesh` with the blended [`material::WaterMaterial`]
//! (every backend, WebGL2 included).
//!
//! - The bridge sends [`WaterMsg`]s: a region's ground recipes when its
//!   snapshot loads or is replaced, its removal, and every applied
//!   `SetBlocks` (the edited chunk's column).
//! - A tile is built from the recipe (`ColumnGround::recipe_water`) minus the
//!   cells holding a block that is neither air nor liquid (liquid blocks
//!   still count as water until worldgen stops writing them), read from the
//!   bridge's `SimChunk` mirrors on the main thread ([`tile_input`]) once its
//!   region's snapshot chunks have all landed, then meshed on the compute
//!   pool ([`mesh::WaterMeshJob`], at most [`MAX_WATER_MESH_TASKS`] in
//!   flight). A `ChunkEdited` re-meshes every tile it touches.
//! - Tile entities are `SessionScoped` children of the region root, at the
//!   tile's origin, and cast no shadow.

pub mod material;
pub mod mesh;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use bevy::asset::RenderAssetUsages;
use bevy::light::NotShadowCaster;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::tasks::{block_on, futures_lite::future, Task};
use game::{
    BlockId, Chunk, ColumnCoords, ColumnGround, RegionId, CHUNK_BLOCKS, REGION_BLOCKS,
    REGION_CHUNKS,
};

use self::material::{sync_water_material_cut, WaterMaterial, WaterMaterialHandle};
use self::mesh::{WaterColumn, WaterMeshData, WaterMeshJob, WaterRun, WaterTileInput, SIDES};
use crate::session::{SessionResetExt, SessionScoped};

/// Edge of a water tile in blocks: each region is cut into tiles this wide,
/// each one Bevy mesh.
pub const WATER_TILE_BLOCKS: i32 = 64;

const _: () = {
    let mut i = 0;
    while i < game::REGION_BLOCKS_CHOICES.len() {
        assert!(
            game::REGION_BLOCKS_CHOICES[i] % WATER_TILE_BLOCKS as usize == 0,
            "a water tile must divide every region size"
        );
        i += 1;
    }
};

/// Water tint, linear RGB.
pub const WATER_COLOR: [f32; 3] = [0.10, 0.32, 0.45];
/// Light absorbed per metre of water: alpha = `1 − exp(−this × depth_m)`.
pub const WATER_ABSORPTION_PER_M: f32 = 0.35;
/// Least alpha of a water face (shallow water still reads as water).
pub const WATER_MIN_ALPHA: f32 = 0.35;
/// Most alpha of a water face (the bed of deep water still shows a little).
pub const WATER_MAX_ALPHA: f32 = 0.92;
/// Water tile meshes in flight on the compute pool at once.
pub const MAX_WATER_MESH_TASKS: usize = 8;
/// Water tile inputs gathered per frame (each reads up to a few hundred
/// thousand blocks from the chunk mirrors on the main thread).
pub const WATER_TILE_BUILDS_PER_FRAME: usize = 2;

/// Alpha of water `depth_m` deep (client render code: f32 `exp` is fine).
pub fn water_alpha(depth_m: f32) -> f32 {
    (1.0 - (-WATER_ABSORPTION_PER_M * depth_m).exp()).clamp(WATER_MIN_ALPHA, WATER_MAX_ALPHA)
}

/// What the bridge tells the water pass.
#[derive(Message, Debug, Clone)]
pub enum WaterMsg {
    /// A region snapshot loaded or was replaced: its ground recipes.
    Region {
        region: RegionId,
        ground: Arc<BTreeMap<ColumnCoords, ColumnGround>>,
    },
    /// The region left the window.
    RemoveRegion { region: RegionId },
    /// A `SetBlocks` was applied to the chunk in this column (the water pass
    /// ignores columns with no water in or next to them).
    ChunkEdited {
        region: RegionId,
        column: ColumnCoords,
    },
}

/// One water tile: its drawn entity, whether it needs a (re)mesh, and the
/// mesh in flight.
#[derive(Default)]
pub struct WaterTile {
    pub entity: Option<Entity>,
    pub dirty: bool,
    task: Option<Task<Option<Mesh>>>,
}

/// Every water tile, by region and tile index (region-local block / tile
/// edge). Only tiles holding recipe water exist.
#[derive(Resource, Default)]
pub struct WaterTiles(pub BTreeMap<(RegionId, [i32; 2]), WaterTile>);

/// Each loaded region's ground recipes (from [`WaterMsg::Region`]).
#[derive(Resource, Default)]
pub struct WaterGround(pub BTreeMap<RegionId, Arc<BTreeMap<ColumnCoords, ColumnGround>>>);

/// The bridge, after applying a `SetBlocks` to `chunk` of `region`: tell the
/// water pass which column it was in (from the chunk's region-local origin).
pub fn chunk_edited(world: &mut World, region: RegionId, chunk: Entity) {
    let Some(origin) = world.get::<Transform>(chunk).map(|t| t.translation) else {
        return;
    };
    let units = game::CHUNK_UNITS as f32;
    let (x, z) = ((origin.x / units).round(), (origin.z / units).round());
    if x < 0.0 || z < 0.0 {
        return;
    }
    let column = ColumnCoords::new(x as usize, z as usize);
    if let Some(mut msgs) = world.get_resource_mut::<Messages<WaterMsg>>() {
        msgs.write(WaterMsg::ChunkEdited { region, column });
    }
}

/// Tile index of region-local block column `(x, z)`.
fn tile_of(x: i32, z: i32) -> [i32; 2] {
    [
        x.div_euclid(WATER_TILE_BLOCKS),
        z.div_euclid(WATER_TILE_BLOCKS),
    ]
}

/// Every tile index of a region whose columns hold recipe water.
pub fn water_tiles(ground: &BTreeMap<ColumnCoords, ColumnGround>) -> BTreeSet<[i32; 2]> {
    let cb = CHUNK_BLOCKS;
    let mut out = BTreeSet::new();
    for (c, g) in ground {
        for lx in 0..cb {
            for lz in 0..cb {
                if g.recipe_water(lx, lz).is_some() {
                    out.insert(tile_of((c.x * cb + lx) as i32, (c.z * cb + lz) as i32));
                }
            }
        }
    }
    out
}

/// Tile indices whose tile plus apron covers any block column of `column`'s
/// footprint.
fn tiles_touching(column: ColumnCoords) -> impl Iterator<Item = [i32; 2]> {
    let cb = CHUNK_BLOCKS as i32;
    let t = WATER_TILE_BLOCKS;
    let (fx, fz) = (column.x as i32 * cb, column.z as i32 * cb);
    let overlaps = move |tile: i32, lo: i32| tile * t - 1 <= lo + cb - 1 && tile * t + t >= lo;
    let xs = (fx.div_euclid(t) - 1..=(fx + cb - 1).div_euclid(t) + 1)
        .filter(move |&tx| overlaps(tx, fx));
    xs.flat_map(move |tx| {
        (fz.div_euclid(t) - 1..=(fz + cb - 1).div_euclid(t) + 1)
            .filter(move |&tz| overlaps(tz, fz))
            .map(move |tz| [tx, tz])
    })
}

/// What a cell holds, for the water rule.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Cell {
    /// Air or a liquid block: water where the recipe has it.
    Open,
    /// Any other block (ground, ice, a placed block).
    Solid,
}

/// The mesher input of tile `tile` of `region` (see the module doc): the
/// recipe's water ranges minus the cells holding a block that is neither air
/// nor liquid, read through `chunk_at` (world sim-chunk key, as
/// `bricks::SimChunkIndex` keys it). A cell with no chunk is implied ground
/// at or below its column's surface, else air. Apron sides past the region's
/// edge are marked in `edge` and left empty.
pub fn tile_input<'a>(
    region: RegionId,
    tile: [i32; 2],
    ground: &BTreeMap<ColumnCoords, ColumnGround>,
    chunk_at: impl Fn([i32; 3]) -> Option<&'a Chunk>,
) -> WaterTileInput {
    let t = WATER_TILE_BLOCKS;
    let cb = CHUNK_BLOCKS as i32;
    let rb = REGION_BLOCKS as i32;
    let origin = [tile[0] * t, tile[1] * t];
    let mut input = WaterTileInput::empty(origin);
    input.edge = [
        origin[0] == 0,
        origin[0] + t == rb,
        origin[1] == 0,
        origin[1] + t == rb,
    ];
    let inside = |x: i32, z: i32| (0..rb).contains(&x) && (0..rb).contains(&z);
    let column_of = |x: i32, z: i32| -> Option<(&ColumnGround, usize, usize)> {
        if !inside(x, z) {
            return None;
        }
        let g = ground.get(&ColumnCoords::new((x / cb) as usize, (z / cb) as usize))?;
        Some((g, (x % cb) as usize, (z % cb) as usize))
    };
    let recipe = |x: i32, z: i32| column_of(x, z).and_then(|(g, lx, lz)| g.recipe_water(lx, lz));
    let props = game::block_props();
    // Cells `lo..=hi` of column (x, z), one chunk lookup per chunk crossed.
    let cells = |x: i32, z: i32, lo: i32, hi: i32| -> Vec<Cell> {
        let surface =
            column_of(x, z).map(|(g, lx, lz)| i32::from(g.surface[lx * CHUNK_BLOCKS + lz]));
        let (lx, lz) = ((x % cb) as usize, (z % cb) as usize);
        let mut out = Vec::with_capacity((hi - lo + 1) as usize);
        let mut y = lo;
        while y <= hi {
            let cy = y.div_euclid(cb);
            let end = hi.min(cy * cb + cb - 1);
            let key = [
                region.x * REGION_CHUNKS as i32 + x / cb,
                cy,
                region.z * REGION_CHUNKS as i32 + z / cb,
            ];
            let open = |id: BlockId| {
                if id == BlockId::AIR || props.is_liquid(id) {
                    Cell::Open
                } else {
                    Cell::Solid
                }
            };
            match chunk_at(key) {
                Some(chunk) => match chunk.blocks.uniform() {
                    Some(b) => out.extend((y..=end).map(|_| open(b.id))),
                    None => out.extend(
                        (y..=end)
                            .map(|y| open(chunk.block_at(lx, y.rem_euclid(cb) as usize, lz).id)),
                    ),
                },
                // No chunk: implied ground at or below the surface, else air.
                None => out.extend((y..=end).map(|y| {
                    if surface.is_some_and(|s| y <= s) {
                        Cell::Solid
                    } else {
                        Cell::Open
                    }
                })),
            }
            y = end + 1;
        }
        out
    };
    for z in -1..=t {
        for x in -1..=t {
            let apron_x = x < 0 || x >= t;
            let apron_z = z < 0 || z >= t;
            let (wx, wz) = (origin[0] + x, origin[1] + z);
            if (apron_x && apron_z) || !inside(wx, wz) {
                continue;
            }
            // The y span this column's cells matter over: its own water and
            // its neighbours' (side faces look at it), one above for a top.
            let own = recipe(wx, wz);
            let span = SIDES
                .iter()
                .filter_map(|[dx, dz]| recipe(wx + dx, wz + dz))
                .chain(own)
                .fold(None, |acc: Option<(i32, i32)>, (lo, hi)| {
                    Some(acc.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi))))
                });
            let Some((lo, hi)) = span else { continue };
            let mut column = WaterColumn::default();
            let cells = cells(wx, wz, lo, hi + 1);
            let at = |y: i32| cells[(y - lo) as usize];
            let mut y = lo;
            while y <= hi + 1 {
                if at(y) == Cell::Solid {
                    let start = y;
                    while y < hi + 1 && at(y + 1) == Cell::Solid {
                        y += 1;
                    }
                    column.solid.push((start as i16, y as i16));
                }
                y += 1;
            }
            if let Some((wlo, whi)) = own {
                let mut y = wlo;
                while y <= whi {
                    if at(y) == Cell::Solid {
                        y += 1;
                        continue;
                    }
                    let start = y;
                    while y < whi && at(y + 1) == Cell::Open {
                        y += 1;
                    }
                    column.runs.push(WaterRun {
                        y0: start as i16,
                        y1: y as i16,
                        solid_above: at(y + 1) == Cell::Solid,
                    });
                    y += 1;
                }
            }
            *input.column_mut(x, z) = column;
        }
    }
    input
}

/// A meshed tile as a Bevy mesh (`None` when it has no faces).
pub fn tile_mesh(data: WaterMeshData) -> Option<Mesh> {
    if data.is_empty() {
        return None;
    }
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, data.positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, data.normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, data.colors);
    mesh.insert_indices(Indices::U32(data.indices));
    Some(mesh)
}

fn drop_tile(commands: &mut Commands, tile: WaterTile) {
    if let Some(e) = tile.entity {
        commands.entity(e).try_despawn();
    }
}

fn apply_water_msgs(
    mut commands: Commands,
    mut msgs: MessageReader<WaterMsg>,
    mut tiles: ResMut<WaterTiles>,
    mut grounds: ResMut<WaterGround>,
) {
    for msg in msgs.read() {
        match msg {
            WaterMsg::Region { region, ground } => {
                remove_region(&mut commands, &mut tiles, *region);
                for tile in water_tiles(ground) {
                    tiles.0.insert(
                        (*region, tile),
                        WaterTile {
                            dirty: true,
                            ..default()
                        },
                    );
                }
                grounds.0.insert(*region, ground.clone());
            }
            WaterMsg::RemoveRegion { region } => {
                remove_region(&mut commands, &mut tiles, *region);
                grounds.0.remove(region);
            }
            WaterMsg::ChunkEdited { region, column } => {
                for tile in tiles_touching(*column) {
                    if let Some(t) = tiles.0.get_mut(&(*region, tile)) {
                        t.dirty = true;
                    }
                }
            }
        }
    }
}

fn remove_region(commands: &mut Commands, tiles: &mut WaterTiles, region: RegionId) {
    let keys: Vec<_> = tiles
        .0
        .range((region, [i32::MIN; 2])..=(region, [i32::MAX; 2]))
        .map(|(k, _)| *k)
        .collect();
    for k in keys {
        if let Some(tile) = tiles.0.remove(&k) {
            drop_tile(commands, tile);
        }
    }
}

/// Start meshes for dirty tiles of regions whose snapshot chunks have all
/// landed (their `SimChunk` mirrors are what the input reads).
fn schedule_water_meshes(
    mut tiles: ResMut<WaterTiles>,
    grounds: Res<WaterGround>,
    roots: Res<super::bridge::RegionRoots>,
    index: Option<Res<super::bricks::SimChunkIndex>>,
    chunks: Query<&super::bricks::SimChunk>,
    pending: Query<&ChildOf, With<super::bridge::PendingSnapshotVoxels>>,
) {
    if !tiles.0.values().any(|t| t.dirty && t.task.is_none()) {
        return;
    }
    let mut in_flight = tiles.0.values().filter(|t| t.task.is_some()).count();
    if in_flight >= MAX_WATER_MESH_TASKS {
        return;
    }
    let loading: BTreeSet<Entity> = pending.iter().map(ChildOf::parent).collect();
    let chunk_at = |key: [i32; 3]| -> Option<&Chunk> {
        let e = index.as_ref()?.0.get(&key)?;
        chunks.get(*e).ok().map(|c| &c.0)
    };
    let mut built = 0;
    for ((region, tile), state) in tiles.0.iter_mut() {
        if built >= WATER_TILE_BUILDS_PER_FRAME || in_flight >= MAX_WATER_MESH_TASKS {
            break;
        }
        if !state.dirty || state.task.is_some() {
            continue;
        }
        let ready = roots
            .0
            .get(region)
            .is_some_and(|root| !loading.contains(root));
        let Some(ground) = grounds.0.get(region).filter(|_| ready) else {
            continue;
        };
        let input = tile_input(*region, *tile, ground, chunk_at);
        state.task = Some(super::compute::spawn(WaterMeshJob(input), tile_mesh));
        state.dirty = false;
        built += 1;
        in_flight += 1;
    }
}

/// Swap finished meshes in.
#[allow(clippy::too_many_arguments)]
fn poll_water_meshes(
    mut commands: Commands,
    mut tiles: ResMut<WaterTiles>,
    roots: Res<super::bridge::RegionRoots>,
    mut handle: ResMut<WaterMaterialHandle>,
    meshes: Option<ResMut<Assets<Mesh>>>,
    materials: Option<ResMut<Assets<WaterMaterial>>>,
    fortress: Option<Res<super::fortress_view::FortressViewUniform>>,
) {
    let (Some(mut meshes), Some(mut materials)) = (meshes, materials) else {
        return;
    };
    let b = game::BLOCK_VOXELS as f32;
    for ((region, tile), state) in tiles.0.iter_mut() {
        let Some(task) = state.task.as_mut() else {
            continue;
        };
        let Some(mesh) = block_on(future::poll_once(task)) else {
            continue;
        };
        state.task = None;
        if let Some(old) = state.entity.take() {
            commands.entity(old).try_despawn();
        }
        let (Some(mesh), Some(&root)) = (mesh, roots.0.get(region)) else {
            continue;
        };
        let material = handle.get_or_add(
            &mut materials,
            fortress.as_deref().copied().unwrap_or_default(),
        );
        let origin = Vec3::new(
            (tile[0] * WATER_TILE_BLOCKS) as f32 * b,
            0.0,
            (tile[1] * WATER_TILE_BLOCKS) as f32 * b,
        );
        let e = commands
            .spawn((
                Name::new(format!("water {:?} {:?}", region, tile)),
                Mesh3d(meshes.add(mesh)),
                MeshMaterial3d(material),
                Transform::from_translation(origin),
                Visibility::default(),
                NotShadowCaster,
                SessionScoped,
                ChildOf(root),
            ))
            .id();
        state.entity = Some(e);
    }
}

pub struct WaterPlugin;

impl Plugin for WaterPlugin {
    fn build(&self, app: &mut App) {
        material::embed_water_shader(app);
        app.add_plugins(MaterialPlugin::<WaterMaterial>::default())
            .add_message::<WaterMsg>()
            .add_session_message_clear::<WaterMsg>()
            .add_session_reset::<WaterTiles>()
            .add_session_reset::<WaterGround>()
            .init_resource::<WaterMaterialHandle>()
            .add_systems(
                Update,
                (
                    apply_water_msgs,
                    poll_water_meshes,
                    schedule_water_meshes,
                    sync_water_material_cut,
                )
                    .chain(),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::Block;

    fn install_props() -> (BlockId, BlockId, BlockId) {
        let reg = crate::blocks::load_registry();
        game::install_block_props(reg.props());
        let id = |n: &str| reg.id_of(n).expect(n);
        (id("water"), id("stone"), id("ice"))
    }

    /// One column of ground at chunk column (0, 0): surface `surface`,
    /// recipe water up to `water_top` over block column (lx, lz) = (0, 0).
    fn ground(surface: i16, water_top: i16) -> BTreeMap<ColumnCoords, ColumnGround> {
        let mut g = ColumnGround::plain(
            vec![surface; game::COLUMN_FOOTPRINT].into_boxed_slice(),
            0,
            BlockId(1),
            BlockId(2),
        );
        g.water_top[0] = water_top;
        [(ColumnCoords::new(0, 0), g)].into_iter().collect()
    }

    /// R6.1: liquid blocks count as water, other blocks are cut out of the
    /// recipe range, and a block over the run marks it `solid_above`.
    #[test]
    fn recipe_minus_solid_blocks() {
        // Every cell below lies in chunk y 0 only with 16-block chunks.
        if CHUNK_BLOCKS < 16 {
            return;
        }
        let (water, stone, ice) = install_props();
        let (surface, top) = (2, 8);
        let g = ground(surface, top);
        let mut chunk = Chunk::uniform(Block::new(BlockId::AIR));
        let set = |c: &mut Chunk, y: i16, id| c.set_block(0, y as usize, 0, Block::new(id));
        for y in 0..=surface {
            set(&mut chunk, y, stone);
        }
        set(&mut chunk, 3, water);
        set(&mut chunk, 5, stone);
        set(&mut chunk, top, ice);
        let region = RegionId::new(0, 0);
        let input = tile_input(region, [0, 0], &g, |k| (k == [0, 0, 0]).then_some(&chunk));
        let runs = &input.column(0, 0).runs;
        assert_eq!(
            runs,
            &vec![
                WaterRun {
                    y0: 3,
                    y1: 4,
                    solid_above: true
                },
                WaterRun {
                    y0: 6,
                    y1: top - 1,
                    solid_above: true
                },
            ]
        );
        assert!(input.column(0, 0).is_solid(5));
        // The dry neighbour east (its chunk cells are air): open over the
        // whole span the pond's sides look at.
        let east = input.column(1, 0);
        assert!(east.runs.is_empty() && east.solid.is_empty(), "{east:?}");
        assert_eq!(
            input.edge,
            [
                true,
                REGION_BLOCKS as i32 == WATER_TILE_BLOCKS,
                true,
                REGION_BLOCKS as i32 == WATER_TILE_BLOCKS
            ]
        );
    }

    #[test]
    fn edits_touch_their_tile_and_the_aprons_they_lie_in() {
        let cb = CHUNK_BLOCKS;
        let t = WATER_TILE_BLOCKS as usize;
        let inner: Vec<_> = tiles_touching(ColumnCoords::new(t / cb / 2, t / cb / 2)).collect();
        assert_eq!(inner, vec![[0, 0]]);
        let edge: Vec<_> = tiles_touching(ColumnCoords::new(t / cb, 0)).collect();
        assert_eq!(edge, vec![[0, -1], [0, 0], [1, -1], [1, 0]]);
    }
}
